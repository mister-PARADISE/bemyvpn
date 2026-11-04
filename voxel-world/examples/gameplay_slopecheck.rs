//! Gameplay terrain pass: where does the analytic slope disagree with finite differences?
//! `cargo run --release --example gameplay_slopecheck -- [seed]`
use voxel_world::gen::{Biome, WorldGen};
use voxel_world::noise::{hash_f01, mix64};

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2026);
    let g = WorldGen::new(seed);
    let mut rows: Vec<(f32, String)> = Vec::new();
    let mut buckets: std::collections::BTreeMap<String, (usize, f64, usize)> = Default::default();
    for i in 0..4000u64 {
        let x = (hash_f01(mix64(i * 2 + 5)) as f64 - 0.5) * 30_000.0;
        let z = (hash_f01(mix64(i * 2 + 6)) as f64 - 0.5) * 30_000.0;
        let c = g.column(x, z);
        if matches!(c.biome, Biome::Hoodoos | Biome::Badlands | Biome::Volcanic) {
            continue;
        }
        let e = 0.02;
        let hx = (g.column(x + e, z).height - g.column(x - e, z).height) / (2.0 * e as f32);
        let hz = (g.column(x, z + e).height - g.column(x, z - e).height) / (2.0 * e as f32);
        let fd = (hx * hx + hz * hz).sqrt();
        let err = (c.slope - fd).abs() / (0.05 + fd);
        let cd = g.climate.sample_d(x, z, 0.0);
        let sh = g.terrain.shape(&cd, x, z, 0.0);
        let pm = g.terrain.patch_mix(x, z, 0.0);
        let r = sh.regions;
        let key = format!(
            "{}{}{}{}{}",
            if r.rock > 0.0 { "R" } else { "-" },
            if r.terrace > 0.01 { "T" } else { "-" },
            if sh.mountain > 12.0 { "M" } else { "-" },
            if pm.edge < 95.0 { "E" } else { "-" },
            if r.channel > 0.0 || cd.river_dist < cd.river_width + 300.0 { "V" } else { "-" }
        );
        let b = buckets.entry(key.clone()).or_default();
        b.0 += 1;
        b.1 += err as f64;
        b.2 += (err > 0.35) as usize;
        rows.push((err, format!("{err:.3} slope {:.3} fd {fd:.3} {key} {:?} mtn {:.0} terr {:.2} h {:.1} at ({x:.1},{z:.1})", c.slope, r.style, sh.mountain, r.terrace, c.height)));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for r in rows.iter().take(25) {
        println!("{}", r.1);
    }
    let mut errs: Vec<f32> = rows.iter().map(|r| r.0).collect();
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("median {:.4} p90 {:.4}", errs[errs.len() / 2], errs[errs.len() * 9 / 10]);
    for (k, (n, s, bad)) in buckets {
        println!("{k}: n {n:5} mean {:.3} >0.35: {bad}", s / n as f64);
    }
}
