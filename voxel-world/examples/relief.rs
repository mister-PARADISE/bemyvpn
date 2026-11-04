//! Dev tool: quick relief preview of a region (top-down hillshade + perspective raymarch over a
//! precomputed height grid). Usage:
//! cargo run --release --example relief -- SEED CX CZ N CELL CAMX CAMY CAMZ YAW PITCH OUT
use rayon::prelude::*;
use std::path::Path;
use voxel_world::gen::{WorldGen, SEA_LEVEL};
use voxel_world::imageio::write_png_rgb8;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let f = |i: usize, d: f64| a.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(d);
    let seed = f(0, 42.0) as u64;
    let (cx, cz) = (f(1, 0.0), f(2, 0.0));
    let n = f(3, 1024.0) as usize;
    let cell = f(4, 4.0);
    let cam = [f(5, cx), f(6, 600.0), f(7, cz - 2000.0)];
    let (yaw, pitch) = (f(8, 0.0), f(9, -0.15));
    let out = a.get(10).cloned().unwrap_or("relief".into());
    let gen = WorldGen::new(seed);
    let x0 = cx - n as f64 * cell * 0.5;
    let z0 = cz - n as f64 * cell * 0.5;
    let t = std::time::Instant::now();
    let cols: Vec<(f32, f32, [u8; 3])> = (0..n * n)
        .into_par_iter()
        .map(|i| {
            let (ix, iz) = (i % n, i / n);
            let c = gen.column_lod(x0 + ix as f64 * cell, z0 + iz as f64 * cell, cell);
            let wl = if c.water_level == i32::MIN { -1e9 } else { c.water_level as f32 };
            (c.height, wl, c.biome.map_color())
        })
        .collect();
    eprintln!("grid {}² in {:.2}s", n, t.elapsed().as_secs_f64());
    let hmax = cols.iter().map(|c| c.0).fold(0.0f32, f32::max);
    let im = cols.iter().position(|c| c.0 == hmax).unwrap();
    eprintln!("peak {:.0} at ({:.0}, {:.0})", hmax, x0 + (im % n) as f64 * cell, z0 + (im / n) as f64 * cell);
    let land = cols.iter().filter(|c| c.0 > SEA_LEVEL as f32).count();
    let mut hs: Vec<f32> = cols.iter().map(|c| c.0).collect();
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!("max {:.0}  p99 {:.0}  p90 {:.0}  p50 {:.0}  land {:.0}%", hmax, hs[hs.len() * 99 / 100], hs[hs.len() * 9 / 10], hs[hs.len() / 2], 100.0 * land as f64 / cols.len() as f64);
    let h = |ix: isize, iz: isize| -> f32 {
        let ix = ix.clamp(0, n as isize - 1) as usize;
        let iz = iz.clamp(0, n as isize - 1) as usize;
        cols[iz * n + ix].0
    };
    let sun = { let (az, el) = (2.3f32, 0.55f32); [el.cos() * az.sin(), el.sin(), el.cos() * az.cos()] };
    let shade_at = |ix: isize, iz: isize| -> f32 {
        let gx = (h(ix + 1, iz) - h(ix - 1, iz)) / (2.0 * cell as f32);
        let gz = (h(ix, iz + 1) - h(ix, iz - 1)) / (2.0 * cell as f32);
        let l = (gx * gx + 1.0 + gz * gz).sqrt();
        ((-gx * sun[0] + sun[1] - gz * sun[2]) / l).max(0.0)
    };
    let color = |ix: isize, iz: isize| -> [f32; 3] {
        let i = iz.clamp(0, n as isize - 1) as usize * n + ix.clamp(0, n as isize - 1) as usize;
        let (hh, wl, bc) = cols[i];
        if wl > hh {
            let d = ((wl - hh) / 30.0).min(1.0);
            return [0.15 - 0.08 * d, 0.35 - 0.15 * d, 0.55 - 0.1 * d];
        }
        let s = 0.25 + 0.85 * shade_at(ix, iz);
        [bc[0] as f32 / 255.0 * s, bc[1] as f32 / 255.0 * s, bc[2] as f32 / 255.0 * s]
    };
    // top-down
    let mut img = vec![0u8; n * n * 3];
    for iz in 0..n {
        for ix in 0..n {
            let c = color(ix as isize, iz as isize);
            for k in 0..3 {
                img[(iz * n + ix) * 3 + k] = (c[k].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
            }
        }
    }
    write_png_rgb8(Path::new(&format!("{out}_top.png")), n as u32, n as u32, &img).unwrap();
    // perspective
    let (w, ht) = (960usize, 540usize);
    let fov = 60f32.to_radians();
    let (sy, cy) = (yaw as f32).sin_cos();
    let (sp, cp) = (pitch as f32).sin_cos();
    let fwd = [sy * cp, sp, cy * cp];
    let right = [cy, 0.0, -sy];
    let up = [-sy * sp, cp, -cy * sp];
    let hb = |x: f32, z: f32| -> f32 {
        let gx = (x - x0 as f32) / cell as f32;
        let gz = (z - z0 as f32) / cell as f32;
        let (ix, iz) = (gx.floor(), gz.floor());
        let (fx, fz) = (gx - ix, gz - iz);
        let (ix, iz) = (ix as isize, iz as isize);
        let a = h(ix, iz) * (1.0 - fx) + h(ix + 1, iz) * fx;
        let b = h(ix, iz + 1) * (1.0 - fx) + h(ix + 1, iz + 1) * fx;
        let base = a * (1.0 - fz) + b * fz;
        let i = iz.clamp(0, n as isize - 1) as usize * n + ix.clamp(0, n as isize - 1) as usize;
        base.max(cols[i].1)
    };
    let ext = n as f32 * cell as f32;
    let pix: Vec<[u8; 3]> = (0..w * ht)
        .into_par_iter()
        .map(|p| {
            let (px, py) = (p % w, p / w);
            let u = ((px as f32 + 0.5) / w as f32 * 2.0 - 1.0) * (fov * 0.5).tan() * w as f32 / ht as f32;
            let v = (1.0 - (py as f32 + 0.5) / ht as f32 * 2.0) * (fov * 0.5).tan();
            let mut d = [fwd[0] + u * right[0] + v * up[0], fwd[1] + u * right[1] + v * up[1], fwd[2] + u * right[2] + v * up[2]];
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            for k in 0..3 { d[k] /= l; }
            let sky = [0.55 + 0.2 * (1.0 - d[1].max(0.0)), 0.7 + 0.1 * (1.0 - d[1].max(0.0)), 0.9];
            let mut t = 1.0f32;
            let mut prev = 0.0f32;
            let mut hit = None;
            while t < ext * 1.2 {
                let q = [cam[0] as f32 + d[0] * t, cam[1] as f32 + d[1] * t, cam[2] as f32 + d[2] * t];
                if q[1] < hb(q[0], q[2]) {
                    let (mut lo, mut hi) = (prev, t);
                    for _ in 0..12 {
                        let m = 0.5 * (lo + hi);
                        let q = [cam[0] as f32 + d[0] * m, cam[1] as f32 + d[1] * m, cam[2] as f32 + d[2] * m];
                        if q[1] < hb(q[0], q[2]) { hi = m } else { lo = m }
                    }
                    hit = Some(hi);
                    break;
                }
                prev = t;
                t += (t * 0.004).max(0.5);
            }
            let c = match hit {
                None => sky,
                Some(t) => {
                    let q = [cam[0] as f32 + d[0] * t, cam[2] as f32 + d[2] * t];
                    let gx = ((q[0] - x0 as f32) / cell as f32).round() as isize;
                    let gz = ((q[1] - z0 as f32) / cell as f32).round() as isize;
                    let c = color(gx, gz);
                    let fog = 1.0 - (-t / 9000.0).exp();
                    [c[0] * (1.0 - fog) + sky[0] * fog, c[1] * (1.0 - fog) + sky[1] * fog, c[2] * (1.0 - fog) + sky[2] * fog]
                }
            };
            [(c[0].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8, (c[1].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8, (c[2].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8]
        })
        .collect();
    let flat: Vec<u8> = pix.iter().flat_map(|c| c.iter().copied()).collect();
    write_png_rgb8(Path::new(&format!("{out}_persp.png")), w as u32, ht as u32, &flat).unwrap();
}
