//! Debug: shaded top-down map of the synthetic terrain (helps choosing test cameras).
use voxel_world::imageio::write_png_rgb8;
use voxel_world::render::synth::SynthTerrain;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(7);
    let scale: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(16.0);
    let n = 1024usize;
    let t = SynthTerrain::new(seed);
    let mut rgb = vec![0u8; n * n * 3];
    for j in 0..n {
        for i in 0..n {
            let x = (i as f64 - n as f64 / 2.0) * scale;
            let z = (j as f64 - n as f64 / 2.0) * scale;
            let (h, mesa) = t.height(x, z, scale);
            let (h2, _) = t.height(x + scale, z + scale, scale);
            let shade = (1.0 + (h2 - h) / scale * 2.0).clamp(0.3, 1.6);
            let c = if h < 140.0 {
                [30.0, 60.0 + h * 0.5, 120.0 + h * 0.6]
            } else if mesa > 0.5 {
                [190.0, 100.0, 50.0]
            } else if h > 360.0 {
                [240.0, 240.0, 245.0]
            } else {
                let k = ((h - 140.0) / 220.0).min(1.0);
                [60.0 + 120.0 * k, 140.0 - 20.0 * k, 50.0 + 60.0 * k]
            };
            let o = (j * n + i) * 3;
            for q in 0..3 {
                rgb[o + q] = (c[q] * shade).clamp(0.0, 255.0) as u8;
            }
            if (i == n / 2 || j == n / 2) && (i + j) % 4 == 0 {
                rgb[o] = 255;
                rgb[o + 1] = 0;
                rgb[o + 2] = 0;
            }
        }
    }
    let p = std::path::Path::new("target/agent-render/synth_map.png");
    write_png_rgb8(p, n as u32, n as u32, &rgb).unwrap();
    // Print some heights near origin.
    for &(x, z) in &[(0.0, 0.0), (60.0, 40.0), (20.0, 95.0), (1000.0, 0.0), (0.0, 1000.0)] {
        eprintln!("H({x},{z}) = {:.1}", t.height(x, z, 0.0).0);
    }
}
