//! world-build: full-res 2x2 box vs column_lod(fp) at given points.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<i64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let gen = WorldGen::new(a[0] as u64);
    let (cx, cz) = if a.len() >= 3 {
        (a[1] as i32, a[2] as i32)
    } else {
        let mut best = (f32::MIN, 0, 0);
        for j in -8..=8 {
            for i in -8..=8 {
                let (x, z) = (i * 400, j * 400);
                let c = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                let score = if wet { -1.0 } else { c.slope.min(1.5) + c.jaggedness * 0.05 + (c.height - 140.0).clamp(0.0, 80.0) * 0.01 };
                if score > best.0 {
                    best = (score, x, z);
                }
            }
        }
        println!("land centre {:?}", best);
        (best.1, best.2)
    };
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
    // fair comparison: L(fp) vs the fp×fp box average of full-resolution heights
    for fp in [4i32, 8, 16] {
        let (mut s1, mut s2, mut n) = (0.0f64, 0.0f64, 0);
        for j in (-64..64).step_by(fp as usize * 2) {
            for i in (-64..64).step_by(fp as usize * 2) {
                let (x0, z0) = (cx + i, cz + j);
                let mut m = 0.0;
                for q in 0..fp {
                    for p in 0..fp {
                        m += gen.column((x0 + p) as f64 + 0.5, (z0 + q) as f64 + 0.5).height as f64;
                    }
                }
                m /= (fp * fp) as f64;
                let l = gen.column_lod(x0 as f64 + fp as f64 * 0.5, z0 as f64 + fp as f64 * 0.5, fp as f64).height as f64;
                s1 += l - m;
                s2 += (l - m) * (l - m);
                n += 1;
            }
        }
        println!("fair box fp {fp}: mean {:.3} rms {:.3} (n {n})", s1 / n as f64, (s2 / n as f64).sqrt());
    }
    for (k, fp) in [0.0, 1.0, 2.0, 4.0, 8.0, 16.0].iter().enumerate() {
        let s = stats[k];
        println!("fp {fp}: mean {:.3} rms {:.3}", s.0 / s.2 as f64, (s.1 / s.2 as f64).sqrt());
    }
}
