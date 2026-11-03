//! gen-core micro-benchmarks (single thread).
use std::time::Instant;
use voxel_world::gen::WorldGen;
use voxel_world::noise::Simplex;
fn main() {
    let g = WorldGen::new(1);
    let n = 200_000u64;
    let pts: Vec<(f64, f64)> = (0..n).map(|i| ((i % 1000) as f64 * 7.3 - 3000.0, (i / 1000) as f64 * 11.1 - 1000.0)).collect();
    let s = Simplex::new(5);
    let t = Instant::now();
    let mut acc = 0.0f32;
    for &(x, z) in &pts {
        acc += s.n2d(x * 0.01, z * 0.01).0;
    }
    eprintln!("n2d: {:.1} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    let fs = voxel_world::gen::climate::FastSimplex::new(5);
    let t = Instant::now();
    for &(x, z) in &pts {
        acc += fs.n2d(x * 0.01, z * 0.01).0;
    }
    eprintln!("fast n2d: {:.1} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    let f = voxel_world::gen::climate::DFbm::new(3, 1000.0, 4, 2.0, 0.5);
    let t = Instant::now();
    for &(x, z) in &pts {
        acc += f.sample(x, z, 0.0).v;
    }
    eprintln!("DFbm 4 oct: {:.1} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    let t = Instant::now();
    for &(x, z) in &pts {
        acc += g.climate.sample_raw(x, z, 0.0)[0];
    }
    eprintln!("climate raw: {:.1} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    for &fp in &[0.0, 8.0] {
        let t = Instant::now();
        let cds: Vec<_> = pts.iter().map(|&(x, z)| g.climate.sample_d(x, z, fp)).collect();
        let tc = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let t = Instant::now();
        let mut mtn = 0usize;
        let shapes: Vec<_> = cds.iter().zip(&pts).map(|(cd, &(x, z))| { let s = g.terrain.shape(cd, x, z, fp); if s.mountain > 12.0 { mtn += 1; } s }).collect();
        let ts = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let t = Instant::now();
        for ((cd, sh), &(x, z)) in cds.iter().zip(&shapes).zip(&pts) {
            acc += g.terrain.finish(cd, sh, x, z).height;
        }
        let tf = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        eprintln!("fp {fp}: climate {tc:.0} ns, shape {ts:.0} ns, finish {tf:.0} ns; mountain cols {:.1}%", 100.0 * mtn as f64 / n as f64);
    }
    eprintln!("{acc}");
}
