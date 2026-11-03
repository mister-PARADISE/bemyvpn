//! gen-core review: false-colour maps of climate fields (orogeny M, effective erosion,
//! humidity, rain shadow, wind) + height over a large square.
//! cargo run --release --example gencore_review_fields -- --seed 42 [--half 20000] [--px 800]
use rayon::prelude::*;
use voxel_world::gen::{WorldGen, SEA_LEVEL};
use voxel_world::imageio::write_png_rgb8;
fn arg<T: std::str::FromStr>(args: &[String], name: &str, def: T) -> T {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(def)
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = arg(&args, "--seed", 42);
    let half: f64 = arg(&args, "--half", 20000.0);
    let px: usize = arg(&args, "--px", 800);
    let g = WorldGen::new(seed);
    let step = 2.0 * half / px as f64;
    let data: Vec<[u8; 9]> = (0..px * px)
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % px, k / px);
            let x = -half + (i as f64 + 0.5) * step;
            let z = -half + (j as f64 + 0.5) * step;
            let cd = g.climate.sample_d(x, z, step);
            let col = g.column_lod(x, z, step);
            let land = col.height >= SEA_LEVEL as f32;
            let m = (cd.orogeny.v * 255.0) as u8;
            let h = ((col.height - 60.0) / 440.0 * 255.0).clamp(0.0, 255.0) as u8;
            let hu = ((cd.h.v + 1.0) * 127.0) as u8;
            let sh = (cd.shadow * 255.0 * 2.0).min(255.0) as u8;
            let e = ((cd.e.v + 1.0) * 127.0) as u8;
            [
                if land { m } else { m / 3 }, if land { h } else { 0 }, if land { 60 } else { 120 },
                if land { 255 - hu } else { 30 }, if land { hu } else { 30 }, sh,
                e, e, if land { e } else { 255 },
            ]
        })
        .collect();
    for (t, name) in ["orogeny", "humidity", "erosion"].iter().enumerate() {
        let mut buf = vec![0u8; px * px * 3];
        for k in 0..px * px {
            buf[k * 3..k * 3 + 3].copy_from_slice(&data[k][t * 3..t * 3 + 3]);
        }
        // wind arrows
        if *name == "humidity" {
            let n = 20;
            for a in 0..n {
                for b in 0..n {
                    let ci = (a as f64 + 0.5) * px as f64 / n as f64;
                    let cj = (b as f64 + 0.5) * px as f64 / n as f64;
                    let (wx, wz) = g.climate.wind(-half + ci * step, -half + cj * step);
                    for s in 0..18 {
                        let ii = (ci + wx as f64 * s as f64) as usize;
                        let jj = (cj + wz as f64 * s as f64) as usize;
                        if ii < px && jj < px {
                            let c = if s > 13 { [255, 255, 0] } else { [255, 255, 255] };
                            buf[(jj * px + ii) * 3..(jj * px + ii) * 3 + 3].copy_from_slice(&c);
                        }
                    }
                }
            }
        }
        let p = format!("target/agent-gencore-review/fields_seed{seed}_{name}.png");
        write_png_rgb8(std::path::Path::new(&p), px as u32, px as u32, &buf).unwrap();
        println!("wrote {p}");
    }
}
