use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    for (ox, oz) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
        let cd = g.climate.sample_d(x + ox, z + oz, 0.0);
        let sh = g.terrain.shape(&cd, x + ox, z + oz, 0.0);
        println!(
            "({:+},{:+}) h {:9.3} mtn {:8.3} c {:.4} e {:.4} pv {:.4} t {:.3} hu {:.3} rdist {:.2} gate {:.3} w {:.2} reg {:?}",
            ox, oz, sh.height.v, sh.mountain, cd.c.v, cd.e.v, cd.pv.v, cd.t.v, cd.h.v, cd.river_dist, cd.river_gate, cd.river_width, sh.regions
        );
    }
}
