//! render-review: shading uniformity of a flat open plane (ambient noise metric) + stale-cache check.
use voxel_world::gen::Biome;
use voxel_world::materials::Material;
use voxel_world::math::Vec3;
use voxel_world::render::{render, save_png, Camera, RenderSettings, Sun};
use voxel_world::world::{BuildParams, HfCell, Span, World};

fn flat(mat: Material, top: i32) -> World {
    let p = BuildParams { center_x: 0, center_z: 0, l0_size: 256, hf_size: 256, hf_levels: 2, cave_radius: 0, feature_radius: 0 };
    World::synthetic(
        p,
        move |_, _| vec![Span::new(0, top, mat)],
        move |_, _, _| HfCell { h: top as f32, water: f32::NEG_INFINITY, albedo: mat.albedo(), mat, biome: Biome::Beach },
    )
}

fn stats(img: &voxel_world::render::Image, y0: u32, y1: u32) -> (f64, f64) {
    // Relative std of luminance over a central band of rows, after removing each row's mean
    // (perspective/aerial gradients) — what remains is texture + shading noise.
    let mut acc = 0.0;
    let mut n = 0.0;
    let mut mean_all = 0.0;
    for y in y0..y1 {
        let row: Vec<f64> = (img.width / 4..img.width * 3 / 4).map(|x| img.get(x, y).luminance() as f64).collect();
        let m = row.iter().sum::<f64>() / row.len() as f64;
        mean_all += m;
        for v in &row {
            acc += (v - m) * (v - m);
            n += 1.0;
        }
    }
    let mean = mean_all / (y1 - y0) as f64;
    ((acc / n).sqrt() / mean, mean)
}

fn main() {
    let cam = Camera { pos: [0.5, 150.0, 0.5], yaw: 0.0, pitch: -0.5, fov_y_deg: 50.0 };
    let s = RenderSettings { width: 480, height: 270, aa: false, ..Default::default() };
    for (mat, name) in [(Material::Sand, "sand"), (Material::Snow, "snow")] {
        let w = flat(mat, 140);
        for el in [4.5, 35.0] {
            let sun = Sun { azimuth_deg: 160.0, elevation_deg: el };
            let img = render(&w, &cam, &sun, &s);
            let (rs, m) = stats(&img, 150, 260);
            eprintln!("{name} sun {el}: rel std {:.4} mean {:.4}", rs, m);
            save_png(&img, std::path::Path::new(&format!("target/agent-render-review/flat_{name}_{el}.png"))).unwrap();
        }
    }
    // Stale per-thread caches: the same world under two suns, rendered on one thread in sequence,
    // must equal the render made on a fresh thread.
    let wa = flat(Material::Sand, 140);
    let sun_a = Sun { azimuth_deg: 160.0, elevation_deg: 35.0 };
    let sun_b = Sun { azimuth_deg: 20.0, elevation_deg: 8.0 };
    let fresh = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(|| render(&wa, &cam, &sun_b, &s));
    let seq = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(|| {
        let _ = render(&wa, &cam, &sun_a, &s);
        render(&wa, &cam, &sun_b, &s)
    });
    let diff = fresh.pixels.iter().zip(&seq.pixels).filter(|(p, q)| p.to_array() != q.to_array()).count();
    let maxrel = fresh.pixels.iter().zip(&seq.pixels).map(|(p, q)| ((p.luminance() - q.luminance()).abs() / p.luminance().max(1e-4)) as f64).fold(0.0, f64::max);
    eprintln!("stale-cache: {} of {} pixels differ (max rel lum diff {:.3})", diff, fresh.pixels.len(), maxrel);
    let _ = Vec3::ZERO;
}
