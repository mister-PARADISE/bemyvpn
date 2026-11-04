//! Voxel chunk builder + face-culled mesher (API.md "Chunk vertex format").
//!
//! A chunk covers x ∈ [32·cx, 32·cx + 32), z likewise. It is voxelised with a one-column margin
//! (34 × 34 columns) into a dense y-major byte grid spanning only the y range that can hold a
//! visible face, trees are composited exactly like the world builder (order-independent
//! priorities, "first writer wins"), then every interior block emits the faces that touch a
//! non-opaque neighbour:
//! * kind 0 opaque: faces toward air / water / leaves;
//! * kind 1 water: faces toward air / leaves only (no walls inside water, none against ground);
//! * kind 2 leaves: faces toward air / water (leaf–leaf faces culled; draw double-sided).
//!
//! Per vertex: voxel corner AO (opaque + leaves occlude) and a sky-light estimate: the mean
//! "openness" of the 2×2 columns around the vertex at the air-side height (open sky 1, below
//! leaves only 0.55, below rock 0.15) → flat open ground 255, cliff walls ≈ 145, overhang
//! undersides and forest floors darker. Quads are rotated so the AO interpolation diagonal
//! follows the darker corners (no anisotropy). Coplanar +Y faces with identical attributes are
//! greedily merged (per kind, see `merge_mask`; default water + foliage).
use crate::features::{FeatureCache, Run};
use crate::terrain::{deep_fill, Liquids, Nodes, Shape, WH};
use voxel_world::gen::{ColumnInfo, WorldGen};
use voxel_world::materials::{foliage_tint, grass_tint, linear_to_srgb, srgb8, terracotta_band, Material, MATERIAL_COUNT};
use voxel_world::math::Vec3;

pub const CS: i32 = 32;
pub const W: i32 = CS + 2;
const WU: usize = W as usize;
const LAYER: usize = WU * WU;

/// Material classes.
const C_AIR: u8 = 0;
const C_WATER: u8 = 1;
const C_LEAF: u8 = 2;
const C_FULL: u8 = 3;

/// Default +Y merging: water and foliage only. Merged opaque tops would create T-junctions
/// whose sub-pixel rasterisation cracks sparkle on solid ground; under translucent water or
/// alpha-tested leaves they are invisible. `vw_set_merge(7)` enables opaque merging too.
pub const DEFAULT_MERGE: u32 = 0b110;

pub const KIND_OPAQUE: usize = 0;
pub const KIND_WATER: usize = 1;
pub const KIND_FOLIAGE: usize = 2;
/// Cross-quad plants (grass tufts, tall grass, flowers): normal index 6, plant type in flag bits 1..3.
pub const KIND_PLANT: usize = 3;

/// Bytes per vertex / per quad.
pub const VB: usize = 12;
pub const QB: usize = 4 * VB;

/// Normal index order: +X, −X, +Y, −Y, +Z, −Z.
pub const NORMALS: [[i32; 3]; 6] = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];

/// Quad corners (offsets from the block's min corner), CCW seen from outside.
pub const CORNERS: [[[i32; 3]; 4]; 6] = [
    [[1, 0, 0], [1, 1, 0], [1, 1, 1], [1, 0, 1]],
    [[0, 0, 0], [0, 0, 1], [0, 1, 1], [0, 1, 0]],
    [[0, 1, 0], [0, 1, 1], [1, 1, 1], [1, 1, 0]],
    [[0, 0, 0], [1, 0, 0], [1, 0, 1], [0, 0, 1]],
    [[0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]],
    [[0, 0, 0], [0, 1, 0], [1, 1, 0], [1, 0, 0]],
];

#[inline]
fn doff(dx: i32, dy: i32, dz: i32) -> isize {
    dy as isize * LAYER as isize + dz as isize * WU as isize + dx as isize
}

/// Per direction: neighbour offset, and per corner the (side1, side2, corner) offsets relative
/// to the air-side voxel, plus the corner's (x, z) offset for the sky estimate.
struct FaceTab {
    nb: [isize; 6],
    ao: [[[isize; 3]; 4]; 6],
}

fn face_tab() -> FaceTab {
    let mut nb = [0isize; 6];
    let mut ao = [[[0isize; 3]; 4]; 6];
    for d in 0..6 {
        let n = NORMALS[d];
        nb[d] = doff(n[0], n[1], n[2]);
        let axis = (0..3).find(|&a| n[a] != 0).unwrap();
        let (t1, t2) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        for k in 0..4 {
            let c = CORNERS[d][k];
            let s1 = if c[t1] == 1 { 1 } else { -1 };
            let s2 = if c[t2] == 1 { 1 } else { -1 };
            let mut a = [0i32; 3];
            a[t1] = s1;
            let o1 = doff(a[0], a[1], a[2]);
            let mut b = [0i32; 3];
            b[t2] = s2;
            let o2 = doff(b[0], b[1], b[2]);
            let mut cc = [0i32; 3];
            cc[t1] = s1;
            cc[t2] = s2;
            let o3 = doff(cc[0], cc[1], cc[2]);
            ao[d][k] = [o1, o2, o3];
        }
    }
    FaceTab { nb, ao }
}

pub struct ChunkBuilder {
    tab: FaceTab,
    cls: [u8; 256],
    occ: [bool; 256],
    pub merge_mask: u32,
    /// Composite trees/boulders (tests switch this off to inspect bare terrain).
    pub features: bool,
    liquids: Option<(u64, Liquids)>,
    pub nodes: Nodes,
    pub feats: FeatureCache,
    // region
    cols: Vec<ColumnInfo>,
    shapes: Vec<Shape>,
    vox: Vec<u8>,
    yb: i32,
    yt: i32,
    y_lo: Vec<i32>,
    top_nonair: Vec<i32>,
    top_any: Vec<i32>,
    top_opq: Vec<i32>,
    tint_g: Vec<u32>,
    tint_f: Vec<u32>,
    terra: Vec<u32>,
    runs: Vec<Run>,
    band: Vec<bool>,
    mask: Vec<u64>,
    // output
    pub out: [Vec<u8>; 4],
    pub ymin: i32,
    pub ymax: i32,
    /// Blocks per voxel edge: 1 for exact chunks, 2^level for LOD sections.
    scale: i32,
    lod_floor: Vec<i32>,
    lod_leaf: Vec<u16>,
    lod_log: Vec<u16>,
    lod_mat: Vec<u8>,
    /// Exact water surface minus the LOD-quantised one, in blocks (sent in vertex flag bits 1..7).
    lod_wdiff: Vec<i8>,
}

impl Default for ChunkBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn pack_srgb(c: Vec3) -> u32 {
    let q = |v: f32| ((linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32).min(255);
    q(c.x) | (q(c.y) << 8) | (q(c.z) << 16)
}

/// Terracotta: the material table stores the palest band; the tint carries band / palest.
pub fn terracotta_base() -> Vec3 {
    srgb8(210, 178, 161)
}

impl ChunkBuilder {
    pub fn new() -> Self {
        let mut cls = [C_FULL; 256];
        let mut occ = [true; 256];
        cls[Material::Air as usize] = C_AIR;
        occ[Material::Air as usize] = false;
        cls[Material::Water as usize] = C_WATER;
        occ[Material::Water as usize] = false;
        for m in 0..MATERIAL_COUNT {
            if Material::from_u8(m as u8).is_leaves() {
                cls[m] = C_LEAF;
            }
        }
        let base = terracotta_base();
        let terra = (0..WH)
            .map(|y| {
                let b = terracotta_band(y as f32 + 0.5, 0.0);
                pack_srgb(Vec3::new(b.x / base.x, b.y / base.y, b.z / base.z))
            })
            .collect();
        ChunkBuilder {
            tab: face_tab(),
            cls,
            occ,
            merge_mask: DEFAULT_MERGE,
            features: true,
            liquids: None,
            nodes: Nodes::new(),
            feats: FeatureCache::new(),
            cols: vec![ColumnInfo::default(); LAYER],
            shapes: vec![Shape::default(); LAYER],
            vox: Vec::new(),
            yb: 0,
            yt: 0,
            y_lo: vec![0; LAYER],
            top_nonair: vec![0; LAYER],
            top_any: vec![0; LAYER],
            top_opq: vec![0; LAYER],
            tint_g: vec![u32::MAX; LAYER],
            tint_f: vec![u32::MAX; LAYER],
            terra,
            runs: Vec::new(),
            band: vec![false; WH as usize + 2],
            mask: vec![0; (CS * CS) as usize],
            out: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            ymin: 0,
            ymax: 0,
            scale: 1,
            lod_floor: vec![0; LAYER],
            lod_leaf: Vec::new(),
            lod_log: Vec::new(),
            lod_mat: Vec::new(),
            lod_wdiff: vec![0; LAYER],
        }
    }

    /// Forget all memoised generator data (call when the seed changes).
    pub fn reset(&mut self) {
        self.nodes.clear();
        self.feats.clear();
        self.liquids = None;
    }

    #[inline]
    fn vidx(&self, lx: i32, y: i32, lz: i32) -> usize {
        (y - self.yb + 1) as usize * LAYER + lz as usize * WU + lx as usize
    }

    /// Build + mesh chunk (cx, cz). Results in `out`, `ymin`, `ymax`.
    pub fn build(&mut self, gen: &WorldGen, cx: i32, cz: i32) {
        if self.liquids.as_ref().map(|l| l.0) != Some(gen.seed) {
            self.reset();
            self.liquids = Some((gen.seed, Liquids::new(gen.seed)));
        }
        for o in self.out.iter_mut() {
            o.clear();
        }
        self.scale = 1;
        self.lod_wdiff.fill(0);
        let rx0 = cx * CS - 1;
        let rz0 = cz * CS - 1;
        self.nodes.trim(6000);

        // ---- 1. columns at block centres (with margin)
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                self.cols[c] = gen.column((rx0 + lx) as f64 + 0.5, (rz0 + lz) as f64 + 0.5);
                self.tint_g[c] = u32::MAX;
                self.tint_f[c] = u32::MAX;
            }
        }
        // ---- 2. lattice nodes + column shapes
        {
            let cols = &self.cols;
            let lookup = |bx: i32, bz: i32| -> Option<ColumnInfo> {
                let (lx, lz) = (bx - rx0, bz - rz0);
                if lx >= 0 && lz >= 0 && lx < W && lz < W {
                    Some(cols[(lz * W + lx) as usize])
                } else {
                    None
                }
            };
            for lz in 0..W {
                for lx in 0..W {
                    let c = (lz * W + lx) as usize;
                    self.shapes[c] = Shape::new(&mut self.nodes, gen, rx0 + lx, rz0 + lz, &cols[c], &lookup);
                }
            }
        }
        // ---- 3. features touching the region
        self.feats.trim_around(cx, cz);
        self.runs.clear();
        let ftop = if self.features { self.feats.collect_runs(gen, &mut self.nodes, rx0, rz0, W, &mut self.runs) } else { 0 };

        // world trees (ultra-rare colossal landmarks) touching the region
        let wts = crate::worldtree::trees_for(gen, rx0, rz0, W, 1);

        // ---- 4. vertical range
        let mut min_floor = WH;
        let mut yt = 1;
        for s in &self.shapes {
            min_floor = min_floor.min(s.floor);
            yt = yt.max(s.hi).max(s.wl);
        }
        yt = yt.max(ftop).max(crate::worldtree::top(&wts, 1)).min(WH);
        let yb = (min_floor - 1).max(0);
        self.yb = yb;
        self.yt = yt;
        let nl = (yt - yb + 2) as usize;
        self.vox.clear();
        self.vox.resize(nl * LAYER, Material::Air as u8);
        self.vox[..LAYER].fill(Material::Stone as u8); // pad layer y = yb − 1: solid

        // per-column lowest y that may carry a visible face
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                let mut lo = self.shapes[c].floor - 1;
                if lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1 {
                    for nc in [c - 1, c + 1, c - WU, c + WU] {
                        lo = lo.min(self.shapes[nc].floor);
                    }
                }
                self.y_lo[c] = lo.max(yb);
            }
        }

        // ---- 5. voxelise columns
        let liq = &self.liquids.as_ref().unwrap().1;
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                let interior = lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1;
                let s = self.shapes[c];
                let col = self.cols[c];
                let (x, z) = (rx0 + lx, rz0 + lz);
                let nb = (s.hi - s.floor).max(0) as usize;
                s.band(&self.nodes, &mut self.band[..nb]);
                let liquid = liq.liquid(&col);
                let mat_lo = if interior { self.y_lo[c] } else { WH };
                let mut depth = 0i32;
                let mut in_run = false;
                let base = c + LAYER; // layer index for y = yb
                for y in (yb..yt).rev() {
                    let solid = if y < s.floor {
                        true
                    } else if y < s.hi {
                        self.band[(y - s.floor) as usize]
                    } else {
                        false
                    };
                    let m = if solid {
                        let d = if in_run { depth } else { 0 };
                        in_run = true;
                        depth = d + 1;
                        if y >= mat_lo {
                            gen.surface_material(x, y, z, d, &col)
                        } else {
                            deep_fill(y)
                        }
                    } else {
                        in_run = false;
                        depth = 0;
                        if y < s.wl {
                            liquid
                        } else {
                            Material::Air
                        }
                    };
                    self.vox[base + (y - yb) as usize * LAYER] = m as u8;
                }
                // ice on the top liquid block
                if s.wl > yb && s.wl <= yt && liquid == Material::Water {
                    let i = base + (s.wl - 1 - yb) as usize * LAYER;
                    if self.vox[i] == Material::Water as u8 && liq.freezes(gen, x as f64 + 0.5, z as f64 + 0.5, &col) {
                        self.vox[i] = Material::Ice as u8;
                    }
                }
            }
        }

        // ---- 6. composite features (runs sorted by column, decreasing priority)
        self.composite();
        crate::worldtree::stamp(&wts, &mut self.vox, yb, yt, rx0, rz0, W, 1);

        // ---- 7. per-column heights for sky light and meshing ranges
        for c in 0..LAYER {
            let mut tn = yb - 1;
            let mut ta = yb - 1;
            let mut to = yb - 1;
            let mut y = yt - 1;
            while y >= yb {
                let m = self.vox[c + (y - yb + 1) as usize * LAYER];
                let k = self.cls[m as usize];
                if k != C_AIR {
                    if tn < yb {
                        tn = y;
                    }
                    if k != C_WATER && ta < yb {
                        ta = y;
                    }
                    if k == C_FULL {
                        to = y;
                        break;
                    }
                }
                y -= 1;
            }
            self.top_nonair[c] = tn;
            self.top_any[c] = ta.max(to);
            self.top_opq[c] = to;
        }

        // ---- 8. mesh
        self.mesh(gen, rx0, rz0);
        self.plants(gen, rx0, rz0);
    }

    /// Grass tufts, tall grass and flowers on exposed grass blocks (exact chunks only): two
    /// diagonal quads per plant (an "X"), integer corners so the 12-byte format still fits.
    fn plants(&mut self, gen: &WorldGen, rx0: i32, rz0: i32) {
        use voxel_world::gen::Biome;
        use voxel_world::noise::{hash2, hash_f01, mix64};
        let seed = gen.seed ^ 0x9A55_7EED;
        for lz in 1..W - 1 {
            for lx in 1..W - 1 {
                let c = (lz * W + lx) as usize;
                let y = self.top_opq[c];
                if y < self.yb || y + 2 >= self.yt {
                    continue;
                }
                let vi = self.vidx(lx, y, lz);
                if self.vox[vi] != Material::Grass as u8 || self.cls[self.vox[vi + LAYER] as usize] != C_AIR {
                    continue;
                }
                let col = self.cols[c];
                let (dens, flowers) = match col.biome {
                    Biome::Plains | Biome::Meadow => (0.55, 0.06),
                    Biome::Savanna => (0.6, 0.0),
                    Biome::Forest | Biome::BirchForest => (0.4, 0.03),
                    Biome::DarkForest | Biome::Jungle => (0.45, 0.01),
                    Biome::Taiga | Biome::OldGrowthTaiga | Biome::AlpineGrove => (0.25, 0.01),
                    Biome::Swamp => (0.5, 0.0),
                    _ => (0.3, 0.02),
                };
                let (x, z) = (rx0 + lx, rz0 + lz);
                let h = hash2(seed, x, z);
                let r = hash_f01(h);
                if r >= dens {
                    continue;
                }
                let r2 = hash_f01(mix64(h));
                // plant type: 0 short grass, 1 tall grass (2 blocks), 2..7 flowers
                let tall_ok = self.cls[self.vox[vi + 2 * LAYER] as usize] == C_AIR;
                let kind: u8 = if r2 < flowers / dens { 2 + (mix64(h ^ 0x51) % 6) as u8 } else if tall_ok && r2 > 0.86 { 1 } else { 0 };
                let hgt = if kind == 1 { 2 } else { 1 };
                let tint = self.tint(c, Material::Grass, y);
                let sky = (self.sky_open(c, y + 1) * 255.0) as u8;
                let (bx, bz) = ((lx - 1) as u8, (lz - 1) as u8);
                let (y0, y1) = ((y + 1) as u16, (y + 1 + hgt) as u16);
                let out = &mut self.out[KIND_PLANT];
                for (ax, az, bx2, bz2) in [(bx, bz, bx + 1, bz + 1), (bx + 1, bz, bx, bz + 1)] {
                    for (vx, vz, vy) in [(ax, az, y0), (ax, az, y1), (bx2, bz2, y1), (bx2, bz2, y0)] {
                        out.extend_from_slice(&[
                            vx, vz, vy as u8, (vy >> 8) as u8, 6, Material::Grass as u8, 3, kind << 1,
                            tint as u8, (tint >> 8) as u8, (tint >> 16) as u8, sky,
                        ]);
                    }
                }
                self.ymax = self.ymax.max(y + 1 + hgt);
            }
        }
    }

    fn composite(&mut self) {
        let (yb, yt) = (self.yb, self.yt);
        let n = self.runs.len();
        let mut i = 0;
        while i < n {
            let col = self.runs[i].col as usize;
            let mut e = i;
            while e < n && self.runs[e].col as usize == col {
                e += 1;
            }
            let mut ylo = i32::MAX;
            let mut yhi = i32::MIN;
            for r in &self.runs[i..e] {
                let a = (r.y0 as i32).max(yb);
                let b = (r.y1 as i32).min(yt);
                if a >= b {
                    continue;
                }
                ylo = ylo.min(a);
                yhi = yhi.max(b);
                for y in a..b {
                    let vi = col + (y - yb + 1) as usize * LAYER;
                    let cur = self.vox[vi];
                    if cur & 0x80 != 0 {
                        continue; // written by a higher-priority run
                    }
                    if cur == Material::Air as u8 || (r.replace && cur != Material::Bedrock as u8) {
                        self.vox[vi] = r.mat as u8 | 0x80;
                    }
                }
            }
            if ylo < yhi {
                for y in ylo..yhi {
                    self.vox[col + (y - yb + 1) as usize * LAYER] &= 0x7F;
                }
            }
            i = e;
        }
    }

    #[inline]
    fn sky_open(&self, c: usize, ys: i32) -> f32 {
        if ys > self.top_any[c] {
            1.0
        } else if ys > self.top_opq[c] {
            0.55
        } else {
            0.15
        }
    }

    /// Sky light at vertex (vx, vz) (region-local integer corner), air-side height ys.
    #[inline]
    fn sky_at(&self, vx: i32, vz: i32, ys: i32) -> u8 {
        let c = (vz * W + vx) as usize;
        let s = self.sky_open(c, ys) + self.sky_open(c - 1, ys) + self.sky_open(c - WU, ys) + self.sky_open(c - WU - 1, ys);
        (s * (255.0 / 4.0) + 0.5) as u8
    }

    fn tint(&mut self, c: usize, m: Material, y: i32) -> u32 {
        if m.grass_tinted() {
            if self.tint_g[c] == u32::MAX {
                let col = &self.cols[c];
                self.tint_g[c] = pack_srgb(grass_tint(col.surface_temperature, col.climate.humidity));
            }
            self.tint_g[c]
        } else if m.foliage_tinted() {
            if self.tint_f[c] == u32::MAX {
                let col = &self.cols[c];
                self.tint_f[c] = pack_srgb(foliage_tint(col.surface_temperature, col.climate.humidity));
            }
            self.tint_f[c]
        } else if m == Material::Terracotta {
            self.terra[(y * self.scale + self.scale / 2).clamp(0, WH - 1) as usize]
        } else {
            0xFF_FFFF
        }
    }

    fn mesh(&mut self, _gen: &WorldGen, _rx0: i32, _rz0: i32) {
        let (yb, yt) = (self.yb, self.yt);
        let mut ylo_all = i32::MAX;
        let mut yhi_all = i32::MIN;
        for lz in 1..W - 1 {
            for lx in 1..W - 1 {
                let c = (lz * W + lx) as usize;
                ylo_all = ylo_all.min(self.y_lo[c]);
                yhi_all = yhi_all.max(self.top_nonair[c]);
            }
        }
        self.ymin = i32::MAX;
        self.ymax = i32::MIN;
        let mut ylo = vec![0i32; (CS * CS) as usize];
        let mut yhi = vec![0i32; (CS * CS) as usize];
        for lz in 1..W - 1 {
            for lx in 1..W - 1 {
                let c = (lz * W + lx) as usize;
                let k = ((lz - 1) * CS + lx - 1) as usize;
                ylo[k] = self.y_lo[c];
                yhi[k] = self.top_nonair[c];
            }
        }
        let mut y = ylo_all.max(yb);
        let yend = yhi_all.min(yt - 1);
        while y <= yend {
            let mut any_mask = false;
            for lz in 1..W - 1 {
                for lx in 1..W - 1 {
                    let k = ((lz - 1) * CS + lx - 1) as usize;
                    if y < ylo[k] || y > yhi[k] {
                        continue;
                    }
                    let vi = self.vidx(lx, y, lz);
                    let m = self.vox[vi];
                    let cl = self.cls[m as usize];
                    if cl == C_AIR {
                        continue;
                    }
                    for d in 0..6 {
                        let nbm = self.vox[(vi as isize + self.tab.nb[d]) as usize];
                        let ncl = self.cls[nbm as usize];
                        let vis = match cl {
                            C_FULL => ncl != C_FULL,
                            C_LEAF => ncl == C_AIR || ncl == C_WATER,
                            _ => ncl == C_AIR || ncl == C_LEAF,
                        };
                        if !vis {
                            continue;
                        }
                        if self.face(lx, y, lz, vi, m, cl, d) {
                            any_mask = true;
                        }
                    }
                }
            }
            if any_mask {
                self.flush_mask(y);
            }
            y += 1;
        }
        if self.ymin > self.ymax {
            self.ymin = yb;
            self.ymax = yb;
        }
    }

    /// Emit (or queue for merging) one face. Returns true when queued into the +Y mask.
    #[allow(clippy::too_many_arguments)]
    fn face(&mut self, lx: i32, y: i32, lz: i32, vi: usize, m: u8, cl: u8, d: usize) -> bool {
        let air = (vi as isize + self.tab.nb[d]) as usize;
        let mut ao = [3u8; 4];
        for k in 0..4 {
            let [o1, o2, o3] = self.tab.ao[d][k];
            let s1 = self.occ[self.vox[(air as isize + o1) as usize] as usize];
            let s2 = self.occ[self.vox[(air as isize + o2) as usize] as usize];
            let s3 = self.occ[self.vox[(air as isize + o3) as usize] as usize];
            ao[k] = if s1 && s2 { 0 } else { 3 - (s1 as u8 + s2 as u8 + s3 as u8) };
        }
        let ys = match d {
            2 => y + 1,
            3 => y - 1,
            _ => y,
        };
        let mut sky = [0u8; 4];
        for k in 0..4 {
            let cc = CORNERS[d][k];
            sky[k] = self.sky_at(lx + cc[0], lz + cc[2], ys);
        }
        let c = (lz * W + lx) as usize;
        let mat = Material::from_u8(m);
        let tint = self.tint(c, mat, y);
        let top_exposed = self.cls[self.vox[vi + LAYER] as usize] != C_FULL;
        let mut flags = top_exposed as u8;
        if cl == C_WATER && d == 2 {
            flags |= ((self.lod_wdiff[c] as u8) & 0x7F) << 1;
        }
        let kind = match cl {
            C_WATER => KIND_WATER,
            C_LEAF => KIND_FOLIAGE,
            _ => KIND_OPAQUE,
        };
        if d == 2 && self.merge_mask & (1 << kind) != 0 && ao.iter().all(|&a| a == ao[0]) && sky.iter().all(|&s| s == sky[0]) {
            let key = (1u64 << 63)
                | m as u64
                | (kind as u64) << 8
                | (ao[0] as u64) << 10
                | (sky[0] as u64) << 12
                | (tint as u64) << 20
                | (flags as u64) << 44;
            self.mask[((lz - 1) * CS + lx - 1) as usize] = key;
            return true;
        }
        // AO-aware diagonal: connect the darker pair
        let b = |k: usize| ao[k] as u32 * 256 + sky[k] as u32;
        let rot = if b(0) + b(2) > b(1) + b(3) { 1 } else { 0 };
        let out = &mut self.out[kind];
        let (bx, bz) = (lx - 1, lz - 1);
        for i in 0..4 {
            let k = (i + rot) & 3;
            let cc = CORNERS[d][k];
            let vy = (y + cc[1]) as u16;
            out.extend_from_slice(&[
                (bx + cc[0]) as u8,
                (bz + cc[2]) as u8,
                vy as u8,
                (vy >> 8) as u8,
                d as u8,
                m,
                ao[k],
                flags,
                tint as u8,
                (tint >> 8) as u8,
                (tint >> 16) as u8,
                sky[k],
            ]);
        }
        self.ymin = self.ymin.min(y);
        self.ymax = self.ymax.max(y + 1);
        false
    }

    /// Greedy-merge the queued +Y faces of layer y.
    fn flush_mask(&mut self, y: i32) {
        let n = CS as usize;
        let yy = (y + 1) as u16;
        for z in 0..n {
            let mut x = 0;
            while x < n {
                let key = self.mask[z * n + x];
                if key == 0 {
                    x += 1;
                    continue;
                }
                let mut w = 1;
                while x + w < n && self.mask[z * n + x + w] == key {
                    w += 1;
                }
                let mut h = 1;
                'grow: while z + h < n {
                    for i in 0..w {
                        if self.mask[(z + h) * n + x + i] != key {
                            break 'grow;
                        }
                    }
                    h += 1;
                }
                for dz in 0..h {
                    self.mask[(z + dz) * n + x..(z + dz) * n + x + w].fill(0);
                }
                let m = key as u8;
                let kind = ((key >> 8) & 3) as usize;
                let ao = ((key >> 10) & 3) as u8;
                let sky = ((key >> 12) & 0xFF) as u8;
                let tint = ((key >> 20) & 0xFF_FFFF) as u32;
                let flags = ((key >> 44) & 0xFF) as u8;
                let (x0, z0, x1, z1) = (x as u8, z as u8, (x + w) as u8, (z + h) as u8);
                let out = &mut self.out[kind];
                for (vx, vz) in [(x0, z0), (x0, z1), (x1, z1), (x1, z0)] {
                    out.extend_from_slice(&[
                        vx,
                        vz,
                        yy as u8,
                        (yy >> 8) as u8,
                        2,
                        m,
                        ao,
                        flags,
                        tint as u8,
                        (tint >> 8) as u8,
                        (tint >> 16) as u8,
                        sky,
                    ]);
                }
                self.ymin = self.ymin.min(y);
                self.ymax = self.ymax.max(y + 1);
                x += w;
            }
        }
    }

    /// Material at a world block inside the last built chunk's region (tests / debugging).
    pub fn block_at(&self, cx: i32, cz: i32, x: i32, y: i32, z: i32) -> Option<Material> {
        let (lx, lz) = (x - (cx * CS - 1), z - (cz * CS - 1));
        if lx < 0 || lz < 0 || lx >= W || lz >= W {
            return None;
        }
        if y < self.yb {
            return Some(Material::Stone);
        }
        if y >= self.yt {
            return Some(Material::Air);
        }
        Some(Material::from_u8(self.vox[self.vidx(lx, y, lz)]))
    }

    pub fn quad_count(&self, kind: usize) -> usize {
        self.out[kind].len() / QB
    }
}

// =====================================================================================
// LOD sections (Voxy-style): the same generator sampled at 2^level blocks per voxel, the same
// mesher and vertex format (positions in LOD-voxel units; the viewer scales by 2^level).
// =====================================================================================
impl ChunkBuilder {
    /// Build + mesh LOD section (level ≥ 1) covering LOD cells [32·sx, 32·sx + 32) (likewise z),
    /// i.e. blocks [32·sx·2^level, 32·(sx+1)·2^level).
    pub fn build_lod(&mut self, gen: &WorldGen, level: u32, sx: i32, sz: i32) {
        use voxel_world::gen::MAX_FEATURE_RADIUS;
        use voxel_world::noise::{hash2, hash_f01};
        if self.liquids.as_ref().map(|l| l.0) != Some(gen.seed) {
            self.reset();
            self.liquids = Some((gen.seed, Liquids::new(gen.seed)));
        }
        for o in self.out.iter_mut() {
            o.clear();
        }
        let s = 1i32 << level.min(10);
        self.scale = s;
        let sf = s as f64;
        let nly = WH / s;
        let rx0 = sx * CS - 1;
        let rz0 = sz * CS - 1;

        // ---- 1. columns at LOD cell centres (band-limited to the cell footprint)
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                let wx = ((rx0 + lx) as f64 + 0.5) * sf;
                let wz = ((rz0 + lz) as f64 + 0.5) * sf;
                self.cols[c] = gen.column_lod(wx, wz, sf);
                self.tint_g[c] = u32::MAX;
                self.tint_f[c] = u32::MAX;
            }
        }
        // ---- 2. vertical profile per column (in LOD voxels)
        //   below `floor` solid; [floor, hi) evaluated (3D density at voxel centres); ≥ hi air
        let mut yb = nly;
        let mut yt = 1;
        let mut lo_hi = vec![(0i32, 0i32, 0i32); LAYER];
        for c in 0..LAYER {
            let col = self.cols[c];
            let jag = if s <= 8 { col.jaggedness.max(0.0) } else { 0.0 };
            let lo = (((col.height - jag - 1.0) / s as f32).floor() as i32).clamp(1, nly);
            let hi = (((col.height + jag + 1.0) / s as f32).ceil() as i32).clamp(lo, nly);
            let wl = if col.water_level == i32::MIN { 0 } else { ((col.water_level + s / 2) / s).clamp(0, nly) };
            self.lod_wdiff[c] = if col.water_level == i32::MIN { 0 } else { (col.water_level - wl * s).clamp(-64, 63) as i8 };
            lo_hi[c] = (lo, hi, wl);
            self.lod_floor[c] = lo;
            yb = yb.min(lo - 1);
            yt = yt.max(hi).max(wl);
        }
        // ---- 3. trees: exact features rasterised for small voxels, canopy statistics beyond
        let feat_detail = s <= 4;
        let mut ftop = 0;
        if feat_detail {
            let n = LAYER * nly as usize;
            self.lod_leaf.clear();
            self.lod_leaf.resize(n, 0);
            self.lod_log.clear();
            self.lod_log.resize(n, 0);
            self.lod_mat.clear();
            self.lod_mat.resize(n * 2, 0);
            let (bx0, bz0) = (rx0 * s, rz0 * s);
            let (bx1, bz1) = ((rx0 + W) * s, (rz0 + W) * s);
            let r = MAX_FEATURE_RADIUS;
            for f in gen.features_in(bx0 - r, bz0 - r, bx1 + r, bz1 + r) {
                let root = gen.column(f.root_x as f64 + 0.5, f.root_z as f64 + 0.5);
                if root.water_level != i32::MIN && root.water_level as f32 > root.height + 0.5 {
                    continue;
                }
                let ground = ((root.height - 0.5).ceil() as i32).clamp(1, WH - 1);
                for fc in &f.columns {
                    let bx = f.root_x + fc.dx as i32;
                    let bz = f.root_z + fc.dz as i32;
                    let lx = bx.div_euclid(s) - rx0;
                    let lz = bz.div_euclid(s) - rz0;
                    if lx < 0 || lz < 0 || lx >= W || lz >= W {
                        continue;
                    }
                    let c = (lz * W + lx) as usize;
                    let is_log = fc.mat.is_log();
                    let is_leaf = fc.mat.is_leaves();
                    if !is_log && !is_leaf {
                        continue;
                    }
                    for y in (ground + fc.y0 as i32).max(0)..(ground + fc.y1 as i32).min(WH) {
                        let j = (y / s) as usize;
                        let i = j * LAYER + c;
                        if is_log {
                            self.lod_log[i] = self.lod_log[i].saturating_add(1);
                            self.lod_mat[i * 2] = fc.mat as u8;
                        } else {
                            self.lod_leaf[i] = self.lod_leaf[i].saturating_add(1);
                            self.lod_mat[i * 2 + 1] = fc.mat as u8;
                        }
                        ftop = ftop.max(j as i32 + 1);
                    }
                }
            }
        }
        let canopy_seed = gen.seed ^ 0xC4A0_9E37;
        if !feat_detail {
            ftop = 0;
            for c in 0..LAYER {
                let (_, hi, _) = lo_hi[c];
                ftop = ftop.max(hi + 4);
            }
        }
        let wts = crate::worldtree::trees_for(gen, rx0, rz0, W, s);
        ftop = ftop.max(crate::worldtree::top(&wts, s));
        yt = yt.max(ftop).min(nly) + 1;
        let yb = yb.max(0);
        let yt = yt.min(nly);
        self.yb = yb;
        self.yt = yt;
        let nl = (yt - yb + 2) as usize;
        self.vox.clear();
        self.vox.resize(nl * LAYER, Material::Air as u8);
        self.vox[..LAYER].fill(Material::Stone as u8);

        for c in 0..LAYER {
            let lx = c as i32 % W;
            let lz = c as i32 / W;
            let mut lo = self.lod_floor[c] - 1;
            if lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1 {
                for nc in [c - 1, c + 1, c - WU, c + WU] {
                    lo = lo.min(self.lod_floor[nc] - 1);
                }
            }
            self.y_lo[c] = lo.max(yb);
        }

        // ---- 4. voxelise
        let liq = &self.liquids.as_ref().unwrap().1;
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                let interior = lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1;
                let col = self.cols[c];
                let (lo, hi, wl) = lo_hi[c];
                let bx = (rx0 + lx) * s + s / 2;
                let bz = (rz0 + lz) * s + s / 2;
                let wx = bx as f64 + 0.5 * ((s & 1) as f64);
                let wz = bz as f64 + 0.5 * ((s & 1) as f64);
                let liquid = liq.liquid(&col);
                let base = c + LAYER;
                let mut depth_vox = 0i32;
                let mut in_run = false;
                let mat_lo = if interior { self.y_lo[c] } else { nly };
                for j in (yb..yt).rev() {
                    let yc = j * s + s / 2;
                    let solid = if j < lo {
                        true
                    } else if j >= hi {
                        false
                    } else if col.jaggedness > 0.0 && s <= 8 {
                        gen.density(wx, yc as f64 + 0.5, wz, &col) > 0.0
                    } else {
                        (yc as f32 + 0.5) < col.height
                    };
                    let m = if solid {
                        let d = if in_run { depth_vox } else { 0 };
                        in_run = true;
                        depth_vox = d + 1;
                        if j >= mat_lo {
                            // representative block: the top block of the voxel for the exposed face
                            let by = if d == 0 { j * s + s - 1 } else { yc };
                            gen.surface_material(bx, by.min(WH - 1), bz, d * s, &col)
                        } else {
                            deep_fill(yc)
                        }
                    } else {
                        in_run = false;
                        depth_vox = 0;
                        if j < wl {
                            liquid
                        } else {
                            Material::Air
                        }
                    };
                    let mut mm = m;
                    if mm == Material::Air && feat_detail {
                        let i = j as usize * LAYER + c;
                        let thr_leaf = (s * s / 2).max(1) as u16;
                        let thr_log = s.max(1) as u16;
                        if self.lod_log[i] >= thr_log {
                            mm = Material::from_u8(self.lod_mat[i * 2]);
                        } else if self.lod_leaf[i] >= thr_leaf {
                            mm = Material::from_u8(self.lod_mat[i * 2 + 1]);
                        }
                    }
                    self.vox[base + (j - yb) as usize * LAYER] = mm as u8;
                }
                // statistical canopy for big voxels: dithered blocky tree tops
                if !feat_detail && (wl as f32) * (s as f32) < col.height + 0.5 {
                    let cp = gen.canopy_lod(wx, wz, &col, sf);
                    if cp.coverage > 0.05 && cp.leaf != Material::Air {
                        let hsh = hash_f01(hash2(canopy_seed ^ level as u64, rx0 + lx, rz0 + lz));
                        if hsh < cp.coverage * 1.15 {
                            let top = ((col.height - 0.5) / s as f32).ceil() as i32;
                            let layers = ((cp.height / s as f32) * (0.6 + 0.8 * hsh)).round().max(1.0) as i32;
                            for k in 0..layers {
                                let j = top + k;
                                if j >= yb && j < yt {
                                    let i = base + (j - yb) as usize * LAYER;
                                    if self.vox[i] == Material::Air as u8 {
                                        self.vox[i] = cp.leaf as u8;
                                    }
                                }
                            }
                        }
                    }
                }
                // ice on the top liquid voxel
                if wl > yb && wl <= yt && liquid == Material::Water {
                    let i = base + (wl - 1 - yb) as usize * LAYER;
                    if self.vox[i] == Material::Water as u8 && liq.freezes(gen, wx, wz, &col) {
                        self.vox[i] = Material::Ice as u8;
                    }
                }
            }
        }
        crate::worldtree::stamp(&wts, &mut self.vox, yb, yt, rx0, rz0, W, s);
        // ---- 5. skirts: opaque margin voxels count as air so section borders always get walls
        //        (closes cracks against neighbours drawn at a different level)
        for lz in 0..W {
            for lx in 0..W {
                if lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1 {
                    continue;
                }
                let c = (lz * W + lx) as usize;
                for l in (1..nl).rev() {
                    let i = c + l * LAYER;
                    let k = self.cls[self.vox[i] as usize];
                    if k == C_FULL {
                        self.vox[i] = Material::Air as u8;
                    }
                }
            }
        }
        // ---- 6. heights for sky light / meshing ranges (same as exact chunks)
        for c in 0..LAYER {
            let mut tn = yb - 1;
            let mut ta = yb - 1;
            let mut to = yb - 1;
            let mut y = yt - 1;
            while y >= yb {
                let m = self.vox[c + (y - yb + 1) as usize * LAYER];
                let k = self.cls[m as usize];
                if k != C_AIR {
                    if tn < yb {
                        tn = y;
                    }
                    if k != C_WATER && ta < yb {
                        ta = y;
                    }
                    if k == C_FULL {
                        to = y;
                        break;
                    }
                }
                y -= 1;
            }
            self.top_nonair[c] = tn;
            self.top_any[c] = ta.max(to);
            self.top_opq[c] = to;
        }
        self.mesh(gen, rx0, rz0);
    }
}
