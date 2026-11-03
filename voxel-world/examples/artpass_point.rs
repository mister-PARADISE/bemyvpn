//! Art-pass probe: heights and regions in a window around a point.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<f64> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let (seed, x0, z0, step) = (a[0] as u64, a[1], a[2], a.get(3).copied().unwrap_or(25.0));
    let g = WorldGen::new(seed);
    for dj in -4..=4 {
        let mut line = String::new();
        for di in -4..=4 {
            let (x, z) = (x0 + di as f64 * step, z0 + dj as f64 * step);
            let c = g.column(x, z);
            line += &format!("{:5.0}", c.height);
        }
        eprintln!("{line}");
    }
    let c = g.column(x0, z0);
    let cd = g.climate.sample_d(x0, z0, 0.0);
    let r = g.terrain.regions(&cd, x0, z0, 0.0);
    eprintln!("{:?}\nE* {:.2} C {:.2} pv {:.2} orog {:.2} {:?}", c, cd.e.v, cd.c.v, cd.pv.v, cd.orogeny.v, r);
}
