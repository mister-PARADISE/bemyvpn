//! gen-core review: statistics of the river-noise gradient (for the distance regulariser).
use voxel_world::gen::WorldGen;
fn main() {
    let mut g = Vec::new();
    for seed in [1u64, 42, 2026] {
        let w = WorldGen::new(seed);
        for i in 0..100000 {
            let x = (i % 400) as f64 * 97.0 - 20000.0;
            let z = (i / 400) as f64 * 151.0 - 20000.0;
            g.push(w.climate.sample_d(x, z, 0.0).river_n.grad_len());
        }
    }
    g.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| g[((g.len() - 1) as f64 * p) as usize];
    println!("|grad n_r| p10 {:e} p25 {:e} p50 {:e} p90 {:e}", q(0.1), q(0.25), q(0.5), q(0.9));
}
