//! world-build micro-benchmarks of the generator calls used in the hot paths (single thread).
use std::time::Instant;
use voxel_world::gen::WorldGen;

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    let n = 40_000u64;
    let pts: Vec<(f64, f64)> =
        (0..n).map(|i| ((i % 200) as f64 * 9.3 - 900.0 + 0.5, (i / 200) as f64 * 9.1 - 900.0 + 0.5)).collect();
    let t = Instant::now();
    let cols: Vec<_> = pts.iter().map(|&(x, z)| g.column(x, z)).collect();
    eprintln!("column: {:.0} ns", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    for fp in [2.0, 4.0, 8.0, 32.0] {
        let t = Instant::now();
        let mut acc = 0.0;
        for &(x, z) in &pts {
            acc += g.column_lod(x, z, fp).height;
        }
        eprintln!("column_lod fp {fp}: {:.0} ns ({acc:.0})", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    }
    let jag: Vec<f32> = cols.iter().map(|c| c.jaggedness).collect();
    let mut js = jag.clone();
    js.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "jaggedness quantiles: 50% {:.1} 75% {:.1} 90% {:.1} 99% {:.1} max {:.1}; frac>0 {:.2}",
        js[js.len() / 2],
        js[js.len() * 3 / 4],
        js[js.len() * 9 / 10],
        js[js.len() * 99 / 100],
        js[js.len() - 1],
        jag.iter().filter(|&&j| j > 0.0).count() as f32 / n as f32
    );
    let t = Instant::now();
    let mut acc = 0.0f32;
    let mut cnt = 0u64;
    for c in &cols {
        for dy in -8..8 {
            acc += g.density(c.x, c.height as f64 + dy as f64 * 0.7, c.z, c);
            cnt += 1;
        }
    }
    eprintln!("density (near surface): {:.0} ns ({acc:.0})", t.elapsed().as_secs_f64() * 1e9 / cnt as f64);
    let t = Instant::now();
    let mut cnt = 0u64;
    for c in cols.iter().take(4000) {
        let mut y = 2.0;
        while y < c.height as f64 + 20.0 {
            acc += g.cave_field(c.x, y, c.z, c);
            cnt += 1;
            y += 4.0;
        }
    }
    eprintln!("cave_field: {:.0} ns ({cnt} samples, {acc:.0})", t.elapsed().as_secs_f64() * 1e9 / cnt as f64);
    let t = Instant::now();
    let mut cnt = 0u64;
    let mut h = 0u64;
    for c in cols.iter().take(4000) {
        let top = c.height as i32;
        for y in (top - 30).max(0)..top {
            h = h.wrapping_add(g.surface_material(c.x as i32, y, c.z as i32, top - y, c) as u64);
            cnt += 1;
        }
    }
    eprintln!("surface_material: {:.0} ns ({h})", t.elapsed().as_secs_f64() * 1e9 / cnt as f64);
    let t = Instant::now();
    let mut cnt = 0u64;
    for c in cols.iter().take(4000) {
        for y in (4..c.height as i32).step_by(3) {
            h = h.wrapping_add(g.cave_fill(c.x as i32, y, c.z as i32, c) as u64);
            cnt += 1;
        }
    }
    eprintln!("cave_fill: {:.0} ns", t.elapsed().as_secs_f64() * 1e9 / cnt as f64);
    let t = Instant::now();
    let mut nf = 0;
    let mut ncol = 0;
    for i in 0..16 {
        let f = g.features_in(i * 64 - 512, -300, i * 64 - 448, -236);
        nf += f.len();
        ncol += f.iter().map(|f| f.columns.len()).sum::<usize>();
    }
    eprintln!(
        "features_in 64x64: {:.2} ms per tile, {} features, {} columns",
        t.elapsed().as_secs_f64() * 1e3 / 16.0,
        nf,
        ncol
    );
    let t = Instant::now();
    let mut acc = 0.0;
    for c in &cols {
        acc += g.canopy_lod(c.x, c.z, c, 4.0).coverage;
    }
    eprintln!("canopy_lod: {:.0} ns ({acc:.0})", t.elapsed().as_secs_f64() * 1e9 / n as f64);
}
