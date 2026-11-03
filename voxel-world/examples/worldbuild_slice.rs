//! world-build visual check: builds a small world and writes PNGs to target/agent-worldbuild/:
//! * `s{seed}_l{l0}_top.png`      — top-down map of level 0 (top material, hill-shaded, water tinted),
//!                            framed by heightfield level 1 (to check continuity at the border);
//! * `s{seed}_l{l0}_xy_{k}.png`   — vertical x–y cross-sections (one block = 2×2 px) coloured by
//!                            material: caves, strata, aquifers, water, trees, overhangs.
//!
//! usage: cargo run --release --example worldbuild_slice -- [seed] [cx cz] [l0]
use std::path::Path;
use voxel_world::gen::{WorldGen, SEA_LEVEL};
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{far_albedo, linear_to_srgb, Material};
use voxel_world::math::Vec3;
use voxel_world::world::{build_world, BuildParams, World};

fn srgb(c: Vec3) -> [u8; 3] {
    let f = |v: f32| (linear_to_srgb(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [f(c.x), f(c.y), f(c.z)]
}

/// Debug colour of a block (distinct strata colours so the cut-away is readable).
fn block_colour(w: &World, m: Material, x: i32, y: i32, z: i32) -> Vec3 {
    let (t, h) = w.l0.climate_at(x, z);
    match m {
        Material::Air => {
            let s = 0.75 + 0.25 * (y as f32 / 512.0);
            Vec3::new(0.55 * s, 0.7 * s, 0.95 * s)
        }
        Material::Water => Vec3::new(0.05, 0.18, 0.75),
        Material::Lava => Vec3::new(1.0, 0.35, 0.02),
        Material::Granite => Vec3::new(0.45, 0.2, 0.15),
        Material::Andesite => Vec3::new(0.32, 0.32, 0.36),
        Material::Calcite => Vec3::new(0.85, 0.85, 0.8),
        Material::Deepslate => Vec3::new(0.06, 0.06, 0.08),
        Material::Bedrock => Vec3::new(0.01, 0.01, 0.01),
        Material::Stone => Vec3::new(0.2, 0.2, 0.2),
        _ => far_albedo(m, t, h, y as f32),
    }
}

fn pick_centre(gen: &WorldGen) -> (i32, i32) {
    let mut best = (f32::MIN, 0, 0);
    for j in -20..=20 {
        for i in -20..=20 {
            let (x, z) = (i * 300, j * 300);
            let c = gen.column(x as f64 + 0.5, z as f64 + 0.5);
            if c.water_level != i32::MIN && c.water_level as f32 > c.height {
                continue;
            }
            let cov = gen.canopy_lod(x as f64, z as f64, &c, 1.0).coverage;
            let mut water = 0.0;
            let mut relief = 0.0f32;
            for (dx, dz) in [(120, 0), (-120, 0), (0, 120), (0, -120), (90, 90), (-90, -90), (90, -90), (-90, 90)] {
                let o = gen.column((x + dx) as f64 + 0.5, (z + dz) as f64 + 0.5);
                if o.water_level != i32::MIN && o.water_level as f32 > o.height {
                    water = 1.0;
                }
                relief = relief.max((o.height - c.height).abs());
            }
            let score = cov + water + (relief / 60.0).min(1.5) + c.jaggedness * 0.03;
            if score > best.0 {
                best = (score, x, z);
            }
        }
    }
    (best.1, best.2)
}

fn main() {
    let a: Vec<i64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let seed = *a.first().unwrap_or(&42) as u64;
    let gen = WorldGen::new(seed);
    let (cx, cz) = if a.len() >= 3 { (a[1] as i32, a[2] as i32) } else { pick_centre(&gen) };
    let l0 = a.get(3).map(|&v| v as u32).unwrap_or(384);
    let p = BuildParams {
        center_x: cx,
        center_z: cz,
        l0_size: l0,
        hf_size: l0.max(256),
        hf_levels: 2,
        cave_radius: l0 / 2,
        feature_radius: l0 / 2,
    };
    eprintln!("seed {seed} centre ({cx},{cz}) l0 {l0}");
    let w = build_world(&gen, &p);
    let dir = Path::new("target/agent-worldbuild");
    let n = l0 as i32;
    let (ox, oz) = (w.l0.origin_x, w.l0.origin_z);

    // ---- top-down map: level 0 inside, level 1 around it (2 px per block)
    let lv = &w.levels[0];
    let margin = 64;
    let side = n + 2 * margin;
    let sc = 2;
    let wpx = (side * sc) as u32;
    let mut img = vec![0u8; (wpx * wpx * 3) as usize];
    let sun = Vec3::new(-0.6, 0.65, -0.45).normalize();
    let top_at = |x: i32, z: i32| -> f32 {
        if w.l0.contains(x, z) {
            w.l0.top(x, z) as f32
        } else {
            let (i, j) = lv.cell_of(x as f64 + 0.5, z as f64 + 0.5);
            let q = lv.index(i, j);
            lv.h[q].max(lv.water[q])
        }
    };
    for pz in 0..side {
        for px in 0..side {
            let (x, z) = (ox - margin + px, oz - margin + pz);
            let (col, water, lvl0) = if w.l0.contains(x, z) {
                let s = w.l0.spans(x, z);
                let top = *s.last().unwrap();
                let wet = top.mat == Material::Water;
                let ground = s.iter().rev().find(|s| s.mat.is_opaque()).map(|s| s.mat).unwrap_or(Material::Stone);
                (block_colour(&w, if wet { ground } else { top.mat }, x, top.y1 as i32 - 1, z), wet, true)
            } else {
                let (i, j) = lv.cell_of(x as f64 + 0.5, z as f64 + 0.5);
                let q = lv.index(i, j);
                (Vec3::from_array(lv.albedo[q]), lv.water[q] > lv.h[q], false)
            };
            let gx = top_at(x + 1, z) - top_at(x - 1, z);
            let gz = top_at(x, z + 1) - top_at(x, z - 1);
            let nrm = Vec3::new(-gx * 0.5, 1.0, -gz * 0.5).normalize();
            let mut c = col * (0.35 + 0.9 * nrm.dot(sun).max(0.0));
            if water {
                c = c * 0.35 + Vec3::new(0.03, 0.1, 0.4);
            }
            if !lvl0 {
                c = c * 0.85;
            }
            let rgb = srgb(c);
            for dy in 0..sc {
                for dx in 0..sc {
                    let o = (((pz * sc + dy) as u32 * wpx + (px * sc + dx) as u32) * 3) as usize;
                    img[o..o + 3].copy_from_slice(&rgb);
                }
            }
        }
    }
    let path = dir.join(format!("s{seed}_l{l0}_top.png"));
    write_png_rgb8(&path, wpx, wpx, &img).unwrap();
    eprintln!("wrote {}", path.display());

    // ---- vertical x–y sections (2 px per block), y from the lowest interesting level
    let max_top = (0..n * n).map(|k| w.l0.top[k as usize]).max().unwrap() as i32;
    let y1 = (max_top + 12).min(512);
    let y0 = 0;
    let hpx = (y1 - y0) as u32 * 2;
    for (k, dz) in [-n / 4, -n / 12, n / 12, n / 4].iter().enumerate() {
        let z = cz + dz;
        let wpx = n as u32 * 2;
        let mut img = vec![0u8; (wpx * hpx * 3) as usize];
        for py in 0..(y1 - y0) {
            let y = y1 - 1 - py;
            for px in 0..n {
                let x = ox + px;
                let m = w.l0.block(x, y, z);
                let mut c = block_colour(&w, m, x, y, z);
                if y == SEA_LEVEL && m == Material::Air {
                    c = c * 0.8;
                }
                let rgb = srgb(c);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let o = (((py * 2 + dy) as u32 * wpx + (px * 2 + dx) as u32) * 3) as usize;
                        img[o..o + 3].copy_from_slice(&rgb);
                    }
                }
            }
        }
        let path = dir.join(format!("s{seed}_l{l0}_xy_{k}.png"));
        write_png_rgb8(&path, wpx, hpx, &img).unwrap();
        eprintln!("wrote {} (z = {z}, y {y0}..{y1})", path.display());
    }
}
