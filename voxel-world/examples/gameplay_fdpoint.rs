//! Gameplay pass debug: analytic vs FD gradient of the height and the climate at one point.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    let e = 0.02;
    let f = |x: f64, z: f64| {
        let cd = g.climate.sample_d(x, z, 0.0);
        let sh = g.terrain.shape_full(&g.climate, &cd, x, z, 0.0);
        (cd, sh)
    };
    let (cd, sh) = f(x, z);
    let (cpx, spx) = f(x + e, z);
    let (cmx, smx) = f(x - e, z);
    let fdx = |a: f32, b: f32| (a - b) / (2.0 * e as f32);
    println!("h {:.4} grad ({:.4},{:.4}) fd ({:.4},{:.4}) mountain {:.3}", sh.height.v, sh.height.dx, sh.height.dz, fdx(spx.height.v, smx.height.v), 0.0, sh.mountain);
    println!("mountain fd {:.4}", fdx(spx.mountain, smx.mountain));
    for (n, a, p, m) in [("c", cd.c, cpx.c, cmx.c), ("e", cd.e, cpx.e, cmx.e), ("e_raw", cd.e_raw, cpx.e_raw, cmx.e_raw), ("pv", cd.pv, cpx.pv, cmx.pv), ("orog", cd.orogeny, cpx.orogeny, cmx.orogeny), ("chi", cd.character, cpx.character, cmx.character), ("river_n", cd.river_n, cpx.river_n, cmx.river_n)] {
        println!("{n:8} v {:.5} dx {:.6} fd {:.6}", a.v, a.dx, fdx(p.v, m.v));
    }
    let parts = g.terrain.debug_parts(&cd, x, z);
    let pp = g.terrain.debug_parts(&cpx, x + e, z);
    let pm = g.terrain.debug_parts(&cmx, x - e, z);
    for (i, n) in ["base", "ridge", "hills", "mountain", "final", "m"].iter().enumerate() {
        println!("{n:8} v {:.4} fd {:.4}", parts[i], fdx(pp[i], pm[i]));
    }
    println!("m.dx+base.dx {:.4}", parts[6]);
}
