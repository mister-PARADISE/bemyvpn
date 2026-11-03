//! gen-core review: contract metrics over a large area (DESIGN.md §1–3, §2a).
//!
//! cargo run --release --example gencore_review_stats -- [--seeds 1,42,2026] [--half 20000] [--step 32] [--fp 0]
//!   [--img target/agent-gencore-review]
use rayon::prelude::*;
use voxel_world::gen::{Biome, ColumnInfo, WorldGen, BIOME_COUNT, SEA_LEVEL};
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::Material;

fn arg<T: std::str::FromStr>(args: &[String], name: &str, def: T) -> T {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(def)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds: String = arg(&args, "--seeds", "1,42,2026".to_string());
    let half: f64 = arg(&args, "--half", 20_000.0);
    let step: f64 = arg(&args, "--step", 32.0);
    let fp: f64 = arg(&args, "--fp", 0.0);
    let img: String = arg(&args, "--img", String::new());
    let sea = SEA_LEVEL as f32;
    for seed in seeds.split(',').map(|s| s.parse::<u64>().unwrap()) {
        let g = WorldGen::new(seed);
        let n = (2.0 * half / step) as usize;
        let t0 = std::time::Instant::now();
        let cols: Vec<ColumnInfo> = (0..n)
            .into_par_iter()
            .flat_map_iter(|j| {
                let g = &g;
                (0..n).map(move |i| g.column_lod(-half + (i as f64 + 0.5) * step, -half + (j as f64 + 0.5) * step, fp))
            })
            .collect();
        let dt = t0.elapsed().as_secs_f64();
        let land: Vec<&ColumnInfo> = cols.iter().filter(|c| c.height >= sea).collect();
        let nl = land.len().max(1) as f64;
        let mtn = land.iter().filter(|c| c.height > sea + 150.0).count() as f64 / nl;
        let high = land.iter().filter(|c| c.height > 430.0).count();
        let hmax = cols.iter().map(|c| c.height).fold(0.0f32, f32::max);
        // top material: snow below the snow line?
        let mut snow_low = 0usize;
        let mut snow_any = 0usize;
        let mut snow_hist = [0usize; 12];
        for c in &land {
            let (x, z) = (c.x.floor() as i32, c.z.floor() as i32);
            let y = c.height.floor() as i32;
            let m = g.surface_material(x, y, z, 0, c);
            if matches!(m, Material::Snow | Material::PackedIce | Material::Ice) {
                snow_any += 1;
                if c.height < 300.0 {
                    snow_low += 1;
                }
                snow_hist[((c.height as usize) / 50).min(11)] += 1;
            }
        }
        let mut counts = [0usize; BIOME_COUNT];
        for c in &cols {
            counts[c.biome as usize] += 1;
        }
        let water_cols = cols.iter().filter(|c| c.water_level > c.height as i32).count();
        let lakes = cols.iter().filter(|c| c.water_level > SEA_LEVEL && c.water_level > c.height as i32 && c.biome != Biome::Volcanic).count();
        // mountain connected components (H > SEA+150) — 4-neighbour flood fill
        let mut lab = vec![0u32; n * n];
        let mut comps: Vec<(usize, f64, f32)> = Vec::new(); // (cells, extent km, top)
        let mut next = 1u32;
        for k in 0..n * n {
            if lab[k] != 0 || cols[k].height <= sea + 150.0 {
                continue;
            }
            let mut stack = vec![k];
            lab[k] = next;
            let mut cells = Vec::new();
            while let Some(p) = stack.pop() {
                cells.push(p);
                let (i, j) = (p % n, p / n);
                let nb = [(i.wrapping_sub(1), j), (i + 1, j), (i, j.wrapping_sub(1)), (i, j + 1)];
                for (a, b) in nb {
                    if a < n && b < n {
                        let q = b * n + a;
                        if lab[q] == 0 && cols[q].height > sea + 150.0 {
                            lab[q] = next;
                            stack.push(q);
                        }
                    }
                }
            }
            // principal-axis length (2·√(3λ_max)·step)
            let m = cells.len() as f64;
            let (mut mx, mut mz) = (0.0, 0.0);
            for &p in &cells {
                mx += (p % n) as f64;
                mz += (p / n) as f64;
            }
            mx /= m;
            mz /= m;
            let (mut sxx, mut szz, mut sxz) = (0.0, 0.0, 0.0);
            for &p in &cells {
                let (dx, dz) = ((p % n) as f64 - mx, (p / n) as f64 - mz);
                sxx += dx * dx;
                szz += dz * dz;
                sxz += dx * dz;
            }
            sxx /= m;
            szz /= m;
            sxz /= m;
            let tr = sxx + szz;
            let det = sxx * szz - sxz * sxz;
            let l1 = tr / 2.0 + ((tr * tr / 4.0 - det).max(0.0)).sqrt();
            let top = cells.iter().map(|&p| cols[p].height).fold(0.0f32, f32::max);
            comps.push((cells.len(), 2.0 * (3.0 * l1).sqrt() * step / 1000.0, top));
            next += 1;
        }
        comps.sort_by(|a, b| b.0.cmp(&a.0));
        let big: Vec<String> = comps.iter().take(8).map(|(c, l, t)| format!("{:.1}km²/{:.1}km/{:.0}", *c as f64 * step * step / 1e6, l, t)).collect();
        let small = comps.iter().filter(|c| (c.0 as f64) * step * step < 1.0e6).count();
        println!(
            "seed {seed}: {} cols in {dt:.1}s | land {:.1}% | mountainous(H>{}) {:.1}% of land | H>430 cols {} | max H {hmax:.0} | water cols {:.1}% lake cols {:.2}%",
            cols.len(),
            100.0 * land.len() as f64 / cols.len() as f64,
            SEA_LEVEL + 150,
            100.0 * mtn,
            high,
            100.0 * water_cols as f64 / cols.len() as f64,
            100.0 * lakes as f64 / cols.len() as f64
        );
        println!("   mountain comps: {} (<1 km²: {small}); largest: {}", comps.len(), big.join(", "));
        println!(
            "   snow top: {:.2}% of land, BELOW y300: {:.3}% of land; by 50-block band: {:?}",
            100.0 * snow_any as f64 / nl,
            100.0 * snow_low as f64 / nl,
            snow_hist
        );
        let mut v: Vec<(usize, usize)> = counts.iter().cloned().enumerate().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let s: Vec<String> = v
            .iter()
            .map(|(b, k)| format!("{} {:.2}%", Biome::from_u8(*b as u8).name(), 100.0 * *k as f64 / cols.len() as f64))
            .collect();
        println!("   biomes: {}", s.join(", "));
        // lowland snowy biomes
        let lowsnow = cols
            .iter()
            .filter(|c| {
                c.height < 300.0
                    && matches!(
                        c.biome,
                        Biome::SnowyPlains | Biome::SnowyTaiga | Biome::SnowyBeach | Biome::FrozenOcean | Biome::FrozenRiver | Biome::SnowySlopes | Biome::SnowyPeaks | Biome::Glacier
                    )
            })
            .count();
        println!("   snowy/frozen biome columns below y300: {:.2}%", 100.0 * lowsnow as f64 / cols.len() as f64);

        if !img.is_empty() {
            // biome map with hillshade, downsampled to ≤1024
            let s = n.min(1024);
            let r = n / s;
            let mut buf = vec![0u8; s * s * 3];
            for j in 0..s {
                for i in 0..s {
                    let k = (j * r) * n + i * r;
                    let c = &cols[k];
                    let k2 = (j * r) * n + (i * r + 1).min(n - 1);
                    let k3 = ((j * r + 1).min(n - 1)) * n + i * r;
                    let hx = cols[k2].height.max(sea) - c.height.max(sea);
                    let hz = cols[k3].height.max(sea) - c.height.max(sea);
                    let sh = (1.0 + (-hx - hz) / (step as f32) * 1.5).clamp(0.4, 1.6);
                    let mut col = c.biome.map_color();
                    if c.water_level > c.height as i32 && c.water_level > SEA_LEVEL && c.biome != Biome::Volcanic {
                        col = [0, 200, 255];
                    }
                    for ch in 0..3 {
                        buf[(j * s + i) * 3 + ch] = (col[ch] as f32 * sh).min(255.0) as u8;
                    }
                }
            }
            let p = std::path::Path::new(&img).join(format!("stats_seed{seed}.png"));
            write_png_rgb8(&p, s as u32, s as u32, &buf).unwrap();
            println!("   wrote {}", p.display());
        }
    }
    // seed-independence of the origin
    let mut cs = Vec::new();
    for seed in 0..12u64 {
        let g = WorldGen::new(seed);
        let c = g.climate(0.0, 0.0);
        cs.push(format!("{:.3}/{:.3}", c.continentalness, c.river));
    }
    println!("origin C/river per seed: {}", cs.join(" "));
}
