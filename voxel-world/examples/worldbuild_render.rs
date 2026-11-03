//! world-build: render the real `build_world` output with the renderer (quick visual check of
//! level-0 / heightfield continuity, water, trees, caves in a cut-away).
//! usage: worldbuild_render seed cx cz [w h] [yaw] [hf_levels] [l0] [cutaway(0/1)]
use voxel_world::gen::WorldGen;
use voxel_world::render::{render_with_stats, save_png, Camera, RenderSettings, Sun, ViewMode};
use voxel_world::world::{build_world, BuildParams};

fn main() {
    let a: Vec<f64> = std::env::args().skip(1).filter_map(|s| s.parse().ok()).collect();
    let g = |i: usize, d: f64| *a.get(i).unwrap_or(&d);
    let seed = g(0, 42.0) as u64;
    let (cx, cz) = (g(1, 0.0) as i32, g(2, 0.0) as i32);
    let (w, h) = (g(3, 640.0) as u32, g(4, 360.0) as u32);
    let yaw = g(5, 45.0);
    let levels = g(6, 3.0) as u32;
    let l0 = g(7, 1024.0) as u32;
    let cut = g(8, 0.0) > 0.5;
    let gen = WorldGen::new(seed);
    let p = BuildParams {
        center_x: cx,
        center_z: cz,
        l0_size: l0,
        hf_size: 2048,
        hf_levels: if cut { 0 } else { levels },
        cave_radius: if cut { l0 } else { 384 },
        feature_radius: l0 / 2,
    };
    let world = build_world(&gen, &p);
    let top = world.l0.top(cx, cz) as f64;
    let (cam, mode) = if cut {
        let r = 70;
        let mode = ViewMode::Diorama { min_x: cx - r, max_x: cx + r, min_z: cz - r, max_z: cz + r, min_y: 20 };
        let d = 330.0;
        let (sy, cy) = yaw.to_radians().sin_cos();
        let pos = [cx as f64 - sy * d, top + 140.0, cz as f64 - cy * d];
        (Camera::look_at(pos, [cx as f64, top * 0.55, cz as f64], 40.0), mode)
    } else {
        (Camera { pos: [cx as f64 + 0.5, top + 40.0, cz as f64 + 0.5], yaw: yaw.to_radians(), pitch: (-6f64).to_radians(), fov_y_deg: 60.0 }, ViewMode::Normal)
    };
    let sun = Sun { azimuth_deg: yaw + 60.0, elevation_deg: 22.0 };
    let s = RenderSettings { width: w, height: h, mode, ..Default::default() };
    let (img, st) = render_with_stats(&world, &cam, &sun, &s);
    let path = format!("target/agent-worldbuild/render_s{seed}_{cx}_{cz}_y{}{}.png", yaw as i32, if cut { "_cut" } else { "" });
    save_png(&img, std::path::Path::new(&path)).unwrap();
    eprintln!("render {:.1}s+{:.1}s -> {path}", st.pass1_s, st.aa_s);
}
