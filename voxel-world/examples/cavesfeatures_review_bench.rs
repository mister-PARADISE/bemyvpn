//! Per-species build cost and features_in cost (caves-features review).
use voxel_world::gen::features::{build_feature_with, Growth, Species, ALL_SPECIES};
use voxel_world::gen::{Biome, WorldGen};
fn main() {
    if std::env::args().nth(1).as_deref() == Some("build") {
        // world build at a forest spot: the builder logs per-stage timings on stderr
        let g = WorldGen::new(1);
        let p = voxel_world::world::BuildParams { center_x: -7800, center_z: 5900, l0_size: 1024, hf_size: 512, hf_levels: 1, cave_radius: 256, feature_radius: 512 };
        let t0 = std::time::Instant::now();
        let w = voxel_world::world::build_world(&g, &p);
        eprintln!("build_world total {:.2} s", t0.elapsed().as_secs_f64());
        drop(w);
        return;
    }
    for &sp in ALL_SPECIES.iter() {
        let n = if matches!(sp, Species::AncientOak | Species::Sequoia | Species::JungleGiant | Species::Baobab | Species::Willow | Species::ColossalSpruce) { 50 } else { 2000 };
        let t0 = std::time::Instant::now();
        let mut runs = 0;
        for v in 0..n {
            let f = build_feature_with(sp, 0, 0, voxel_world::noise::mix64(v), &Growth::simple(Biome::Forest, 1.0));
            runs += f.columns.len();
        }
        eprintln!("{:16} {:8.1} µs/tree  {:6.0} runs", sp.name(), t0.elapsed().as_secs_f64() * 1e6 / n as f64, runs as f64 / n as f64);
    }
    let g = WorldGen::new(1);
    for (fx, fz) in [(2000, -1000), (-8000, 5678)] {
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for k in 0..16 {
            n += g.features_in(fx + (k % 4) * 64, fz + (k / 4) * 64, fx + (k % 4) * 64 + 64, fz + (k / 4) * 64 + 64).len();
        }
        let dt = t0.elapsed().as_secs_f64();
        let t1 = std::time::Instant::now();
        let mut m = 0;
        for k in 0..16 {
            m += g.features.roots_in(&g, fx + (k % 4) * 64, fz + (k / 4) * 64, fx + (k % 4) * 64 + 64, fz + (k / 4) * 64 + 64).len();
        }
        eprintln!("({fx},{fz}) features_in 256²: {:.1} ms ({n} features); roots only {:.1} ms ({m})", dt * 1e3, t1.elapsed().as_secs_f64() * 1e3);
    }
}

#[allow(dead_code)]
fn unused() {}
