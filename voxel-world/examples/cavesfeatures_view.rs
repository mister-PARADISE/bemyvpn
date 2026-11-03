//! Debug views for caves + features (owner: caves-features).
//!
//! `cargo run --release --example cavesfeatures_view -- [caves|plan|features|trees|all] [seed]`
//! Writes PNGs into target/agent-cavesfeatures/.
use std::path::PathBuf;
use voxel_world::gen::features::{build_feature, Species, TreeCtx};
use voxel_world::gen::{Biome, CaveFill, ColumnInfo, Feature, FeatureKind, WorldGen, SEA_LEVEL};
use voxel_world::math::Vec3;
use voxel_world::imageio::write_png_rgb8;
use voxel_world::materials::{linear_to_srgb, Material};

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/agent-cavesfeatures");
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

/// Find a land spot whose height lies in [lo, hi].
fn find_spot(g: &WorldGen, lo: f32, hi: f32, start: (f64, f64)) -> (f64, f64) {
    for r in 0..400 {
        for k in 0..(8 * r.max(1)) {
            let a = k as f64 / (8 * r.max(1)) as f64 * std::f64::consts::TAU;
            let x = start.0 + (r as f64 * 200.0) * a.cos();
            let z = start.1 + (r as f64 * 200.0) * a.sin();
            let c = g.column(x, z);
            if c.height >= lo && c.height <= hi && c.water_level == i32::MIN {
                return (x.floor(), z.floor());
            }
        }
    }
    start
}

/// Lattice-interpolated cave field sampler (emulates the world builder: 4×4×4 lattice).
struct Lattice<'a> {
    g: &'a WorldGen,
}
impl<'a> Lattice<'a> {
    fn field(&self, cache: &mut std::collections::HashMap<(i32, i32), ColumnInfo>, x: f64, y: f64, z: f64) -> f32 {
        let lx = (x / 4.0).floor() as i32;
        let ly = (y / 4.0).floor() as i32;
        let lz = (z / 4.0).floor() as i32;
        let fx = (x / 4.0 - lx as f64) as f32;
        let fy = (y / 4.0 - ly as f64) as f32;
        let fz = (z / 4.0 - lz as f64) as f32;
        let mut v = [0.0f32; 8];
        for (i, vv) in v.iter_mut().enumerate() {
            let (dx, dy, dz) = ((i & 1) as i32, ((i >> 1) & 1) as i32, ((i >> 2) & 1) as i32);
            let key = (lx + dx, lz + dz);
            let col = *cache.entry(key).or_insert_with(|| self.g.column(key.0 as f64 * 4.0, key.1 as f64 * 4.0));
            *vv = self.g.cave_field(key.0 as f64 * 4.0, (ly + dy) as f64 * 4.0, key.1 as f64 * 4.0, &col);
        }
        let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = l(v[0], v[1], fx);
        let x10 = l(v[2], v[3], fx);
        let x01 = l(v[4], v[5], fx);
        let x11 = l(v[6], v[7], fx);
        l(l(x00, x10, fy), l(x01, x11, fy), fz)
    }
}

fn caves_slices(g: &WorldGen, seed: u64) {
    let (cx, cz) = find_spot(g, 230.0, 300.0, (3000.0, -2000.0));
    eprintln!("cave slices around ({cx}, {cz})");
    let w = 512usize;
    let hgt = 300usize;
    let zs = [0.0, 37.0, 91.0, 160.0];
    let gap = 6;
    let total_h = zs.len() * (hgt + gap);
    let mut img = vec![0u8; w * total_h * 3];
    let lat = Lattice { g };
    let mut carved = 0usize;
    let mut under = 0usize;
    let mut leaks = 0usize;
    for (si, dzs) in zs.iter().enumerate() {
        let mut state = vec![0u8; w * hgt];
        let z = cz + dzs + 0.5;
        let mut cache = std::collections::HashMap::new();
        for px in 0..w {
            let x = cx - 256.0 + px as f64 + 0.5;
            let col = g.column(x, z);
            for py in 0..hgt {
                let y = (hgt - 1 - py) as f64 + 0.5;
                let solid = g.density(x, y, z, &col) > 0.0;
                let mut c: [u8; 3];
                if !solid {
                    c = if (y as i32) < col.water_level { [40, 80, 200] } else { [170, 205, 240] };
                } else {
                    under += 1;
                    let f = lat.field(&mut cache, x, y, z);
                    if f > 0.0 {
                        carved += 1;
                        let fill = g.cave_fill(x.floor() as i32, y.floor() as i32, z.floor() as i32, &col);
                        state[py * w + px] = 1 + fill as u8;
                        c = match fill {
                            CaveFill::Air => [18, 16, 20],
                            CaveFill::Water => [30, 70, 190],
                            CaveFill::Lava => [250, 110, 20],
                        };
                    } else {
                        let d = (col.height - y as f32).max(0.0);
                        let m = g.surface_material(x as i32, y as i32, z as i32, d as i32, &col);
                        c = srgb(m.albedo());
                        if m == Material::Grass {
                            c = [90, 150, 60];
                        }
                        if f > -1.5 {
                            // wall halo to see the field's smoothness
                            c = [c[0] / 2 + 40, c[1] / 2 + 30, c[2] / 2 + 20];
                        }
                    }
                }
                if !solid {
                    state[py * w + px] = 9;
                }
                let row = si * (hgt + gap) + py;
                let o = (row * w + px) * 3;
                img[o..o + 3].copy_from_slice(&c);
            }
        }
        for py in 0..hgt {
            for px in 1..w {
                let (a, b) = (state[py * w + px - 1], state[py * w + px]);
                if (a == 2 && b == 1) || (a == 1 && b == 2) {
                    leaks += 1;
                    if leaks < 6 {
                        let x = cx - 256.0 + px as f64;
                        let y = (hgt - 1 - py) as f64 + 0.5;
                        let z = cz + dzs + 0.5;
                        eprintln!("leak at x={x} y={y} z={z}: wt(x-1)={:?} wt(x)={:?} wt(x+1)={:?}", g.caves.water_table(x - 0.5, z), g.caves.water_table(x + 0.5, z), g.caves.water_table(x + 1.5, z));
                    }
                }
            }
        }
    }
    eprintln!("water/air side contacts inside caves: {leaks}");
    eprintln!("slice carve fraction {:.2}%", 100.0 * carved as f64 / under.max(1) as f64);
    let p = out_dir().join(format!("seed{seed}_cave_slices.png"));
    write_png_rgb8(&p, w as u32, total_h as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

/// X-ray plan views: for y-ranges, the highest carved point of each (x, z), coloured by the
/// dominant component and shaded by height — shows the network topology.
fn caves_plan(g: &WorldGen, seed: u64) {
    let (cx, cz) = find_spot(g, 230.0, 300.0, (3000.0, -2000.0));
    let w = 512usize;
    let ranges = [(20.0, 70.0), (70.0, 120.0), (120.0, 170.0), (170.0, 230.0)];
    let gap = 6;
    let total_w = ranges.len() * (w + gap);
    let mut img = vec![255u8; total_w * w * 3];
    let cols: Vec<ColumnInfo> = (0..(w / 2) * (w / 2))
        .map(|i| {
            let (ix, iz) = (i % (w / 2), i / (w / 2));
            g.column(cx - 256.0 + ix as f64 * 2.0 + 1.0, cz - 256.0 + iz as f64 * 2.0 + 1.0)
        })
        .collect();
    for (si, &(y0, y1)) in ranges.iter().enumerate() {
        for pz in 0..w {
            for px in 0..w {
                let col = &cols[(pz / 2) * (w / 2) + px / 2];
                let x = cx - 256.0 + px as f64 + 0.5;
                let z = cz - 256.0 + pz as f64 + 0.5;
                let mut c = [110u8, 110, 110];
                if y0 as f32 > col.height {
                    c = [200, 225, 245];
                } else {
                    let mut y = (y1 as f32).min(col.height) as f64;
                    while y >= y0 {
                        let p = g.caves.parts(x, y, z, col);
                        if p.total > 0.0 {
                            let v = [p.cheese, p.tunnels, p.shafts, p.ravine];
                            let k = (0..4).fold(0, |b, i| if v[i] > v[b] { i } else { b });
                            let base: [f32; 3] = match k {
                                0 => [190.0, 60.0, 40.0],
                                1 => [40.0, 60.0, 200.0],
                                2 => [40.0, 190.0, 80.0],
                                _ => [220.0, 170.0, 30.0],
                            };
                            let t = 0.35 + 0.65 * ((y - y0) / (y1 - y0)) as f32;
                            let fill = g.cave_fill(x as i32, y as i32, z as i32, col);
                            c = [(base[0] * t) as u8, (base[1] * t) as u8, (base[2] * t) as u8];
                            if fill == CaveFill::Water {
                                c = [c[0] / 3, c[1] / 2 + 60, 255];
                            }
                            break;
                        }
                        y -= 2.0;
                    }
                }
                let o = (pz * total_w + si * (w + gap) + px) * 3;
                img[o..o + 3].copy_from_slice(&c);
            }
        }
    }
    let p = out_dir().join(format!("seed{seed}_cave_plan.png"));
    write_png_rgb8(&p, total_w as u32, w as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}


// ------------------------------------------------------------------ isometric voxel renderer

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

fn species_gallery() {
    let groups: [(&str, &[Species], Biome, f32); 3] = [
        ("broadleaf", &[Species::Oak, Species::SwampOak, Species::BigOak, Species::Birch, Species::TallBirch, Species::DarkOak], Biome::Forest, 6.0),
        ("conifer_tropical", &[Species::Spruce, Species::Pine, Species::MegaSpruce, Species::Jungle, Species::MegaJungle, Species::Acacia], Biome::Taiga, 4.0),
        ("small", &[Species::Bush, Species::Cactus, Species::Boulder, Species::IceSpire, Species::Palm, Species::FallenLog], Biome::Taiga, 7.0),
    ];
    for (name, sps, biome, sc) in groups {
        let mut rows = Vec::new();
        for &sp in sps {
            let mut row = Vec::new();
            let count = if sc > 6.5 { 7 } else { 4 };
            for v in 0..count as u64 {
                let b = match sp {
                    Species::Jungle | Species::MegaJungle | Species::Palm => Biome::Jungle,
                    Species::Acacia => Biome::Savanna,
                    Species::Cactus => Biome::Desert,
                    Species::IceSpire => Biome::SnowyPlains,
                    Species::Boulder if v % 2 == 1 => Biome::SnowySlopes,
                    Species::DarkOak | Species::SwampOak => Biome::DarkForest,
                    _ => biome,
                };
                let f = build_feature(sp, 0, 0, voxel_world::noise::mix64(v * 7919 + sp as u64), &TreeCtx { biome: b, scale: 1.0 });
                let r = f.radius() + 1;
                let top = f.columns.iter().map(|c| c.y1 as i32).max().unwrap_or(1);
                let mut vx = Vox::new(-r, -2, -r, 2 * r + 2, top + 3, 2 * r + 2);
                for z in -r..=r + 1 {
                    for x in -r..=r + 1 {
                        vx.set(x, -2, z, Material::Dirt);
                        vx.set(x, -1, z, if b == Biome::Desert { Material::Sand } else { Material::Grass });
                    }
                }
                vx.place(&f, 0);
                let (t, h) = match b {
                    Biome::Jungle => (0.9, 0.9),
                    Biome::Savanna => (0.8, -0.4),
                    Biome::DarkForest => (0.3, 0.8),
                    _ => (0.3, 0.3),
                };
                row.push(render_iso(&vx, sc, t, h));
            }
            rows.push(row);
        }
        let (w, h, img) = compose(&rows, 6);
        let p = out_dir().join(format!("trees_{name}.png"));
        write_png_rgb8(&p, w as u32, h as u32, &img).unwrap();
        eprintln!("wrote {}", p.display());
    }
}

/// Find a spot of a given biome whose surroundings (±r) share it, flat-ish.
fn find_biome(g: &WorldGen, b: Biome, r: f64) -> Option<(i32, i32)> {
    for i in 0..60000u64 {
        let h = voxel_world::noise::mix64(i ^ 0xB10E);
        let x = voxel_world::noise::hash_f01(h) as f64 * 30000.0 - 15000.0;
        let z = voxel_world::noise::hash_f01(voxel_world::noise::mix64(h)) as f64 * 30000.0 - 15000.0;
        let c = g.column(x, z);
        if c.biome != b || c.slope > 0.5 {
            continue;
        }
        let ok = [(-r, -r), (r, -r), (-r, r), (r, r), (0.0, r), (r, 0.0)].iter().all(|&(dx, dz)| g.column(x + dx, z + dz).biome == b);
        if ok {
            return Some((x as i32, z as i32));
        }
    }
    None
}

fn top_solid(col: &ColumnInfo) -> i32 {
    (col.height - 0.5).floor() as i32
}

/// In-situ iso scene: N×N terrain columns (surface materials) + placed features.
fn scene(g: &WorldGen, cx: i32, cz: i32, n: i32, s: f32) -> (usize, usize, Vec<u8>) {
    let (x0, z0) = (cx - n / 2, cz - n / 2);
    let cols: Vec<ColumnInfo> = (0..n * n).map(|i| g.column((x0 + i % n) as f64 + 0.5, (z0 + i / n) as f64 + 0.5)).collect();
    let tops: Vec<i32> = cols.iter().map(top_solid).collect();
    let ymin = tops.iter().min().unwrap() - 6;
    let ymax = tops.iter().max().unwrap() + 40;
    let mut vx = Vox::new(x0, ymin, z0, n, ymax - ymin, n);
    for k in 0..n {
        for i in 0..n {
            let c = &cols[(k * n + i) as usize];
            let t = tops[(k * n + i) as usize];
            let mut lo = t - 2;
            for (di, dk) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let (ii, kk) = (i + di, k + dk);
                if ii >= 0 && kk >= 0 && ii < n && kk < n {
                    lo = lo.min(tops[(kk * n + ii) as usize] - 1);
                }
            }
            lo = lo.max(ymin);
            for y in lo..=t {
                vx.set(x0 + i, y, z0 + k, g.surface_material(x0 + i, y, z0 + k, t - y, c));
            }
            if c.water_level != i32::MIN {
                for y in t + 1..c.water_level {
                    vx.set(x0 + i, y, z0 + k, Material::Water);
                }
            }
        }
    }
    let feats = g.features_in(x0 - 12, z0 - 12, x0 + n + 12, z0 + n + 12);
    for f in &feats {
        let ground = if f.root_x >= x0 && f.root_x < x0 + n && f.root_z >= z0 && f.root_z < z0 + n {
            tops[((f.root_z - z0) * n + (f.root_x - x0)) as usize] + 1
        } else {
            top_solid(&g.column(f.root_x as f64 + 0.5, f.root_z as f64 + 0.5)) + 1
        };
        vx.place(f, ground);
    }
    let c = &cols[(n / 2 * n + n / 2) as usize];
    render_iso(&vx, s, c.surface_temperature.clamp(-1.0, 1.0), c.climate.humidity)
}

fn scenes(g: &WorldGen, seed: u64) {
    let biomes = [
        Biome::DarkForest,
        Biome::Forest,
        Biome::BirchForest,
        Biome::Taiga,
        Biome::OldGrowthTaiga,
        Biome::Jungle,
        Biome::Savanna,
        Biome::Plains,
        Biome::Desert,
        Biome::Swamp,
        Biome::AlpineGrove,
        Biome::SnowyPlains,
    ];
    let mut imgs = Vec::new();
    for b in biomes {
        if let Some((x, z)) = find_biome(g, b, 40.0) {
            eprintln!("scene {} at ({x},{z})", b.name());
            imgs.push(scene(g, x, z, 72, 2.5));
        } else {
            eprintln!("no {} found", b.name());
        }
    }
    let rows: Vec<Vec<_>> = imgs.chunks(3).map(|c| c.to_vec()).collect();
    let (w, h, img) = compose(&rows, 4);
    let p = out_dir().join(format!("seed{seed}_scenes.png"));
    write_png_rgb8(&p, w as u32, h as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

/// Top-down feature map around several biome spots: canopy top material shaded by height.
fn feature_map(g: &WorldGen, seed: u64) {
    let n = 256i32;
    let mut panels = Vec::new();
    for b in [Biome::DarkForest, Biome::Taiga, Biome::Jungle, Biome::Savanna, Biome::Forest, Biome::Desert] {
        let Some((cx, cz)) = find_biome(g, b, 30.0) else { continue };
        let (x0, z0) = (cx - n / 2, cz - n / 2);
        let cols: Vec<ColumnInfo> = (0..n * n).map(|i| g.column((x0 + i % n) as f64 + 0.5, (z0 + i / n) as f64 + 0.5)).collect();
        let mut top = vec![(i32::MIN, Material::Air); (n * n) as usize];
        let feats = g.features_in(x0 - 12, z0 - 12, x0 + n + 12, z0 + n + 12);
        let mut roots = Vec::new();
        for f in &feats {
            let ground = top_solid(&g.column(f.root_x as f64 + 0.5, f.root_z as f64 + 0.5)) + 1;
            roots.push((f.root_x - x0, f.root_z - z0));
            for c in &f.columns {
                let (x, z) = (f.root_x + c.dx as i32 - x0, f.root_z + c.dz as i32 - z0);
                if x < 0 || z < 0 || x >= n || z >= n {
                    continue;
                }
                let y = ground + c.y1 as i32 - 1;
                let t = &mut top[(z * n + x) as usize];
                if y > t.0 {
                    *t = (y, c.mat);
                }
            }
        }
        let mut img = vec![0u8; (n * n * 3) as usize];
        for i in 0..(n * n) as usize {
            let c = &cols[i];
            let ground = top_solid(c);
            let (y, m) = if top[i].0 > ground { top[i] } else { (ground, g.surface_material(x0 + i as i32 % n, ground, z0 + i as i32 / n, 0, c)) };
            let mut col = mat_color(m, c.surface_temperature.clamp(-1.0, 1.0), c.climate.humidity);
            if c.water_level != i32::MIN && c.water_level > ground + 1 && top[i].0 <= ground {
                col = Vec3::new(0.05, 0.12, 0.35);
            }
            let rel = (y - ground) as f32;
            let k = 0.65 + 0.35 * (rel / 25.0).min(1.0) + 0.0;
            // hillshade by the height difference to the west neighbour
            let west = if i % n as usize > 0 { top[i - 1].0.max(top_solid(&cols[i - 1])) } else { y };
            let sh = (1.0 + 0.08 * (y - west) as f32).clamp(0.6, 1.4);
            img[i * 3..i * 3 + 3].copy_from_slice(&srgb(col * (k * sh)));
        }
        for (rx, rz) in roots {
            if rx >= 0 && rz >= 0 && rx < n && rz < n {
                let i = (rz * n + rx) as usize;
                img[i * 3..i * 3 + 3].copy_from_slice(&[90, 50, 20]);
            }
        }
        eprintln!("map {} at ({cx},{cz}): {} features", b.name(), feats.len());
        panels.push((n as usize, n as usize, img));
    }
    let rows: Vec<Vec<_>> = panels.chunks(3).map(|c| c.to_vec()).collect();
    let (w, h, img) = compose(&rows, 4);
    let p = out_dir().join(format!("seed{seed}_feature_map.png"));
    write_png_rgb8(&p, w as u32, h as u32, &img).unwrap();
    eprintln!("wrote {}", p.display());
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("all").to_string();
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let g = WorldGen::new(seed);
    let _ = (FeatureKind::Oak, SEA_LEVEL);
    if mode == "caves" || mode == "all" {
        caves_slices(&g, seed);
    }
    if mode == "plan" || mode == "all" {
        caves_plan(&g, seed);
    }
    if mode == "trees" || mode == "all" {
        species_gallery();
    }
    if mode == "scenes" || mode == "all" {
        scenes(&g, seed);
    }
    if mode == "features" || mode == "all" {
        feature_map(&g, seed);
    }
}
