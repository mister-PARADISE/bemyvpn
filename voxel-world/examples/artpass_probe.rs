//! Art-pass probe: quantiles of the mountain components inside orogeny belts.
use voxel_world::gen::WorldGen;

fn q(v: &mut Vec<f32>) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
    format!("p5 {:.2} p25 {:.2} p50 {:.2} p75 {:.2} p95 {:.2} p99 {:.2}", p(0.05), p(0.25), p(0.5), p(0.75), p(0.95), p(0.99))
}

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    let (mut base, mut ridge, mut mtn, mut h, mut e, mut slope) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for j in 0..400 {
        for i in 0..400 {
            let x = -16000.0 + i as f64 * 80.0 + 0.5;
            let z = -16000.0 + j as f64 * 80.0 + 0.5;
            let cd = g.climate.sample_d(x, z, 0.0);
            if cd.orogeny.v < 0.5 {
                continue;
            }
            let p = g.terrain.debug_parts(&cd, x, z);
            let c = g.column(x, z);
            base.push(p[0]);
            ridge.push(p[1] / 300.0);
            mtn.push(p[3]);
            h.push(c.height);
            e.push(cd.e.v);
            slope.push(c.slope);
        }
    }
    eprintln!("n = {}", h.len());
    eprintln!("base   {}", q(&mut base));
    eprintln!("ridge  {}", q(&mut ridge));
    eprintln!("mtn    {}", q(&mut mtn));
    eprintln!("height {}", q(&mut h));
    eprintln!("E*     {}", q(&mut e));
    eprintln!("slope  {}", q(&mut slope));
}
