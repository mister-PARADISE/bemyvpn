//! Gameplay terrain pass: close-range hillshade + biome map (walking scale).
//! `cargo run --release --example gameplay_map -- SEED CX CZ BLOCKS_PER_PX OUT.png [SIZE]`
//! Each pixel = BLOCKS_PER_PX blocks; light from the north-west; biome colours tinted by relief,
//! water blue, snow white. Use 1–4 blocks/px to judge landforms on a 100–300 block scale.
use rayon::prelude::*;
use voxel_world::gen::{Biome, WorldGen, SEA_LEVEL};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let cx: f64 = a[1].parse().unwrap();
    let cz: f64 = a[2].parse().unwrap();
    let s: f64 = a[3].parse().unwrap();
    let out = &a[4];
    let n: usize = a.get(5).and_then(|v| v.parse().ok()).unwrap_or(1024);
    let g = WorldGen::new(seed);
    let fp = if s <= 1.0 { 0.0 } else { s };
    let t = std::time::Instant::now();
    let cols: Vec<_> = (0..(n + 1) * (n + 1))
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % (n + 1), k / (n + 1));
            let x = cx + (i as f64 - n as f64 * 0.5) * s + 0.5;
            let z = cz + (j as f64 - n as f64 * 0.5) * s + 0.5;
            let c = g.column_lod(x, z, fp);
            // top of the 3D band (overhang tops / spires) for the shading height
            (c.height, c.water_level, c.biome, c.jaggedness, c.slope)
        })
        .collect();
    eprintln!("{} columns in {:.2}s", cols.len(), t.elapsed().as_secs_f64());
    let mut img = vec![0u8; n * n * 3];
    let sea = SEA_LEVEL as f32;
    img.par_chunks_mut(n * 3).enumerate().for_each(|(j, row)| {
        for i in 0..n {
            let c = cols[j * (n + 1) + i];
            let cx1 = cols[j * (n + 1) + i + 1];
            let cz1 = cols[(j + 1) * (n + 1) + i];
            let (h, wl, b, jag, slope) = c;
            let hb = h.floor();
            let dx = (cx1.0.floor() - hb) / s as f32;
            let dz = (cz1.0.floor() - hb) / s as f32;
            // light from NW-up
            let (lx, ly, lz) = (-0.6f32, 0.7f32, -0.4f32);
            let nl = (1.0 + dx * dx + dz * dz).sqrt();
            let sh = ((-dx * lx + ly - dz * lz) / nl).max(0.0);
            let shade = 0.15 + 1.05 * sh;
            let mut col = b.map_color().map(|v| v as f32 / 255.0);
            let wet = wl != i32::MIN && wl as f32 > h;
            if wet {
                let d = (wl as f32 - h).min(30.0) / 30.0;
                col = [0.15 - 0.1 * d, 0.35 - 0.15 * d, 0.75 - 0.2 * d];
            } else if matches!(b, Biome::SnowyPeaks | Biome::SnowySlopes | Biome::SnowyPlains | Biome::Glacier) {
                col = [0.95, 0.96, 0.98];
            }
            if !wet && slope > 1.2 {
                col = [0.45, 0.43, 0.42];
            }
            if !wet && jag > 0.5 {
                col = [col[0] * 0.9 + 0.05, col[1] * 0.9, col[2] * 0.9 + 0.08];
            }
            if !wet {
                // desaturate so the relief reads first
                let l = 0.3 * col[0] + 0.55 * col[1] + 0.15 * col[2];
                col = [0.55 * col[0] + 0.45 * l, 0.55 * col[1] + 0.45 * l, 0.55 * col[2] + 0.45 * l];
            }
            let k = if wet { 0.85 + 0.15 * shade } else { shade };
            // faint contour lines every 10 blocks
            let cont = if !wet && ((hb as i32).rem_euclid(10) == 0) && (cx1.0.floor() as i32 / 10 != hb as i32 / 10 || cz1.0.floor() as i32 / 10 != hb as i32 / 10) { 0.82 } else { 1.0 };
            let _ = sea;
            for ch in 0..3 {
                let v = (col[ch] * k * cont).clamp(0.0, 1.0);
                row[i * 3 + ch] = (v.powf(1.0 / 2.2) * 255.0) as u8;
            }
        }
    });
    let f = std::fs::File::create(out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), n as u32, n as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
    eprintln!("→ {out}");
}
