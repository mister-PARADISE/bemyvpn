//! Art-pass probe: open-plains weight coverage and relief inside plains.
use voxel_world::gen::WorldGen;
fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    let (mut land, mut p5, mut p9) = (0usize, 0usize, 0usize);
    let mut rel_hist = [0usize; 6];
    let mut chis: Vec<f32> = vec![];
    let mut es = vec![];
    for j in 0..160 {
        for i in 0..160 {
            let x = -8000.0 + i as f64 * 100.0 + 0.5;
            let z = -8000.0 + j as f64 * 100.0 + 0.5;
            let c = g.column(x, z);
            if c.height < 140.0 || c.water_level != i32::MIN {
                continue;
            }
            land += 1;
            let cd = g.climate.sample_d(x, z, 0.0);
            let r = g.terrain.regions(&cd, x, z, 0.0);
            es.push(cd.e.v);
            chis.push(g.terrain.character_at(x, z));
            if r.plain > 0.5 {
                p5 += 1;
            }
            if r.plain > 0.9 {
                p9 += 1;
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for dj in -4..=4 {
                    for di in -4..=4 {
                        let h = g.column(x + di as f64 * 25.0, z + dj as f64 * 25.0).height.max(140.0);
                        lo = lo.min(h);
                        hi = hi.max(h);
                    }
                }
                if hi - lo > 8.0 && r.swamp < 0.1 && (x as i64 + z as i64) % 700 == 0 {
                    let cd = g.climate.sample_d(x, z, 0.0);
                    eprintln!("  ({x:.0},{z:.0}) relief {:.0} h {:.0} biome {:?} river_dist {:.0} gate {:.2} desert {:.2} bad {:.2} lake {:.2} swamp {:.2}", hi - lo, c.height, c.biome, cd.river_dist, cd.river_gate, r.desert, r.badlands, r.lake, r.swamp);
                }
                let k = match hi - lo { r if r < 3.0 => 0, r if r < 6.0 => 1, r if r < 12.0 => 2, r if r < 24.0 => 3, r if r < 48.0 => 4, _ => 5 };
                rel_hist[k] += 1;
            }
        }
    }
    es.sort_by(|a, b| a.partial_cmp(b).unwrap());
    chis.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |v: &Vec<f32>, p: f64| v[((v.len() - 1) as f64 * p) as usize];
    eprintln!("chi on land: p10 {:.2} p25 {:.2} p40 {:.2} p50 {:.2} p60 {:.2} p75 {:.2} p90 {:.2}", q(&chis, 0.1), q(&chis, 0.25), q(&chis, 0.4), q(&chis, 0.5), q(&chis, 0.6), q(&chis, 0.75), q(&chis, 0.9));
    eprintln!("land {land}: plain>0.5 {:.1}%, >0.9 {:.1}%; relief hist in plains (<3,<6,<12,<24,<48,>48): {:?}", 100.0 * p5 as f64 / land as f64, 100.0 * p9 as f64 / land as f64, rel_hist);
    eprintln!("E* quantiles on land: p25 {:.2} p50 {:.2} p75 {:.2}", es[es.len() / 4], es[es.len() / 2], es[es.len() * 3 / 4]);
}
