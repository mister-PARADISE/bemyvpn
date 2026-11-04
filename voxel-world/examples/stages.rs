use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    for p in a[1..].chunks(2) {
        let c = g.column(p[0], p[1]);
        println!("({}, {}) h {:.1} slope {:.2} biome {:?}", p[0], p[1], c.height, c.slope, c.biome);
        for (n, d) in g.terrain.shape_stages(&g.climate, p[0], p[1], 0.0) {
            println!("   {:<18} {:8.2}  grad {:.2}", n, d.v, d.grad_len());
        }
    }
}
