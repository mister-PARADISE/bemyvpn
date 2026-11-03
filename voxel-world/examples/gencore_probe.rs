use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    for (dx, dz) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
        let (xx, zz) = (x + dx, z + dz);
        let cd = g.climate.sample_d(xx, zz, 0.0);
        let p = g.terrain.debug_parts(&cd, xx, zz);
        let sh = g.terrain.shape(&cd, xx, zz, 0.0);
        println!("steer {:?} ({dx},{dz}) final {:.2} base {:.2} mtn {:.2} mtn_pre {:.2} ridge {:.2} rdist {:.1} w {:.1} gate {:.2} e {:.3} c {:.3} reg {:?}", sh.steer, p[4], p[0], p[3], p[5], p[1], cd.river_dist, cd.river_width, cd.river_gate, cd.e.v, cd.c.v, sh.regions);
    }
}
