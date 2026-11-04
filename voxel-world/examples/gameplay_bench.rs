//! Gameplay terrain pass: timing breakdown of the column pipeline.
//! `cargo run --release --example gameplay_bench -- [seed]`
use voxel_world::gen::WorldGen;

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    parts(&g);
    let n = 200_000u64;
    let pos = |i: u64| ((i % 1000) as f64 * 7.3 - 3000.0, (i / 1000) as f64 * 11.1 - 1000.0);
    for &fp in &[0.0, 8.0, 32.0, 128.0] {
        // warm caches
        for i in 0..20_000 {
            let (x, z) = pos(i);
            std::hint::black_box(g.terrain.column(&g.climate, x, z, fp));
        }
        let t0 = std::time::Instant::now();
        let mut acc = 0.0f32;
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.climate.sample_d(x, z, fp).c.v;
        }
        let t_clim = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        let t0 = std::time::Instant::now();
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.terrain.column(&g.climate, x, z, fp).height;
        }
        let t_col = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        // density inside the band
        let cols: Vec<_> = (0..256).map(|i| g.column(pos(i * 37).0, pos(i * 37).1)).collect();
        let t0 = std::time::Instant::now();
        let m = 2_000_000usize;
        for i in 0..m {
            let c = &cols[i % 256];
            let y = c.height as f64 + ((i / 256) % 16) as f64 - 8.0;
            acc += g.density(c.x + (i % 7) as f64, y, c.z, c);
        }
        let t_den = t0.elapsed().as_secs_f64() * 1e9 / m as f64;
        println!("fp {fp:>5}: climate {t_clim:.3} µs, column {t_col:.3} µs (shape+biome {:.3}), density {t_den:.1} ns  [{acc}]", t_col - t_clim);
    }
}

#[allow(dead_code)]
pub fn parts(g: &WorldGen) {
    let n = 200_000u64;
    let pos = |i: u64| ((i % 1000) as f64 * 7.3 - 3000.0, (i / 1000) as f64 * 11.1 - 1000.0);
    for &fp in &[0.0, 128.0] {
        let mut acc = 0.0f32;
        let t = std::time::Instant::now();
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.terrain.patch_mix(x, z, fp).p[0].v;
        }
        let t_pm = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let pm = g.terrain.patch_mix(0.0, 0.0, fp);
        let t = std::time::Instant::now();
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.terrain.patch_relief(&pm, x, z, fp).v;
        }
        let t_pr = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let cds: Vec<_> = (0..n).map(|i| { let (x, z) = pos(i); g.climate.sample_d(x, z, fp) }).collect();
        let t = std::time::Instant::now();
        let mut shs = Vec::with_capacity(n as usize);
        for i in 0..n {
            let (x, z) = pos(i);
            shs.push(g.terrain.shape(&cds[i as usize], x, z, fp));
        }
        let t_sh = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let t = std::time::Instant::now();
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.terrain.shape_full(&g.climate, &cds[i as usize], x, z, fp).height.v;
        }
        let t_sf = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        let t = std::time::Instant::now();
        for i in 0..n {
            let (x, z) = pos(i);
            acc += g.terrain.finish(&cds[i as usize], &shs[i as usize], x, z).height;
        }
        let t_fi = t.elapsed().as_secs_f64() * 1e9 / n as f64;
        println!("fp {fp}: patch_mix {t_pm:.0} ns, patch_relief {t_pr:.0} ns, shape {t_sh:.0} ns, shape_full {t_sf:.0} ns, finish {t_fi:.0} ns [{acc}]");
    }
}
