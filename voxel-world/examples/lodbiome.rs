use voxel_world::gen::WorldGen;
fn main() {
    let g = WorldGen::new(42);
    for &fp in &[4.0f64, 32.0, 128.0, 256.0] {
        let mut counts = std::collections::BTreeMap::new();
        let mut mats = std::collections::BTreeMap::new();
        let n = 200;
        for j in 0..n { for i in 0..n {
            let (x, z) = (-2000.0 + i as f64 * 40.0 + 7.0, -9000.0 + j as f64 * 40.0 + 3.0);
            let c = g.column_lod(x, z, fp);
            *counts.entry(format!("{:?}", c.biome)).or_insert(0) += 1;
            if c.water_level == i32::MIN || (c.water_level as f32) <= c.height {
                let y = ((c.height - 0.5).ceil() as i32).max(1);
                voxel_world::gen::biome::set_material_footprint(fp as f32);
                let m = g.surface_material(x as i32, y - 1, z as i32, 0, &c);
                voxel_world::gen::biome::set_material_footprint(0.0);
                *mats.entry(format!("{:?}", m)).or_insert(0) += 1;
            }
        }}
        let mut v: Vec<_> = counts.into_iter().collect(); v.sort_by(|a, b| b.1.cmp(&a.1));
        let mut w: Vec<_> = mats.into_iter().collect(); w.sort_by(|a, b| b.1.cmp(&a.1));
        println!("fp {fp}: biomes {:?}\n   top mats {:?}", &v[..8.min(v.len())], &w[..10.min(w.len())]);
    }
}
