use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    for k in -4..=4 {
        let zz = z + k as f64 * 0.25;
        let cd = g.climate.sample_d(x, zz, 0.0);
        let p = g.terrain.debug_parts(&cd, x, zz);
        let sh = g.terrain.shape(&cd, x, zz, 0.0);
        println!("z {zz:.2} parts(base,ridge*300,hills*300,mountain,final) {:?} grad ({:.3},{:.3})", p, sh.height.dx, sh.height.dz);
    }
}
