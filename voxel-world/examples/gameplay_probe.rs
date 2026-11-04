//! Gameplay terrain pass: print shape internals along a line.
//! `cargo run --release --example gameplay_probe -- SEED X0 Z0 X1 Z1 [N]`
use voxel_world::gen::WorldGen;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let f = |i: usize| -> f64 { a[i].parse().unwrap() };
    let (x0, z0, x1, z1) = (f(1), f(2), f(3), f(4));
    let n: usize = a.get(5).and_then(|v| v.parse().ok()).unwrap_or(40);
    let g = WorldGen::new(seed);
    println!("{:>8} {:>8} {:>7} {:>5} {:>6} {:>14} {:>10} {:>5} {:>5} {:>5} {:>5} {:>5} {:>5} {:>6}", "x", "z", "h", "slope", "mtn", "biome", "style", "terr", "rock", "plain", "chan", "M", "C", "jag");
    for i in 0..=n {
        let t = i as f64 / n as f64;
        let (x, z) = (x0 + (x1 - x0) * t, z0 + (z1 - z0) * t);
        let cd = g.climate.sample_d(x, z, 0.0);
        let sh = g.terrain.shape_full(&g.climate, &cd, x, z, 0.0);
        let c = g.column(x, z);
        let r = sh.regions;
        println!(
            "{:8.1} {:8.1} {:7.2} {:5.2} {:6.1} {:>14} {:>10} {:5.2} {:5.2} {:5.2} {:5.2} {:5.2} {:5.2} {:6.2}",
            x,
            z,
            c.height,
            c.slope,
            sh.mountain,
            c.biome.name(),
            format!("{:?}", r.style),
            r.terrace,
            r.rock,
            r.plain,
            r.channel,
            cd.orogeny.v,
            cd.c.v,
            c.jaggedness
        );
    }
}
