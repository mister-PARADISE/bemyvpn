//! Gameplay pass debug: FD check of every stage of the height construction at one point.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    let e = 0.01;
    let s0 = g.terrain.shape_stages(&g.climate, x, z, 0.0);
    let px = g.terrain.shape_stages(&g.climate, x + e, z, 0.0);
    let mx = g.terrain.shape_stages(&g.climate, x - e, z, 0.0);
    let pz = g.terrain.shape_stages(&g.climate, x, z + e, 0.0);
    let mz = g.terrain.shape_stages(&g.climate, x, z - e, 0.0);
    for i in 0..s0.len() {
        let (n, h) = s0[i];
        let fx = (px[i].1.v - mx[i].1.v) / (2.0 * e as f32);
        let fz = (pz[i].1.v - mz[i].1.v) / (2.0 * e as f32);
        println!("{n:18} h {:9.4} d ({:8.4},{:8.4}) fd ({:8.4},{:8.4})", h.v, h.dx, h.dz, fx, fz);
    }
}
