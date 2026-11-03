#[cfg(test)]
mod t {
    use voxel_world::gen::*;
    use std::time::Instant;
    #[test]
    fn bench_probe() {
        let t = Instant::now();
        let g = WorldGen::new(42);
        eprintln!("new: {:?}", t.elapsed());
        for &(x0, z0) in &[(0i32, 0i32), (5000, -3000), (-8000, 2000), (12000, 9000)] {
            let t = Instant::now();
            let mut acc = 0.0;
            let mut jag = 0;
            for z in 0..34 { for x in 0..34 { let c = g.column((x0 + x) as f64 + 0.5, (z0 + z) as f64 + 0.5); acc += c.height; if c.jaggedness > 0.0 { jag += 1; } } }
            let tc = t.elapsed();
            let t = Instant::now();
            let f = g.features_in(x0 - 25, z0 - 25, x0 + 59, z0 + 59);
            let tf = t.elapsed();
            let t = Instant::now();
            let mut a2 = 0.0;
            for j in 0..65 { for i in 0..65 { let c = g.column_lod((x0 + i * 16) as f64, (z0 + j * 16) as f64, 16.0); a2 += c.height; a2 += g.canopy_lod(0.0,0.0,&c,16.0).coverage; } }
            let tl = t.elapsed();
            eprintln!("({x0},{z0}) cols1156 {:?} (jag {jag}) avg h {:.0}; features {} in {:?}; lod 65^2 {:?} {a2}", tc, acc / 1156.0, f.len(), tf, tl);
        }
    }
}
