//! render-review: texture averages vs far albedo (L0 ↔ heightfield colour continuity).
use voxel_world::materials::{self, Material};
use voxel_world::render::texture::{Textures, FACE_SIDE, FACE_TOP};
fn main() {
    let t = Textures::new();
    for (te, hu) in [(0.9f32, 0.8f32), (0.3, 0.2), (-0.5, 0.0), (0.8, -0.6)] {
        for m in [Material::Grass, Material::OakLeaves, Material::JungleLeaves, Material::Sand, Material::Stone, Material::Snow] {
            let tint = if m.grass_tinted() { materials::grass_tint(te, hu) } else if m.foliage_tinted() { materials::foliage_tint(te, hu) } else { voxel_world::math::Vec3::ONE };
            let near_top = t.average(m, FACE_TOP).color(tint);
            let near_side = t.average(m, FACE_SIDE).color(tint);
            let far = materials::far_albedo(m, te, hu, 150.0);
            eprintln!("T{te} H{hu} {:?}: top {:.3?} side {:.3?} far {:.3?} lum ratio top/far {:.2}", m, near_top.to_array(), near_side.to_array(), far.to_array(), near_top.luminance() / far.luminance());
        }
    }
}
