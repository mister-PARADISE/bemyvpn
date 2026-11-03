//! gen-core review: dry columns whose 3D band reaches below sea level.
use voxel_world::gen::{WorldGen, SEA_LEVEL};
fn main() {
    let g = WorldGen::new(1);
    let (mut n, mut bad, mut carved) = (0, 0, 0);
    for i in 0..400_000u64 {
        let x = ((voxel_world::noise::hash_f01(voxel_world::noise::mix64(i * 2)) as f64 - 0.5) * 40000.0).floor() + 0.5;
        let z = ((voxel_world::noise::hash_f01(voxel_world::noise::mix64(i * 2 + 1)) as f64 - 0.5) * 40000.0).floor() + 0.5;
        let c = g.column(x, z);
        if c.water_level != i32::MIN || c.jaggedness == 0.0 { continue; }
        n += 1;
        if c.height - c.jaggedness < SEA_LEVEL as f32 {
            bad += 1;
            for y in (c.height - c.jaggedness).floor() as i32..SEA_LEVEL {
                if g.density(x, y as f64 + 0.5, z, &c) <= 0.0 && (y as f32) < c.height { carved += 1; break; }
            }
        }
    }
    println!("dry jagged columns {n}: band below sea level {bad}, with air pocket below sea level {carved}");
}
