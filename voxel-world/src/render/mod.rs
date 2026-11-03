//! CPU ray-traced renderer. OWNER: render agent.
//!
//! Pipeline of [`render`]:
//! 1. per-frame precomputation ([`shade::Frame::new`]): atmosphere LUTs (transmittance,
//!    multiple scattering, sky-view + aerial perspective), SH sky irradiance, procedural texture
//!    atlas with mip chain, far-ground statistics;
//! 2. one sample per pixel through the pixel centre (rayon over row bands, per-thread caches);
//! 3. adaptive anti-aliasing: pixels whose tone-mapped luminance differs from a neighbour by more
//!    than a threshold receive 4 or 8 extra stratified samples (with jittered sun directions → soft
//!    shadow edges), resolved with a tone-map-aware (Karis) weighted mean;
//! 4. exposure (`settings.exposure` × a calibrated base × a gentle auto-exposure factor) and a
//!    multi-scale bloom of the brightest pixels.
//!
//! The returned [`Image`] is **linear HDR with exposure already applied**; [`save_png`] only
//! tone-maps (ACES fit), converts to sRGB and dithers. Everything is deterministic: each pixel is
//! a pure function of the inputs (caches only memoise pure functions).
pub mod atmosphere;
pub mod post;
pub mod shade;
pub mod synth;
pub mod texture;
pub mod trace;
pub mod types;

pub use types::*;

use crate::math::Vec3;
use crate::world::World;
use rayon::prelude::*;
use shade::{Ctx, Frame};

/// Base exposure calibrated so that a sunlit mid-albedo surface at noon maps to mid-tones.
pub const BASE_EXPOSURE: f32 = 0.42;

/// Sub-pixel offsets for the extra AA samples (8-rook pattern, centred, in pixel units).
const AA_PATTERN: [(f32, f32); 8] = [
    (-0.4375, -0.0625),
    (-0.1875, -0.3125),
    (0.0625, -0.4375),
    (0.3125, -0.1875),
    (0.4375, 0.0625),
    (0.1875, 0.3125),
    (-0.0625, 0.4375),
    (-0.3125, 0.1875),
];

/// Statistics of the last render (for logs / benchmarks).
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub setup_s: f64,
    pub pass1_s: f64,
    pub aa_s: f64,
    pub post_s: f64,
    pub aa_pixels: usize,
    pub exposure: f32,
}

/// Render the world. Returns a linear HDR image (exposure applied).
pub fn render(world: &World, cam: &Camera, sun: &Sun, settings: &RenderSettings) -> Image {
    render_with_stats(world, cam, sun, settings).0
}

/// [`render`] plus timing statistics.
pub fn render_with_stats(world: &World, cam: &Camera, sun: &Sun, settings: &RenderSettings) -> (Image, RenderStats) {
    let mut st = RenderStats::default();
    let t0 = std::time::Instant::now();
    let frame = Frame::new(world, cam, sun, settings);
    st.setup_s = t0.elapsed().as_secs_f64();
    let (w, h) = (settings.width as usize, settings.height as usize);
    let band = 4usize;

    // Pass 1: one sample per pixel.
    let t1 = std::time::Instant::now();
    let mut base = vec![Vec3::ZERO; w * h];
    base.par_chunks_mut(w * band).enumerate().for_each(|(ci, chunk)| {
        let mut ctx = Ctx::new();
        for (k, px) in chunk.iter_mut().enumerate() {
            let y = ci * band + k / w;
            let x = k % w;
            *px = sample(&frame, x as f32 + 0.5, y as f32 + 0.5, 0, &mut ctx);
        }
    });
    st.pass1_s = t1.elapsed().as_secs_f64();

    // Exposure: base × user × gentle auto-exposure (log-average luminance, partial adaptation).
    let mut exposure = BASE_EXPOSURE * settings.exposure;
    if !matches!(settings.mode, ViewMode::Diorama { .. }) {
        let step = 7usize;
        let mut acc = 0.0f64;
        let mut n = 0.0f64;
        for i in (0..w * h).step_by(step) {
            let l = base[i].luminance().clamp(1e-4, 50.0) as f64;
            acc += l.ln();
            n += 1.0;
        }
        let avg = (acc / n.max(1.0)).exp() as f32;
        let target = 0.42f32;
        let auto = (target / avg.max(1e-4)).powf(0.5).clamp(0.5, 2.5);
        exposure *= auto;
    }
    st.exposure = exposure;

    // Pass 2: adaptive AA.
    let t2 = std::time::Instant::now();
    let mut out = vec![Vec3::ZERO; w * h];
    let mut extra_count = 0usize;
    if settings.aa {
        let luma: Vec<f32> = base.iter().map(|c| post::tm_luma(*c * exposure)).collect();
        let counts: Vec<u8> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                let c = luma[i];
                let mut m = 0.0f32;
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                        continue;
                    }
                    m = m.max((luma[yy as usize * w + xx as usize] - c).abs());
                }
                if m > 0.085 {
                    8
                } else if m > 0.035 {
                    4
                } else {
                    0
                }
            })
            .collect();
        extra_count = counts.iter().filter(|&&c| c > 0).count();
        out.par_chunks_mut(w * band).enumerate().for_each(|(ci, chunk)| {
            let mut ctx = Ctx::new();
            for (k, px) in chunk.iter_mut().enumerate() {
                let y = ci * band + k / w;
                let x = k % w;
                let i = y * w + x;
                let n = counts[i] as usize;
                let c0 = base[i];
                if n == 0 {
                    *px = c0;
                    continue;
                }
                // Karis (tone-map aware) weighted resolve.
                let wt = |c: Vec3| 1.0 / (1.0 + (c * exposure).luminance().max(0.0));
                let w0 = wt(c0);
                let mut acc = c0 * w0;
                let mut wsum = w0;
                let stride = 8 / n;
                for s in 0..n {
                    let (ox, oy) = AA_PATTERN[s * stride];
                    let c = sample(&frame, x as f32 + 0.5 + ox, y as f32 + 0.5 + oy, (s + 1) as u32, &mut ctx);
                    let ws = wt(c);
                    acc += c * ws;
                    wsum += ws;
                }
                *px = acc / wsum;
            }
        });
    } else {
        out.copy_from_slice(&base);
    }
    st.aa_s = t2.elapsed().as_secs_f64();
    st.aa_pixels = extra_count;

    let t3 = std::time::Instant::now();
    let mut img = Image { width: settings.width, height: settings.height, pixels: out };
    for p in img.pixels.iter_mut() {
        *p = if p.is_finite() { *p * exposure } else { Vec3::ZERO };
    }
    post::bloom(&mut img, 2.5, 0.06);
    st.post_s = t3.elapsed().as_secs_f64();
    (img, st)
}

#[inline]
fn sample(frame: &Frame, px: f32, py: f32, jit: u32, ctx: &mut Ctx) -> Vec3 {
    let d = frame.primary_dir(px, py);
    let s = frame.radiance(frame.cam, d, 0.0, 0, jit, ctx);
    if s.color.is_finite() {
        s.color
    } else {
        Vec3::ZERO
    }
}

/// Tone-map (ACES fit; exposure was already applied by [`render`]), convert to sRGB with light
/// dithering and write an 8-bit PNG.
pub fn save_png(img: &Image, path: &std::path::Path) -> std::io::Result<()> {
    let rgb = post::to_srgb8(img);
    crate::imageio::write_png_rgb8(path, img.width, img.height, &rgb)
}
