//! gen-core review: statistics of the orogeny belt field gradient (for the G0 constant).
use voxel_world::gen::climate::DFbm;
use voxel_world::noise::{derive_seed, salt};
fn main() {
    let mut g = Vec::new();
    let mut b = Vec::new();
    for seed in [1u64, 42, 2026] {
        let f = DFbm::new(derive_seed(seed, salt("climate.orogeny")), 30000.0, 2, 2.0, 0.4);
        for i in 0..200000 {
            let x = (i % 500) as f64 * 173.0 - 40000.0;
            let z = (i / 500) as f64 * 211.0 - 40000.0;
            let s = f.sample(x, z, 0.0);
            g.push(s.grad_len());
            b.push(s.v.abs());
        }
    }
    g.sort_by(|a, b| a.partial_cmp(b).unwrap());
    b.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |v: &Vec<f32>, p: f64| v[((v.len() - 1) as f64 * p) as usize];
    println!("|grad b| p10 {:e} p50 {:e} p90 {:e}", q(&g, 0.1), q(&g, 0.5), q(&g, 0.9));
    println!("|b| p10 {} p25 {} p50 {} p90 {}", q(&b, 0.1), q(&b, 0.25), q(&b, 0.5), q(&b, 0.9));
}
