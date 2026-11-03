//! render-review: dry block whose top is exactly at the water surface, next to shallow water.
use voxel_world::gen::Biome;
use voxel_world::materials::Material;
use voxel_world::math::Vec3;
use voxel_world::render::shade::{Ctx, Frame};
use voxel_world::render::{Camera, RenderSettings, Sun};
use voxel_world::world::{BuildParams, HfCell, Span, World};

fn main() {
    let p = BuildParams { center_x: 0, center_z: 0, l0_size: 64, hf_size: 64, hf_levels: 1, cave_radius: 0, feature_radius: 0 };
    let w = World::synthetic(
        p,
        |x, z| {
            if (x == 0 && z == 0) || (x == 3 && z == 0) {
                vec![Span::new(0, 140, Material::Gravel)]
            } else if x == 6 && z == 0 {
                vec![Span::new(0, 141, Material::Gravel)]
            } else if x == 9 && z == 0 {
                vec![Span::new(0, 140, Material::Sand)]
            } else {
                vec![Span::new(0, 139, Material::Sand), Span::new(139, 140, Material::Water)]
            }
        },
        |_, _, _| HfCell { h: 139.0, water: 140.0, albedo: Vec3::splat(0.4), mat: Material::Sand, biome: Biome::Beach },
    );
    let s = RenderSettings { width: 320, height: 180, aa: false, ..Default::default() };
    let cam = Camera::look_at([4.5, 150.0, -12.0], [4.5, 140.0, 0.5], 50.0);
    for el in [4.5, 30.0] {
        let sun = Sun { azimuth_deg: 0.0, elevation_deg: el };
        let f = Frame::new(&w, &cam, &sun, &s);
        let mut ctx = Ctx::new();
        for (x, y, z, name) in [(0.5, 140.0, 0.5, "gravel@140"), (3.5, 140.0, 0.5, "gravel@140b"), (6.5, 141.0, 0.5, "gravel@141"), (9.5, 140.0, 0.5, "sand@140"), (12.5, 140.0, 0.5, "water")] {
            let d = Vec3::new((x - cam.pos[0]) as f32, (y - cam.pos[1]) as f32, (z - cam.pos[2]) as f32).normalize();
            let r = f.radiance(cam.pos, d, 0.0, 0, 0, &mut ctx);
            eprintln!("sun {el}: {name}: {:?}", r.color);
        }
    }
}
