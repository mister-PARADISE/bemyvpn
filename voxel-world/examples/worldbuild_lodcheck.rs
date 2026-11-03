//! world-build: compare level-0 voxel tops with the far-LOD expectation (terrain + canopy).
use voxel_world::gen::WorldGen;
use voxel_world::world::{build_world, BuildParams};

fn main() {
    let a: Vec<i64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let seed = *a.first().unwrap_or(&2026) as u64;
    let gen = WorldGen::new(seed);
    let centres: Vec<(i32, i32)> = if a.len() >= 3 {
        vec![(a[1] as i32, a[2] as i32)]
    } else {
        let mut v = Vec::new();
        for j in -3..=3 {
            for i in -3..=3 {
                v.push((i * 1500, j * 1500));
            }
        }
        v
    };
    for (cx, cz) in centres {
        let p = BuildParams { center_x: cx, center_z: cz, l0_size: 128, hf_size: 64, hf_levels: 0, cave_radius: 0, feature_radius: 1000 };
        let w = build_world(&gen, &p);
        let p0 = BuildParams { feature_radius: 0, ..p };
        let w0 = build_world(&gen, &p0);
        let (mut dt, mut dterr, mut can, mut cov_s, mut hgt_s, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0);
        let mut covered = 0usize;
        for j in 16..112 {
            for i in 16..112 {
                let (x, z) = (w.l0.origin_x + i, w.l0.origin_z + j);
                let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                if col.water_level != i32::MIN && col.water_level as f32 > col.height {
                    continue;
                }
                let top = w.l0.spans(x, z).iter().filter(|s| s.mat.is_opaque()).map(|s| s.y1).max().unwrap() as f64;
                let top0 = w0.l0.spans(x, z).iter().filter(|s| s.mat.is_opaque()).map(|s| s.y1).max().unwrap() as f64;
                if top > top0 + 0.5 {
                    covered += 1;
                }
                dt += top - top0;
                dterr += top0 - col.height as f64;
                let c = gen.canopy_lod(x as f64 + 0.5, z as f64 + 0.5, &col, 2.0);
                can += (c.coverage * c.height) as f64;
                cov_s += c.coverage as f64;
                hgt_s += c.height as f64;
                n += 1;
            }
        }
        if n == 0 {
            continue;
        }
        let nf = n as f64;
        println!(
            "({cx:6},{cz:6}) n {n:5}: voxel tree excess {:6.2}  canopy_lod cov*h {:6.2} | covered frac {:.2} vs lod cov {:.2}, lod h {:5.1} | terrain bias {:+.3}",
            dt / nf,
            can / nf,
            covered as f64 / nf,
            cov_s / nf,
            hgt_s / nf,
            dterr / nf
        );
    }
}
