//! Art-pass probe: shape components along a row.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let (seed, x0, z0, x1, n) = (a[0] as u64, a[1], a[2], a[3], a[4] as usize);
    let g = WorldGen::new(seed);
    for k in 0..=n {
        let x = x0 + (x1 - x0) * k as f64 / n as f64;
        let cd = g.climate.sample_d(x, z0, 0.0);
        let p = g.terrain.debug_parts(&cd, x, z0);
        let r = g.terrain.regions(&cd, x, z0, 0.0);
        let c = g.column(x, z0);
        eprintln!(
            "x {x:8.1}: base {:6.1} mtn {:6.1} shape_h {:6.1} col_h {:6.1} plain {:.2} chi {:.2} rdist {:6.0} gate {:.2} rw {:.1} E* {:.2} orog {:.2} wl {}",
            p[0], p[3], p[4], c.height, r.plain, g.terrain.character_at(x, z0), cd.river_dist, cd.river_gate, cd.river_width, cd.e.v, cd.orogeny.v, c.water_level
        );
    }
}
