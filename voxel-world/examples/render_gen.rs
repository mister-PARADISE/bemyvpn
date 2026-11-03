//! Renderer test on the *real* gen-core terrain with a simplified world adapter (no caves, no
//! trees, density band ignored): level-0 columns from `gen.column` + `surface_material`,
//! heightfields from `column_lod`. Used to judge shading on real data while the world builder
//! is being written by another agent.
//!
//! usage: cargo run --release --example render_gen -- [seed] [cx cz] [width height] [l0] [yaws…]
use rayon::prelude::*;
use std::time::Instant;
use voxel_world::gen::{WorldGen, SEA_LEVEL};
use voxel_world::materials::{self, Material};
use voxel_world::render::{render_with_stats, save_png, Camera, RenderSettings, Sun, ViewMode};
use voxel_world::world::{BuildParams, HeightLevel, HfCell, Level0, Level0Row, Span, World};

fn build(gen: &WorldGen, p: BuildParams) -> World {
    let (ox, oz) = p.l0_origin();
    let n = p.l0_size as i32;
    let rows: Vec<Level0Row> = (0..n)
        .into_par_iter()
        .map(|j| {
            let mut row = Level0Row::with_capacity(n as usize);
            let mut spans: Vec<Span> = Vec::with_capacity(16);
            for i in 0..n {
                let (x, z) = (ox + i, oz + j);
                let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                let top = (col.height.floor() as i32 + 1).clamp(1, 511);
                spans.clear();
                let deep = (top - 6).max(0);
                if deep > 0 {
                    spans.push(Span::new(0, deep, Material::Stone));
                }
                for y in deep..top {
                    let m = gen.surface_material(x, y, z, top - 1 - y, &col);
                    match spans.last_mut() {
                        Some(s) if s.mat == m && s.y1 as i32 == y => s.y1 += 1,
                        _ => spans.push(Span::new(y, y + 1, m)),
                    }
                }
                if col.water_level > top {
                    let liquid = if col.biome == voxel_world::gen::Biome::Volcanic && col.water_level > SEA_LEVEL { Material::Lava } else { Material::Water };
                    spans.push(Span::new(top, col.water_level, liquid));
                }
                spans.retain(|s| s.mat != Material::Air && s.y1 > s.y0);
                row.push_column(&spans, col.biome, col.surface_temperature, col.climate.humidity);
            }
            row
        })
        .collect();
    let l0 = Level0::from_rows(ox, oz, p.l0_size, rows);
    let levels = (1..=p.hf_levels)
        .map(|k| {
            let (lx, lz) = p.level_origin(k);
            let cell = (1u32 << k) as f64;
            HeightLevel::from_fn(k, lx, lz, p.hf_size, |x, z| {
                let col = gen.column_lod(x, z, cell);
                let top = col.height.floor() as i32;
                let mat = gen.surface_material(x.floor() as i32, top, z.floor() as i32, 0, &col);
                let albedo = materials::far_albedo(mat, col.surface_temperature, col.climate.humidity, col.height);
                let water = if col.water_level as f32 > col.height { col.water_level as f32 } else { f32::NEG_INFINITY };
                HfCell { h: col.height, water, albedo, mat, biome: col.biome }
            })
        })
        .collect();
    World { seed: gen.seed, params: p, l0, levels }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let g = |i: usize, d: f64| a.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(d);
    let seed = g(0, 1.0) as u64;
    let (cx, cz) = (g(1, 0.0) as i32, g(2, 0.0) as i32);
    let (w, h) = (g(3, 960.0) as u32, g(4, 540.0) as u32);
    let l0 = g(5, 1024.0) as u32;
    let yaws: Vec<f64> = if a.len() > 6 { a[6..].iter().filter_map(|s| s.parse().ok()).collect() } else { vec![0.0, 90.0, 180.0, 270.0] };
    let gen = WorldGen::new(seed);
    // Camera: highest column within ±300 blocks of the requested centre.
    let mut best = (f32::MIN, cx, cz);
    for j in (-300..=300).step_by(12) {
        for i in (-300..=300).step_by(12) {
            let c = gen.column((cx + i) as f64, (cz + j) as f64);
            if c.height > best.0 {
                best = (c.height, cx + i, cz + j);
            }
        }
    }
    let p = BuildParams { center_x: best.1, center_z: best.2, l0_size: l0, hf_size: 2048, hf_levels: 3, cave_radius: 0, feature_radius: 0 };
    let t = Instant::now();
    let world = build(&gen, p);
    eprintln!("adapter build {:.1}s, camera column {:?}", t.elapsed().as_secs_f64(), best);
    for yaw in yaws {
        let cam = Camera { pos: [best.1 as f64 + 0.5, best.0 as f64 + 25.0, best.2 as f64 + 0.5], yaw: yaw.to_radians(), pitch: (-8f64).to_radians(), fov_y_deg: 60.0 };
        let sun = Sun { azimuth_deg: yaw + 50.0, elevation_deg: 28.0 };
        let s = RenderSettings { width: w, height: h, mode: ViewMode::Normal, ..Default::default() };
        let (img, st) = render_with_stats(&world, &cam, &sun, &s);
        let path = format!("target/agent-render/gen_s{}_{}_{}_y{}.png", seed, cx, cz, yaw as i32);
        save_png(&img, std::path::Path::new(&path)).unwrap();
        eprintln!("yaw {yaw}: pass1 {:.2}s aa {:.2}s -> {path}", st.pass1_s, st.aa_s);
    }
}
