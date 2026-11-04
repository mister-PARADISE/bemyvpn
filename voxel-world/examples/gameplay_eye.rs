//! Gameplay terrain pass: player-eye views (camera 2–20 blocks above the ground).
//!
//! `cargo run --release --example gameplay_eye -- SEED OUTDIR [KINDS] [CX CZ]`
//! KINDS: comma list of open,forest,dry,mtn,coast,river,any (default: all) — one view each,
//! spot found by hashed random sampling within 3 km of (CX, CZ), view direction = the most
//! varied of 12 directions.
//! `cargo run --release --example gameplay_eye -- SEED OUTDIR at CAMX CAMY CAMZ TX TY TZ`
use voxel_world::gen::{Biome, ColumnInfo, WorldGen, SEA_LEVEL};
use voxel_world::noise::{hash_f01, mix64};
use voxel_world::render::{render, save_png, Camera, RenderSettings, Sun};
use voxel_world::world::{build_world, BuildParams};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let out = std::path::PathBuf::from(&a[1]);
    std::fs::create_dir_all(&out).unwrap();
    let g = WorldGen::new(seed);
    if a.get(2).map(|s| s.as_str()) == Some("at") {
        let f = |i: usize| -> f64 { a[i].parse().unwrap() };
        let cam = [f(3), f(4), f(5)];
        let tgt = [f(6), f(7), f(8)];
        shoot(&g, cam, tgt, &out.join(format!("s{seed}_at_{}_{}.png", cam[0] as i64, cam[2] as i64)));
        return;
    }
    let kinds: Vec<String> = a
        .get(2)
        .map(|s| s.split(',').map(|k| k.to_string()).collect())
        .unwrap_or_else(|| ["open", "forest", "dry", "mtn", "coast", "river"].iter().map(|s| s.to_string()).collect());
    let (cx, cz) = if a.len() > 4 { (a[3].parse().unwrap(), a[4].parse().unwrap()) } else { (0.0, 0.0) };
    for (ki, kind) in kinds.iter().enumerate() {
        let Some((x, z)) = find(&g, kind, cx, cz, seed ^ ki as u64) else {
            eprintln!("{kind}: no spot");
            continue;
        };
        let c = g.column(x, z);
        let ground = c.height.max(c.water_level as f32).max(SEA_LEVEL as f32) as f64;
        let eye = match kind.as_str() {
            "forest" => 22.0,
            k if k.starts_with("style:") => 7.0,
            "mtn" => 8.0,
            "open" => 5.0,
            "river" => 12.0,
            _ => 9.0,
        };
        // most varied direction
        let mut best = (f32::MIN, 0.0f64);
        for k in 0..12 {
            let ang = k as f64 * std::f64::consts::TAU / 12.0;
            let (mut s1, mut s2, mut n) = (0.0f32, 0.0f32, 0.0f32);
            let mut blocked = false;
            for r in 1..=40 {
                let d = r as f64 * 8.0;
                let cc = g.column_lod(x + d * ang.sin(), z + d * ang.cos(), 2.0);
                let h = cc.height.max(SEA_LEVEL as f32);
                if r < 4 && h as f64 > ground + eye - 1.0 {
                    blocked = true;
                }
                s1 += h;
                s2 += h * h;
                n += 1.0;
            }
            let var = (s2 / n - (s1 / n) * (s1 / n)).max(0.0).sqrt();
            let score = var - if blocked { 100.0 } else { 0.0 };
            if score > best.0 {
                best = (score, ang);
            }
        }
        let ang = best.1;
        let cam = [x, ground + eye, z];
        let tgt = [x + 100.0 * ang.sin(), ground + eye - 8.0, z + 100.0 * ang.cos()];
        eprintln!("{kind}: ({x:.0}, {z:.0}) biome {:?} h {:.1} → camera {:?}", c.biome, c.height, cam);
        shoot(&g, cam, tgt, &out.join(format!("s{seed}_{ki}_{kind}.png")));
    }
}

fn want(g: &WorldGen, kind: &str, c: &ColumnInfo) -> bool {
    use Biome::*;
    let dry = c.water_level == i32::MIN;
    let sea = SEA_LEVEL as f32;
    if let Some(st) = kind.strip_prefix("style:") {
        if !dry || c.height < sea + 3.0 {
            return false;
        }
        let cd = g.climate.sample_d(c.x, c.z, 0.0);
        let r = g.terrain.shape(&cd, c.x, c.z, 0.0).regions;
        return format!("{:?}", r.style) == st && r.plain < 0.5 && cd.orogeny.v < 0.3;
    }
    match kind {
        "open" => dry && matches!(c.biome, Plains | Meadow | Savanna) && c.height < sea + 80.0,
        "forest" => dry && matches!(c.biome, Forest | BirchForest | DarkForest | Taiga | OldGrowthTaiga | Jungle),
        "dry" => dry && matches!(c.biome, Desert | Badlands | Hoodoos | Savanna),
        "mtn" => dry && c.height > sea + 140.0 && c.slope < 0.5,
        "coast" => dry && matches!(c.biome, Beach | StonyShore),
        "river" => dry && c.climate.river > 0.6 && c.height < sea + 3.0,
        "snow" => dry && matches!(c.biome, SnowySlopes | SnowyPlains | Glacier) && c.slope < 0.6,
        _ => dry && c.height > sea + 1.0,
    }
}

fn find(g: &WorldGen, kind: &str, cx: f64, cz: f64, salt: u64) -> Option<(f64, f64)> {
    for i in 0..40_000u64 {
        let h = mix64(salt.wrapping_mul(0x9E37_79B9) ^ i);
        let x = (cx + (hash_f01(h) as f64 - 0.5) * 6000.0).floor() + 0.5;
        let z = (cz + (hash_f01(mix64(h)) as f64 - 0.5) * 6000.0).floor() + 0.5;
        let c = g.column_lod(x, z, 4.0);
        if want(g, kind, &c) {
            let c = g.column(x, z);
            if want(g, kind, &c) {
                return Some((x, z));
            }
        }
    }
    None
}

fn shoot(g: &WorldGen, cam: [f64; 3], tgt: [f64; 3], path: &std::path::Path) {
    let p = BuildParams {
        center_x: cam[0] as i32,
        center_z: cam[2] as i32,
        l0_size: 512,
        hf_size: 512,
        hf_levels: 4,
        cave_radius: 160,
        feature_radius: if std::env::var("BARE").is_ok() { 0 } else { 256 },
    };
    let t = std::time::Instant::now();
    let w = build_world(g, &p);
    let tb = t.elapsed().as_secs_f64();
    let camera = Camera::look_at(cam, tgt, 70.0);
    let dir = [tgt[0] - cam[0], tgt[2] - cam[2]];
    let az = dir[0].atan2(dir[1]).to_degrees() + 125.0;
    let sun = Sun { azimuth_deg: az, elevation_deg: 38.0 };
    let s = RenderSettings { width: 960, height: 540, aa: false, ..Default::default() };
    let img = render(&w, &camera, &sun, &s);
    save_png(&img, path).unwrap();
    eprintln!("  build {tb:.1}s, total {:.1}s → {}", t.elapsed().as_secs_f64(), path.display());
}
