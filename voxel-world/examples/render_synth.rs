//! Renders the renderer test scenes on synthetic analytic worlds into target/agent-render/.
//!
//! usage: cargo run --release --example render_synth -- [scene|all] [width height] [l0 hf levels] [--no-aa]
//! scenes: noon, sunset, high, diorama, cave
use std::time::Instant;
use voxel_world::render::synth::{synthetic_world, SynthTerrain};
use voxel_world::render::{render_with_stats, save_png, Camera, RenderSettings, Sun, ViewMode};

struct SceneDef {
    name: &'static str,
    center: (i32, i32),
    cam: [f64; 3],
    yaw: f64,
    pitch: f64,
    fov: f64,
    sun: (f64, f64),
    l0: u32,
    hf: u32,
    levels: u32,
    mode: ViewMode,
    haze: f32,
    exposure: f32,
}

fn scenes(t: &SynthTerrain) -> Vec<SceneDef> {
    let ground = |x: f64, z: f64| t.height(x, z, 0.0).0;
    vec![
        SceneDef {
            name: "noon",
            center: (0, 0),
            cam: [-25.0, ground(-25.0, -40.0) + 22.0, -40.0],
            yaw: 35f64.to_radians(),
            pitch: (-9f64).to_radians(),
            fov: 60.0,
            sun: (150.0, 58.0),
            l0: 2048,
            hf: 2048,
            levels: 3,
            mode: ViewMode::Normal,
            haze: 1.0,
            exposure: 1.0,
        },
        SceneDef {
            name: "sunset",
            center: (600, 0),
            cam: [600.0, 175.0, 0.0],
            yaw: 95f64.to_radians(),
            pitch: (-4f64).to_radians(),
            fov: 55.0,
            sun: (100.0, 4.0),
            l0: 2048,
            hf: 2048,
            levels: 3,
            mode: ViewMode::Normal,
            haze: 1.0,
            exposure: 1.0,
        },
        SceneDef {
            name: "high",
            center: (0, 0),
            cam: [0.0, 2600.0, 0.0],
            yaw: 200f64.to_radians(),
            pitch: (-28f64).to_radians(),
            fov: 60.0,
            sun: (60.0, 32.0),
            l0: 2048,
            hf: 2048,
            levels: 4,
            mode: ViewMode::Normal,
            haze: 1.0,
            exposure: 1.0,
        },
        SceneDef {
            name: "diorama",
            center: (40, 70),
            cam: [40.0 - 190.0, ground(40.0, 70.0) + 150.0, 70.0 - 170.0],
            yaw: 0.0,
            pitch: 0.0,
            fov: 42.0,
            sun: (215.0, 42.0),
            l0: 256,
            hf: 256,
            levels: 0,
            mode: ViewMode::Diorama { min_x: -45, max_x: 125, min_z: -15, max_z: 155, min_y: 60 },
            haze: 1.0,
            exposure: 1.0,
        },
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_aa = args.iter().any(|a| a == "--no-aa");
    let args: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let which = args.first().map(|s| s.as_str()).unwrap_or("all").to_string();
    let w: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let h: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let l0o: Option<u32> = args.get(3).and_then(|s| s.parse().ok());
    let hfo: Option<u32> = args.get(4).and_then(|s| s.parse().ok());
    let lvo: Option<u32> = args.get(5).and_then(|s| s.parse().ok());
    let seed = 7u64;
    let terrain = SynthTerrain::new(seed);
    for sc in scenes(&terrain) {
        if which != "all" && which != sc.name {
            continue;
        }
        let tb = Instant::now();
        let l0 = l0o.unwrap_or(sc.l0);
        let world = synthetic_world(seed, sc.center.0, sc.center.1, l0, hfo.unwrap_or(sc.hf), lvo.unwrap_or(sc.levels));
        let build_s = tb.elapsed().as_secs_f64();
        let mut cam = Camera { pos: sc.cam, yaw: sc.yaw, pitch: sc.pitch, fov_y_deg: sc.fov };
        if let ViewMode::Diorama { min_x, max_x, min_z, max_z, min_y } = sc.mode {
            let c = [(min_x + max_x) as f64 * 0.5, (min_y as f64 + terrain.height(40.0, 70.0, 0.0).0) * 0.5, (min_z + max_z) as f64 * 0.5];
            cam = Camera::look_at(sc.cam, c, sc.fov);
        }
        let sun = Sun { azimuth_deg: sc.sun.0, elevation_deg: sc.sun.1 };
        let settings = RenderSettings { width: w, height: h, aa: !no_aa, exposure: sc.exposure, mode: sc.mode, haze: sc.haze, ..Default::default() };
        let tr = Instant::now();
        let (img, st) = render_with_stats(&world, &cam, &sun, &settings);
        let render_s = tr.elapsed().as_secs_f64();
        let path = format!("target/agent-render/synth_{}_{}x{}.png", sc.name, w, h);
        save_png(&img, std::path::Path::new(&path)).unwrap();
        eprintln!(
            "{}: build {:.2}s render {:.2}s (setup {:.2} pass1 {:.2} aa {:.2} [{} px] post {:.2}) exposure {:.3} -> {}",
            sc.name, build_s, render_s, st.setup_s, st.pass1_s, st.aa_s, st.aa_pixels, st.post_s, st.exposure, path
        );
    }
}
