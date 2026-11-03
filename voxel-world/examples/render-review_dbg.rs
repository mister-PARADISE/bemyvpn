//! render-review: debug the shading of one pixel of a planned shot.
use voxel_world::gen::WorldGen;
use voxel_world::render::shade::{Ctx, Frame};
use voxel_world::render::trace::Ray;
use voxel_world::render::RenderSettings;
use voxel_world::world::build_world;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = args[0].parse().unwrap();
    let idx: usize = args[1].parse().unwrap();
    let (px, py): (f32, f32) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let gen = WorldGen::new(seed);
    let shots = voxel_world::scout::plan_shots(&gen);
    let shot = shots.iter().find(|s| s.index == idx).unwrap();
    let world = build_world(&gen, &shot.build);
    let s = RenderSettings { width: 1920, height: 1080, aa: false, exposure: shot.exposure, mode: shot.mode, haze: shot.haze, max_distance: 100_000.0 };
    let f = Frame::new(&world, &shot.camera, &shot.sun, &s);
    let mut ctx = Ctx::new();
    let d = f.primary_dir(px, py);
    let r = f.radiance(shot.camera.pos, d, 0.0, 0, 0, &mut ctx);
    eprintln!("radiance {:?} depth {}", r.color, r.depth);
    let ray = Ray::new(shot.camera.pos, [d.x as f64, d.y as f64, d.z as f64]);
    let h = f.scene.trace(&ray, 0.0, 1e5, true).unwrap();
    eprintln!("hit {:?}", h);
    let o = [h.p[0], h.p[1] + 1e-3, h.p[2]];
    let sd = f.sun;
    let sr = Ray::new(o, [sd.x as f64, sd.y as f64, sd.z as f64]);
    eprintln!("sun {:?} occluded {:?}", sd, f.scene.trace(&sr, 0.0, 1e9, false));
    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        let (x, z) = (h.p[0].floor() as i32 + dx, h.p[2].floor() as i32 + dz);
        eprintln!("nb {dx},{dz}: {:?}", &world.l0.spans(x, z)[world.l0.spans(x, z).len().saturating_sub(2)..]);
    }
}
