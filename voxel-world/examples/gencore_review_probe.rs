//! gen-core review probe: exposed side faces in badlands / hoodoos (terracotta vs stone).
use voxel_world::gen::{Biome, WorldGen};
use voxel_world::materials::Material;
fn main() {
    let g = WorldGen::new(2026);
    let mut n = 0usize;
    let mut stone_faces = 0usize;
    let mut faces = 0usize;
    let mut maxdrop = 0.0f32;
    for j in 0..1500 {
        for i in 0..1500 {
            let x = -20000.0 + i as f64 * 26.7 + 0.5;
            let z = -20000.0 + j as f64 * 26.7 + 0.5;
            let c = g.column(x, z);
            if !matches!(c.biome, Biome::Badlands | Biome::Hoodoos) { continue; }
            n += 1;
            for (ox, oz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                let c1 = g.column(x + ox, z + oz);
                let top = c.height.floor() as i32;
                let nb = c1.height.floor() as i32;
                maxdrop = maxdrop.max((top - nb) as f32);
                for y in (nb + 1)..=top {
                    let m = g.surface_material(x.floor() as i32, y, z.floor() as i32, top - y, &c);
                    faces += 1;
                    if !matches!(m, Material::Terracotta | Material::RedSand | Material::Sand | Material::RedSandstone) { stone_faces += 1; }
                }
            }
        }
    }
    println!("badlands/hoodoo cols {n}, exposed side blocks {faces}, non-terracotta {stone_faces} ({:.1}%), max drop {maxdrop}", 100.0 * stone_faces as f64 / faces.max(1) as f64);
}
