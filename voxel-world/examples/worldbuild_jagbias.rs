//! world-build: expected excess of the voxel top over H as a function of jaggedness J, and
//! band-limited column_lod vs box-filtered full-resolution heights.
use voxel_world::gen::WorldGen;

fn main() {
    for seed in [1u64, 42, 2026] {
        let gen = WorldGen::new(seed);
        let mut bins = vec![(0.0f64, 0.0f64, 0usize); 8];
        let mut lod = (0.0f64, 0usize);
        for j in -60..60 {
            for i in -60..60 {
                let (bx, bz) = (i * 97 + 13, j * 101 + 7);
                let c = gen.column(bx as f64 + 0.5, bz as f64 + 0.5);
                // lod vs 2x2 box
                let cl = gen.column_lod(bx as f64 + 1.0, bz as f64 + 1.0, 2.0);
                let mut m = 0.0;
                for (a, b) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    m += gen.column((bx + a) as f64 + 0.5, (bz + b) as f64 + 0.5).height as f64 * 0.25;
                }
                if c.slope > 0.8 {
                    lod.0 += cl.height as f64 - m;
                    lod.1 += 1;
                }
                if c.jaggedness <= 0.0 {
                    continue;
                }
                let jj = c.jaggedness;
                let mut top = 0;
                for y in ((c.height - jj - 2.0) as i32..(c.height + jj + 2.0) as i32).rev() {
                    if gen.density(bx as f64 + 0.5, y as f64 + 0.5, bz as f64 + 0.5, &c) > 0.0 {
                        top = y + 1;
                        break;
                    }
                }
                let k = ((jj / 4.0) as usize).min(7);
                bins[k].0 += (top as f64 - c.height as f64) / jj as f64;
                bins[k].1 += top as f64 - c.height as f64;
                bins[k].2 += 1;
            }
        }
        print!("seed {seed}: ");
        for (k, b) in bins.iter().enumerate() {
            if b.2 > 0 {
                print!("J[{}-{}): excess/J {:.3} excess {:.2} (n {}) | ", k * 4, k * 4 + 4, b.0 / b.2 as f64, b.1 / b.2 as f64, b.2);
            }
        }
        println!("\n   lod(fp2) - box2x2 on slopes>0.8: {:.3} (n {})", lod.0 / lod.1.max(1) as f64, lod.1);
    }
}
