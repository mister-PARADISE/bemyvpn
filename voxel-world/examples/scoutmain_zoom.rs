//! scout-main debug tool: plan the shots of a seed and render a zoomed map around every shot
//! (camera + target) with its marker and view cone, plus an overview map.
//!
//! cargo run --release --example scoutmain_zoom -- SEED [SCALE] [SIZE]
use voxel_world::gen::WorldGen;
use voxel_world::imageio::write_png_rgb8;
use voxel_world::scout::{map, plan_shots_detailed};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a.first().and_then(|s| s.parse().ok()).unwrap_or(1);
    let scale: f64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let size: u32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(512);
    let gen = WorldGen::new(seed);
    let t = std::time::Instant::now();
    let plan = plan_shots_detailed(&gen);
    eprintln!("planned in {:.2}s", t.elapsed().as_secs_f64());
    let dir = std::path::PathBuf::from("target/agent-scoutmain");
    for p in &plan {
        let c = &p.shot.camera;
        eprintln!(
            "{} {:<13} cam ({:.0},{:.0},{:.0}) yaw {:.0} pitch {:.1} ground {:.0} {} | {} | {} | score {:.2}{}",
            p.shot.index,
            p.shot.name,
            c.pos[0],
            c.pos[1],
            c.pos[2],
            c.yaw.to_degrees(),
            c.pitch.to_degrees(),
            p.ground_y,
            p.biome.name(),
            p.shot.title,
            p.note,
            p.score,
            if p.fallback { " FALLBACK" } else { "" }
        );
        let (cx, cz) = if p.shot.name == "orbital" {
            (p.target[0], p.target[1])
        } else {
            (0.5 * (c.pos[0] + p.target[0]) + 400.0 * c.yaw.sin(), 0.5 * (c.pos[2] + p.target[1]) + 400.0 * c.yaw.cos())
        };
        let sc = if p.shot.name == "orbital" { scale * 6.0 } else if p.shot.name == "cutaway" { scale * 0.5 } else { scale };
        let mp = map::MapParams { cx, cz, size, scale: sc };
        let rgb = map::render_map(&gen, mp, std::slice::from_ref(p));
        let path = dir.join(format!("zoom_s{}_{}_{}.png", seed, p.shot.index, p.shot.name));
        write_png_rgb8(&path, size, size, &rgb).unwrap();
    }
}
