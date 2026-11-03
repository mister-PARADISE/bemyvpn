//! Synthetic analytic worlds for renderer tests, benchmarks and the `render_synth` example.
//!
//! The terrain is a closed-form function `H(x, z)` built from the frozen noise toolkit:
//!
//! * continent field `C = Fbm(λ=7000)` → ocean basins where `C < 0`, smoothly lifted continents;
//! * mountain massifs `A(C) · Ridged(λ=1700)^1.3` (sharp crest lines), gated by a second low-frequency
//!   field so ranges are separated by lowlands;
//! * hills `Fbm(λ=420)` and roughness `Fbm(λ=38)`;
//! * a badlands mesa province with smooth terraces `h' = (⌊h/s⌋ + smoothstep(0.82, 1, frac(h/s)))·s`;
//! * near the origin a hand-placed showcase: a torus **arch**, a horizontal **cave tunnel** (tube around a
//!   parametric curve), a lava pool and a crystal cluster.
//!
//! Level 0 rasterises `H` into exact voxel spans (with water, sand, snow, terracotta, and a
//! jittered-grid point process of ellipsoid-canopy trees); heightfield levels sample the *same*
//! function band-limited (`sample2_lod`) so the clipmap is seamless.
use crate::gen::Biome;
use crate::materials::{self, Material};
use crate::math::{smoothstep, Vec3};
use crate::noise::{derive_seed, hash2, hash_f01, salt, Fbm, Ridged};
use crate::world::{BuildParams, HeightLevel, HfCell, Level0, Level0Row, Span, World};

const SEA: f64 = 140.0;

pub struct SynthTerrain {
    cont: Fbm,
    range: Fbm,
    ridged: Ridged,
    hills: Fbm,
    rough: Fbm,
    mesa: Fbm,
    forest: Fbm,
    climate: Fbm,
    seed: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SynthColumn {
    pub h: f64,
    pub mesa: f32,
    pub forest: f32,
    pub temp: f32,
    pub humid: f32,
    pub slope: f32,
}

impl SynthTerrain {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        SynthTerrain {
            cont: Fbm::new(s("cont"), 7000.0, 6, 2.0, 0.5),
            range: Fbm::new(s("range"), 3200.0, 3, 2.0, 0.5),
            ridged: Ridged::new(s("ridged"), 1700.0, 7),
            hills: Fbm::new(s("hills"), 420.0, 6, 2.0, 0.5),
            rough: Fbm::new(s("rough"), 38.0, 3, 2.0, 0.5),
            mesa: Fbm::new(s("mesa"), 2600.0, 3, 2.0, 0.5),
            forest: Fbm::new(s("forest"), 900.0, 4, 2.0, 0.5),
            climate: Fbm::new(s("climate"), 5000.0, 3, 2.0, 0.5),
            seed,
        }
    }

    /// Height of the analytic terrain at (x, z) band-limited to `footprint` blocks.
    pub fn height(&self, x: f64, z: f64, fp: f64) -> (f64, f32) {
        // Offset: lattice noises vanish at the origin (would create a spike there).
        let (x, z) = (x + 3171.37, z - 1529.71);
        let c = self.cont.sample2_lod(x, z, fp) as f64 * 3.2 + 0.25;
        // Base: deep ocean → shelf → coast → lowland.
        let base = if c < 0.0 {
            SEA - 4.0 + 90.0 * (c.max(-1.2)) // ocean floor down to ~32
        } else {
            SEA - 4.0 + 70.0 * (c.min(1.5)) / (1.0 + c.min(1.5))
        };
        let land = smoothstep(0.0, 0.35, c as f32) as f64;
        let rg = smoothstep(-0.12, 0.18, self.range.sample2_lod(x + 900.0, z - 300.0, fp)) as f64;
        let r = self.ridged.sample2_lod(x, z, fp) as f64;
        let mountains = land * rg * 400.0 * r.max(0.0).powf(1.5);
        let hills = land * (14.0 + 18.0 * (1.0 - rg)) * self.hills.sample2_lod(x, z, fp) as f64 * 2.2;
        let rough = if fp < 20.0 { self.rough.sample2_lod(x, z, fp) as f64 * 4.0 * land } else { 0.0 };
        let mut h = base + mountains + hills + rough + land * 10.0;
        let mesa = smoothstep(0.22, 0.32, self.mesa.sample2_lod(x - 4000.0, z + 2500.0, fp)) * land as f32;
        if mesa > 0.0 && h > SEA + 3.0 {
            let s = 11.0;
            let v = (h - SEA) / s;
            let t = (v.floor() + smoothstep(0.82, 1.0, v.fract() as f32) as f64) * s + SEA;
            h = h + (t + 6.0 - h) * mesa as f64;
        }
        (h.clamp(4.0, 500.0), mesa)
    }

    pub fn column(&self, x: f64, z: f64, fp: f64) -> SynthColumn {
        let (h, mesa) = self.height(x, z, fp);
        let e = fp.max(1.0);
        let hx = self.height(x + e, z, fp).0 - self.height(x - e, z, fp).0;
        let hz = self.height(x, z + e, fp).0 - self.height(x, z - e, fp).0;
        let slope = ((hx * hx + hz * hz).sqrt() / (2.0 * e)) as f32;
        let forest = smoothstep(-0.05, 0.2, self.forest.sample2_lod(x, z, fp));
        let t0 = self.climate.sample2_lod(x, z, fp) * 2.5;
        let temp = (t0 - ((h - SEA).max(0.0) as f32) / 250.0).clamp(-1.0, 1.0);
        let humid = (self.climate.sample2_lod(z + 7000.0, x, fp) * 2.5 + 0.2).clamp(-1.0, 1.0);
        SynthColumn { h, mesa, forest, temp, humid, slope }
    }

    /// Top material for a column.
    pub fn top_material(&self, col: &SynthColumn) -> Material {
        let h = col.h;
        if col.mesa > 0.5 && h > SEA + 3.0 {
            return if col.slope > 0.8 { Material::Terracotta } else { Material::RedSand };
        }
        if h < SEA + 2.5 {
            if h < SEA - 6.0 {
                Material::Gravel
            } else {
                Material::Sand
            }
        } else if h > 360.0 || col.temp < -0.75 {
            if col.slope > 1.6 {
                Material::Stone
            } else {
                Material::Snow
            }
        } else if col.slope > 1.1 {
            Material::Stone
        } else {
            Material::Grass
        }
    }

    /// Tree at jittered grid cell (cx, cz) (cell 9 blocks): (root x, root z, trunk height, canopy radius).
    pub fn tree_in_cell(&self, cx: i32, cz: i32) -> Option<Tree> {
        let hsh = hash2(self.seed ^ 0x7EE5, cx, cz);
        let rx = cx * 9 + 1 + (hsh % 7) as i32;
        let rz = cz * 9 + 1 + ((hsh >> 8) % 7) as i32;
        let col = self.column(rx as f64 + 0.5, rz as f64 + 0.5, 0.0);
        if self.top_material(&col) != Material::Grass || col.slope > 0.7 {
            return None;
        }
        let p = 0.08 + 0.85 * col.forest;
        if hash_f01(hash2(self.seed ^ 0xF00, cx, cz)) > p {
            return None;
        }
        let birch = hash_f01(hsh.rotate_left(17)) < 0.25;
        let spruce = col.temp < 0.0;
        let trunk = 4 + ((hsh >> 20) % 4) as i32;
        let r = 2.2 + hash_f01(hsh.rotate_left(29)) as f64 * 1.4;
        let (log, leaves) = if spruce {
            (Material::SpruceLog, Material::SpruceLeaves)
        } else if birch {
            (Material::BirchLog, Material::BirchLeaves)
        } else {
            (Material::OakLog, Material::OakLeaves)
        };
        let ground = col.h.floor() as i32 + 1;
        Some((rx, rz, trunk, r, log, leaves, ground))
    }

    /// Exact voxel spans of the column (x, z) (sorted, no air).
    pub fn spans(&self, x: i32, z: i32) -> Vec<Span> {
        self.spans_with(x, z, None)
    }

    /// Like `spans`, with an optional precomputed tree grid.
    pub fn spans_with(&self, x: i32, z: i32, grid: Option<&TreeGrid>) -> Vec<Span> {
        let col = self.column(x as f64 + 0.5, z as f64 + 0.5, 0.0);
        self.spans_col(x, z, &col, grid)
    }

    /// Biome label for a column (for tints/water colour only).
    pub fn biome_of(&self, col: &SynthColumn, top: Material, cov: f32) -> Biome {
        if col.h < SEA - 30.0 {
            Biome::DeepOcean
        } else if col.h < SEA {
            Biome::Ocean
        } else if col.mesa > 0.5 {
            Biome::Badlands
        } else if top == Material::Snow {
            Biome::SnowyPeaks
        } else if cov > 0.4 {
            Biome::Forest
        } else {
            Biome::Plains
        }
    }

    /// Spans of column (x, z) given its precomputed `SynthColumn`.
    pub fn spans_col(&self, x: i32, z: i32, col: &SynthColumn, grid: Option<&TreeGrid>) -> Vec<Span> {
        let col = *col;
        let hi = col.h.floor() as i32 + 1; // solid blocks y < hi
        let top = self.top_material(&col);
        let mut mats: Vec<(i32, i32, Material)> = Vec::with_capacity(8);
        mats.push((0, 3, Material::Bedrock));
        let under = match top {
            Material::Grass => Material::Dirt,
            Material::Sand => Material::Sandstone,
            Material::RedSand | Material::Terracotta => Material::Terracotta,
            _ => Material::Stone,
        };
        let d = if top == Material::Terracotta || top == Material::RedSand { 40 } else { 4 };
        mats.push((3, (hi - d).max(3), Material::Stone));
        mats.push(((hi - d).max(3), hi - 1, under));
        mats.push((hi - 1, hi, top));
        let mut cells: Vec<Material> = Vec::new();
        let ymax = 330i32;
        let showcase = (x - 40).abs() < 90 && (z - 70).abs() < 90;
        if showcase {
            // Dense per-y evaluation for the showcase region.
            cells.resize(ymax as usize, Material::Air);
            for &(a, b, m) in &mats {
                for y in a.max(0)..b.min(ymax) {
                    cells[y as usize] = m;
                }
            }
            let (xf, zf) = (x as f64 + 0.5, z as f64 + 0.5);
            // Arch: torus of major radius 14 around (60, hA, 40) in the plane z = const.
            let ha = self.column(60.0, 40.0, 0.0).h.floor();
            let hc = self.column(20.0, 95.0, 0.0).h - 9.0;
            for y in 0..ymax {
                let yf = y as f64 + 0.5;
                let (dx, dy, dz) = (xf - 60.0, yf - ha, zf - 40.0);
                let ring = ((dx * dx + dy * dy).sqrt() - 14.0).hypot(dz / 1.3);
                if ring < 3.2 && dy > -2.0 {
                    cells[y as usize] = if ring < 2.6 { Material::Sandstone } else { Material::Stone };
                }
                // Cave: tube of radius 3.5 around the curve (s, hC + 6 sin(s/17), 95 + 10 cos(s/23)).
                let cy = hc + 6.0 * (xf / 17.0).sin();
                let cz = 95.0 + 10.0 * (xf / 23.0).cos();
                let r = ((yf - cy) * (yf - cy) + (zf - cz) * (zf - cz)).sqrt();
                if r < 3.6 && xf > -20.0 && xf < 110.0 {
                    cells[y as usize] = Material::Air;
                    if (yf - cy) < -2.4 && (xf - 50.0).abs() < 4.0 {
                        cells[y as usize] = Material::Lava;
                    }
                }
                // Crystal cluster on the cave wall.
                let (qx, qy, qz) = (xf - 75.0, yf - (hc + 6.0 * (75.0f64 / 17.0).sin() + 3.0), zf - cz);
                if (qx * qx + qy * qy * 0.6 + qz * qz).sqrt() < 2.5 && r >= 3.0 && r < 5.0 {
                    cells[y as usize] = Material::Crystal;
                }
            }
        }
        // Water.
        let water_top = SEA as i32;
        let mut spans = Vec::with_capacity(10);
        if showcase {
            for y in hi..water_top {
                if y >= 0 && (y as usize) < cells.len() && cells[y as usize] == Material::Air {
                    cells[y as usize] = Material::Water;
                }
            }
        }
        // Trees (only if dry ground).
        let mut tree_cells: Vec<(i32, i32, Material)> = Vec::new();
        if hi > water_top {
            let (gx, gz) = (x.div_euclid(9), z.div_euclid(9));
            for cz in gz - 1..=gz + 1 {
                for cx in gx - 1..=gx + 1 {
                    let tree = match grid {
                        Some(g) => g.get(self, cx, cz),
                        None => self.tree_in_cell(cx, cz),
                    };
                    if let Some((rx, rz, trunk, r, log, leaves, ground)) = tree {
                        let (dx, dz) = ((x - rx) as f64, (z - rz) as f64);
                        let ctr = ground as f64 + trunk as f64;
                        let ry = if log == Material::SpruceLog { r * 1.9 } else { r * 0.85 };
                        let q = (dx * dx + dz * dz) / (r * r);
                        if q < 1.0 {
                            let half = ry * (1.0 - q).sqrt();
                            let cy = if log == Material::SpruceLog { ctr - ry * 0.35 } else { ctr };
                            let y0 = (cy - half + 0.5).ceil() as i32;
                            let y1 = (cy + half + 0.5).floor() as i32;
                            if y1 > y0 {
                                tree_cells.push((y0, y1, leaves));
                            }
                        }
                        if x == rx && z == rz {
                            tree_cells.push((ground, ground + trunk + 1, log));
                        }
                    }
                }
            }
        }
        if showcase {
            for &(a, b, m) in &tree_cells {
                for y in a.max(0)..b.min(ymax) {
                    let c = &mut cells[y as usize];
                    if *c == Material::Air || (m.is_log() && c.is_leaves()) {
                        *c = m;
                    }
                }
            }
            // Compress cells to spans.
            let mut y = 0usize;
            while y < cells.len() {
                let m = cells[y];
                let mut e = y + 1;
                while e < cells.len() && cells[e] == m {
                    e += 1;
                }
                if m != Material::Air {
                    spans.push(Span::new(y as i32, e as i32, m));
                }
                y = e;
            }
            return spans;
        }
        for &(a, b, m) in &mats {
            if b > a {
                spans.push(Span::new(a, b, m));
            }
        }
        if water_top > hi {
            spans.push(Span::new(hi, water_top, Material::Water));
        }
        if !tree_cells.is_empty() {
            // Merge tree runs (above ground) into the span list: logs override leaves.
            let lo = tree_cells.iter().map(|c| c.0).min().unwrap().max(hi);
            let top_y = tree_cells.iter().map(|c| c.1).max().unwrap();
            if top_y > lo {
                let mut cells = vec![Material::Air; (top_y - lo) as usize];
                for &(a, b, m) in &tree_cells {
                    for y in a.max(lo)..b {
                        let c = &mut cells[(y - lo) as usize];
                        if *c == Material::Air || (m.is_log() && c.is_leaves()) {
                            *c = m;
                        }
                    }
                }
                let mut y = 0usize;
                while y < cells.len() {
                    let m = cells[y];
                    let mut e = y + 1;
                    while e < cells.len() && cells[e] == m {
                        e += 1;
                    }
                    if m != Material::Air {
                        spans.push(Span::new(lo + y as i32, lo + e as i32, m));
                    }
                    y = e;
                }
            }
        }
        spans
    }

    /// Heightfield cell for level k (cell centre (x, z)).
    pub fn hf_cell(&self, k: u32, x: f64, z: f64) -> HfCell {
        let fp = (1u32 << k) as f64;
        let col = self.column(x, z, fp);
        let top = self.top_material(&col);
        let mut albedo = materials::far_albedo(top, col.temp, col.humid, col.h as f32);
        let mut h = col.h as f32;
        let mut cov = 0.0f32;
        if top == Material::Grass {
            cov = (0.08 + 0.85 * col.forest) * 0.75 * (1.0 - smoothstep(0.4, 0.7, col.slope));
            let leaves = if col.temp < 0.0 {
                Material::SpruceLeaves.albedo()
            } else {
                Material::OakLeaves.albedo() * materials::foliage_tint(col.temp, col.humid)
            };
            albedo = albedo.lerp(leaves * 0.8, cov);
            h += cov * 7.0;
        }
        let water = if col.h < SEA { SEA as f32 } else { f32::NEG_INFINITY };
        let biome = self.biome_of(&col, top, cov);
        let _ = Vec3::ZERO;
        HfCell { h, water, albedo, mat: top, biome }
    }
}

/// (root x, root z, trunk height, canopy radius, log, leaves, ground y).
pub type Tree = (i32, i32, i32, f64, Material, Material, i32);

/// Precomputed jittered-grid tree point process over a rectangle of cells.
pub struct TreeGrid {
    c0: (i32, i32),
    n: (i32, i32),
    cells: Vec<Option<Tree>>,
}

impl TreeGrid {
    pub fn new(t: &SynthTerrain, x0: i32, z0: i32, x1: i32, z1: i32) -> TreeGrid {
        use rayon::prelude::*;
        let c0 = (x0.div_euclid(9) - 1, z0.div_euclid(9) - 1);
        let n = (x1.div_euclid(9) - c0.0 + 2, z1.div_euclid(9) - c0.1 + 2);
        let cells = (0..n.0 * n.1).into_par_iter().map(|k| t.tree_in_cell(c0.0 + k % n.0, c0.1 + k / n.0)).collect();
        TreeGrid { c0, n, cells }
    }
    #[inline]
    pub fn get(&self, t: &SynthTerrain, cx: i32, cz: i32) -> Option<Tree> {
        let (i, j) = (cx - self.c0.0, cz - self.c0.1);
        if i >= 0 && j >= 0 && i < self.n.0 && j < self.n.1 {
            self.cells[(j * self.n.0 + i) as usize]
        } else {
            t.tree_in_cell(cx, cz)
        }
    }
}

/// Build a synthetic world centred at (cx, cz).
pub fn synthetic_world(seed: u64, cx: i32, cz: i32, l0_size: u32, hf_size: u32, hf_levels: u32) -> World {
    let terrain = SynthTerrain::new(seed);
    let params = BuildParams {
        center_x: cx,
        center_z: cz,
        l0_size,
        hf_size,
        hf_levels,
        cave_radius: 256,
        feature_radius: l0_size / 2,
    };
    use rayon::prelude::*;
    let (ox, oz) = params.l0_origin();
    let grid = TreeGrid::new(&terrain, ox, oz, ox + l0_size as i32, oz + l0_size as i32);
    // Level 0 assembled row by row so every column carries its own climate (biome tints).
    let rows: Vec<Level0Row> = (0..l0_size as i32)
        .into_par_iter()
        .map(|j| {
            let mut row = Level0Row::with_capacity(l0_size as usize);
            for i in 0..l0_size as i32 {
                let (x, z) = (ox + i, oz + j);
                let col = terrain.column(x as f64 + 0.5, z as f64 + 0.5, 0.0);
                let spans = terrain.spans_col(x, z, &col, Some(&grid));
                let biome = terrain.biome_of(&col, terrain.top_material(&col), col.forest);
                row.push_column(&spans, biome, col.temp, col.humid);
            }
            row
        })
        .collect();
    let l0 = Level0::from_rows(ox, oz, l0_size, rows);
    let levels = (1..=hf_levels)
        .map(|k| {
            let (lx, lz) = params.level_origin(k);
            HeightLevel::from_fn(k, lx, lz, hf_size, |x, z| terrain.hf_cell(k, x, z))
        })
        .collect();
    World { seed, params, l0, levels }
}
