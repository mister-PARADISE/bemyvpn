use voxel_world::render::synth::SynthTerrain;
fn main() {
    let t = SynthTerrain::new(7);
    for k in -10..=10 {
        let x = k as f64 * 100.0;
        eprint!("{:.0} ", t.height(x, 0.0, 16.0).0);
    }
    eprintln!();
    for k in -10..=10 {
        let z = k as f64 * 100.0;
        eprint!("{:.0} ", t.height(0.0, z, 16.0).0);
    }
    eprintln!();
}
