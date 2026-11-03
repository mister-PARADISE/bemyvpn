//! Art-pass close-up renders with hand-placed or auto-found cameras.
//!
//! cargo run --release --example artpass_view -- SEED KIND OUT.png [cx cz]
//! KIND: cliff (steep face with rock-shelter notches), stack (sea stacks), snow (snowy face),
//!       plain (open plains), mesa (badlands), or `at` with explicit
//!       `cam_x cam_y cam_z tgt_x tgt_y tgt_z`.
use voxel_world::gen::{Biome, WorldGen, SEA_LEVEL};
use voxel_world::render::{render, save_png, Camera, RenderSettings, Sun};
use voxel_world::world::{build_world, BuildParams};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let kind = a[1].as_str();
    let out = std::path::PathBuf::from(&a[2]);
    let f = |i: usize| -> f64 { a[i].parse().unwrap() };
    let g = WorldGen::new(seed);
    let (cx, cz) = if a.len() > 4 && kind != "at" { (f(3), f(4)) } else { (0.0, 0.0) };
    let (cam, tgt) = if kind == "at" {
        ([f(3), f(4), f(5)], [f(6), f(7), f(8)])
    } else {
        find(&g, kind, cx, cz)
    };
    eprintln!("camera {:?} → {:?}", cam, tgt);
    let p = BuildParams {
        center_x: cam[0] as i32,
        center_z: cam[2] as i32,
        l0_size: 1024,
        hf_size: 1024,
        hf_levels: 4,
        cave_radius: 0,
        feature_radius: 400,
    };
    let t = std::time::Instant::now();
    let w = build_world(&g, &p);
    eprintln!("build {:.1}s", t.elapsed().as_secs_f64());
    let camera = Camera::look_at(cam, tgt, 60.0);
    let dir = [tgt[0] - cam[0], tgt[2] - cam[2]];
    let az = dir[0].atan2(dir[1]).to_degrees() + 120.0;
    let sun = Sun { azimuth_deg: az, elevation_deg: 32.0 };
    let s = RenderSettings { width: 960, height: 540, aa: false, ..Default::default() };
    let img = render(&w, &camera, &sun, &s);
    save_png(&img, &out).unwrap();
    eprintln!("render {:.1}s → {}", t.elapsed().as_secs_f64(), out.display());
}

fn find(g: &WorldGen, kind: &str, cx: f64, cz: f64) -> ([f64; 3], [f64; 3]) {
    let sea = SEA_LEVEL as f32;
    let mut best = (f32::MIN, 0.0f64, 0.0f64);
    let step = 12.0;
    let n = 500;
    for j in 0..n {
        for i in 0..n {
            let x = cx - 3000.0 + i as f64 * step + 0.5;
            let z = cz - 3000.0 + j as f64 * step + 0.5;
            let c = g.column_lod(x, z, 4.0);
            let score = match kind {
                "cliff" => {
                    if c.water_level != i32::MIN || c.height < sea + 20.0 {
                        continue;
                    }
                    c.slope.min(2.5) + 0.02 * c.jaggedness - 0.002 * (c.height - 260.0).abs()
                }
                "stack" => {
                    if c.slope < 3.0 || c.height < sea + 6.0 || c.height > sea + 40.0 {
                        continue;
                    }
                    if c.climate.continentalness > -0.12 {
                        continue;
                    }
                    c.height
                }
                "snow" => {
                    if !matches!(c.biome, Biome::SnowyPeaks | Biome::SnowySlopes | Biome::Glacier) {
                        continue;
                    }
                    c.slope.min(1.4) + 0.01 * (c.height - 380.0)
                }
                "plain" => {
                    if c.water_level != i32::MIN || !matches!(c.biome, Biome::Plains | Biome::Meadow | Biome::Savanna) {
                        continue;
                    }
                    -c.slope - 0.01 * (c.height - sea - 6.0).abs()
                }
                "mesa" => {
                    if !matches!(c.biome, Biome::Badlands) {
                        continue;
                    }
                    c.slope.min(3.0)
                }
                _ => panic!("kind"),
            };
            // light pseudo-random tie-break so we do not always pick the grid corner
            let score = score + 0.001 * ((i * 7 + j * 13) % 17) as f32;
            if score > best.0 {
                best = (score, x, z);
            }
        }
    }
    let (_, x, z) = best;
    let c = g.column(x, z);
    // camera: back off along the downhill direction (approximate by sampling around)
    let mut low = (f32::MAX, 0.0, 0.0);
    let r = match kind {
        "plain" => 60.0,
        "snow" => 260.0,
        "stack" => 70.0,
        _ => 90.0,
    };
    for k in 0..16 {
        let ang = k as f64 * std::f64::consts::TAU / 16.0;
        let (px, pz) = (x + r * ang.sin(), z + r * ang.cos());
        let h = g.column(px, pz).height.max(sea);
        if h < low.0 {
            low = (h, px, pz);
        }
    }
    let up = match kind {
        "plain" => 25.0,
        "snow" => 60.0,
        "stack" => 14.0,
        _ => 18.0,
    };
    let cam = [low.1, low.0 as f64 + up, low.2];
    let tgt = if kind == "plain" {
        [x + (x - low.1) * 8.0, c.height as f64 - 5.0, z + (z - low.2) * 8.0]
    } else {
        [x, c.height as f64 - 4.0, z]
    };
    (cam, tgt)
}
