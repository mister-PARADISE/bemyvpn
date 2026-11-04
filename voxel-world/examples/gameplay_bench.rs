//! Gameplay terrain pass: timing breakdown of the column pipeline.
//! `cargo run --release --example gameplay_bench -- [seed]`
use voxel_world::gen::WorldGen;

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
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
