//! Debug views for caves + features (owner: caves-features).
//!
//! `cargo run --release --example cavesfeatures_view -- [caves|plan|features|trees|all] [seed]`
//! Writes PNGs into target/agent-cavesfeatures/.
use std::path::PathBuf;
use voxel_world::gen::{CaveFill, ColumnInfo, FeatureKind, WorldGen, SEA_LEVEL};
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{linear_to_srgb, Material};

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/agent-cavesfeatures");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn srgb(c: voxel_world::math::Vec3) -> [u8; 3] {
    [
        (linear_to_srgb(c.x) * 255.0).clamp(0.0, 255.0) as u8,
        (linear_to_srgb(c.y) * 255.0).clamp(0.0, 255.0) as u8,
        (linear_to_srgb(c.z) * 255.0).clamp(0.0, 255.0) as u8,
    ]
}

/// Find a land spot whose height lies in [lo, hi].
fn find_spot(g: &WorldGen, lo: f32, hi: f32, start: (f64, f64)) -> (f64, f64) {
    for r in 0..400 {
        for k in 0..(8 * r.max(1)) {
            let a = k as f64 / (8 * r.max(1)) as f64 * std::f64::consts::TAU;
            let x = start.0 + (r as f64 * 200.0) * a.cos();
            let z = start.1 + (r as f64 * 200.0) * a.sin();
            let c = g.column(x, z);
            if c.height >= lo && c.height <= hi && c.water_level == i32::MIN {
                return (x.floor(), z.floor());
            }
        }
    }
    start
}

/// Lattice-interpolated cave field sampler (emulates the world builder: 4×4×4 lattice).
struct Lattice<'a> {
    g: &'a WorldGen,
}
impl<'a> Lattice<'a> {
    fn field(&self, cache: &mut std::collections::HashMap<(i32, i32), ColumnInfo>, x: f64, y: f64, z: f64) -> f32 {
        let lx = (x / 4.0).floor() as i32;
        let ly = (y / 4.0).floor() as i32;
        let lz = (z / 4.0).floor() as i32;
        let fx = (x / 4.0 - lx as f64) as f32;
        let fy = (y / 4.0 - ly as f64) as f32;
        let fz = (z / 4.0 - lz as f64) as f32;
        let mut v = [0.0f32; 8];
        for (i, vv) in v.iter_mut().enumerate() {
            let (dx, dy, dz) = ((i & 1) as i32, ((i >> 1) & 1) as i32, ((i >> 2) & 1) as i32);
            let key = (lx + dx, lz + dz);
            let col = *cache.entry(key).or_insert_with(|| self.g.column(key.0 as f64 * 4.0, key.1 as f64 * 4.0));
            *vv = self.g.cave_field(key.0 as f64 * 4.0, (ly + dy) as f64 * 4.0, key.1 as f64 * 4.0, &col);
        }
        let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = l(v[0], v[1], fx);
        let x10 = l(v[2], v[3], fx);
        let x01 = l(v[4], v[5], fx);
        let x11 = l(v[6], v[7], fx);
        l(l(x00, x10, fy), l(x01, x11, fy), fz)
    }
}

fn caves_slices(g: &WorldGen, seed: u64) {
    let (cx, cz) = find_spot(g, 230.0, 300.0, (3000.0, -2000.0));
    eprintln!("cave slices around ({cx}, {cz})");
    let w = 512usize;
    let hgt = 300usize;
    let zs = [0.0, 37.0, 91.0, 160.0];
    let gap = 6;
    let total_h = zs.len() * (hgt + gap);
    let mut img = vec![0u8; w * total_h * 3];
    let lat = Lattice { g };
    let mut carved = 0usize;
    let mut under = 0usize;
    for (si, dzs) in zs.iter().enumerate() {
        let z = cz + dzs + 0.5;
        let mut cache = std::collections::HashMap::new();
        for px in 0..w {
            let x = cx - 256.0 + px as f64 + 0.5;
            let col = g.column(x, z);
            for py in 0..hgt {
                let y = (hgt - 1 - py) as f64 + 0.5;
                let solid = g.density(x, y, z, &col) > 0.0;
                let mut c: [u8; 3];
                if !solid {
                    c = if (y as i32) < col.water_level { [40, 80, 200] } else { [170, 205, 240] };
                } else {
                    under += 1;
                    let f = lat.field(&mut cache, x, y, z);
                    if f > 0.0 {
                        carved += 1;
                        let fill = g.cave_fill(x.floor() as i32, y.floor() as i32, z.floor() as i32, &col);
                        c = match fill {
                            CaveFill::Air => [18, 16, 20],
                            CaveFill::Water => [30, 70, 190],
                            CaveFill::Lava => [250, 110, 20],
                        };
                    } else {
                        let d = (col.height - y as f32).max(0.0);
                        let m = g.surface_material(x as i32, y as i32, z as i32, d as i32, &col);
                        c = srgb(m.albedo());
                        if m == Material::Grass {
                            c = [90, 150, 60];
                        }
                        if f > -1.5 {
                            // wall halo to see the field's smoothness
                            c = [c[0] / 2 + 40, c[1] / 2 + 30, c[2] / 2 + 20];
                        }
                    }
                }
                let row = si * (hgt + gap) + py;
                let o = (row * w + px) * 3;
                img[o..o + 3].copy_from_slice(&c);
            }
        }
    }
    eprintln!("slice carve fraction {:.2}%", 100.0 * carved as f64 / under.max(1) as f64);
    let p = out_dir().join(format!("seed{seed}_cave_slices.png"));
    write_png_rgb8(&p, w as u32, total_h as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

/// Horizontal (plan) sections of the raw cave field at several y, coloured by component.
fn caves_plan(g: &WorldGen, seed: u64) {
    let (cx, cz) = find_spot(g, 230.0, 300.0, (3000.0, -2000.0));
    let w = 512usize;
    let ys = [40.0, 80.0, 120.0, 170.0];
    let gap = 6;
    let total_w = ys.len() * (w + gap);
    let mut img = vec![255u8; total_w * w * 3];
    // columns once (sampled every 2 blocks, nearest)
    let cols: Vec<ColumnInfo> = (0..(w / 2) * (w / 2))
        .map(|i| {
            let (ix, iz) = (i % (w / 2), i / (w / 2));
            g.column(cx - 256.0 + ix as f64 * 2.0 + 1.0, cz - 256.0 + iz as f64 * 2.0 + 1.0)
        })
        .collect();
    for (si, &y) in ys.iter().enumerate() {
        for pz in 0..w {
            for px in 0..w {
                let col = &cols[(pz / 2) * (w / 2) + px / 2];
                let x = cx - 256.0 + px as f64 + 0.5;
                let z = cz - 256.0 + pz as f64 + 0.5;
                let c = if (y as f32) > col.height {
                    [200, 225, 245]
                } else {
                    let p = g.caves.parts(x, y, z, col);
                    if p.total > 0.0 {
                        let m = p.cheese.max(p.tunnels).max(p.shafts).max(p.ravine);
                        if m == p.cheese {
                            [150, 40, 30]
                        } else if m == p.tunnels {
                            [20, 20, 30]
                        } else if m == p.shafts {
                            [30, 140, 60]
                        } else {
                            [160, 120, 20]
                        }
                    } else {
                        let v = (120.0 + p.total.clamp(-20.0, 0.0) * 2.0) as u8;
                        [v, v, v]
                    }
                };
                let o = (pz * total_w + si * (w + gap) + px) * 3;
                img[o..o + 3].copy_from_slice(&c);
            }
        }
    }
    let p = out_dir().join(format!("seed{seed}_cave_plan.png"));
    write_png_rgb8(&p, total_w as u32, w as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("all").to_string();
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    let _ = (FeatureKind::Oak, SEA_LEVEL);
    if mode == "caves" || mode == "all" {
        caves_slices(&g, seed);
    }
    if mode == "plan" || mode == "all" {
        caves_plan(&g, seed);
    }
}
