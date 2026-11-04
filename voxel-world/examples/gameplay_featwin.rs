//! Gameplay pass debug: features rooted in/near a window.
use voxel_world::gen::WorldGen;
fn main() {
    let a: Vec<i32> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let g = WorldGen::new(a[0] as u64);
    let (x, z) = (a[1], a[2]);
    let fs = g.features_in(x - 24, z - 24, x + 120, z + 120);
    let mut big: Vec<_> = fs.iter().filter(|f| f.radius() >= 6).map(|f| (f.kind, f.root_x, f.root_z, f.radius(), f.columns.len())).collect();
    big.sort_by_key(|b| -(b.4 as i64));
    println!("{} features, big ones: {:?}", fs.len(), &big[..big.len().min(12)]);
    let c = g.column(x as f64 + 48.0, z as f64 + 48.0);
    println!("centre col: {:?} river {:.2} h {:.2}", c.biome, c.climate.river, c.height);
}
