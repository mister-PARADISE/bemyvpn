//! Tree catalogue for the caves-features review: ≥ 8 genome instances per species + giants.
//! `cargo run --release --example cavesfeatures_review_gallery -- [group|all]`
//! Writes PNGs into target/agent-cavesfeatures-review/.
use std::path::PathBuf;
use voxel_world::gen::features::{build_feature_with, Growth, Species};
use voxel_world::gen::{Biome, Feature};
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{linear_to_srgb, Material};
use voxel_world::math::Vec3;

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/agent-cavesfeatures-review");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn srgb(c: voxel_world::math::Vec3) -> [u8; 3] {
    [
        (linear_to_srgb(c.x) * 255.0).clamp(0.0, 255.0) as u8,
        (linear_to_srgb(c.y) * 255.0).clamp(0.0, 255.0) as u8,
        (linear_to_srgb(c.z) * 255.0).clamp(0.0, 255.0) as u8,
    ]
}


#[allow(dead_code)]
struct Vox {
    x0: i32,
    y0: i32,
    z0: i32,
    nx: i32,
    ny: i32,
    nz: i32,
    v: Vec<Material>,
}
impl Vox {
    fn new(x0: i32, y0: i32, z0: i32, nx: i32, ny: i32, nz: i32) -> Self {
        Vox { x0, y0, z0, nx, ny, nz, v: vec![Material::Air; (nx * ny * nz) as usize] }
    }
    fn idx(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (i, j, k) = (x - self.x0, y - self.y0, z - self.z0);
        if i < 0 || j < 0 || k < 0 || i >= self.nx || j >= self.ny || k >= self.nz {
            return None;
        }
        Some(((j * self.nz + k) * self.nx + i) as usize)
    }
    fn get(&self, x: i32, y: i32, z: i32) -> Material {
        self.idx(x, y, z).map(|i| self.v[i]).unwrap_or(Material::Air)
    }
    fn set(&mut self, x: i32, y: i32, z: i32, m: Material) {
        if let Some(i) = self.idx(x, y, z) {
            self.v[i] = m;
        }
    }
    /// Place a feature with the world builder's rules, ground = first air y at the root.
    fn place(&mut self, f: &Feature, ground: i32) {
        for c in &f.columns {
            let (x, z) = (f.root_x + c.dx as i32, f.root_z + c.dz as i32);
            for y in c.y0..c.y1 {
                let wy = ground + y as i32;
                let cur = self.get(x, wy, z);
                let ok = if c.replace_solid {
                    true
                } else if cur == Material::Air || cur == Material::Water {
                    true
                } else {
                    c.mat.is_log() && cur.is_leaves()
                };
                if ok {
                    self.set(x, wy, z, c.mat);
                }
            }
        }
    }
}

fn mat_color(m: Material, temp: f32, hum: f32) -> Vec3 {
    use voxel_world::materials::{foliage_tint, grass_tint};
    let a = m.albedo();
    if m.grass_tinted() {
        a.mul_elem(grass_tint(temp, hum))
    } else if m.foliage_tinted() {
        a.mul_elem(foliage_tint(temp, hum))
    } else {
        a
    }
}

/// Render voxels isometrically (viewer at +x, +y, +z). Returns (w, h, rgb).
fn render_iso(vx: &Vox, s: f32, temp: f32, hum: f32) -> (usize, usize, Vec<u8>) {
    let c30 = 0.866f32;
    // screen bounds
    let corners = [
        (vx.x0, vx.y0, vx.z0),
        (vx.x0 + vx.nx, vx.y0, vx.z0),
        (vx.x0, vx.y0, vx.z0 + vx.nz),
        (vx.x0 + vx.nx, vx.y0, vx.z0 + vx.nz),
        (vx.x0, vx.y0 + vx.ny, vx.z0),
        (vx.x0 + vx.nx, vx.y0 + vx.ny, vx.z0 + vx.nz),
        (vx.x0, vx.y0 + vx.ny, vx.z0 + vx.nz),
        (vx.x0 + vx.nx, vx.y0 + vx.ny, vx.z0),
    ];
    let proj = |x: f32, y: f32, z: f32| ((x - z) * c30 * s, (x + z) * 0.5 * s - y * s);
    let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for &(x, y, z) in &corners {
        let (u, v) = proj(x as f32, y as f32, z as f32);
        umin = umin.min(u);
        umax = umax.max(u);
        vmin = vmin.min(v);
        vmax = vmax.max(v);
    }
    let w = (umax - umin) as usize + 4;
    let h = (vmax - vmin) as usize + 4;
    let mut zb = vec![f32::MIN; w * h];
    let mut img = vec![0u8; w * h * 3];
    for i in 0..w * h {
        let t = (i / w) as f32 / h as f32;
        img[i * 3..i * 3 + 3].copy_from_slice(&[(200.0 - 40.0 * t) as u8, (215.0 - 30.0 * t) as u8, (232.0 - 20.0 * t) as u8]);
    }
    let steps = (s * 1.6).ceil() as i32 + 1;
    for j in 0..vx.ny {
        for k in 0..vx.nz {
            for i in 0..vx.nx {
                let (x, y, z) = (vx.x0 + i, vx.y0 + j, vx.z0 + k);
                let m = vx.get(x, y, z);
                if m == Material::Air {
                    continue;
                }
                let base = mat_color(m, temp, hum);
                let hv = voxel_world::noise::hash3(77, x, y, z);
                for face in 0..3 {
                    let (nx, ny, nz) = match face {
                        0 => (x, y + 1, z),
                        1 => (x + 1, y, z),
                        _ => (x, y, z + 1),
                    };
                    let nb = vx.get(nx, ny, nz);
                    if nb != Material::Air && !(nb == Material::Water && m != Material::Water) {
                        continue;
                    }
                    if m == Material::Water && face != 0 {
                        continue;
                    }
                    let shade = [1.0, 0.72, 0.56][face];
                    // ambient occlusion-lite: darker if the block above is solid
                    let ao = if face != 0 && vx.get(x, y + 1, z) != Material::Air { 0.85 } else { 1.0 };
                    for a in 0..steps {
                        for b in 0..steps {
                            let fa = (a as f32 + 0.5) / steps as f32;
                            let fb = (b as f32 + 0.5) / steps as f32;
                            let p = match face {
                                0 => (x as f32 + fa, y as f32 + 1.0, z as f32 + fb),
                                1 => (x as f32 + 1.0, y as f32 + fa, z as f32 + fb),
                                _ => (x as f32 + fa, y as f32 + fb, z as f32 + 1.0),
                            };
                            let (u, v) = proj(p.0, p.1, p.2);
                            let px = (u - umin) as usize + 2;
                            let py = (v - vmin) as usize + 2;
                            if px >= w || py >= h {
                                continue;
                            }
                            let d = p.0 + p.1 + p.2;
                            if d <= zb[py * w + px] {
                                continue;
                            }
                            zb[py * w + px] = d;
                            // 4×4 texels per face
                            let tx = (fa * 4.0) as i32;
                            let ty = (fb * 4.0) as i32;
                            let th = voxel_world::noise::hash3(hv, tx, ty, face as i32);
                            let mut t = 0.88 + 0.24 * voxel_world::noise::hash_f01(th);
                            if m.is_leaves() && (th >> 20) & 7 == 0 {
                                t *= 0.55;
                            }
                            let c = base * (shade * ao * t);
                            img[(py * w + px) * 3..(py * w + px) * 3 + 3].copy_from_slice(&srgb(c));
                        }
                    }
                }
            }
        }
    }
    (w, h, img)
}

/// Blit rows of images into one canvas.
fn compose(rows: &[Vec<(usize, usize, Vec<u8>)>], pad: usize) -> (usize, usize, Vec<u8>) {
    let width = rows.iter().map(|r| r.iter().map(|i| i.0 + pad).sum::<usize>()).max().unwrap_or(1) + pad;
    let height = rows.iter().map(|r| r.iter().map(|i| i.1).max().unwrap_or(0) + pad).sum::<usize>() + pad;
    let mut out = vec![245u8; width * height * 3];
    let mut oy = pad;
    for r in rows {
        let mut ox = pad;
        let rh = r.iter().map(|i| i.1).max().unwrap_or(0);
        for (w, h, img) in r {
            let dy = rh - h; // bottom-align
            for y in 0..*h {
                let dst = ((oy + dy + y) * width + ox) * 3;
                out[dst..dst + w * 3].copy_from_slice(&img[y * w * 3..(y + 1) * w * 3]);
            }
            ox += w + pad;
        }
        oy += rh + pad;
    }
    (width, height, out)
}


fn biome_for(sp: Species, v: u64) -> Biome {
    use Species::*;
    match sp {
        Jungle | MegaJungle | Palm | JungleGiant => Biome::Jungle,
        Acacia | Baobab => Biome::Savanna,
        Cactus => Biome::Desert,
        IceSpire => Biome::SnowyPlains,
        Boulder if v % 2 == 1 => Biome::SnowySlopes,
        DarkOak => Biome::DarkForest,
        SwampOak | Willow => Biome::Swamp,
        Spruce | Pine | MegaSpruce | ColossalSpruce | Sequoia | Snag | Stump | FallenLog => Biome::OldGrowthTaiga,
        _ => Biome::Forest,
    }
}

fn tile(f: &Feature, b: Biome, sc: f32) -> (usize, usize, Vec<u8>) {
    let r = f.radius() + 1;
    let top = f.columns.iter().map(|c| c.y1 as i32).max().unwrap_or(1);
    let mut vx = Vox::new(-r, -2, -r, 2 * r + 2, top + 3, 2 * r + 2);
    for z in -r..=r + 1 {
        for x in -r..=r + 1 {
            vx.set(x, -2, z, Material::Dirt);
            vx.set(x, -1, z, if b == Biome::Desert { Material::Sand } else { Material::Grass });
        }
    }
    vx.place(f, 0);
    let (t, h) = match b {
        Biome::Jungle => (0.9, 0.9),
        Biome::Savanna => (0.8, -0.4),
        Biome::DarkForest => (0.3, 0.8),
        Biome::Swamp => (0.6, 0.9),
        _ => (0.3, 0.3),
    };
    render_iso(&vx, sc, t, h)
}

fn gallery(name: &str, sps: &[Species], count: u64, per_row: usize, sc: f32, windy: bool) {
    let mut rows = Vec::new();
    for &sp in sps {
        let mut row = Vec::new();
        for v in 0..count {
            let b = biome_for(sp, v);
            let stand = (v as f32 + 0.5) / count as f32 * 2.0 - 1.0;
            let g = Growth { biome: b, scale: 1.0, stand, wind: (0.8, 0.6), windy: if windy { 1.0 } else { 0.0 } };
            let f = build_feature_with(sp, 0, 0, voxel_world::noise::mix64(v * 7919 + sp as u64 * 31 + 5), &g);
            row.push(tile(&f, b, sc));
            if row.len() == per_row {
                rows.push(std::mem::take(&mut row));
            }
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }
    let (w, h, img) = compose(&rows, 4);
    let p = out_dir().join(format!("gallery_{name}.png"));
    write_png_rgb8(&p, w as u32, h as u32, &img).unwrap();
    eprintln!("wrote {} ({w}×{h})", p.display());
}

fn main() {
    use Species::*;
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let list: [(Species, f32, bool); 28] = [
        (Oak, 7.0, false),
        (SwampOak, 6.0, false),
        (BigOak, 5.0, false),
        (Birch, 7.0, false),
        (TallBirch, 6.0, false),
        (DarkOak, 5.5, false),
        (Spruce, 5.0, false),
        (Spruce, 5.0, true),
        (Pine, 5.0, false),
        (Pine, 5.0, true),
        (MegaSpruce, 3.2, false),
        (ColossalSpruce, 2.4, false),
        (Jungle, 5.0, false),
        (MegaJungle, 3.2, false),
        (Acacia, 5.5, false),
        (Palm, 6.0, false),
        (Bush, 9.0, false),
        (Cactus, 9.0, false),
        (Boulder, 8.0, false),
        (IceSpire, 5.0, false),
        (FallenLog, 7.0, false),
        (Snag, 6.0, false),
        (Stump, 10.0, false),
        (AncientOak, 2.4, false),
        (Sequoia, 1.7, false),
        (JungleGiant, 2.0, false),
        (Baobab, 3.0, false),
        (Willow, 3.0, false),
    ];
    for (sp, sc, windy) in list {
        let name = if windy { format!("{}_windy", sp.name()) } else { sp.name().to_string() };
        if which == "all" || which == sp.name() || which == name {
            gallery(&name, &[sp], 8, 4, sc, windy);
        }
    }
}
