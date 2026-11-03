//! gen-core debug tool: biome map, hillshaded relief, "satellite" albedo map and vertical
//! cross-sections of the 3D density.
//!
//! cargo run --release --example gencore_map -- --seed 1 [--cx 0 --cz 0 --size 2048 --scale 8]
//!     [--out target/agent-gencore] [--no-xsec] [--full]
use rayon::prelude::*;
use std::path::PathBuf;
use voxel_world::gen::{Biome, ColumnInfo, WorldGen, SEA_LEVEL, WORLD_HEIGHT};
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{far_albedo, grass_tint, linear_to_srgb, terracotta_band, Material};
use voxel_world::math::Vec3;

fn arg<T: std::str::FromStr>(args: &[String], name: &str, def: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(def)
}

fn to8(c: Vec3) -> [u8; 3] {
    let f = |v: f32| (linear_to_srgb(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [f(c.x), f(c.y), f(c.z)]
}

fn srgb(c: [u8; 3]) -> Vec3 {
    voxel_world::materials::srgb8(c[0], c[1], c[2])
}

fn hypsometric(h: f32) -> Vec3 {
    // (height, sRGB colour) stops
    const STOPS: [(f32, [u8; 3]); 8] = [
        (140.0, [92, 140, 74]),
        (165.0, [128, 166, 92]),
        (200.0, [186, 186, 120]),
        (250.0, [176, 146, 98]),
        (310.0, [150, 118, 92]),
        (370.0, [140, 134, 128]),
        (420.0, [205, 205, 210]),
        (480.0, [250, 250, 255]),
    ];
    if h <= STOPS[0].0 {
        return srgb(STOPS[0].1);
    }
    for w in STOPS.windows(2) {
        if h <= w[1].0 {
            let t = (h - w[0].0) / (w[1].0 - w[0].0);
            return srgb(w[0].1).lerp(srgb(w[1].1), t);
        }
    }
    srgb(STOPS[7].1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = arg(&args, "--seed", 1);
    let cx: f64 = arg(&args, "--cx", 0.0);
    let cz: f64 = arg(&args, "--cz", 0.0);
    let size: usize = arg(&args, "--size", 2048);
    let scale: f64 = arg(&args, "--scale", 8.0);
    let out: PathBuf = PathBuf::from(arg(&args, "--out", "target/agent-gencore".to_string()));
    let tag: String = arg(&args, "--tag", String::new());
    let xsec = !args.iter().any(|a| a == "--no-xsec");
    let full = args.iter().any(|a| a == "--full");
    let fp = if full { 0.0 } else { scale };

    let g = WorldGen::new(seed);
    let t0 = std::time::Instant::now();
    let x0 = cx - size as f64 * scale * 0.5;
    let z0 = cz - size as f64 * scale * 0.5;
    let cols: Vec<ColumnInfo> = (0..size)
        .into_par_iter()
        .flat_map_iter(|j| {
            let g = &g;
            (0..size).map(move |i| {
                g.column_lod(x0 + (i as f64 + 0.5) * scale, z0 + (j as f64 + 0.5) * scale, fp)
            })
        })
        .collect();
    let dt = t0.elapsed().as_secs_f64();
    eprintln!(
        "seed {seed}: {} columns in {:.2}s ({:.3} µs/col on all cores)",
        cols.len(),
        dt,
        dt * 1e6 / cols.len() as f64
    );

    // statistics
    let mut counts = [0usize; voxel_world::gen::BIOME_COUNT];
    let mut hmax = 0.0f32;
    let mut water = 0usize;
    for c in &cols {
        counts[c.biome as usize] += 1;
        hmax = hmax.max(c.height);
        if c.water_level > c.height as i32 {
            water += 1;
        }
    }
    eprintln!("max height {hmax:.1}, water {:.1}%", 100.0 * water as f64 / cols.len() as f64);
    {
        // highest summits, at least 2 km apart
        let mut idx: Vec<usize> = (0..cols.len()).collect();
        idx.sort_by(|&a, &b| cols[b].height.partial_cmp(&cols[a].height).unwrap());
        let mut tops: Vec<usize> = Vec::new();
        for k in idx {
            let c = &cols[k];
            if tops.iter().all(|&t| ((cols[t].x - c.x).powi(2) + (cols[t].z - c.z).powi(2)).sqrt() > 2000.0) {
                tops.push(k);
            }
            if tops.len() == 5 {
                break;
            }
        }
        for t in tops {
            let c = &cols[t];
            eprintln!("  summit {:.0} at ({:.0}, {:.0}) {:?}", c.height, c.x, c.z, c.biome);
        }
    }
    let mut v: Vec<(usize, usize)> = counts.iter().cloned().enumerate().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let s: Vec<String> = v
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(b, n)| format!("{} {:.1}%", Biome::from_u8(*b as u8).name(), 100.0 * *n as f64 / cols.len() as f64))
        .collect();
    eprintln!("biomes: {}", s.join(", "));

    // hillshade
    let hgt = |i: usize, j: usize| cols[j.min(size - 1) * size + i.min(size - 1)].height.max(SEA_LEVEL as f32 - 0.0);
    let light = Vec3::new(-1.0, 1.1, -1.0).normalize();
    let shade: Vec<f32> = (0..size * size)
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % size, k / size);
            let hx = (hgt(i + 1, j) - hgt(i.saturating_sub(1), j)) / (2.0 * scale as f32);
            let hz = (hgt(i, j + 1) - hgt(i, j.saturating_sub(1))) / (2.0 * scale as f32);
            let n = Vec3::new(-hx, 1.0, -hz).normalize();
            n.dot(light).max(0.0) / light.y.max(0.3)
        })
        .collect();

    let mut biome_img = vec![0u8; size * size * 3];
    let mut relief_img = vec![0u8; size * size * 3];
    let mut sat_img = vec![0u8; size * size * 3];
    biome_img
        .par_chunks_mut(3)
        .zip(relief_img.par_chunks_mut(3))
        .zip(sat_img.par_chunks_mut(3))
        .enumerate()
        .for_each(|(k, ((b, r), s))| {
            let c = &cols[k];
            let sh = shade[k];
            let wet = c.water_level > c.height as i32;
            let bc = srgb(c.biome.map_color()) * (0.55 + 0.45 * sh);
            b.copy_from_slice(&to8(bc));
            let col = if wet {
                let depth = c.water_level as f32 - c.height;
                let lava = c.water_level > SEA_LEVEL && c.biome == Biome::Volcanic;
                if lava {
                    Vec3::new(1.0, 0.25, 0.02)
                } else {
                    let t = (depth / 60.0).clamp(0.0, 1.0);
                    srgb([90, 170, 210]).lerp(srgb([12, 30, 90]), t.sqrt())
                }
            } else {
                let mut cc = hypsometric(c.height) * (0.35 + 0.75 * sh);
                // contour lines every 40 blocks
                let fh = c.height / 40.0;
                if (fh - fh.round()).abs() < 0.04 * scale as f32 / 8.0 {
                    cc = cc * 0.8;
                }
                cc
            };
            r.copy_from_slice(&to8(col));
            // satellite-style: top material albedo
            let (x, z) = ((c.x).floor() as i32, (c.z).floor() as i32);
            let y = c.height.floor() as i32;
            let mat = g.surface_material(x, y, z, 0, c);
            let mut a = far_albedo(mat, c.surface_temperature, c.climate.humidity, y as f32);
            if wet {
                let depth = c.water_level as f32 - c.height;
                if c.water_level > SEA_LEVEL && c.biome == Biome::Volcanic {
                    a = Vec3::new(1.0, 0.3, 0.05);
                } else {
                    let wcol = voxel_world::materials::water_tint(if c.biome == Biome::Swamp { 0.7 } else { 0.0 }, c.climate.temperature);
                    a = a.lerp(wcol * 0.35, (depth / 8.0).clamp(0.3, 0.95));
                }
            }
            s.copy_from_slice(&to8(a * (0.25 + 0.85 * sh)));
        });
    let name = |kind: &str| out.join(format!("seed{seed}{tag}_{kind}.png"));
    write_png_rgb8(&name("biome"), size as u32, size as u32, &biome_img).unwrap();
    write_png_rgb8(&name("relief"), size as u32, size as u32, &relief_img).unwrap();
    write_png_rgb8(&name("satellite"), size as u32, size as u32, &sat_img).unwrap();
    eprintln!("wrote {}", name("*").display());

    if !xsec {
        return;
    }
    // ---- vertical cross-sections through the highest jagged terrain
    let mut cands: Vec<(f32, usize)> = cols
        .iter()
        .enumerate()
        .filter(|(_, c)| c.jaggedness > 8.0)
        .map(|(k, c)| (c.height + c.jaggedness * 4.0, k))
        .collect();
    cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let mut picked: Vec<usize> = Vec::new();
    for &(_, k) in &cands {
        let (i, j) = (k % size, k / size);
        if picked.iter().all(|&p| {
            let (pi, pj) = (p % size, p / size);
            (pi as i64 - i as i64).abs() + (pj as i64 - j as i64).abs() > (size / 6) as i64
        }) {
            picked.push(k);
        }
        if picked.len() == 3 {
            break;
        }
    }
    let w = 1024usize;
    let hgt_px = WORLD_HEIGHT as usize;
    for (si, &k) in picked.iter().enumerate() {
        let c = &cols[k];
        let zc = c.z.floor() as i32;
        let xc = c.x.floor() as i32;
        let mut img = vec![0u8; w * hgt_px * 3];
        let rows: Vec<Vec<[u8; 3]>> = (0..w)
            .into_par_iter()
            .map(|i| {
                let x = xc - (w as i32) / 2 + i as i32;
                let col = g.column(x as f64 + 0.5, zc as f64 + 0.5);
                let mut out = vec![[0u8; 3]; hgt_px];
                let mut depth = -1i32;
                for y in (0..WORLD_HEIGHT).rev() {
                    let d = g.density(x as f64 + 0.5, y as f64 + 0.5, zc as f64 + 0.5, &col);
                    let px = if d > 0.0 {
                        depth += 1;
                        let m = g.surface_material(x, y, zc, depth, &col);
                        let a = match m {
                            Material::Grass | Material::Moss => grass_tint(col.surface_temperature, col.climate.humidity),
                            Material::Terracotta => terracotta_band(y as f32, 0.0),
                            _ => m.albedo(),
                        };
                        to8(a * if col.jaggedness > 0.0 && (col.height - y as f32).abs() < col.jaggedness { 1.0 } else { 0.85 })
                    } else {
                        depth = -1;
                        if y < col.water_level {
                            if col.water_level > SEA_LEVEL && col.biome == Biome::Volcanic { [255, 90, 20] } else { [40, 90, 200] }
                        } else {
                            let t = y as f32 / 512.0;
                            to8(srgb([200, 225, 250]).lerp(srgb([110, 160, 230]), t))
                        }
                    };
                    out[y as usize] = px;
                }
                // mark the 2D height and band edges
                let hy = col.height.floor() as i32;
                if (0..WORLD_HEIGHT).contains(&hy) && col.jaggedness == 0.0 {
                    // nothing
                }
                out
            })
            .collect();
        for (i, colpx) in rows.iter().enumerate() {
            for y in 0..hgt_px {
                let row = hgt_px - 1 - y;
                let o = (row * w + i) * 3;
                img[o..o + 3].copy_from_slice(&colpx[y]);
            }
        }
        let p = out.join(format!("seed{seed}{tag}_xsec{si}.png"));
        write_png_rgb8(&p, w as u32, hgt_px as u32, &img).unwrap();
        eprintln!("xsec {si}: z={zc}, x={}..{} biome {:?} J={:.1} → {}", xc - w as i32 / 2, xc + w as i32 / 2, c.biome, c.jaggedness, p.display());
    }
}
