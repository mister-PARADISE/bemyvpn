//! gen-core review: dump everything about one column and its +x neighbour.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let (seed, x, z) = (a[0] as u64, a[1], a[2]);
    let g = WorldGen::new(seed);
    for dx in [-2.0, -1.0, 0.0, 1.0, 2.0, 3.0] {
        let cd = g.climate.sample_d(x + dx, z, 0.0);
        let c = g.column(x + dx, z);
        let parts = g.terrain.debug_parts(&cd, x + dx, z);
        println!(
            "x{:+} n {:.4} g {:.2e} h {:.1} slope {:.2} biome {:?} | rdist {:.1} rw {:.1} gate {:.2} E {:.2} M {:.2} pv {:.2} | base {:.0} mtn {:.0} | lake? wl {}",
            dx, cd.river_n.v, cd.river_n.grad_len(), c.height, c.slope, c.biome, cd.river_dist, cd.river_width, cd.river_gate, cd.e.v, cd.orogeny.v, cd.pv.v, parts[0], parts[3], c.water_level
        );
    }
}
