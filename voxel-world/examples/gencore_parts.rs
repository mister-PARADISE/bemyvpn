//! gen-core: hillshade of individual height components (debugging creases).
use voxel_world::gen::climate::ClimateSampler;
use voxel_world::gen::TerrainShaper;
use voxel_world::imageio::write_png_rgb8;
use voxel_world::math::Vec3;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cx: f64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1900.0);
    let cz: f64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(-4188.0);
    let cs = ClimateSampler::new(1);
    let ts = TerrainShaper::new(1);
    let n = 1024usize;
    let scale = 2.0;
    let parts = ts.debug_parts_names();
    let mut fields = vec![vec![0.0f32; n * n]; parts.len()];
    for j in 0..n {
        for i in 0..n {
            let x = cx + (i as f64 - n as f64 / 2.0) * scale;
            let z = cz + (j as f64 - n as f64 / 2.0) * scale;
            let cd = cs.sample_d(x, z, 0.0);
            let v = ts.debug_parts(&cd, x, z);
            for k in 0..parts.len() {
                fields[k][j * n + i] = v[k];
            }
        }
    }
    let light = Vec3::new(-1.0, 1.1, -1.0).normalize();
    for (k, name) in parts.iter().enumerate() {
        let h = &fields[k];
        let mut img = vec![0u8; n * n * 3];
        for j in 1..n - 1 {
            for i in 1..n - 1 {
                let hx = (h[j * n + i + 1] - h[j * n + i - 1]) / (2.0 * scale as f32);
                let hz = (h[(j + 1) * n + i] - h[(j - 1) * n + i]) / (2.0 * scale as f32);
                let nn = Vec3::new(-hx, 1.0, -hz).normalize();
                let s = (nn.dot(light).max(0.0) * 230.0) as u8;
                img[(j * n + i) * 3..(j * n + i) * 3 + 3].copy_from_slice(&[s, s, s]);
            }
        }
        write_png_rgb8(std::path::Path::new(&format!("target/agent-gencore/part_{name}.png")), n as u32, n as u32, &img).unwrap();
    }
}
