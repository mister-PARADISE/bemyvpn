//! gen-core review: lake candidate rejection statistics.
use std::collections::HashMap;
use voxel_world::gen::WorldGen;
fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(42);
    let g = WorldGen::new(seed);
    let mut why: HashMap<&str, usize> = HashMap::new();
    let mut ok = Vec::new();
    for iz in -12..12 {
        for ix in -12..12 {
            if let Some((cx, cz, r, res)) = g.terrain.lake_debug(&g.climate, ix, iz) {
                match res {
                    Ok(l) => { *why.entry("OK").or_default() += 1; ok.push((cx, cz, r, l)); }
                    Err(e) => *why.entry(e).or_default() += 1,
                }
            }
        }
    }
    println!("{why:?}");
    for (cx, cz, r, l) in ok.iter().take(6) {
        // profile across the lake (step r/8)
        let mut prof = Vec::new();
        for k in -12..=12 {
            let x = cx + *r as f64 * k as f64 / 8.0;
            let c = g.column(x, *cz);
            prof.push(format!("{:.0}{}", c.height, if c.water_level > c.height as i32 { "w" } else { "" }));
        }
        println!("lake at ({cx:.0},{cz:.0}) r {r:.0} level {l}: {}", prof.join(" "));
    }
}
// cross-section helper appended below
