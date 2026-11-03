//! Reviewer probes for caves/features (owner: caves-features review).
use voxel_world::gen::WorldGen;
use voxel_world::noise::{hash_f01, mix64, Fbm, derive_seed, salt};

fn main() {
    let g = WorldGen::new(1);
    // 1) cheese fBm distribution
    let f = Fbm::new(derive_seed(1, salt("caves.cheese")), 150.0, 3, 2.0, 0.5);
    let mut v: Vec<f32> = (0..400_000u64)
        .map(|i| {
            let h = mix64(i);
            f.sample3(hash_f01(h) as f64 * 20000.0, hash_f01(mix64(h)) as f64 * 400.0, hash_f01(mix64(h ^ 9)) as f64 * 20000.0)
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    let above = v.iter().filter(|&&x| x > 0.62).count() as f64 / v.len() as f64;
    let above5 = v.iter().filter(|&&x| x > 0.5).count() as f64 / v.len() as f64;
    eprintln!("cheese n: p50 {:.3} p90 {:.3} p99 {:.3} p999 {:.3} max {:.3}; P(n>0.62) = {:.5}, P(n>0.5)={:.5}", q(0.5), q(0.9), q(0.99), q(0.999), v[v.len() - 1], above, above5);
    // 2) column cost
    let t0 = std::time::Instant::now();
    let mut acc = 0.0;
    for i in 0..20000 {
        let c = g.column(i as f64 * 13.7, i as f64 * -7.3);
        acc += c.height;
    }
    eprintln!("column: {:.0} ns ({acc})", t0.elapsed().as_secs_f64() * 1e9 / 20000.0);
}
