//! Gameplay pass: fixed workload for callgrind (instruction counts are load-independent).
use voxel_world::gen::WorldGen;
fn main() {
    let fp: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let n: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(30_000);
    let g = WorldGen::new(1);
    let mut acc = 0.0f32;
    for i in 0..n {
        let x = (i % 1000) as f64 * 7.3 - 3000.0;
        let z = (i / 1000) as f64 * 11.1 - 1000.0;
        acc += g.terrain.column(&g.climate, x, z, fp).height;
    }
    println!("{acc}");
}
