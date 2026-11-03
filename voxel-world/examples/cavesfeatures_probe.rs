//! Probe one spot: biome, height, snow-line distance, feature counts by kind (owner: caves-features).
//! `cargo run --release --example cavesfeatures_probe -- SEED X Z`
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let seed: u64 = a[1].parse().unwrap();
    let x: i32 = a[2].parse().unwrap();
    let z: i32 = a[3].parse().unwrap();
    let g = WorldGen::new(seed);
    let c = g.column(x as f64 + 0.5, z as f64 + 0.5);
    let env = g.features.env(&c, 0.0);
    eprintln!(
        "{:?} h={:.1} T={:.2} rel={:.1} scale={:.2} p={:?} slope={:.2}",
        c.biome, c.height, c.climate.temperature, env.rel, env.scale, env.p, c.slope
    );
    let f = g.features_in(x - 36, z - 36, x + 36, z + 36);
    let mut counts = std::collections::BTreeMap::new();
    for ft in &f {
        *counts.entry(format!("{:?}", ft.kind)).or_insert(0) += 1;
    }
    eprintln!("{counts:?}");
}
