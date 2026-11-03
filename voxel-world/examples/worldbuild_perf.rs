//! world-build: time default builds.
//! usage: worldbuild_perf [hf_levels] [seed cx cz]...
use voxel_world::gen::WorldGen;
use voxel_world::world::build::build_world_stats;
use voxel_world::world::BuildParams;

/// Process CPU time (user + system), seconds.
fn cpu_s() -> f64 {
    let st = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = st.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

fn main() {
    let a: Vec<i64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let levels = *a.first().unwrap_or(&5) as u32;
    let runs: Vec<(u64, i32, i32)> = if a.len() >= 4 {
        a[1..].chunks(3).map(|c| (c[0] as u64, c[1] as i32, c[2] as i32)).collect()
    } else {
        // per seed: the highest point and the densest forest of a coarse scan
        let mut v = Vec::new();
        for seed in [1u64, 42, 2026] {
            let gen = WorldGen::new(seed);
            let (mut hi, mut fo) = ((f32::MIN, 0, 0), (f32::MIN, 0, 0));
            for j in -16..=16 {
                for i in -16..=16 {
                    let (x, z) = (i * 500, j * 500);
                    let c = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                    if c.height > hi.0 {
                        hi = (c.height, x, z);
                    }
                    let cov = gen.canopy_lod(x as f64, z as f64, &c, 1.0).coverage * c.slope.min(0.6).max(0.2);
                    if cov > fo.0 {
                        fo = (cov, x, z);
                    }
                }
            }
            v.push((seed, hi.1, hi.2));
            v.push((seed, fo.1, fo.2));
        }
        v
    };
    for (seed, cx, cz) in runs {
        let gen = WorldGen::new(seed);
        let cave: u32 = std::env::var("CAVE").ok().and_then(|s| s.parse().ok()).unwrap_or(384);
        let p = BuildParams { center_x: cx, center_z: cz, hf_levels: levels, cave_radius: cave, ..Default::default() };
        let c0 = cpu_s();
        let (w, st) = build_world_stats(&gen, &p);
        let cpu = cpu_s() - c0;
        println!(
            "seed {seed} ({cx},{cz}) hf {levels}: total {:.2}s (cpu {cpu:.1}s = {:.1}s on 4 idle cores) | l0 {:.2} feat {:.2} asm {:.2} levels {:?} | spans {} ({:.1}/col) features {} | mem {:.0} MB",
            st.total_s,
            cpu / 4.0,
            st.level0_s,
            st.features_s,
            st.assemble_s,
            st.levels_s.iter().map(|t| (t * 100.0).round() / 100.0).collect::<Vec<_>>(),
            st.span_count,
            st.span_count as f64 / (p.l0_size as f64).powi(2),
            st.feature_count,
            st.memory_bytes as f64 / 1048576.0
        );
        drop(w);
    }
    let hwm = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmHWM")).map(|l| l.to_string()))
        .unwrap_or_default();
    println!("peak RSS: {hwm}");
}
