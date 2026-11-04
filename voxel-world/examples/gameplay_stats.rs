//! Gameplay terrain pass: playability metrics (DESIGN §0 + gameplay goals).
//! `cargo run --release --example gameplay_stats -- [seeds…]` (default 1 42 2026)
//!
//! * walk: fraction of non-mountain dry land columns whose block height differs by ≤ 1 from all
//!   4 neighbours (target ≥ 80 %);
//! * flat: fraction of 300×300 regions (≥ 50 % non-mountain land) containing a 16×16 area with
//!   block-height range ≤ 2 (target 100 %);
//! * slopes: shares of non-mountain land with |Δh| over 4 blocks per block < 0.15 (shelf),
//!   0.15–0.6 (ramp), > 0.9 (cliff) — bimodality = few ramps;
//! * interest: fraction of 250×250 windows with a cliff (≥ 4 blocks within 2 columns), water, a
//!   rock landform or an overhang band;
//! * patches: equivalent diameters of connected same-biome land areas (16-block grid);
//! * µs/column.
use rayon::prelude::*;
use voxel_world::gen::{Biome, WorldGen, SEA_LEVEL};
use voxel_world::noise::{hash_f01, mix64};

fn main() {
    let seeds: Vec<u64> = {
        let v: Vec<u64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
        if v.is_empty() {
            vec![1, 42, 2026]
        } else {
            v
        }
    };
    for seed in seeds {
        let g = WorldGen::new(seed);
        let sea = SEA_LEVEL as f32;
        let mtn = |c: &voxel_world::gen::ColumnInfo| c.height > sea + 130.0 || c.jaggedness > 9.0;
        // ---------------- regions of 300×300 (walk / flat / slope / interest)
        let nreg = 48usize;
        let t0 = std::time::Instant::now();
        let res: Vec<Option<[f64; 9]>> = (0..nreg)
            .into_par_iter()
            .map(|k| {
                let h = mix64(seed.wrapping_mul(0x51ED) ^ k as u64);
                let ox = ((hash_f01(h) as f64 - 0.5) * 24_000.0).floor();
                let oz = ((hash_f01(mix64(h)) as f64 - 0.5) * 24_000.0).floor();
                let n = 300usize;
                let mut hb = vec![0i32; n * n];
                let mut ok = vec![false; n * n];
                let mut wet_any = false;
                let mut rock_any = false;
                let mut jag_any = false;
                for j in 0..n {
                    for i in 0..n {
                        let c = g.column(ox + i as f64 + 0.5, oz + j as f64 + 0.5);
                        let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                        hb[j * n + i] = c.height.floor() as i32;
                        ok[j * n + i] = !wet && !mtn(&c) && c.height >= sea;
                        wet_any |= wet && c.height > sea - 30.0 && c.climate.continentalness > -0.1;
                        rock_any |= matches!(c.biome, Biome::Hoodoos) || (c.slope > 6.0 && c.height > sea + 3.0);
                        jag_any |= c.jaggedness > 2.0 && c.height > sea + 3.0;
                    }
                }
                let land = ok.iter().filter(|&&b| b).count();
                if land < n * n / 2 {
                    return None;
                }
                let (mut walk, mut wn) = (0usize, 0usize);
                let (mut shelf, mut ramp, mut cliff, mut sn) = (0usize, 0usize, 0usize, 0usize);
                let mut long_ramp = 0usize;
                let mut cliff_any = false;
                for j in 2..n - 2 {
                    for i in 2..n - 2 {
                        let p = j * n + i;
                        if !ok[p] {
                            continue;
                        }
                        let h0 = hb[p];
                        let d = [hb[p - 1], hb[p + 1], hb[p - n], hb[p + n]].iter().map(|&v| (v - h0).abs()).max().unwrap();
                        wn += 1;
                        walk += (d <= 1) as usize;
                        let sx = (hb[p + 2] - hb[p - 2]) as f32 / 4.0;
                        let sz = (hb[p + 2 * n] - hb[p - 2 * n]) as f32 / 4.0;
                        let s = (sx * sx + sz * sz).sqrt();
                        sn += 1;
                        if s < 0.15 {
                            shelf += 1;
                        } else if s < 0.6 {
                            ramp += 1;
                        } else if s > 0.9 {
                            cliff += 1;
                        }
                        if (hb[p + 2] - hb[p]).abs() >= 4 || (hb[p + 2 * n] - hb[p]).abs() >= 4 {
                            cliff_any = true;
                        }
                        // sustained uniform ramp: the 4-block slope along x (or z) stays in
                        // [0.2, 0.9] with one sign at p − 8, p, p + 8 (≥ 20 blocks of plain ramp)
                        if i >= 10 && i < n - 10 && j >= 10 && j < n - 10 {
                            let sxa = |q: usize| (hb[q + 2] - hb[q - 2]) as f32 / 4.0;
                            let sza = |q: usize| (hb[q + 2 * n] - hb[q - 2 * n]) as f32 / 4.0;
                            let ramp3 = |a: f32, b: f32, c: f32| {
                                let ok = |v: f32| (0.2..=0.9).contains(&v.abs());
                                ok(a) && ok(b) && ok(c) && a.signum() == b.signum() && b.signum() == c.signum()
                            };
                            if ramp3(sxa(p - 8), sxa(p), sxa(p + 8)) || ramp3(sza(p - 8 * n), sza(p), sza(p + 8 * n)) {
                                long_ramp += 1;
                            }
                        }
                    }
                }
                // flat 16×16 window with range ≤ 2 (sliding min/max along rows, then columns)
                let w = 16usize;
                let m = n - w + 1;
                let mut rmin = vec![0i32; n * m];
                let mut rmax = vec![0i32; n * m];
                for j in 0..n {
                    for i in 0..m {
                        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
                        let mut all = true;
                        for k in 0..w {
                            let p = j * n + i + k;
                            all &= ok[p];
                            lo = lo.min(hb[p]);
                            hi = hi.max(hb[p]);
                        }
                        rmin[j * m + i] = if all { lo } else { i32::MIN / 2 };
                        rmax[j * m + i] = if all { hi } else { i32::MAX / 2 };
                    }
                }
                let mut flat = false;
                'f: for j in 0..m {
                    for i in 0..m {
                        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
                        for k in 0..w {
                            lo = lo.min(rmin[(j + k) * m + i]);
                            hi = hi.max(rmax[(j + k) * m + i]);
                        }
                        if hi - lo <= 2 {
                            flat = true;
                            break 'f;
                        }
                    }
                }
                let interest = cliff_any || wet_any || rock_any || jag_any;
                Some([
                    walk as f64 / wn.max(1) as f64,
                    flat as u8 as f64,
                    shelf as f64 / sn.max(1) as f64,
                    ramp as f64 / sn.max(1) as f64,
                    cliff as f64 / sn.max(1) as f64,
                    interest as u8 as f64,
                    cliff_any as u8 as f64,
                    (wet_any || rock_any || jag_any) as u8 as f64,
                    long_ramp as f64 / sn.max(1) as f64,
                ])
            })
            .collect();
        let ok: Vec<[f64; 9]> = res.into_iter().flatten().collect();
        let mean = |k: usize| ok.iter().map(|r| r[k]).sum::<f64>() / ok.len().max(1) as f64;
        let cols = ok.len() as f64 * 90_000.0;
        let el = t0.elapsed().as_secs_f64();
        // ---------------- biome patch sizes (16-block grid, 12×12 km)
        let step = 16.0;
        let n = 750usize;
        let grid: Vec<(u8, bool)> = (0..n * n)
            .into_par_iter()
            .map(|k| {
                let (i, j) = (k % n, k / n);
                let c = g.column_lod(-6000.0 + i as f64 * step, -6000.0 + j as f64 * step, 8.0);
                let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                (c.biome as u8, !wet && c.height >= sea)
            })
            .collect();
        let mut seen = vec![false; n * n];
        let mut sizes = Vec::new();
        for s0 in 0..n * n {
            if seen[s0] || !grid[s0].1 {
                continue;
            }
            let b = grid[s0].0;
            let mut stack = vec![s0];
            seen[s0] = true;
            let mut cnt = 0usize;
            while let Some(p) = stack.pop() {
                cnt += 1;
                let (i, j) = (p % n, p / n);
                let nb = [(i.wrapping_sub(1), j), (i + 1, j), (i, j.wrapping_sub(1)), (i, j + 1)];
                for (a, c) in nb {
                    if a < n && c < n {
                        let q = c * n + a;
                        if !seen[q] && grid[q].1 && grid[q].0 == b {
                            seen[q] = true;
                            stack.push(q);
                        }
                    }
                }
            }
            if cnt >= 4 {
                sizes.push(2.0 * ((cnt as f64 * step * step) / std::f64::consts::PI).sqrt());
            }
        }
        // biome changes per km along straight walks (land only; rivers/shores transparent)
        let (mut changes, mut walked) = (0usize, 0.0f64);
        let mut runs: Vec<f64> = Vec::new();
        let mut distinct_sum = 0.0f64;
        for t in 0..60u64 {
            let mut seq: Vec<Biome> = Vec::new();
            let h = mix64(seed.wrapping_mul(0x7A5) ^ t);
            let (x0, z0) = ((hash_f01(h) as f64 - 0.5) * 12_000.0, (hash_f01(mix64(h)) as f64 - 0.5) * 12_000.0);
            let a = hash_f01(mix64(h ^ 9)) as f64 * std::f64::consts::TAU;
            let mut last: Option<Biome> = None;
            for k in 0..250 {
                let (x, z) = (x0 + a.sin() * k as f64 * 8.0, z0 + a.cos() * k as f64 * 8.0);
                let c = g.column_lod(x, z, 4.0);
                let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                if wet || c.height < sea || matches!(c.biome, Biome::River | Biome::Beach | Biome::StonyShore) {
                    continue;
                }
                walked += 8.0;
                seq.push(c.biome);
                if let Some(l) = last {
                    if l != c.biome {
                        changes += 1;
                    }
                }
                last = Some(c.biome);
            }
            // run lengths after dropping flicker runs shorter than 40 blocks
            let mut rl: Vec<(Biome, usize)> = Vec::new();
            for b in seq {
                match rl.last_mut() {
                    Some((lb, n)) if *lb == b => *n += 1,
                    _ => rl.push((b, 1)),
                }
            }
            let kept: Vec<(Biome, usize)> = rl.into_iter().filter(|r| r.1 >= 5).collect();
            let mut merged: Vec<(Biome, usize)> = Vec::new();
            for r in kept {
                match merged.last_mut() {
                    Some((lb, n)) if *lb == r.0 => *n += r.1,
                    _ => merged.push(r),
                }
            }
            if merged.len() > 2 {
                // interior runs only (the ends are cut by the transect)
                for r in &merged[1..merged.len() - 1] {
                    runs.push(r.1 as f64 * 8.0);
                }
            }
            let mut d: Vec<Biome> = merged.iter().map(|r| r.0).collect();
            d.sort();
            d.dedup();
            distinct_sum += d.len() as f64;
        }
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rq = |p: f64| if runs.is_empty() { 0.0 } else { runs[((runs.len() - 1) as f64 * p) as usize] };
        println!("seed {seed}: biome runs (≥40 blocks) n={} p25 {:.0} median {:.0} p75 {:.0} | distinct biomes per 2 km walk {:.2}", runs.len(), rq(0.25), rq(0.5), rq(0.75), distinct_sum / 60.0);
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| sizes[((sizes.len() - 1) as f64 * p) as usize];
        // area-weighted median diameter: the patch size a walker is typically inside
        let tot: f64 = sizes.iter().map(|d| d * d).sum();
        let mut acc = 0.0;
        let mut wmed = 0.0;
        for d in &sizes {
            acc += d * d;
            if acc >= 0.5 * tot {
                wmed = *d;
                break;
            }
        }
        println!(
            "seed {seed}: {} regions | walk {:.1}% | flat-spot {:.0}% | shelf {:.1}% ramp {:.1}% cliff {:.1}% long-ramp {:.1}% | interest {:.0}% (cliff {:.0}%, other {:.0}%) | biome changes/km {:.2} | patches n={} d p25 {:.0} p50 {:.0} p90 {:.0} area-median {:.0} | {:.2} µs/col (4 thr)",
            ok.len(),
            100.0 * mean(0),
            100.0 * mean(1),
            100.0 * mean(2),
            100.0 * mean(3),
            100.0 * mean(4),
            100.0 * mean(8),
            100.0 * mean(5),
            100.0 * mean(6),
            100.0 * mean(7),
            changes as f64 * 1000.0 / walked.max(1.0),
            sizes.len(),
            q(0.25),
            q(0.5),
            q(0.9),
            wmed,
            el * 1e6 / cols.max(1.0)
        );
    }
}
