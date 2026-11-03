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
    let mut leaks = 0usize;
    for (si, dzs) in zs.iter().enumerate() {
        let mut state = vec![0u8; w * hgt];
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
                        state[py * w + px] = 1 + fill as u8;
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
                if !solid {
                    state[py * w + px] = 9;
                }
                let row = si * (hgt + gap) + py;
                let o = (row * w + px) * 3;
                img[o..o + 3].copy_from_slice(&c);
            }
        }
        for py in 0..hgt {
            for px in 1..w {
                let (a, b) = (state[py * w + px - 1], state[py * w + px]);
                if (a == 2 && b == 1) || (a == 1 && b == 2) {
                    leaks += 1;
                    if leaks < 6 {
                        let x = cx - 256.0 + px as f64;
                        let y = (hgt - 1 - py) as f64 + 0.5;
                        let z = cz + dzs + 0.5;
                        eprintln!("leak at x={x} y={y} z={z}: wt(x-1)={:?} wt(x)={:?} wt(x+1)={:?}", g.caves.water_table(x - 0.5, z), g.caves.water_table(x + 0.5, z), g.caves.water_table(x + 1.5, z));
                    }
                }
            }
        }
    }
    eprintln!("water/air side contacts inside caves: {leaks}");
    eprintln!("slice carve fraction {:.2}%", 100.0 * carved as f64 / under.max(1) as f64);
    let p = out_dir().join(format!("seed{seed}_cave_slices.png"));
    write_png_rgb8(&p, w as u32, total_h as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

/// X-ray plan views: for y-ranges, the highest carved point of each (x, z), coloured by the
/// dominant component and shaded by height — shows the network topology.
fn caves_plan(g: &WorldGen, seed: u64) {
    let (cx, cz) = find_spot(g, 230.0, 300.0, (3000.0, -2000.0));
    let w = 512usize;
    let ranges = [(20.0, 70.0), (70.0, 120.0), (120.0, 170.0), (170.0, 230.0)];
    let gap = 6;
    let total_w = ranges.len() * (w + gap);
    let mut img = vec![255u8; total_w * w * 3];
    let cols: Vec<ColumnInfo> = (0..(w / 2) * (w / 2))
        .map(|i| {
            let (ix, iz) = (i % (w / 2), i / (w / 2));
            g.column(cx - 256.0 + ix as f64 * 2.0 + 1.0, cz - 256.0 + iz as f64 * 2.0 + 1.0)
        })
        .collect();
    for (si, &(y0, y1)) in ranges.iter().enumerate() {
        for pz in 0..w {
            for px in 0..w {
                let col = &cols[(pz / 2) * (w / 2) + px / 2];
                let x = cx - 256.0 + px as f64 + 0.5;
                let z = cz - 256.0 + pz as f64 + 0.5;
                let mut c = [110u8, 110, 110];
                if y0 as f32 > col.height {
                    c = [200, 225, 245];
                } else {
                    let mut y = (y1 as f32).min(col.height) as f64;
                    while y >= y0 {
                        let p = g.caves.parts(x, y, z, col);
                        if p.total > 0.0 {
                            let v = [p.cheese, p.tunnels, p.shafts, p.ravine];
                            let k = (0..4).fold(0, |b, i| if v[i] > v[b] { i } else { b });
                            let base: [f32; 3] = match k {
                                0 => [190.0, 60.0, 40.0],
                                1 => [40.0, 60.0, 200.0],
                                2 => [40.0, 190.0, 80.0],
                                _ => [220.0, 170.0, 30.0],
                            };
                            let t = 0.35 + 0.65 * ((y - y0) / (y1 - y0)) as f32;
                            let fill = g.cave_fill(x as i32, y as i32, z as i32, col);
                            c = [(base[0] * t) as u8, (base[1] * t) as u8, (base[2] * t) as u8];
                            if fill == CaveFill::Water {
                                c = [c[0] / 3, c[1] / 2 + 60, 255];
                            }
                            break;
                        }
                        y -= 2.0;
                    }
                }
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
