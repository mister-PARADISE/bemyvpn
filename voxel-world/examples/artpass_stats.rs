//! Art-pass statistics: plains fraction, mountain shape, badlands neighbourhood, biome mix.
//! `cargo run --release --example artpass_stats -- <seed> [half_extent]`
use rayon::prelude::*;
use voxel_world::gen::{Biome, WorldGen, BIOME_COUNT, SEA_LEVEL};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let half: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10_000.0);
    let g = WorldGen::new(seed);
    let sea = SEA_LEVEL as f32;

    // ---- relief over 200 m windows (9×9 samples at 25 m)
    let step = 25.0;
    let n = (2.0 * half / step) as usize;
    let t0 = std::time::Instant::now();
    let grid: Vec<(f32, bool, Biome, f32)> = (0..n * n)
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % n, k / n);
            let x = -half + i as f64 * step + 0.5;
            let z = -half + j as f64 * step + 0.5;
            let c = g.column(x, z);
            let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
            (c.height, wet, c.biome, c.slope)
        })
        .collect();
    let us = t0.elapsed().as_secs_f64() * 1e6 * 4.0 / (n * n) as f64;
    let (mut land, mut flat, mut flat12) = (0usize, 0usize, 0usize);
    let w = 4usize;
    for j in (w..n - w).step_by(3) {
        for i in (w..n - w).step_by(3) {
            let (h, wet, _, _) = grid[j * n + i];
            if wet || h < sea {
                continue;
            }
            land += 1;
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for dj in 0..=2 * w {
                for di in 0..=2 * w {
                    let (hh, wt, _, _) = grid[(j + dj - w) * n + (i + di - w)];
                    let hh = if wt { hh.max(sea) } else { hh };
                    lo = lo.min(hh);
                    hi = hi.max(hh);
                }
            }
            if hi - lo < 6.0 {
                flat += 1;
            }
            if hi - lo < 12.0 {
                flat12 += 1;
            }
        }
    }
    eprintln!(
        "seed {seed}: flat (<6 m relief over 200 m) {:.1}% of land, <12 m {:.1}%  (column ≈ {us:.2} µs/core)",
        100.0 * flat as f64 / land as f64,
        100.0 * flat12 as f64 / land as f64
    );

    // ---- mountains: high flat caps
    let (mut high, mut high_flat, mut very_high) = (0usize, 0usize, 0usize);
    for &(h, _, _, s) in &grid {
        if h > 380.0 {
            high += 1;
            if s < 0.2 {
                high_flat += 1;
            }
        }
        if h > 455.0 {
            very_high += 1;
        }
    }
    eprintln!(
        "high (>380) {:.2}% of samples, of which flat (slope<0.2) {:.1}%; >455: {:.3}%",
        100.0 * high as f64 / grid.len() as f64,
        100.0 * high_flat as f64 / high.max(1) as f64,
        100.0 * very_high as f64 / grid.len() as f64
    );

    // ---- biome mix and badlands neighbourhood
    let mut cnt = [0usize; BIOME_COUNT];
    for &(_, _, b, _) in &grid {
        cnt[b as usize] += 1;
    }
    let total: usize = cnt.iter().sum();
    let mut line = String::new();
    for (i, &k) in cnt.iter().enumerate() {
        if k > 0 {
            line += &format!("{}:{:.1} ", Biome::from_u8(i as u8).name(), 100.0 * k as f64 / total as f64);
        }
    }
    eprintln!("{line}");
    let (mut bad, mut green) = (0usize, 0usize);
    let r = 12usize; // 300 m
    for j in (r..n - r).step_by(2) {
        for i in (r..n - r).step_by(2) {
            if !matches!(grid[j * n + i].2, Biome::Badlands | Biome::Hoodoos) {
                continue;
            }
            for (di, dj) in [(r as i64, 0i64), (-(r as i64), 0), (0, r as i64), (0, -(r as i64))] {
                let b = grid[((j as i64 + dj) as usize) * n + (i as i64 + di) as usize].2;
                bad += 1;
                if matches!(b, Biome::Forest | Biome::DarkForest | Biome::BirchForest | Biome::Jungle | Biome::Taiga | Biome::OldGrowthTaiga | Biome::Swamp | Biome::Meadow) {
                    green += 1;
                }
            }
        }
    }
    eprintln!("badlands 300 m neighbours that are green forest/meadow: {:.1}% ({bad})", 100.0 * green as f64 / bad.max(1) as f64);
}
