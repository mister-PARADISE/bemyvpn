//! Art-pass hillshade: `artpass_relief SEED CX CZ METRES_PER_PX OUT.png [SIZE]`.
//! Grey hillshade (sun from the NW), water blue, the surface material colour of the top block
//! blended in (snow white, rock grey, sand, grass…), so landforms and snow patterns are visible.
use rayon::prelude::*;
use voxel_world::gen::WorldGen;
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{far_albedo, linear_to_srgb};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let (cx, cz, mpp): (f64, f64, f64) = (a[1].parse().unwrap(), a[2].parse().unwrap(), a[3].parse().unwrap());
    let out = std::path::PathBuf::from(&a[4]);
    let size: usize = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(768);
    let g = WorldGen::new(seed);
    let fp = if mpp > 1.5 { mpp } else { 0.0 };
    let rows: Vec<Vec<u8>> = (0..size)
        .into_par_iter()
        .map(|j| {
            let mut row = Vec::with_capacity(size * 3);
            for i in 0..size {
                let x = cx + (i as f64 - size as f64 / 2.0) * mpp;
                let z = cz + (j as f64 - size as f64 / 2.0) * mpp;
                let c = g.column_lod(x, z, fp);
                let hx = g.column_lod(x + mpp, z, fp).height - c.height;
                let hz = g.column_lod(x, z + mpp, fp).height - c.height;
                let (nx, nz) = (-hx / mpp as f32, -hz / mpp as f32);
                let inv = 1.0 / (nx * nx + 1.0 + nz * nz).sqrt();
                let shade = ((-0.55 * nx - 0.55 * nz + 0.63) * inv).max(0.0) * 1.15 + 0.12;
                let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                let col = if wet {
                    [0.05, 0.15, 0.35]
                } else {
                    let ty = c.height.floor() as i32;
                    let m = g.surface_material(x.floor() as i32, ty, z.floor() as i32, 0, &c);
                    let al = far_albedo(m, c.surface_temperature, c.climate.humidity, c.height);
                    [al.x * shade, al.y * shade, al.z * shade]
                };
                for k in 0..3 {
                    row.push((linear_to_srgb(col[k].min(1.0)) * 255.0) as u8);
                }
            }
            row
        })
        .collect();
    let rgb: Vec<u8> = rows.concat();
    write_png_rgb8(&out, size as u32, size as u32, &rgb).unwrap();
}
