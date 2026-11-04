//! Gameplay pass debug: heights and reported gradients along a short line (FD check).
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z, dir) = (a[1], a[2], a[3] as i32);
    let step = a.get(4).copied().unwrap_or(0.05);
    let mut prev: Option<f32> = None;
    for i in -10..=10 {
        let (px, pz) = if dir == 0 { (x + i as f64 * step, z) } else { (x, z + i as f64 * step) };
        let cd = g.climate.sample_d(px, pz, 0.0);
        let sh = g.terrain.shape_full(&g.climate, &cd, px, pz, 0.0);
        let d = if dir == 0 { sh.height.dx } else { sh.height.dz };
        let fd = prev.map(|p| (sh.height.v - p) / step as f32).unwrap_or(f32::NAN);
        println!("{:+.3}: h {:.5} d {:.4} fd(back) {:.4} terr {:.3} mtn {:.2}", i as f64 * step, sh.height.v, d, fd, sh.regions.terrace, sh.mountain);
        prev = Some(sh.height.v);
    }
}
