//! world-build: full-res 2x2 box vs column_lod(fp) at given points.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<i64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let gen = WorldGen::new(a[0] as u64);
    let (cx, cz) = (a[1] as i32, a[2] as i32);
    let mut stats = vec![(0.0f64, 0.0f64, 0usize); 6];
    for j in (-64..64).step_by(2) {
        for i in (-64..64).step_by(2) {
            let (x, z) = (cx + i, cz + j);
            let mut m = 0.0;
            let mut mx = f32::MIN;
            let mut mn = f32::MAX;
            for (p, q) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let c = gen.column((x + p) as f64 + 0.5, (z + q) as f64 + 0.5);
                m += c.height as f64 * 0.25;
                mx = mx.max(c.height);
                mn = mn.min(c.height);
            }
            for (k, fp) in [0.0, 1.0, 2.0, 4.0, 8.0, 16.0].iter().enumerate() {
                let l = gen.column_lod(x as f64 + 1.0, z as f64 + 1.0, *fp).height as f64;
                stats[k].0 += l - m;
                stats[k].1 += (l - m) * (l - m);
                stats[k].2 += 1;
            }
            if i == 0 && j % 16 == 0 {
                let c = gen.column(x as f64 + 1.0, z as f64 + 1.0);
                println!("({x},{z}) box {m:.1} [{mn:.1},{mx:.1}] lod2 {:.1} lod0 {:.1} biome {:?} slope {:.2}", gen.column_lod(x as f64 + 1.0, z as f64 + 1.0, 2.0).height, c.height, c.biome, c.slope);
            }
        }
    }
    for (k, fp) in [0.0, 1.0, 2.0, 4.0, 8.0, 16.0].iter().enumerate() {
        let s = stats[k];
        println!("fp {fp}: mean {:.3} rms {:.3}", s.0 / s.2 as f64, (s.1 / s.2 as f64).sqrt());
    }
}
