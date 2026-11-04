//! Gameplay pass debug: FD check of the patch blend and relief.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    let e = 0.01;
    let pm = g.terrain.patch_mix(x, z, 0.0);
    let px = g.terrain.patch_mix(x + e, z, 0.0);
    let mx = g.terrain.patch_mix(x - e, z, 0.0);
    let pz = g.terrain.patch_mix(x, z + e, 0.0);
    let mz = g.terrain.patch_mix(x, z - e, 0.0);
    println!("edge {} style {:?}", pm.edge, pm.style);
    for k in 0..13 {
        let fx = (px.p[k].v - mx.p[k].v) / (2.0 * e as f32);
        let fz = (pz.p[k].v - mz.p[k].v) / (2.0 * e as f32);
        println!("p{k}: v {:.4} d ({:.5},{:.5}) fd ({:.5},{:.5})", pm.p[k].v, pm.p[k].dx, pm.p[k].dz, fx, fz);
    }
    let fx = (px.off.v - mx.off.v) / (2.0 * e as f32);
    let fz = (pz.off.v - mz.off.v) / (2.0 * e as f32);
    println!("off: v {:.4} d ({:.5},{:.5}) fd ({:.5},{:.5})", pm.off.v, pm.off.dx, pm.off.dz, fx, fz);
    let r = |x: f64, z: f64| g.terrain.patch_relief(&g.terrain.patch_mix(x, z, 0.0), x, z, 0.0);
    let r0 = r(x, z);
    println!("relief v {:.4} d ({:.4},{:.4}) fd ({:.4},{:.4})", r0.v, r0.dx, r0.dz, (r(x + e, z).v - r(x - e, z).v) / (2.0 * e as f32), (r(x, z + e).v - r(x, z - e).v) / (2.0 * e as f32));
}
