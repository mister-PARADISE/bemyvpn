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

thread_local! {
    /// Per-thread memo of pure per-vertex quantities (never changes results, only speed).
    static CTX: std::cell::RefCell<Ctx> = std::cell::RefCell::new(Ctx::new());
}

/// Sub-pixel offset of AA sample `k` ≥ 1: a (0,2)-sequence (Hammersley base 2 × van der Corput
/// scrambled) so that any prefix of 4, 8, 16 samples is well stratified.
#[inline]
fn aa_offset(k: u32) -> (f32, f32) {
    let k = k - 1;
    let n = 16.0;
    let x = (k as f32 + 0.5) / n;
    let y = ((k.reverse_bits() >> 28) as f32 + 0.5) / n;
    // Cranley–Patterson rotation keeps the pixel centre (sample 0) away from the set.
    ((x + 0.53).fract() - 0.5, (y + 0.29).fract() - 0.5)
}

/// Two-stage adaptive resolve of one pixel: `n` (4 or 8) stratified samples, then up to 16 when
/// the samples themselves disagree strongly (sub-pixel geometry, sun glints, foliage).
#[inline]
fn resolve_pixel(frame: &Frame, x: usize, y: usize, c0: Vec3, n: usize, exposure: f32, ctx: &mut Ctx) -> Vec3 {
    // Karis (tone-map aware) weights: bright outliers cannot dominate the average.
    let wt = |c: Vec3| 1.0 / (1.0 + (c * exposure).luminance().max(0.0));
    let w0 = wt(c0);
    let mut acc = c0 * w0;
    let mut wsum = w0;
    let (mut lo, mut hi) = (post::tm_luma(c0 * exposure), post::tm_luma(c0 * exposure));
    let order: [u32; 16] = [1, 9, 5, 13, 3, 11, 7, 15, 2, 10, 6, 14, 4, 12, 8, 16];
    let mut taken = 0usize;
    let mut target = n;
    while taken < target {
        let k = order[taken];
        let (ox, oy) = aa_offset(k);
        let c = sample(frame, x as f32 + 0.5 + ox, y as f32 + 0.5 + oy, k, ctx);
        let l = post::tm_luma(c * exposure);
        lo = lo.min(l);
        hi = hi.max(l);
        let ws = wt(c);
        acc += c * ws;
        wsum += ws;
        taken += 1;
        if taken == target && target < 16 && hi - lo > 0.18 {
            target = 16;
        }
    }
    acc / wsum
}

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
    if settings.width == 0 || settings.height == 0 {
        return (Image::new(settings.width, settings.height), st);
    }
    let t0 = std::time::Instant::now();
    let frame = Frame::new(world, cam, sun, settings);
    st.setup_s = t0.elapsed().as_secs_f64();
    let (w, h) = (settings.width as usize, settings.height as usize);
    let band = 4usize;

    // Pass 1: one sample per pixel.
    let t1 = std::time::Instant::now();
    let mut base = vec![Vec3::ZERO; w * h];
    base.par_chunks_mut(w * band).enumerate().for_each(|(ci, chunk)| {
        CTX.with(|cell| {
            let ctx = &mut *cell.borrow_mut();
            for (k, px) in chunk.iter_mut().enumerate() {
                let y = ci * band + k / w;
                let x = k % w;
                *px = sample(&frame, x as f32 + 0.5, y as f32 + 0.5, 0, ctx);
            }
        })
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
        let auto = (target / avg.max(1e-4)).powf(0.5).clamp(0.5, 3.5);
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
            CTX.with(|cell| {
                let ctx = &mut *cell.borrow_mut();
                for (k, px) in chunk.iter_mut().enumerate() {
                    let y = ci * band + k / w;
                    let x = k % w;
                    let i = y * w + x;
                    let n = counts[i] as usize;
                    let c0 = base[i];
                    *px = if n == 0 { c0 } else { resolve_pixel(&frame, x, y, c0, n, exposure, ctx) };
                }
            })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::synth::synthetic_world;

    fn small_world() -> World {
        synthetic_world(7, 30, 80, 128, 64, 2)
    }

    #[test]
    fn render_is_deterministic_across_thread_counts() {
        let w = small_world();
        let cam = Camera { pos: [30.0, 260.0, 60.0], yaw: 0.6, pitch: -0.25, fov_y_deg: 60.0 };
        let sun = Sun { azimuth_deg: 130.0, elevation_deg: 35.0 };
        let s = RenderSettings { width: 96, height: 64, ..Default::default() };
        let run = |threads: usize| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            pool.install(|| render(&w, &cam, &sun, &s))
        };
        let a = run(1);
        let b = run(4);
        assert_eq!(a.pixels.len(), 96 * 64);
        for (p, q) in a.pixels.iter().zip(b.pixels.iter()) {
            assert!(p.is_finite());
            assert_eq!(p.to_array().map(f32::to_bits), q.to_array().map(f32::to_bits));
        }
        // Well exposed: mean display luminance in a sane band.
        let rgb = post::to_srgb8(&a);
        let mean = rgb.iter().map(|&v| v as f64).sum::<f64>() / rgb.len() as f64;
        assert!(mean > 40.0 && mean < 220.0, "mean {}", mean);
    }

    #[test]
    fn diorama_and_cave_cameras_render_finite() {
        let w = small_world();
        let sun = Sun { azimuth_deg: 200.0, elevation_deg: 40.0 };
        let mode = ViewMode::Diorama { min_x: 0, max_x: 60, min_z: 40, max_z: 100, min_y: 60 };
        let cam = Camera::look_at([-120.0, 330.0, -60.0], [30.0, 150.0, 70.0], 40.0);
        let s = RenderSettings { width: 64, height: 48, mode, ..Default::default() };
        let img = render(&w, &cam, &sun, &s);
        assert!(img.pixels.iter().all(|p| p.is_finite()));
        // Camera buried inside rock / below spans / under water: no panic, finite output.
        for y in [5.0, 100.0, 139.5, 600.0] {
            let cam = Camera { pos: [31.3, y, 77.7], yaw: 2.0, pitch: 0.3, fov_y_deg: 70.0 };
            let s = RenderSettings { width: 32, height: 24, ..Default::default() };
            let img = render(&w, &cam, &sun, &s);
            assert!(img.pixels.iter().all(|p| p.is_finite()));
        }
    }

    #[test]
    fn tonemap_monotone_and_bounded() {
        let mut prev = -1.0;
        for k in 0..200 {
            let v = k as f32 * 0.05;
            let t = post::tonemap(Vec3::splat(v));
            assert!(t.x >= prev - 1e-6 && t.x <= 1.0);
            prev = t.x;
        }
        assert!(post::tonemap(Vec3::splat(0.0)).x < 0.01);
        assert!(post::tonemap(Vec3::splat(100.0)).x > 0.95);
    }
}
