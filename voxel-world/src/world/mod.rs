//! In-memory world representation for rendering. FROZEN shared file (storage + queries).
//! The generation orchestration (`build_world`) lives in `build.rs` (agent: world-build).
//!
//! Layout (clipmap, all squares centred on the camera's xz):
//! * `Level0` — exact voxels for a `l0_size`² area, stored per column as sorted vertical spans.
//! * `HeightLevel` k = 1..=K — heightfields with cell size 2^k covering `hf_size·2^k` blocks.
//!   Each stores terrain(+canopy) top, water surface, far albedo, top material, biome.
//! * Every level has a MAX-mipmap pyramid for empty-space skipping during ray traversal.

pub mod build;

pub use build::build_world;

use crate::gen::Biome;
use crate::materials::Material;
use crate::math::Vec3;

/// A vertical run of identical blocks filling [y0, y1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub y0: u16,
    pub y1: u16,
    pub mat: Material,
}

impl Span {
    #[inline]
    pub fn new(y0: i32, y1: i32, mat: Material) -> Span {
        Span { y0: y0.clamp(0, u16::MAX as i32) as u16, y1: y1.clamp(0, u16::MAX as i32) as u16, mat }
    }
}

/// Parameters of a world build (all distances in blocks).
#[derive(Clone, Copy, Debug)]
pub struct BuildParams {
    /// Horizontal centre (usually the camera position).
    pub center_x: i32,
    pub center_z: i32,
    /// Side length of the exact-voxel square (power of two, e.g. 2048 → radius 1024).
    pub l0_size: u32,
    /// Cells per side of every heightfield level (power of two, e.g. 2048).
    pub hf_size: u32,
    /// Number of heightfield levels (cells 2, 4, …, 2^hf_levels). 0 = voxels only.
    pub hf_levels: u32,
    /// Caves are carved only within this horizontal radius of the centre.
    pub cave_radius: u32,
    /// Exact features (trees) are placed only within this radius (≤ l0_size/2).
    pub feature_radius: u32,
}

impl Default for BuildParams {
    fn default() -> Self {
        BuildParams {
            center_x: 0,
            center_z: 0,
            l0_size: 2048,
            hf_size: 2048,
            hf_levels: 5,
            cave_radius: 384,
            feature_radius: 1024,
        }
    }
}

impl BuildParams {
    /// World x/z of the (0,0) column of level 0.
    pub fn l0_origin(&self) -> (i32, i32) {
        let h = (self.l0_size / 2) as i32;
        (self.center_x - h, self.center_z - h)
    }
    /// World x/z of the (0,0) cell corner of heightfield level k (k ≥ 1). Aligned to the cell size.
    pub fn level_origin(&self, k: u32) -> (i32, i32) {
        let cell = 1i32 << k;
        let half = (self.hf_size as i64 * cell as i64 / 2) as i32;
        (
            (self.center_x - half).div_euclid(cell) * cell,
            (self.center_z - half).div_euclid(cell) * cell,
        )
    }
}

// ------------------------------------------------------------------ Level 0

/// Builder for one row (fixed z) of level-0 columns. Produced in parallel, then assembled.
#[derive(Default, Clone)]
pub struct Level0Row {
    pub spans: Vec<Span>,
    /// Number of spans of each column in the row (in x order).
    pub counts: Vec<u16>,
    pub biome: Vec<Biome>,
    /// (surface temperature, humidity) quantised ×127.
    pub climate: Vec<[i8; 2]>,
}

impl Level0Row {
    pub fn with_capacity(cols: usize) -> Self {
        Level0Row {
            spans: Vec::with_capacity(cols * 4),
            counts: Vec::with_capacity(cols),
            biome: Vec::with_capacity(cols),
            climate: Vec::with_capacity(cols),
        }
    }
    /// Append one column. `spans` must be sorted by y0, non-overlapping, without Air spans.
    pub fn push_column(&mut self, spans: &[Span], biome: Biome, temperature: f32, humidity: f32) {
        debug_assert!(spans.windows(2).all(|w| w[0].y1 <= w[1].y0), "spans overlap/unsorted");
        debug_assert!(spans.iter().all(|s| s.mat != Material::Air && s.y1 > s.y0));
        self.spans.extend_from_slice(spans);
        self.counts.push(spans.len() as u16);
        self.biome.push(biome);
        self.climate.push([q8(temperature), q8(humidity)]);
    }
}

#[inline]
fn q8(v: f32) -> i8 {
    (v.clamp(-1.0, 1.0) * 127.0).round() as i8
}

pub struct Level0 {
    pub origin_x: i32,
    pub origin_z: i32,
    pub size: u32,
    col_start: Vec<u32>,
    spans: Vec<Span>,
    /// Exclusive top (max y1) of each column's highest span (solid OR water). 0 if empty.
    pub top: Vec<u16>,
    pub biome: Vec<Biome>,
    climate: Vec<[i8; 2]>,
    /// mip[0] = top; mip[m][j*s+i] = max of mip[m-1] over the 2×2 block, side s = size >> m.
    pub mip: Vec<Vec<u16>>,
}

impl Level0 {
    /// Assemble from `size` rows (row j = world z origin_z + j), each holding `size` columns.
    pub fn from_rows(origin_x: i32, origin_z: i32, size: u32, rows: Vec<Level0Row>) -> Level0 {
        assert_eq!(rows.len(), size as usize);
        let n = (size as usize) * (size as usize);
        let total: usize = rows.iter().map(|r| r.spans.len()).sum();
        let mut col_start = Vec::with_capacity(n + 1);
        let mut spans = Vec::with_capacity(total);
        let mut biome = Vec::with_capacity(n);
        let mut climate = Vec::with_capacity(n);
        let mut top = Vec::with_capacity(n);
        for r in rows {
            assert_eq!(r.counts.len(), size as usize);
            let mut off = 0usize;
            for &c in &r.counts {
                col_start.push(spans.len() as u32);
                let col = &r.spans[off..off + c as usize];
                top.push(col.last().map(|s| s.y1).unwrap_or(0));
                spans.extend_from_slice(col);
                off += c as usize;
            }
            biome.extend_from_slice(&r.biome);
            climate.extend_from_slice(&r.climate);
        }
        col_start.push(spans.len() as u32);
        let mip = build_mips_u16(&top, size);
        Level0 { origin_x, origin_z, size, col_start, spans, top, biome, climate, mip }
    }

    /// Convenience constructor (tests / synthetic scenes): `f(x, z)` returns sorted spans.
    pub fn from_fn<F>(origin_x: i32, origin_z: i32, size: u32, f: F) -> Level0
    where
        F: Fn(i32, i32) -> Vec<Span> + Sync,
    {
        use rayon::prelude::*;
        let rows: Vec<Level0Row> = (0..size as i32)
            .into_par_iter()
            .map(|j| {
                let mut row = Level0Row::with_capacity(size as usize);
                for i in 0..size as i32 {
                    let s = f(origin_x + i, origin_z + j);
                    row.push_column(&s, Biome::Plains, 0.3, 0.2);
                }
                row
            })
            .collect();
        Level0::from_rows(origin_x, origin_z, size, rows)
    }

    #[inline]
    pub fn contains(&self, x: i32, z: i32) -> bool {
        let i = x.wrapping_sub(self.origin_x) as u32;
        let j = z.wrapping_sub(self.origin_z) as u32;
        i < self.size && j < self.size
    }

    /// Column index; caller guarantees `contains(x, z)`.
    #[inline]
    pub fn idx(&self, x: i32, z: i32) -> usize {
        let i = (x - self.origin_x) as usize;
        let j = (z - self.origin_z) as usize;
        j * self.size as usize + i
    }

    /// Spans of a column (empty slice outside the level).
    #[inline]
    pub fn spans(&self, x: i32, z: i32) -> &[Span] {
        if !self.contains(x, z) {
            return &[];
        }
        let k = self.idx(x, z);
        &self.spans[self.col_start[k] as usize..self.col_start[k + 1] as usize]
    }

    #[inline]
    pub fn spans_at_index(&self, k: usize) -> &[Span] {
        &self.spans[self.col_start[k] as usize..self.col_start[k + 1] as usize]
    }

    /// Exclusive top of the column (0 outside / empty).
    #[inline]
    pub fn top(&self, x: i32, z: i32) -> u16 {
        if !self.contains(x, z) {
            return 0;
        }
        self.top[self.idx(x, z)]
    }

    /// Block material at integer coordinates (Air outside).
    pub fn block(&self, x: i32, y: i32, z: i32) -> Material {
        if y < 0 {
            return Material::Air;
        }
        let y = y as u32;
        for s in self.spans(x, z) {
            if (s.y0 as u32) > y {
                break;
            }
            if y < s.y1 as u32 {
                return s.mat;
            }
        }
        Material::Air
    }

    /// True if the block is opaque (not air, not water).
    #[inline]
    pub fn is_opaque(&self, x: i32, y: i32, z: i32) -> bool {
        self.block(x, y, z).is_opaque()
    }

    #[inline]
    pub fn biome(&self, x: i32, z: i32) -> Biome {
        if !self.contains(x, z) {
            return Biome::Plains;
        }
        self.biome[self.idx(x, z)]
    }

    /// (surface temperature, humidity) in [-1, 1].
    #[inline]
    pub fn climate_at(&self, x: i32, z: i32) -> (f32, f32) {
        if !self.contains(x, z) {
            return (0.3, 0.2);
        }
        let c = self.climate[self.idx(x, z)];
        (c[0] as f32 / 127.0, c[1] as f32 / 127.0)
    }

    /// Max-mip value at mip level m, mip-grid coords (mi, mj). Returns 0 outside.
    #[inline]
    pub fn mip_max(&self, m: usize, mi: i32, mj: i32) -> u16 {
        let s = (self.size >> m) as i32;
        if mi < 0 || mj < 0 || mi >= s || mj >= s {
            return 0;
        }
        self.mip[m][(mj * s + mi) as usize]
    }

    pub fn span_count(&self) -> usize {
        self.spans.len()
    }
}

fn build_mips_u16(base: &[u16], size: u32) -> Vec<Vec<u16>> {
    let mut mips = vec![base.to_vec()];
    let mut s = size as usize;
    while s > 1 {
        let prev = mips.last().unwrap();
        let ns = s / 2;
        let mut m = vec![0u16; ns * ns];
        for j in 0..ns {
            for i in 0..ns {
                let a = prev[(2 * j) * s + 2 * i];
                let b = prev[(2 * j) * s + 2 * i + 1];
                let c = prev[(2 * j + 1) * s + 2 * i];
                let d = prev[(2 * j + 1) * s + 2 * i + 1];
                m[j * ns + i] = a.max(b).max(c).max(d);
            }
        }
        mips.push(m);
        s = ns;
    }
    mips
}

// ------------------------------------------------------------------ heightfield levels

/// One heightfield cell (value sampled at the cell centre).
#[derive(Clone, Copy, Debug)]
pub struct HfCell {
    /// Top of opaque surface (terrain + expected canopy), world y.
    pub h: f32,
    /// Water surface y, or `f32::NEG_INFINITY` when dry.
    pub water: f32,
    /// Linear far albedo of the top surface (already includes canopy blend & tints).
    pub albedo: Vec3,
    /// Dominant top material (for special shading: snow/ice/sand/water…).
    pub mat: Material,
    pub biome: Biome,
}

pub struct HeightLevel {
    pub k: u32,
    /// Cell size in blocks (2^k).
    pub cell: i32,
    /// World coordinates of the (0,0) cell's min corner.
    pub origin_x: i32,
    pub origin_z: i32,
    pub size: u32,
    pub h: Vec<f32>,
    pub water: Vec<f32>,
    pub albedo: Vec<[f32; 3]>,
    pub mat: Vec<Material>,
    pub biome: Vec<Biome>,
    /// mip[0][i] = max(h, water); mip[m] = 2×2 max of mip[m-1].
    pub mip: Vec<Vec<f32>>,
}

impl HeightLevel {
    /// `f(cx, cz)` receives the WORLD coordinates of the cell centre.
    pub fn from_fn<F>(k: u32, origin_x: i32, origin_z: i32, size: u32, f: F) -> HeightLevel
    where
        F: Fn(f64, f64) -> HfCell + Sync,
    {
        use rayon::prelude::*;
        let cell = 1i32 << k;
        let n = size as usize;
        let rows: Vec<Vec<HfCell>> = (0..n)
            .into_par_iter()
            .map(|j| {
                (0..n)
                    .map(|i| {
                        let cx = origin_x as f64 + (i as f64 + 0.5) * cell as f64;
                        let cz = origin_z as f64 + (j as f64 + 0.5) * cell as f64;
                        f(cx, cz)
                    })
                    .collect()
            })
            .collect();
        let mut h = Vec::with_capacity(n * n);
        let mut water = Vec::with_capacity(n * n);
        let mut albedo = Vec::with_capacity(n * n);
        let mut mat = Vec::with_capacity(n * n);
        let mut biome = Vec::with_capacity(n * n);
        for r in rows {
            for c in r {
                h.push(c.h);
                water.push(c.water);
                albedo.push(c.albedo.to_array());
                mat.push(c.mat);
                biome.push(c.biome);
            }
        }
        let base: Vec<f32> = h.iter().zip(&water).map(|(a, b)| a.max(*b)).collect();
        let mip = build_mips_f32(&base, size);
        HeightLevel { k, cell, origin_x, origin_z, size, h, water, albedo, mat, biome, mip }
    }

    /// Cell index of a world position (may be out of range).
    #[inline]
    pub fn cell_of(&self, x: f64, z: f64) -> (i32, i32) {
        (
            ((x - self.origin_x as f64) / self.cell as f64).floor() as i32,
            ((z - self.origin_z as f64) / self.cell as f64).floor() as i32,
        )
    }

    #[inline]
    pub fn in_range(&self, i: i32, j: i32) -> bool {
        (i as u32) < self.size && (j as u32) < self.size
    }

    #[inline]
    pub fn index(&self, i: i32, j: i32) -> usize {
        j as usize * self.size as usize + i as usize
    }

    /// World-space square covered: [x0, x1) × [z0, z1).
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let ext = self.size as f64 * self.cell as f64;
        (self.origin_x as f64, self.origin_z as f64, self.origin_x as f64 + ext, self.origin_z as f64 + ext)
    }

    #[inline]
    pub fn mip_max(&self, m: usize, mi: i32, mj: i32) -> f32 {
        let s = (self.size >> m) as i32;
        if mi < 0 || mj < 0 || mi >= s || mj >= s {
            return f32::NEG_INFINITY;
        }
        self.mip[m][(mj * s + mi) as usize]
    }

    /// Bilinearly interpolated terrain height at a world position (clamped to the grid).
    pub fn height_bilinear(&self, x: f64, z: f64) -> f32 {
        let gx = (x - self.origin_x as f64) / self.cell as f64 - 0.5;
        let gz = (z - self.origin_z as f64) / self.cell as f64 - 0.5;
        let i0 = gx.floor();
        let j0 = gz.floor();
        let fx = (gx - i0) as f32;
        let fz = (gz - j0) as f32;
        let s = self.size as i32 - 1;
        let cl = |v: f64| (v as i32).clamp(0, s);
        let (i0, j0) = (cl(i0), cl(j0));
        let (i1, j1) = ((i0 + 1).min(s), (j0 + 1).min(s));
        let h = |i: i32, j: i32| self.h[self.index(i, j)];
        let a = h(i0, j0) + (h(i1, j0) - h(i0, j0)) * fx;
        let b = h(i0, j1) + (h(i1, j1) - h(i0, j1)) * fx;
        a + (b - a) * fz
    }
}

fn build_mips_f32(base: &[f32], size: u32) -> Vec<Vec<f32>> {
    let mut mips = vec![base.to_vec()];
    let mut s = size as usize;
    while s > 1 {
        let prev = mips.last().unwrap();
        let ns = s / 2;
        let mut m = vec![0f32; ns * ns];
        for j in 0..ns {
            for i in 0..ns {
                let a = prev[(2 * j) * s + 2 * i];
                let b = prev[(2 * j) * s + 2 * i + 1];
                let c = prev[(2 * j + 1) * s + 2 * i];
                let d = prev[(2 * j + 1) * s + 2 * i + 1];
                m[j * ns + i] = a.max(b).max(c).max(d);
            }
        }
        mips.push(m);
        s = ns;
    }
    mips
}

// ------------------------------------------------------------------ world

pub struct World {
    pub seed: u64,
    pub params: BuildParams,
    pub l0: Level0,
    /// levels[i] has k = i + 1.
    pub levels: Vec<HeightLevel>,
}

impl World {
    /// Synthetic world from closures (used by renderer tests).
    pub fn synthetic<F, G>(params: BuildParams, voxels: F, hf: G) -> World
    where
        F: Fn(i32, i32) -> Vec<Span> + Sync,
        G: Fn(u32, f64, f64) -> HfCell + Sync,
    {
        let (ox, oz) = params.l0_origin();
        let l0 = Level0::from_fn(ox, oz, params.l0_size, voxels);
        let levels = (1..=params.hf_levels)
            .map(|k| {
                let (lx, lz) = params.level_origin(k);
                HeightLevel::from_fn(k, lx, lz, params.hf_size, |x, z| hf(k, x, z))
            })
            .collect();
        World { seed: 0, params, l0, levels }
    }

    /// Rough memory usage in bytes (for logs).
    pub fn memory_bytes(&self) -> usize {
        let l0 = self.l0.span_count() * std::mem::size_of::<Span>()
            + self.l0.top.len() * (4 + 2 + 1 + 2) * 2;
        let hf: usize = self.levels.iter().map(|l| l.h.len() * (4 + 4 + 12 + 1 + 1) * 2).sum();
        l0 + hf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level0_roundtrip_and_mips() {
        let l0 = Level0::from_fn(-8, -8, 16, |x, z| {
            let h = 10 + (x + z).rem_euclid(5);
            vec![Span::new(0, h, Material::Stone), Span::new(h, h + 1, Material::Grass)]
        });
        assert_eq!(l0.block(0, 5, 0), Material::Stone);
        assert_eq!(l0.block(-8, 14, -8), Material::Grass);
        assert_eq!(l0.block(0, 200, 0), Material::Air);
        assert_eq!(l0.block(100, 5, 0), Material::Air);
        let top_max = *l0.top.iter().max().unwrap();
        assert_eq!(l0.mip.last().unwrap()[0], top_max);
        assert_eq!(l0.mip.len(), 5);
    }

    #[test]
    fn origins_aligned() {
        let p = BuildParams { center_x: 1001, center_z: -777, ..Default::default() };
        for k in 1..=4 {
            let (x, z) = p.level_origin(k);
            assert_eq!(x.rem_euclid(1 << k), 0);
            assert_eq!(z.rem_euclid(1 << k), 0);
        }
    }
}
