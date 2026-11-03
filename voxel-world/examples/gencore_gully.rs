//! gen-core: isolated test of the gully (erosion) filter on a synthetic cone.
use voxel_world::gen::WorldGen;
use voxel_world::imageio::write_png_rgb8;
use voxel_world::math::Vec3;

fn main() {
    let g = WorldGen::new(1);
    let n = 1024usize;
    let scale = 2.0f64;
    let amp: f32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0.65);
    let mut h = vec![0.0f32; n * n];
    for j in 0..n {
        for i in 0..n {
            let x = (i as f64 - n as f64 / 2.0) * scale;
            let z = (j as f64 - n as f64 / 2.0) * scale;
            let r = (x * x + z * z).sqrt().max(1e-3);
            let base = 400.0 - 0.45 * r as f32;
            let grad = (-0.45 * (x / r) as f32, -0.45 * (z / r) as f32);
            let gul = g.terrain.gullies(x, z, grad, amp, 0.0);
            h[j * n + i] = base + gul.v;
        }
    }
    let light = Vec3::new(-1.0, 1.1, -1.0).normalize();
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
    write_png_rgb8(std::path::Path::new("target/agent-gencore/gully_test.png"), n as u32, n as u32, &img).unwrap();
}
