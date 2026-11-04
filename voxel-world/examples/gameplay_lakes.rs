//! Gameplay pass: how many lake candidates pass the probe (per seed, 20×20 cells).
use voxel_world::gen::WorldGen;
fn main() {
    for seed in [1u64, 42, 2026] {
        let g = WorldGen::new(seed);
        let mut why = std::collections::BTreeMap::<&str, usize>::new();
        let (mut ok, mut rs) = (0, Vec::new());
        for iz in -10..10 {
            for ix in -10..10 {
                if let Some((_, _, r, res)) = g.terrain.lake_debug(&g.climate, ix, iz) {
                    match res {
                        Ok(_) => {
                            ok += 1;
                            rs.push(r as i32);
                        }
                        Err(e) => *why.entry(e).or_default() += 1,
                    }
                }
            }
        }
        rs.sort();
        println!("seed {seed}: {ok} lakes, radii {rs:?}, rejections {why:?}");
        let mut why = std::collections::BTreeMap::<&str, usize>::new();
        let mut ok = 0;
        for iz in -12..12 {
            for ix in -12..12 {
                if let Some((_, _, _, res)) = g.terrain.pond_debug(&g.climate, ix, iz) {
                    match res {
                        Ok(_) => ok += 1,
                        Err(e) => *why.entry(e).or_default() += 1,
                    }
                }
            }
        }
        println!("   ponds in 10×10 km: {ok}, rejections {why:?}");
    }
}
