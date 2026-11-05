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
//! Caves (exact chunks only, see `terrain.rs`): the cave lattice `F̂` of every column is blended
//! from memoised node columns before voxelisation (only at levels where some corner node is
//! positive, ± 1); the dense grid reaches down to the lowest block with `F̂ > 0` in the region;
//! carved solid blocks take `cave_fill` (air / aquifer water / lava). Below the terrain surface
//! band the voxel pass writes the cheap deep fill, then every such block next to a carved one
//! (cave wall, floor, ceiling) gets its real `surface_material`. Each column is meshed
//! over two y intervals: the surface band and the carved range of itself and its 4 neighbours
//! (± 1), so solid rock between a cave and the surface is never scanned and no face is emitted
//! inside it. Layers are meshed top-down: seen from above, a chunk's surface faces come first in
//! its buffers and its cave faces fail the early depth test.
//!
//! Per vertex: voxel corner AO (opaque + leaves occlude) and a sky-light estimate: the mean
//! "openness" of the 2×2 columns around the vertex at the air-side height (open sky 1, below
//! leaves only 0.55, below rock 0.15 fading to 0 between 4 and 16 blocks under the column's top
//! opaque block) → flat open ground 255, cliff walls ≈ 145, overhang undersides and forest
//! floors darker, cave interiors 0 (the viewer adds a faint cave ambient and a headlamp).
//! Quads are rotated so the AO interpolation diagonal follows the darker corners (no
//! anisotropy). Coplanar +Y faces with identical attributes are greedily merged (per kind, see
//! `merge_mask`; default water + foliage).
use crate::features::{FeatureCache, Run};
use crate::terrain::{carves, deep_fill, Liquids, Nodes, Shape, CAVE_OUT, LAT, MW, WET_GUARD, WH};
use voxel_world::gen::caves::{LAVA_LEVEL, MIN_CARVE_Y};
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
    /// Carve caves in exact chunks (tests switch this off to compare with the bare heightfield).
    pub caves: bool,
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
    // caves: per column bilinear lattice field for levels [cave_j0, cave_j0 + cave_n) at
    // `cpool[cave_off..]`; lowest y with F̂ > 0 (bound, WH = none); carved y range after
    // voxelisation (cav_lo > cav_hi = none); lowest y written per column
    cave_j0: Vec<i32>,
    cave_n: Vec<i32>,
    cave_off: Vec<u32>,
    cpool: Vec<f32>,
    cave_lo_b: Vec<i32>,
    cav_lo: Vec<i32>,
    cav_hi: Vec<i32>,
    wlo: Vec<i32>,
    /// Run depth (saturated) of every solid block below the surface band (`vox` layout).
    dep: Vec<u8>,
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

/// The materials `deep_fill` writes (invisible rock placeholders below the surface band).
#[inline]
fn is_deep_fill(m: u8) -> bool {
    m == Material::Stone as u8 || m == Material::Deepslate as u8 || m == Material::Bedrock as u8
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
            caves: true,
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
            cave_j0: vec![0; LAYER],
            cave_n: vec![0; LAYER],
            cave_off: vec![0; LAYER],
            cpool: Vec::new(),
            cave_lo_b: vec![WH; LAYER],
            cav_lo: vec![WH; LAYER],
            cav_hi: vec![-1; LAYER],
            wlo: vec![0; LAYER],
            dep: Vec::new(),
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
        // ---- 2b. cave lattice: per column F̂ at its lattice levels + lowest carvable y
        let cave_min = self.cave_lattice(gen);

        // ---- 3. features touching the region
        self.feats.trim_around(cx, cz);
        self.runs.clear();
        let ftop = if self.features { self.feats.collect_runs(gen, &mut self.nodes, rx0, rz0, W, self.caves, &mut self.runs) } else { 0 };

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
        let yb = (min_floor - 1).min(cave_min - 1).max(0);
        self.yb = yb;
        self.yt = yt;
        let nl = (yt - yb + 2) as usize;
        self.vox.clear();
        // solid by default: below its lowest written y (`wlo`) every column is uncarved rock,
        // so its class is right without touching it (this includes the pad layer y = yb − 1);
        // the top layer y = yt is air
        self.vox.resize(nl * LAYER, Material::Stone as u8);
        self.vox[(nl - 1) * LAYER..].fill(Material::Air as u8);
        if self.dep.len() < nl * LAYER {
            self.dep.resize(nl * LAYER, 0);
        }

        // per-column lowest y that may carry a visible terrain-surface face, and lowest y written
        // (which also covers the block under the lowest carvable one and the carvable range of
        // the 4 neighbours, whose cave walls this column's blocks form)
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                let interior = lx > 0 && lz > 0 && lx < W - 1 && lz < W - 1;
                let mut lo = self.shapes[c].floor - 1;
                let mut wl = self.cave_lo_b[c] - 1;
                if interior {
                    for nc in [c - 1, c + 1, c - WU, c + WU] {
                        lo = lo.min(self.shapes[nc].floor);
                        wl = wl.min(self.cave_lo_b[nc]);
                    }
                }
                self.y_lo[c] = lo.max(yb);
                self.wlo[c] = wl.min(lo).max(yb);
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
                // caves: this column's lattice levels; no carving within WET_GUARD blocks of the
                // top of a water-covered column (set at its top solid block)
                let (cj0, cn, coff) = (self.cave_j0[c], self.cave_n[c], self.cave_off[c] as usize);
                let wet = col.water_level != i32::MIN && col.water_level as f32 > col.height;
                let mut carve_max = WH;
                let mut seen_solid = false;
                let (mut clo, mut chi) = (WH, -1);
                // `cave_fill` depends on (x, z) only through the aquifer cell: lava at
                // y ≤ LAVA_LEVEL, water below the cell's table (≤ AQ_MAX_LEVEL), else air — one
                // table lookup per column (test `caves_carved_and_point_queries_agree` checks
                // this against `WorldGen::cave_fill`)
                let mut table: Option<Option<f32>> = None;
                for y in (self.wlo[c]..yt).rev() {
                    let solid = if y < s.floor {
                        true
                    } else if y < s.hi {
                        self.band[(y - s.floor) as usize]
                    } else {
                        false
                    };
                    let m = if solid {
                        if !seen_solid {
                            seen_solid = true;
                            if wet {
                                carve_max = y + 1 - WET_GUARD;
                            }
                        }
                        // lattice cell of y: levels j, j + 1 (both inside the column's range
                        // whenever the cell can carve, see `cave_lattice`)
                        let j = y.div_euclid(LAT);
                        let k = j - cj0;
                        let (f0, f1) = if k >= 0 && k + 1 < cn {
                            (self.cpool[coff + k as usize], self.cpool[coff + k as usize + 1])
                        } else {
                            (CAVE_OUT, CAVE_OUT)
                        };
                        let fy = (y - j * LAT) as f32 / LAT as f32;
                        if y >= MIN_CARVE_Y && y < carve_max && carves(f0, f1, fy) {
                            in_run = false;
                            depth = 0;
                            clo = y;
                            chi = chi.max(y);
                            if y <= LAVA_LEVEL {
                                Material::Lava
                            } else {
                                let t = *table.get_or_insert_with(|| gen.caves.water_table(x as f64 + 0.5, z as f64 + 0.5));
                                match t {
                                    Some(l) if (y as f32) < l => Material::Water,
                                    _ => Material::Air,
                                }
                            }
                        } else {
                            let d = if in_run { depth } else { 0 };
                            in_run = true;
                            depth = d + 1;
                            if y >= mat_lo {
                                gen.surface_material(x, y, z, d, &col)
                            } else {
                                // deep rock; the cave walls among it get their real material in
                                // step 5b (run depth kept in `dep`; every rule of
                                // `surface_material` saturates far below 127)
                                self.dep[base + (y - yb) as usize * LAYER] = d.min(127) as u8;
                                deep_fill(y)
                            }
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
                self.cav_lo[c] = clo;
                self.cav_hi[c] = chi;
                // ice on the top liquid block
                if s.wl > yb && s.wl <= yt && liquid == Material::Water {
                    let i = base + (s.wl - 1 - yb) as usize * LAYER;
                    if self.vox[i] == Material::Water as u8 && liq.freezes(gen, x as f64 + 0.5, z as f64 + 0.5, &col) {
                        self.vox[i] = Material::Ice as u8;
                    }
                }
            }
        }

        // ---- 5b. cave walls: deep-fill rock next to a cavity gets its real material
        if self.caves {
            self.cave_walls(gen, rx0, rz0);
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

    /// Step 2b: cave lattice of the region. Evaluates (memoised) every corner node's cave column,
    /// blends each column's levels [ja, jb] (ja = lowest evaluated corner level, jb = the level
    /// above the cell of the column's highest possibly-solid block `hi − 1`) into `cpool` and
    /// finds the lowest y with `F̂ > 0` (`cave_lo_b`, a bound for the dense grid; carving also
    /// needs a solid block below the wet guard). Returns the region minimum (WH = no caves).
    fn cave_lattice(&mut self, gen: &WorldGen) -> i32 {
        self.cpool.clear();
        if !self.caves {
            self.cave_n.fill(0);
            self.cave_lo_b.fill(WH);
            return WH;
        }
        for c in 0..LAYER {
            for k in 0..4 {
                if self.shapes[c].w[k] != 0.0 {
                    self.nodes.ensure_cave(gen, self.shapes[c].nodes[k]);
                }
            }
        }
        let mut cmin = WH;
        for c in 0..LAYER {
            let s = self.shapes[c];
            self.cave_lo_b[c] = WH;
            self.cave_n[c] = 0;
            // levels where some corner node is > 0, dilated by one level: both ends of every
            // cell that can carve. Any other level is stored as CAVE_OUT (its cells have all
            // corner values ≤ 0 at both ends, so they cannot carve either way).
            let mut m = [0u64; MW];
            let mut ja = WH;
            for k in 0..4 {
                if s.w[k] != 0.0 {
                    let cc = &self.nodes.cave[s.nodes[k] as usize];
                    for (mw, pw) in m.iter_mut().zip(cc.pos.iter()) {
                        *mw |= pw;
                    }
                    ja = ja.min(cc.j0);
                }
            }
            if m.iter().all(|&w| w == 0) {
                continue;
            }
            let mut need = m;
            for w in 0..MW {
                need[w] |= (m[w] << 1) | (m[w] >> 1);
                if w > 0 {
                    need[w] |= m[w - 1] >> 63;
                }
                if w + 1 < MW {
                    need[w] |= m[w + 1] << 63;
                }
            }
            let ja = ja.max(MIN_CARVE_Y / LAT);
            let jb = (s.hi - 1).div_euclid(LAT) + 1;
            self.cave_j0[c] = ja;
            if jb <= ja {
                continue;
            }
            let off = self.cpool.len();
            self.cave_off[c] = off as u32;
            self.cave_n[c] = jb - ja + 1;
            for j in ja..=jb {
                let v = if (need[j as usize >> 6] >> (j & 63)) & 1 != 0 { self.nodes.bcave(gen, &s.nodes, &s.w, j) } else { CAVE_OUT };
                self.cpool.push(v);
            }
            // (cells below ja cannot carve: their lower level is CAVE_OUT = −64 and F ≤ ≈ 15)
            'find: for j in ja..jb {
                let k = off + (j - ja) as usize;
                let (f0, f1) = (self.cpool[k], self.cpool[k + 1]);
                if f0 <= 0.0 && f1 <= 0.0 {
                    continue;
                }
                for y in (j * LAT).max(MIN_CARVE_Y)..((j + 1) * LAT).min(s.hi) {
                    if carves(f0, f1, (y - j * LAT) as f32 / LAT as f32) {
                        self.cave_lo_b[c] = y;
                        break 'find;
                    }
                }
            }
            cmin = cmin.min(self.cave_lo_b[c]);
        }
        cmin
    }

    /// Step 5b: below its surface band (`y_lo`) an interior column holds only deep-fill rock and
    /// cave fill. Every carved block marks its deep-fill 6-neighbours (bit 7 of `dep`) — exactly
    /// the rock that can show a face toward a cavity (wall, floor, ceiling) — then each column is
    /// resolved bottom-up with `surface_material` and the run depth kept in `dep` (column order
    /// keeps the generator's per-column geology cache warm).
    fn cave_walls(&mut self, gen: &WorldGen, rx0: i32, rz0: i32) {
        let yb = self.yb;
        // 1. mark from every carved / non-opaque block of every column (margins expose too)
        for lz in 0..W {
            for lx in 0..W {
                let c = (lz * W + lx) as usize;
                if self.cav_lo[c] > self.cav_hi[c] {
                    continue;
                }
                for y in self.cav_lo[c]..=self.cav_hi[c] {
                    let vi = self.vidx(lx, y, lz);
                    let m = self.vox[vi];
                    if self.cls[m as usize] == C_FULL && m != Material::Lava as u8 {
                        continue;
                    }
                    for (d, &o) in self.tab.nb.iter().enumerate() {
                        let n = NORMALS[d];
                        let (nx, ny, nz) = (lx + n[0], y + n[1], lz + n[2]);
                        if nx < 1 || nz < 1 || nx > W - 2 || nz > W - 2 || ny < yb {
                            continue;
                        }
                        let nc = (nz * W + nx) as usize;
                        let ni = (vi as isize + o) as usize;
                        if ny < self.y_lo[nc] && ny >= self.wlo[nc] && is_deep_fill(self.vox[ni]) {
                            self.dep[ni] |= 0x80;
                        }
                    }
                }
            }
        }
        // 2. resolve the marked blocks column by column
        for lz in 1..W - 1 {
            for lx in 1..W - 1 {
                let c = (lz * W + lx) as usize;
                let (mut a, mut b) = (WH, -1);
                for cc in [c, c - 1, c + 1, c - WU, c + WU] {
                    if self.cav_lo[cc] <= self.cav_hi[cc] {
                        a = a.min(self.cav_lo[cc]);
                        b = b.max(self.cav_hi[cc]);
                    }
                }
                let a = (a - 1).max(self.wlo[c]);
                let b = (b + 1).min(self.y_lo[c] - 1);
                if a > b {
                    continue;
                }
                let col = self.cols[c];
                let (x, z) = (rx0 + lx, rz0 + lz);
                for y in a..=b {
                    let vi = self.vidx(lx, y, lz);
                    let dp = self.dep[vi];
                    if dp & 0x80 != 0 && is_deep_fill(self.vox[vi]) {
                        self.vox[vi] = gen.surface_material(x, y, z, (dp & 0x7F) as i32, &col) as u8;
                    }
                }
            }
        }
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
            // under rock: overhang undersides 0.15, fading to 0 from 4 to 16 blocks below the
            // column's top opaque block (cave interiors)
            let t = ((self.top_opq[c] - ys) * self.scale) as f32;
            0.15 * ((16.0 - t) * (1.0 / 12.0)).clamp(0.0, 1.0)
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
        self.ymin = i32::MAX;
        self.ymax = i32::MIN;
        // per column two y intervals: the terrain surface band [y_lo, top_nonair] and the carved
        // range of the column and its 4 neighbours, ± 1 (cave floors, ceilings and side walls)
        let n2 = (CS * CS) as usize;
        let (mut ylo, mut yhi) = (vec![0i32; n2], vec![0i32; n2]);
        let (mut clo, mut chi) = (vec![WH; n2], vec![-1i32; n2]);
        for lz in 1..W - 1 {
            for lx in 1..W - 1 {
                let c = (lz * W + lx) as usize;
                let k = ((lz - 1) * CS + lx - 1) as usize;
                ylo[k] = self.y_lo[c];
                yhi[k] = self.top_nonair[c];
                ylo_all = ylo_all.min(ylo[k]);
                yhi_all = yhi_all.max(yhi[k]);
                let (mut a, mut b) = (WH, -1);
                for cc in [c, c - 1, c + 1, c - WU, c + WU] {
                    if self.cav_lo[cc] <= self.cav_hi[cc] {
                        a = a.min(self.cav_lo[cc]);
                        b = b.max(self.cav_hi[cc]);
                    }
                }
                if a <= b {
                    clo[k] = (a - 1).max(yb);
                    chi[k] = b + 1;
                    ylo_all = ylo_all.min(clo[k]);
                    yhi_all = yhi_all.max(chi[k]);
                }
            }
        }
        // top-down: surface faces come first in the buffers, so seen from above the cave faces of
        // the same chunk fail the early depth test instead of being shaded and overdrawn
        let ystart = ylo_all.max(yb);
        let mut y = yhi_all.min(yt - 1);
        while y >= ystart {
            let mut any_mask = false;
            for lz in 1..W - 1 {
                for lx in 1..W - 1 {
                    let k = ((lz - 1) * CS + lx - 1) as usize;
                    if (y < ylo[k] || y > yhi[k]) && (y < clo[k] || y > chi[k]) {
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
            y -= 1;
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
        let mut tint = tint;
        if cl == C_WATER && d == 2 {
            flags |= ((self.lod_wdiff[c] as u8) & 0x7F) << 1;
            // water depth below this surface (blocks, bucketed so open water still merges) in the
            // tint's red byte: the shader fades the bottom out with depth (no sand halos)
            // (LOD: + half a voxel — band-limited coasts smear into wide flats a few blocks deep,
            // which must still read as water from afar, not as a sand halo)
            let depth = ((y - self.top_opq[c]) * self.scale + self.lod_wdiff[c] as i32 + self.scale / 2).max(1);
            let q = match depth {
                0..=5 => depth,
                6..=7 => 6,
                8..=11 => 8,
                12..=15 => 12,
                16..=23 => 16,
                24..=31 => 24,
                32..=47 => 32,
                _ => 64,
            };
            tint = 0xFF_FF00 | q as u32;
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

    /// Collision grid of the last exact chunk (`build`): `[yb: i32 LE, ny: i32 LE]` followed by
    /// ny·32·32 class bytes, y-major then z then x over the chunk's own 32×32 columns
    /// (0 air / plants, 1 solid — rock, logs, leaves, ice —, 2 water, 3 lava). Rows below `yb`
    /// are solid, rows from `yb + ny` up are air.
    pub fn coll_grid(&self, out: &mut Vec<u8>) {
        out.clear();
        let ny = (self.yt - self.yb).max(0);
        out.extend_from_slice(&self.yb.to_le_bytes());
        out.extend_from_slice(&ny.to_le_bytes());
        out.reserve((ny * CS * CS) as usize);
        let lava = Material::Lava as u8;
        for y in self.yb..self.yb + ny {
            let base = (y - self.yb + 1) as usize * LAYER;
            for lz in 1..=CS as usize {
                for lx in 1..=CS as usize {
                    let m = self.vox[base + lz * WU + lx];
                    out.push(if m == lava {
                        3
                    } else {
                        match self.cls[m as usize] {
                            C_AIR => 0,
                            C_WATER => 2,
                            _ => 1,
                        }
                    });
                }
            }
        }
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
        // band-limited surface materials for voxels of 2^level blocks (reset at the end)
        voxel_world::gen::biome::set_material_footprint((1u32 << level.min(10)) as f32);
        self.build_lod_inner(gen, level, sx, sz);
        voxel_world::gen::biome::set_material_footprint(0.0);
    }

    fn build_lod_inner(&mut self, gen: &WorldGen, level: u32, sx: i32, sz: i32) {
        use voxel_world::gen::MAX_FEATURE_RADIUS;
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
                // wet column: the voxel just below the (rounded) water level is always water — else
                // a seabed a few blocks under the surface rounds up into that voxel and shows as a
                // dry sand flat at sea level (wide beige halos around every coast at high levels)
                let wet = col.water_level != i32::MIN && (col.water_level as f32) > col.height + 0.5 && wl > 0;
                for j in (yb..yt).rev() {
                    let yc = j * s + s / 2;
                    let solid = if wet && j == wl - 1 {
                        false
                    } else if j < lo {
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
                        // coherent world-space clump pattern (the same at every level, fading to
                        // its mean once the voxels outgrow it) instead of per-voxel white noise:
                        // no salt-and-pepper speckle, no flicker when a node changes level
                        let pat = canopy_pattern(canopy_seed, wx, wz, s);
                        if pat < cp.coverage * 1.05 {
                            let top = ((col.height - 0.5) / s as f32).ceil() as i32;
                            let layers = ((cp.height / s as f32) * (0.8 + 0.4 * pat)).round().max(1.0) as i32;
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
        self.cav_lo.fill(WH);
        self.cav_hi.fill(-1);
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

/// Coherent canopy clump pattern in [0, 1] at world (x, z) for voxels of `s` blocks: two octaves
/// of value noise (λ 36 + 15 blocks) faded toward 0.5 as the voxels outgrow them.
fn canopy_pattern(seed: u64, x: f64, z: f64, s: i32) -> f32 {
    use voxel_world::noise::{hash2, hash_f01};
    let vn = |l: f64, salt: u64| -> f32 {
        let (gx, gz) = (x / l, z / l);
        let (ix, iz) = (gx.floor(), gz.floor());
        let (fx, fz) = ((gx - ix) as f32, (gz - iz) as f32);
        let (ix, iz) = (ix as i32, iz as i32);
        let h = |a: i32, b: i32| hash_f01(hash2(seed ^ salt, a, b));
        let (ux, uz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
        let a = h(ix, iz) + (h(ix + 1, iz) - h(ix, iz)) * ux;
        let b = h(ix, iz + 1) + (h(ix + 1, iz + 1) - h(ix, iz + 1)) * ux;
        a + (b - a) * uz
    };
    let k = 1.0 - voxel_world::math::smoothstep(12.0, 40.0, s as f32);
    if k <= 0.0 {
        return 0.5;
    }
    let raw = 0.65 * vn(36.0, 0xC1) + 0.35 * vn(15.0, 0xC2);
    0.5 + (raw - 0.5) * 1.6 * k
}
