//! World build orchestration (DESIGN §6). OWNER: world-build agent.
//!
//! `build_world(gen, params)` turns the pure generator functions of [`WorldGen`] into the clipmap
//! of [`World`]: exact voxel spans around the centre (level 0) and heightfield levels k = 1..=K.
//!
//! # Pipeline (timed per stage on stderr)
//! 1. **Level 0 terrain** — parallel over tiles of 16×16 chunks (one tile = 16 rows × ≤ 128
//!    columns, i.e. a row of up to 8 chunks that share lattice nodes and border columns). Per
//!    tile: `gen.column` at every block centre plus a one-column ring (for exposure), the
//!    `ColumnInfo` of every 4×4 lattice node, the 3-D band and cave lattices, then every column is
//!    voxelised into a reusable 512-entry buffer and run-length encoded into [`Span`]s.
//! 2. **Features** — parallel over 64×64 root tiles inside the feature disc: each feature is
//!    generated exactly once (`features_in`), anchored on the root column's top solid block.
//! 3. **Assembly** — parallel over 16-row strips: feature runs are composited into the columns
//!    they touch (wherever their root is), giving [`Level0Row`]s → [`Level0::from_rows`].
//! 4. **Heightfield levels** via [`HeightLevel::from_fn`], coarse to fine consistent (below).
//!
//! # Mathematics
//!
//! **Residual lattice for the 3-D band.** The terrain density is `ρ(p) = (H(x,z) − y) + R(p)`
//! where the 2-D height `H` carries all sharp detail (hoodoo walls, mesa steps, dunes, gullies)
//! and `R` is a smooth 3-D perturbation (λ ≈ 72) supported in `|H − y| < J`. Trilinear
//! interpolation of `ρ` itself on a 4-block lattice would low-pass `H` (a 4-block box blur
//! erases 5-block spires), so only the smooth part is interpolated:
//! ```text
//!   ρ̂(x,y,z) = H(x,z) − y + I₄[R](x,y,z),      R_n = ρ(p_n) − (H_n − y_n)
//! ```
//! with `I₄` the trilinear interpolant on the world-aligned lattice `4ℤ³ + ½` (block centres of
//! blocks with coordinates ≡ 0 mod 4). Convexity of trilinear weights gives
//! `|I₄[R]| ≤ max_n |R_n| ≤ J_max`, hence `sign ρ̂ = sign(H − y)` outside `[H − J_max, H + J_max]`:
//! only that band is evaluated per column, and where the four corner nodes have `J = 0` the
//! result is the exact heightfield. A block is solid iff `ρ̂ > 0` at its centre, so the voxel
//! surface is `⌈H − ½⌉`, whose mean is `H` (unbiased w.r.t. the heightfield levels).
//!
//! **Caves** use the same lattice: `F̂ = I₄[F − τ]` with the carver field `F` and a radial taper
//! `τ(r) = 30·((r − R_c + w)/w)²₊` (w = min(32, R_c/4)), so passages pinch out smoothly before the
//! cave radius instead of being cut by a cylinder. Along a column, `F̂` is piecewise linear in y
//! between lattice nodes, so a lattice cell is skipped exactly when both of its y-end values are
//! ≤ 0. Carving is suppressed within 6 blocks below the floor of a water-covered column (no
//! leaking sea beds) and below y = 4.
//!
//! **Liquids.** Terrain air (`ρ̂ ≤ 0`, not carved) below `water_level` is liquid: water, or lava
//! for lakes above sea level in the Volcanic biome; the top block freezes to ice for frozen
//! biomes and for lakes above the local snow line (with a ±8-block simplex-dithered margin →
//! patchy shore ice near the line). Carved voxels take `cave_fill` (aquifer water / deep lava).
//!
//! **Exposure-aware material depth.** A block can only be seen from above (top of each solid
//! run), from the side where a neighbour column is lower, or from a cavity. So per-block
//! `surface_material` (with the true depth inside its solid run) is evaluated for depth < 24, for
//! `y ≥ e = min_{4-nbrs}(⌊H_nb − J_nb⌋) − 2` (cliffs, mesa walls, spires) and, inside the cave
//! radius, where the interpolated cave field `F̂ > −3` — `F` is ≈ a signed distance in blocks, so
//! this is a ≈3-block shell around every cavity, found for free from the lattice values the
//! column already holds. Small builds (l0 ≤ 512, the cut-away dioramas whose box faces expose
//! arbitrary interior blocks) evaluate every block. All other blocks are invisible and get
//! Stone / Deepslate (y < 64) / Bedrock (y = 0).
//!
//! **Feature compositing** is order-independent: every feature block gets the priority
//! `(class, hash(root))`, class = replace_solid 3 > log 2 > other 1 > leaves 0; runs are applied
//! in decreasing priority with "first writer wins" (a block is written iff it is air, or solid
//! terrain for `replace_solid`, and no feature wrote it before). Hence logs override leaves,
//! leaves never overwrite logs, the result does not depend on chunking or traversal order, and
//! trees are seamless across chunk borders because each is generated once from its root.
//!
//! **Clipmap consistency.** Level k covers level k−1's square; cells of level k that lie fully
//! inside the finer level are the 2×2 box filter of it (level 1 from the voxel columns: mean
//! opaque top, max water surface, mean albedo, majority material). Cells outside are
//! `column_lod(·, cell)` + `canopy_lod`: `h = H + coverage·canopy_height`, `water = water_level`
//! when above H, albedo = mean `far_albedo` of 4 stratified `surface_material` samples (a 2×2
//! box filter of the surface pattern) blended with the leaf albedo by coverage. Because the
//! voxel surface `⌈H − ½⌉` has mean `H`, heights match across the level-0 boundary without bias.
use super::{BuildParams, HeightLevel, HfCell, Level0, Level0Row, Span, World};
use crate::gen::{Biome, CaveFill, ColumnInfo, Feature, WorldGen, MAX_FEATURE_RADIUS, SEA_LEVEL, WORLD_HEIGHT};
use crate::materials::{far_albedo, Material};
use crate::math::Vec3;
use crate::noise::{derive_seed, hash2, salt, Simplex};
use rayon::prelude::*;
use std::time::Instant;

/// Chunk side (blocks).
pub const CHUNK: i32 = 16;
/// Lattice spacing of the 3-D band / cave interpolation (blocks).
pub const LATTICE: i32 = 4;
/// In large builds, blocks deeper than this in their solid run (and neither side-exposed nor near
/// a cavity) get the plain deep fill instead of `surface_material`.
pub const SURFACE_DEPTH: i32 = 24;
/// Level-0 builds up to this size (cut-away / diorama builds) give every block its true strata;
/// larger builds only the blocks that can be seen (see module docs).
pub const FULL_STRATA_MAX_L0: u32 = 512;
/// Inside the cave radius, solid blocks with interpolated cave field `F̂ > −CAVE_NEAR` (≈ within
/// this many blocks of a cavity) are treated as potentially visible.
const CAVE_NEAR: f32 = 3.0;
/// Columns per level-0 work tile (8 chunks).
const TILE_W: i32 = 128;
/// Root tile size for feature generation.
const FEATURE_TILE: i32 = 64;
/// Carving never happens below this y (DESIGN §4).
const MIN_CARVE_Y: i32 = 4;
/// No carving within this many blocks below the floor of a water-covered column.
const WET_GUARD: i32 = 6;
const WH: i32 = WORLD_HEIGHT;
const WHU: usize = WORLD_HEIGHT as usize;

// voxel kinds of the per-column work buffer
const K_AIR: u8 = 0;
const K_SOLID: u8 = 1;
const K_CAVE_AIR: u8 = 2;
const K_CAVE_WATER: u8 = 3;
const K_CAVE_LAVA: u8 = 4;
const K_LIQUID: u8 = 5;

/// Timings and sizes of one build.
#[derive(Clone, Debug, Default)]
pub struct BuildStats {
    pub level0_s: f64,
    pub features_s: f64,
    pub assemble_s: f64,
    pub levels_s: Vec<f64>,
    pub total_s: f64,
    pub span_count: usize,
    pub feature_count: usize,
    pub memory_bytes: usize,
}

/// Build the world clipmap (see module docs). Logs per-stage timings to stderr.
pub fn build_world(gen: &WorldGen, p: &BuildParams) -> World {
    build_world_stats(gen, p).0
}

/// [`build_world`] that also returns the measured [`BuildStats`].
pub fn build_world_stats(gen: &WorldGen, p: &BuildParams) -> (World, BuildStats) {
    let t_all = Instant::now();
    let mut st = BuildStats::default();
    let (ox, oz) = p.l0_origin();
    let size = p.l0_size as i32;
    let ctx = L0Ctx {
        gen,
        ox,
        oz,
        ccx: p.center_x as f64,
        ccz: p.center_z as f64,
        cave_r: p.cave_radius as f64,
        full_strata: p.l0_size <= FULL_STRATA_MAX_L0,
        ice: Simplex::new(derive_seed(gen.seed, salt("worldbuild.ice"))),
    };

    // ---- 1. level-0 terrain tiles ------------------------------------------------------------
    let t = Instant::now();
    let nstrips = (size + CHUNK - 1) / CHUNK;
    let tw = TILE_W.min(size.max(1));
    let ntx = (size + tw - 1) / tw;
    let tiles: Vec<(i32, i32)> = (0..nstrips).flat_map(|s| (0..ntx).map(move |t| (s, t))).collect();
    let tile_out: Vec<TileOut> = tiles
        .par_iter()
        .map(|&(s, t)| {
            let lz0 = s * CHUNK;
            let lx0 = t * tw;
            build_tile(&ctx, lx0, lz0, tw.min(size - lx0), CHUNK.min(size - lz0))
        })
        .collect();
    st.level0_s = t.elapsed().as_secs_f64();
    let cave_tiles = tile_out.iter().filter(|t| t.caves).count();
    let band_tiles = tile_out.iter().filter(|t| t.band).count();
    eprintln!(
        "[worldbuild] level0 terrain {}² ({} tiles, {} with 3D band, {} with caves): {:.2}s",
        size,
        tile_out.len(),
        band_tiles,
        cave_tiles,
        st.level0_s
    );

    // ---- 2. features ---------------------------------------------------------------------------
    let t = Instant::now();
    let mut root_y = vec![-1i16; (size.max(0) * size.max(0)) as usize];
    for to in &tile_out {
        for lz in 0..to.rows {
            let dst = ((to.lz0 + lz) * size + to.lx0) as usize;
            let src = (lz * to.w) as usize;
            root_y[dst..dst + to.w as usize].copy_from_slice(&to.root_y[src..src + to.w as usize]);
        }
    }
    let placed = place_features(gen, p, ox, oz, size, &root_y);
    drop(root_y);
    st.feature_count = placed.len();
    let mut strip_feats: Vec<Vec<u32>> = vec![Vec::new(); nstrips.max(0) as usize];
    for (fi, pf) in placed.iter().enumerate() {
        let (za, zb) = (pf.root_z - pf.radius - oz, pf.root_z + pf.radius - oz);
        let s0 = za.div_euclid(CHUNK).max(0);
        let s1 = zb.div_euclid(CHUNK).min(nstrips - 1);
        for s in s0..=s1 {
            strip_feats[s as usize].push(fi as u32);
        }
    }
    st.features_s = t.elapsed().as_secs_f64();
    eprintln!("[worldbuild] features: {} placed (radius {}): {:.2}s", placed.len(), p.feature_radius, st.features_s);

    // ---- 3. assemble rows ----------------------------------------------------------------------
    let t = Instant::now();
    let mut strips: Vec<Vec<TileOut>> = (0..nstrips).map(|_| Vec::with_capacity(ntx as usize)).collect();
    for (to, &(s, _)) in tile_out.into_iter().zip(&tiles) {
        strips[s as usize].push(to);
    }
    let rows: Vec<Level0Row> = strips
        .into_par_iter()
        .zip(strip_feats.into_par_iter())
        .flat_map_iter(|(tl, feats)| assemble_strip(tl, &feats, &placed, ox, oz, size))
        .collect();
    drop(placed);
    let l0 = Level0::from_rows(ox, oz, p.l0_size, rows);
    st.assemble_s = t.elapsed().as_secs_f64();
    st.span_count = l0.span_count();
    eprintln!(
        "[worldbuild] assemble level0: {} spans ({:.2}/column): {:.2}s",
        st.span_count,
        st.span_count as f64 / (size.max(1) as f64).powi(2),
        st.assemble_s
    );

    // ---- 4. heightfield levels -----------------------------------------------------------------
    let mut levels: Vec<HeightLevel> = Vec::with_capacity(p.hf_levels as usize);
    for k in 1..=p.hf_levels {
        let t = Instant::now();
        let (lx, lz) = p.level_origin(k);
        let lvl = {
            let finer = levels.last();
            let ctx = &ctx;
            let l0 = &l0;
            HeightLevel::from_fn(k, lx, lz, p.hf_size, move |cx, cz| hf_cell(ctx, k, cx, cz, l0, finer))
        };
        let dt = t.elapsed().as_secs_f64();
        eprintln!("[worldbuild] heightfield level {k} (cell {}, {}² cells): {:.2}s", 1 << k, p.hf_size, dt);
        st.levels_s.push(dt);
        levels.push(lvl);
    }

    let world = World { seed: gen.seed, params: *p, l0, levels };
    st.memory_bytes = memory_estimate(&world);
    st.total_s = t_all.elapsed().as_secs_f64();
    eprintln!(
        "[worldbuild] total {:.2}s; memory ≈ {:.0} MB (world.memory_bytes {:.0} MB)",
        st.total_s,
        st.memory_bytes as f64 / 1048576.0,
        world.memory_bytes() as f64 / 1048576.0
    );
    (world, st)
}

/// Resident size of the built world (spans, per-column arrays, mips, heightfield arrays).
pub fn memory_estimate(w: &World) -> usize {
    let n0 = w.l0.top.len();
    let l0 = w.l0.span_count() * std::mem::size_of::<Span>() + n0 * (4 + 2 + 1 + 2) + n0 * 2 * 4 / 3;
    let hf: usize = w.levels.iter().map(|l| l.h.len() * (4 + 4 + 12 + 1 + 1 + 4 * 4 / 3)).sum();
    l0 + hf
}

// =====================================================================================
// Level 0
// =====================================================================================

struct L0Ctx<'a> {
    gen: &'a WorldGen,
    ox: i32,
    oz: i32,
    ccx: f64,
    ccz: f64,
    cave_r: f64,
    full_strata: bool,
    ice: Simplex,
}

impl L0Ctx<'_> {
    /// Liquid filling terrain air below `water_level`: lava for lakes above sea level in the
    /// Volcanic biome, else water.
    #[inline]
    fn liquid(&self, col: &ColumnInfo) -> Material {
        if col.biome == Biome::Volcanic && col.water_level > SEA_LEVEL {
            Material::Lava
        } else {
            Material::Water
        }
    }

    /// Does the water surface of this column freeze? Frozen biomes, and lakes above the local
    /// snow line (margin dithered by a λ≈23 simplex → patchy ice near the line).
    fn freezes(&self, x: f64, z: f64, col: &ColumnInfo) -> bool {
        if matches!(col.biome, Biome::FrozenOcean | Biome::FrozenRiver) {
            return true;
        }
        if col.water_level <= SEA_LEVEL || self.liquid(col) == Material::Lava {
            return false;
        }
        let rel = col.water_level as f32 - self.gen.surface.snow_line(col);
        if rel < -16.0 {
            return false;
        }
        rel > -8.0 + 8.0 * self.ice.n2(x / 23.0, z / 23.0)
    }

    /// Radial taper of the cave field near the cave radius (blocks subtracted from F).
    #[inline]
    fn cave_taper(&self, r: f64) -> f32 {
        let w = (self.cave_r * 0.25).min(32.0).max(1.0);
        let u = (r - (self.cave_r - w)) / w;
        if u <= 0.0 {
            0.0
        } else {
            (30.0 * u * u) as f32
        }
    }
}

/// Values of a scalar field at the lattice nodes of a tile, for lattice y indices
/// `j0 .. j0 + nj` (node y = 4j + ½). Outside that range the field is `outside`.
struct NodeField {
    j0: i32,
    nj: usize,
    vals: Vec<f32>,
    outside: f32,
}

impl NodeField {
    /// Bilinear blend of the 4 corner node columns at lattice level j.
    #[inline]
    fn bilerp(&self, n: [usize; 4], w: [f32; 4], j: i32) -> f32 {
        let jj = j - self.j0;
        if jj < 0 || jj as usize >= self.nj {
            return self.outside;
        }
        let jj = jj as usize;
        let v = |k: usize| self.vals[n[k] * self.nj + jj];
        w[0] * v(0) + w[1] * v(1) + w[2] * v(2) + w[3] * v(3)
    }
}

/// Output of one level-0 tile (`rows` × `w` columns, row-major).
struct TileOut {
    lx0: i32,
    lz0: i32,
    w: i32,
    rows: i32,
    spans: Vec<Span>,
    counts: Vec<u16>,
    biome: Vec<Biome>,
    clim: Vec<[f32; 2]>,
    /// First air above the top solid block, or −1 when the column is topped by liquid.
    root_y: Vec<i16>,
    band: bool,
    caves: bool,
}

/// Reusable per-column buffers.
struct ColBuf {
    kind: Vec<u8>,
    mats: Vec<Material>,
    bres: Vec<f32>,
    bcav: Vec<f32>,
}

#[inline]
fn push_merge(out: &mut Vec<Span>, y0: i32, y1: i32, mat: Material) {
    if y1 <= y0 || mat == Material::Air {
        return;
    }
    if let Some(l) = out.last_mut() {
        if l.mat == mat && l.y1 as i32 == y0 {
            l.y1 = y1 as u16;
            return;
        }
    }
    out.push(Span::new(y0, y1, mat));
}

/// Material of invisible deep rock outside the cave radius.
#[inline]
fn deep_fill(y: i32) -> Material {
    if y <= 0 {
        Material::Bedrock
    } else if y < 64 {
        Material::Deepslate
    } else {
        Material::Stone
    }
}

fn build_tile(c: &L0Ctx, lx0: i32, lz0: i32, w: i32, rows: i32) -> TileOut {
    let gen = c.gen;
    let x0 = c.ox + lx0;
    let z0 = c.oz + lz0;
    // -- columns at every block centre of the tile plus a one-column ring
    let rw = (w + 2) as usize;
    let rh = (rows + 2) as usize;
    let mut ring: Vec<ColumnInfo> = Vec::with_capacity(rw * rh);
    for gz in 0..rh as i32 {
        for gx in 0..rw as i32 {
            ring.push(gen.column((x0 - 1 + gx) as f64 + 0.5, (z0 - 1 + gz) as f64 + 0.5));
        }
    }
    // -- lattice nodes (world-aligned: block coordinates ≡ 0 mod 4)
    let nx0 = x0.div_euclid(LATTICE);
    let nx1 = (x0 + w - 1).div_euclid(LATTICE) + 1;
    let nz0 = z0.div_euclid(LATTICE);
    let nz1 = (z0 + rows - 1).div_euclid(LATTICE) + 1;
    let ni = (nx1 - nx0 + 1) as usize;
    let nk = (nz1 - nz0 + 1) as usize;
    let mut nodes: Vec<ColumnInfo> = Vec::with_capacity(ni * nk);
    for k in 0..nk as i32 {
        for i in 0..ni as i32 {
            let (x, z) = ((nx0 + i) * LATTICE, (nz0 + k) * LATTICE);
            let (gx, gz) = (x - (x0 - 1), z - (z0 - 1));
            if gx >= 0 && gz >= 0 && (gx as usize) < rw && (gz as usize) < rh {
                nodes.push(ring[gz as usize * rw + gx as usize]);
            } else {
                nodes.push(gen.column(x as f64 + 0.5, z as f64 + 0.5));
            }
        }
    }
    let node_xz = |n: usize| -> (f64, f64) {
        let (i, k) = ((n % ni) as i32, (n / ni) as i32);
        (((nx0 + i) * LATTICE) as f64 + 0.5, ((nz0 + k) * LATTICE) as f64 + 0.5)
    };

    // -- residual lattice of the 3-D band: R_n = ρ(p_n) − (H_n − y_n)
    let res: Option<NodeField> = {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for n in nodes.iter().filter(|n| n.jaggedness > 0.0) {
            lo = lo.min(n.height - n.jaggedness);
            hi = hi.max(n.height + n.jaggedness);
        }
        if lo < hi {
            let j0 = ((lo / LATTICE as f32).floor() as i32).clamp(0, WH / LATTICE);
            let j1 = ((hi / LATTICE as f32).ceil() as i32).clamp(j0, WH / LATTICE);
            let nj = (j1 - j0 + 1) as usize;
            let mut vals = vec![0.0f32; nodes.len() * nj];
            for (n, col) in nodes.iter().enumerate() {
                if col.jaggedness <= 0.0 {
                    continue;
                }
                let (x, z) = node_xz(n);
                for jj in 0..nj {
                    let y = ((j0 + jj as i32) * LATTICE) as f64 + 0.5;
                    let base = col.height - y as f32;
                    if base.abs() < col.jaggedness {
                        vals[n * nj + jj] = gen.density(x, y, z, col) - base;
                    }
                }
            }
            Some(NodeField { j0, nj, vals, outside: 0.0 })
        } else {
            None
        }
    };

    // -- cave lattice (only for tiles touching the cave disc)
    let caves: Option<NodeField> = if c.cave_r > 0.0 {
        let (ax, bx) = (x0 as f64, (x0 + w) as f64);
        let (az, bz) = (z0 as f64, (z0 + rows) as f64);
        let dx = (c.ccx.clamp(ax, bx) - c.ccx).abs();
        let dz = (c.ccz.clamp(az, bz) - c.ccz).abs();
        if dx * dx + dz * dz <= c.cave_r * c.cave_r {
            let top = nodes.iter().map(|n| n.height + n.jaggedness.max(0.0)).fold(0.0f32, f32::max) + 10.0;
            let j1 = ((top / LATTICE as f32).ceil() as i32).clamp(1, WH / LATTICE);
            let nj = (j1 + 1) as usize;
            let mut vals = vec![-64.0f32; nodes.len() * nj];
            for (n, col) in nodes.iter().enumerate() {
                let (x, z) = node_xz(n);
                let r = ((x - c.ccx).powi(2) + (z - c.ccz).powi(2)).sqrt();
                if r > c.cave_r + 8.0 {
                    continue;
                }
                let tap = c.cave_taper(r);
                for jj in 0..nj {
                    let y = (jj as i32 * LATTICE) as f64 + 0.5;
                    vals[n * nj + jj] = gen.cave_field(x, y, z, col) - tap;
                }
            }
            Some(NodeField { j0: 0, nj, vals, outside: -64.0 })
        } else {
            None
        }
    } else {
        None
    };

    // -- voxelise every column
    let ncols = (w * rows) as usize;
    let mut out = TileOut {
        lx0,
        lz0,
        w,
        rows,
        spans: Vec::with_capacity(ncols * 8),
        counts: Vec::with_capacity(ncols),
        biome: Vec::with_capacity(ncols),
        clim: Vec::with_capacity(ncols),
        root_y: Vec::with_capacity(ncols),
        band: res.is_some(),
        caves: caves.is_some(),
    };
    let mut b = ColBuf {
        kind: vec![K_AIR; WHU + 1],
        mats: vec![Material::Air; WHU + 1],
        bres: vec![0.0; WHU / LATTICE as usize + 2],
        bcav: vec![0.0; WHU / LATTICE as usize + 2],
    };
    let cave_r2 = c.cave_r * c.cave_r;
    for lz in 0..rows {
        for lx in 0..w {
            let (x, z) = (x0 + lx, z0 + lz);
            let gi = (lz as usize + 1) * rw + lx as usize + 1;
            let col = &ring[gi];
            // exposure floor from the 4 neighbours' 2-D surfaces
            let expo = [gi - 1, gi + 1, gi - rw, gi + rw]
                .iter()
                .map(|&g| (ring[g].height - ring[g].jaggedness.max(0.0)).floor() as i32 - 2)
                .min()
                .unwrap();
            // corner nodes + bilinear weights
            let i = (x.div_euclid(LATTICE) - nx0) as usize;
            let k = (z.div_euclid(LATTICE) - nz0) as usize;
            let fx = x.rem_euclid(LATTICE) as f32 / LATTICE as f32;
            let fz = z.rem_euclid(LATTICE) as f32 / LATTICE as f32;
            let n00 = k * ni + i;
            let corners = [n00, n00 + 1, n00 + ni, n00 + ni + 1];
            let wts = [(1.0 - fx) * (1.0 - fz), fx * (1.0 - fz), (1.0 - fx) * fz, fx * fz];
            let dxc = x as f64 + 0.5 - c.ccx;
            let dzc = z as f64 + 0.5 - c.ccz;
            let cav = match &caves {
                Some(f) if dxc * dxc + dzc * dzc <= cave_r2 => Some(f),
                _ => None,
            };
            let jmax = match &res {
                Some(_) => corners.iter().map(|&n| nodes[n].jaggedness.max(0.0)).fold(0.0f32, f32::max),
                None => 0.0,
            };
            let before = out.spans.len();
            let root = voxelise_column(c, x, z, col, expo, res.as_ref().filter(|_| jmax > 0.0), jmax, cav, corners, wts, &mut b, &mut out.spans);
            out.counts.push((out.spans.len() - before) as u16);
            out.biome.push(col.biome);
            out.clim.push([col.surface_temperature, col.climate.humidity]);
            out.root_y.push(root);
        }
    }
    out
}

/// Voxelise one column into `spans` (appended). Returns the feature root y (first air above the
/// top solid block) or −1 when the column is topped by liquid.
#[allow(clippy::too_many_arguments)]
fn voxelise_column(
    c: &L0Ctx,
    x: i32,
    z: i32,
    col: &ColumnInfo,
    expo: i32,
    res: Option<&NodeField>,
    jmax: f32,
    cav: Option<&NodeField>,
    corners: [usize; 4],
    wts: [f32; 4],
    b: &mut ColBuf,
    spans: &mut Vec<Span>,
) -> i16 {
    let gen = c.gen;
    let h = col.height;
    // heightfield top: blocks with y + ½ < H
    let hf_top = ((h - 0.5).ceil() as i32).clamp(1, WH);
    let (band_lo, band_hi) = if res.is_some() {
        (((h - jmax - 1.5).floor() as i32).clamp(1, WH), ((h + jmax + 1.5).ceil() as i32).clamp(1, WH))
    } else {
        (hf_top, hf_top)
    };
    let wl = if col.water_level == i32::MIN { 0 } else { col.water_level.clamp(0, WH) };
    let in_caves = cav.is_some();
    let dense_lo = if in_caves || c.full_strata { 0 } else { (band_lo - SURFACE_DEPTH).min(expo).clamp(0, band_lo) };
    let dense_hi = band_hi.max(wl).clamp(dense_lo, WH);
    let (lo, hi) = (dense_lo as usize, dense_hi as usize);
    let kind = &mut b.kind;

    // ---- solid / air from ρ̂ = H − y + I₄[R]
    kind[lo..band_lo as usize].fill(K_SOLID);
    if let Some(rf) = res {
        let ja = band_lo / LATTICE;
        let jb = (band_hi - 1) / LATTICE + 1;
        for j in ja..=jb {
            b.bres[(j - ja) as usize] = rf.bilerp(corners, wts, j);
        }
        for y in band_lo..band_hi {
            let j = y / LATTICE;
            let fy = (y - j * LATTICE) as f32 / LATTICE as f32;
            let r0 = b.bres[(j - ja) as usize];
            let r1 = b.bres[(j - ja + 1) as usize];
            let d = h - (y as f32 + 0.5) + r0 + (r1 - r0) * fy;
            kind[y as usize] = if d > 0.0 { K_SOLID } else { K_AIR };
        }
    }
    kind[band_hi as usize..hi.max(band_hi as usize)].fill(K_AIR);
    if lo == 0 {
        kind[0] = K_SOLID; // bedrock floor
    }
    let mut top = band_hi;
    while top > dense_lo && kind[top as usize - 1] != K_SOLID {
        top -= 1;
    }

    // ---- caves: F̂ = I₄[F − τ] > 0 carves
    let mut jt = -1; // bcav[0..=jt] holds the column's bilinear cave field per lattice level
    if let Some(cf) = cav {
        let wet = col.water_level != i32::MIN && col.water_level as f32 > h;
        let guard = if wet { (top - WET_GUARD).max(0) } else { WH };
        let ymax = top.min(guard);
        jt = (top.max(1) - 1) / LATTICE + 1;
        for j in 0..=jt {
            b.bcav[j as usize] = cf.bilerp(corners, wts, j);
        }
        if ymax > MIN_CARVE_Y {
            let jb = (ymax - 1) / LATTICE + 1;
            for j in (MIN_CARVE_Y / LATTICE)..jb {
                let (f0, f1) = (b.bcav[j as usize], b.bcav[j as usize + 1]);
                if f0 <= 0.0 && f1 <= 0.0 {
                    continue;
                }
                let ya = (j * LATTICE).max(MIN_CARVE_Y);
                let yb = ((j + 1) * LATTICE).min(ymax);
                for y in ya..yb {
                    if kind[y as usize] != K_SOLID {
                        continue;
                    }
                    let fy = (y - j * LATTICE) as f32 / LATTICE as f32;
                    if f0 + (f1 - f0) * fy > 0.0 {
                        kind[y as usize] = match gen.cave_fill(x, y, z, col) {
                            CaveFill::Air => K_CAVE_AIR,
                            CaveFill::Water => K_CAVE_WATER,
                            CaveFill::Lava => K_CAVE_LAVA,
                        };
                    }
                }
            }
        }
    }

    // ---- surface liquids: terrain air below water_level
    let liquid = c.liquid(col);
    for y in lo..(wl as usize).min(hi) {
        if kind[y] == K_AIR {
            kind[y] = K_LIQUID;
        }
    }

    // ---- materials, top-down with depth inside each solid run
    let mut depth = 0i32;
    let mut in_run = false;
    for y in (lo..hi).rev() {
        let yi = y as i32;
        b.mats[y] = match kind[y] {
            K_SOLID => {
                let d = if in_run { depth } else { 0 };
                in_run = true;
                depth = d + 1;
                // visible candidates: top of the run, side-exposed, near a cavity (or every
                // block of a small cut-away build)
                let near_cave = in_caves && {
                    let j = yi / LATTICE;
                    j < jt && b.bcav[j as usize].max(b.bcav[j as usize + 1]) > -CAVE_NEAR
                };
                if c.full_strata || near_cave || d < SURFACE_DEPTH || yi >= expo {
                    gen.surface_material(x, yi, z, d, col)
                } else {
                    deep_fill(yi)
                }
            }
            kd => {
                in_run = false;
                depth = 0;
                match kd {
                    K_CAVE_WATER => Material::Water,
                    K_CAVE_LAVA => Material::Lava,
                    K_LIQUID => liquid,
                    _ => Material::Air,
                }
            }
        };
    }
    if wl as usize > lo && wl as usize <= hi && kind[wl as usize - 1] == K_LIQUID && liquid == Material::Water {
        if c.freezes(x as f64 + 0.5, z as f64 + 0.5, col) {
            b.mats[wl as usize - 1] = Material::Ice;
        }
    }

    // ---- feature root: first air above the top solid block (none if liquid on top)
    let mut root = dense_lo as i16;
    for y in (lo..hi).rev() {
        match kind[y] {
            K_AIR | K_CAVE_AIR => continue,
            K_SOLID => {
                root = y as i16 + 1;
                break;
            }
            _ => {
                root = -1;
                break;
            }
        }
    }

    // ---- run-length encode
    if dense_lo > 0 {
        push_merge(spans, 0, 1, Material::Bedrock);
        push_merge(spans, 1, dense_lo.min(64), Material::Deepslate);
        push_merge(spans, 64, dense_lo, Material::Stone);
    }
    for y in lo..hi {
        let m = b.mats[y];
        if m != Material::Air {
            push_merge(spans, y as i32, y as i32 + 1, m);
        }
    }
    root
}

// =====================================================================================
// Features
// =====================================================================================

struct Placed {
    root_x: i32,
    root_z: i32,
    /// World y of the feature's y = 0 (first air above the root's top solid block).
    base_y: i32,
    radius: i32,
    /// Tie-break priority from the root position (order-independent compositing).
    key: u32,
    f: Feature,
}

fn place_features(gen: &WorldGen, p: &BuildParams, ox: i32, oz: i32, size: i32, root_y: &[i16]) -> Vec<Placed> {
    let fr = p.feature_radius as i64;
    if fr == 0 || size <= 0 {
        return Vec::new();
    }
    let (cx, cz) = (p.center_x, p.center_z);
    // Roots up to MAX_FEATURE_RADIUS outside level 0 still reach into it (no thinned forest
    // margin at the level-0 edge).
    let m = MAX_FEATURE_RADIUS;
    let xa = (ox - m).max((cx as i64 - fr) as i32);
    let xb = (ox + size + m).min((cx as i64 + fr + 1) as i32);
    let za = (oz - m).max((cz as i64 - fr) as i32);
    let zb = (oz + size + m).min((cz as i64 + fr + 1) as i32);
    if xb <= xa || zb <= za {
        return Vec::new();
    }
    let key_seed = derive_seed(gen.seed, salt("worldbuild.feature_key"));
    let mut tiles = Vec::new();
    let mut tz = za;
    while tz < zb {
        let mut tx = xa;
        while tx < xb {
            tiles.push((tx, tz, (tx + FEATURE_TILE).min(xb), (tz + FEATURE_TILE).min(zb)));
            tx += FEATURE_TILE;
        }
        tz += FEATURE_TILE;
    }
    let fr2 = (fr * fr) as f64;
    let per_tile: Vec<Vec<Placed>> = tiles
        .par_iter()
        .map(|&(x0, z0, x1, z1)| {
            gen.features_in(x0, z0, x1, z1)
                .into_iter()
                .filter_map(|f| {
                    let (rx, rz) = (f.root_x, f.root_z);
                    let d2 = (rx as f64 + 0.5 - cx as f64).powi(2) + (rz as f64 + 0.5 - cz as f64).powi(2);
                    if d2 > fr2 || f.columns.is_empty() {
                        return None;
                    }
                    let inside = rx >= ox && rz >= oz && rx < ox + size && rz < oz + size;
                    let ry = if inside {
                        root_y[((rz - oz) * size + (rx - ox)) as usize]
                    } else {
                        let r = f.radius();
                        if rx + r < ox || rz + r < oz || rx - r >= ox + size || rz - r >= oz + size {
                            return None;
                        }
                        exact_root_y(gen, rx, rz)
                    };
                    if ry < 0 {
                        return None;
                    }
                    let radius = f.radius();
                    Some(Placed {
                        root_x: rx,
                        root_z: rz,
                        base_y: ry as i32,
                        radius,
                        key: (hash2(key_seed, rx, rz) >> 32) as u32,
                        f,
                    })
                })
                .collect()
        })
        .collect();
    per_tile.into_iter().flatten().collect()
}

/// Feature root height of a column outside level 0 (no lattice there): exact density scan of
/// the 3-D band; −1 when the column is under water.
fn exact_root_y(gen: &WorldGen, x: i32, z: i32) -> i16 {
    let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
    if col.water_level != i32::MIN && col.water_level as f32 > col.height {
        return -1;
    }
    let h = col.height;
    let j = col.jaggedness.max(0.0);
    let hf_top = ((h - 0.5).ceil() as i32).clamp(1, WH);
    if j <= 0.0 {
        return hf_top as i16;
    }
    let lo = ((h - j - 1.5).floor() as i32).clamp(1, WH);
    let hi = ((h + j + 1.5).ceil() as i32).clamp(lo, WH);
    for y in (lo..hi).rev() {
        if gen.density(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5, &col) > 0.0 {
            return (y + 1) as i16;
        }
    }
    lo as i16
}

/// One feature run in a column of a strip.
#[derive(Clone, Copy)]
struct Run {
    col: u32,
    y0: i16,
    y1: i16,
    mat: Material,
    replace: bool,
    prio: u64,
}

#[inline]
fn run_class(replace: bool, m: Material) -> u64 {
    if replace {
        3
    } else if m.is_log() {
        2
    } else if m.is_leaves() {
        0
    } else {
        1
    }
}

fn assemble_strip(tiles: Vec<TileOut>, feats: &[u32], placed: &[Placed], ox: i32, oz: i32, size: i32) -> Vec<Level0Row> {
    let rows = tiles.first().map(|t| t.rows).unwrap_or(0);
    let lz0 = tiles.first().map(|t| t.lz0).unwrap_or(0);
    let sz = size as usize;
    // ---- gather feature runs of this strip
    let mut runs: Vec<Run> = Vec::new();
    for &fi in feats {
        let pf = &placed[fi as usize];
        for fc in &pf.f.columns {
            if fc.mat == Material::Air {
                continue;
            }
            let lx = pf.root_x + fc.dx as i32 - ox;
            let lz = pf.root_z + fc.dz as i32 - oz - lz0;
            if lx < 0 || lx >= size || lz < 0 || lz >= rows {
                continue;
            }
            let y0 = (pf.base_y + fc.y0 as i32).max(0);
            let y1 = (pf.base_y + fc.y1 as i32).min(WH);
            if y1 <= y0 {
                continue;
            }
            runs.push(Run {
                col: (lz as usize * sz + lx as usize) as u32,
                y0: y0 as i16,
                y1: y1 as i16,
                mat: fc.mat,
                replace: fc.replace_solid,
                prio: (run_class(fc.replace_solid, fc.mat) << 32) | pf.key as u64,
            });
        }
    }
    runs.sort_unstable_by(|a, b| {
        a.col
            .cmp(&b.col)
            .then(b.prio.cmp(&a.prio))
            .then(a.y0.cmp(&b.y0))
            .then(a.y1.cmp(&b.y1))
            .then(a.mat.cmp(&b.mat))
            .then(a.replace.cmp(&b.replace))
    });
    // first run index per column (u32::MAX = none)
    let mut first = vec![u32::MAX; rows.max(0) as usize * sz];
    for (ri, r) in runs.iter().enumerate().rev() {
        first[r.col as usize] = ri as u32;
    }

    let mut out: Vec<Level0Row> = (0..rows).map(|_| Level0Row::with_capacity(sz)).collect();
    let mut buf: Vec<Material> = vec![Material::Air; WHU];
    let mut written: Vec<bool> = vec![false; WHU];
    let mut tmp: Vec<Span> = Vec::with_capacity(64);
    for t in &tiles {
        let mut off = 0usize;
        for lz in 0..t.rows {
            let row = &mut out[lz as usize];
            for lx in 0..t.w {
                let ci = (lz * t.w + lx) as usize;
                let n = t.counts[ci] as usize;
                let col_spans = &t.spans[off..off + n];
                off += n;
                let [temp, hum] = t.clim[ci];
                let gcol = lz as usize * sz + (t.lx0 + lx) as usize;
                let fr = first[gcol];
                if fr == u32::MAX {
                    row.push_column(col_spans, t.biome[ci], temp, hum);
                    continue;
                }
                let mut re = fr as usize;
                while re < runs.len() && runs[re].col as usize == gcol {
                    re += 1;
                }
                composite_column(col_spans, &runs[fr as usize..re], &mut buf, &mut written, &mut tmp);
                row.push_column(&tmp, t.biome[ci], temp, hum);
            }
        }
    }
    out
}

/// Composite feature runs (sorted by decreasing priority) into a column: first writer wins,
/// a block is written iff it is air, or non-air terrain for `replace_solid` runs.
fn composite_column(spans: &[Span], runs: &[Run], buf: &mut [Material], written: &mut [bool], out: &mut Vec<Span>) {
    let ymin = runs.iter().map(|r| r.y0 as i32).min().unwrap();
    let ymax = runs.iter().map(|r| r.y1 as i32).max().unwrap();
    let (a, bb) = (ymin as usize, ymax as usize);
    buf[a..bb].fill(Material::Air);
    written[a..bb].fill(false);
    for s in spans {
        let (s0, s1) = ((s.y0 as i32).max(ymin), (s.y1 as i32).min(ymax));
        if s0 < s1 {
            buf[s0 as usize..s1 as usize].fill(s.mat);
        }
    }
    for r in runs {
        for y in r.y0 as usize..r.y1 as usize {
            if written[y] {
                continue;
            }
            let cur = buf[y];
            if cur == Material::Air || (r.replace && cur != Material::Bedrock) {
                buf[y] = r.mat;
                written[y] = true;
            }
        }
    }
    out.clear();
    for s in spans {
        push_merge(out, s.y0 as i32, (s.y1 as i32).min(ymin), s.mat);
    }
    for y in a..bb {
        push_merge(out, y as i32, y as i32 + 1, buf[y]);
    }
    for s in spans {
        push_merge(out, (s.y0 as i32).max(ymax), s.y1 as i32, s.mat);
    }
}

// =====================================================================================
// Heightfield levels
// =====================================================================================

/// One sample used for the 2×2 box filter.
#[derive(Clone, Copy)]
struct HfSample {
    h: f32,
    water: f32,
    mat: Material,
    albedo: Vec3,
    biome: Biome,
}

fn hf_cell(c: &L0Ctx, k: u32, cx: f64, cz: f64, l0: &Level0, finer: Option<&HeightLevel>) -> HfCell {
    let cell = 1i32 << k;
    let a = (cx - cell as f64 * 0.5).round() as i32;
    let b = (cz - cell as f64 * 0.5).round() as i32;
    if k == 1 {
        if l0.contains(a, b) && l0.contains(a + 1, b + 1) {
            let s = [l0_sample(l0, a, b), l0_sample(l0, a + 1, b), l0_sample(l0, a, b + 1), l0_sample(l0, a + 1, b + 1)];
            return box_filter(&s);
        }
    } else if let Some(f) = finer {
        let fc = f.cell;
        let i0 = (a - f.origin_x).div_euclid(fc);
        let j0 = (b - f.origin_z).div_euclid(fc);
        let n = f.size as i32;
        if i0 >= 0 && j0 >= 0 && i0 + 1 < n && j0 + 1 < n {
            let smp = |i: i32, j: i32| {
                let q = f.index(i, j);
                HfSample {
                    h: f.h[q],
                    water: f.water[q],
                    mat: f.mat[q],
                    albedo: Vec3::from_array(f.albedo[q]),
                    biome: f.biome[q],
                }
            };
            let s = [smp(i0, j0), smp(i0 + 1, j0), smp(i0, j0 + 1), smp(i0 + 1, j0 + 1)];
            return box_filter(&s);
        }
    }
    hf_direct(c, cx, cz, cell)
}

/// Sample a level-0 column for the level-1 box filter.
fn l0_sample(l0: &Level0, x: i32, z: i32) -> HfSample {
    let s = l0.spans(x, z);
    let (temp, hum) = l0.climate_at(x, z);
    let biome = l0.biome(x, z);
    let mut n = s.len();
    let mut water = f32::NEG_INFINITY;
    let mut liquid = Material::Air;
    if n > 0 {
        let top = s[n - 1];
        let ice_on_water = top.mat == Material::Ice && n >= 2 && s[n - 2].mat == Material::Water && s[n - 2].y1 == top.y0;
        if matches!(top.mat, Material::Water | Material::Lava) || ice_on_water {
            water = top.y1 as f32;
            liquid = top.mat;
            n -= 1;
            while n > 0 && matches!(s[n - 1].mat, Material::Water | Material::Ice) {
                n -= 1;
            }
        }
    }
    let (h, ground) = if n > 0 { (s[n - 1].y1 as f32, s[n - 1].mat) } else { (0.0, Material::Bedrock) };
    let albedo = far_albedo(ground, temp, hum, h);
    let mat = if water > h { liquid } else { ground };
    HfSample { h, water: if water > h { water } else { f32::NEG_INFINITY }, mat, albedo, biome }
}

/// 2×2 box filter: mean height & albedo, max water surface, majority material (ties → higher).
fn box_filter(s: &[HfSample; 4]) -> HfCell {
    let h = (s[0].h + s[1].h + s[2].h + s[3].h) * 0.25;
    let water = s.iter().map(|q| q.water).fold(f32::NEG_INFINITY, f32::max);
    let albedo = (s[0].albedo + s[1].albedo + s[2].albedo + s[3].albedo) * 0.25;
    let mat = if water > h {
        s.iter().filter(|q| q.water == water).map(|q| q.mat).next().unwrap_or(Material::Water)
    } else {
        let mut best = (0usize, f32::MIN, Material::Air);
        for q in s.iter().filter(|q| !(q.water > q.h)) {
            let cnt = s.iter().filter(|o| o.mat == q.mat).count();
            if cnt > best.0 || (cnt == best.0 && q.h > best.1) {
                best = (cnt, q.h, q.mat);
            }
        }
        if best.0 == 0 {
            s[0].mat
        } else {
            best.2
        }
    };
    HfCell { h, water: if water > h { water } else { f32::NEG_INFINITY }, albedo, mat, biome: s[0].biome }
}

/// Cell from the band-limited generator: column_lod + canopy_lod + filtered surface albedo.
fn hf_direct(c: &L0Ctx, cx: f64, cz: f64, cell: i32) -> HfCell {
    let gen = c.gen;
    let fp = cell as f64;
    let col = gen.column_lod(cx, cz, fp);
    let h = col.height;
    let wet = col.water_level != i32::MIN && col.water_level as f32 > h;
    let ty = ((h - 0.5).ceil() as i32 - 1).clamp(0, WH - 1);
    let (temp, hum) = (col.surface_temperature, col.climate.humidity);
    let q = fp * 0.25;
    let mut mats = [Material::Air; 4];
    let mut alb = Vec3::ZERO;
    for (s, (sx, sz)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)].iter().enumerate() {
        let x = (cx + sx * q).floor() as i32;
        let z = (cz + sz * q).floor() as i32;
        let m = gen.surface_material(x, ty, z, 0, &col);
        mats[s] = m;
        alb += far_albedo(m, temp, hum, h);
    }
    alb *= 0.25;
    let ground = {
        let mut best = (0usize, mats[0]);
        for m in mats {
            let n = mats.iter().filter(|&&o| o == m).count();
            if n > best.0 {
                best = (n, m);
            }
        }
        best.1
    };
    if wet {
        let mut liq = c.liquid(&col);
        if liq == Material::Water && c.freezes(cx, cz, &col) {
            liq = Material::Ice;
        }
        return HfCell { h, water: col.water_level as f32, albedo: alb, mat: liq, biome: col.biome };
    }
    let can = gen.canopy_lod(cx, cz, &col, fp);
    let cov = can.coverage.clamp(0.0, 1.0);
    if cov <= 0.0 || !can.height.is_finite() {
        return HfCell { h, water: f32::NEG_INFINITY, albedo: alb, mat: ground, biome: col.biome };
    }
    let leaf = far_albedo(can.leaf, temp, hum, h);
    HfCell {
        h: h + cov * can.height.max(0.0),
        water: f32::NEG_INFINITY,
        albedo: alb.lerp(leaf, cov),
        mat: if cov >= 0.5 { can.leaf } else { ground },
        biome: col.biome,
    }
}

// =====================================================================================
// Tests
// =====================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn params(cx: i32, cz: i32, l0: u32, cave: u32, feat: u32, hf: u32, levels: u32) -> BuildParams {
        BuildParams { center_x: cx, center_z: cz, l0_size: l0, hf_size: hf, hf_levels: levels, cave_radius: cave, feature_radius: feat }
    }

    /// Find a land centre with some relief (deterministic scan).
    fn land_centre(gen: &WorldGen) -> (i32, i32) {
        let mut best = (f32::MIN, 0, 0);
        for j in -8..=8 {
            for i in -8..=8 {
                let (x, z) = (i * 400, j * 400);
                let c = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                let wet = c.water_level != i32::MIN && c.water_level as f32 > c.height;
                let score = if wet { -1.0 } else { c.slope.min(1.5) + c.jaggedness * 0.05 + (c.height - 140.0).clamp(0.0, 80.0) * 0.01 };
                if score > best.0 {
                    best = (score, x, z);
                }
            }
        }
        (best.1, best.2)
    }

    fn check_spans(l0: &Level0) {
        let n = l0.size as i32;
        for j in 0..n {
            for i in 0..n {
                let (x, z) = (l0.origin_x + i, l0.origin_z + j);
                let s = l0.spans(x, z);
                for (k, sp) in s.iter().enumerate() {
                    assert!(sp.y1 > sp.y0, "empty span at ({x},{z}): {s:?}");
                    assert!(sp.mat != Material::Air, "air span at ({x},{z})");
                    assert!((sp.y1 as i32) <= WORLD_HEIGHT, "span above world at ({x},{z})");
                    if k > 0 {
                        let pr = s[k - 1];
                        assert!(pr.y1 <= sp.y0, "unsorted/overlapping at ({x},{z}): {s:?}");
                        assert!(!(pr.y1 == sp.y0 && pr.mat == sp.mat), "unmerged spans at ({x},{z}): {s:?}");
                    }
                }
                assert!(!s.is_empty() && s[0].y0 == 0, "column ({x},{z}) has no floor: {s:?}");
            }
        }
    }

    #[test]
    fn span_invariants_and_mips() {
        let gen = WorldGen::new(42);
        let (cx, cz) = land_centre(&gen);
        let w = build_world(&gen, &params(cx, cz, 128, 64, 64, 64, 2));
        check_spans(&w.l0);
        // tops and mips
        let n = w.l0.size as usize;
        for k in 0..n * n {
            assert_eq!(w.l0.top[k], w.l0.spans_at_index(k).last().map(|s| s.y1).unwrap_or(0));
        }
        assert_eq!(w.l0.mip[0], w.l0.top);
        for m in 1..w.l0.mip.len() {
            let (s, ps) = (n >> m, n >> (m - 1));
            for j in 0..s {
                for i in 0..s {
                    let p = &w.l0.mip[m - 1];
                    let mx = p[2 * j * ps + 2 * i].max(p[2 * j * ps + 2 * i + 1]).max(p[(2 * j + 1) * ps + 2 * i]).max(p[(2 * j + 1) * ps + 2 * i + 1]);
                    assert_eq!(w.l0.mip[m][j * s + i], mx);
                }
            }
        }
        assert_eq!(*w.l0.mip.last().unwrap().first().unwrap(), *w.l0.top.iter().max().unwrap());
        for lv in &w.levels {
            for q in 0..lv.h.len() {
                assert!(lv.h[q].is_finite());
                assert_eq!(lv.mip[0][q], lv.h[q].max(lv.water[q]));
                assert!(lv.water[q] == f32::NEG_INFINITY || lv.water[q] > lv.h[q]);
            }
        }
    }

    #[test]
    fn deterministic() {
        let gen = WorldGen::new(7);
        let (cx, cz) = land_centre(&gen);
        let p = params(cx, cz, 96, 48, 48, 32, 2);
        let a = build_world(&gen, &p);
        let b = build_world(&WorldGen::new(7), &p);
        assert_eq!(a.l0.span_count(), b.l0.span_count());
        for k in 0..a.l0.top.len() {
            assert_eq!(a.l0.spans_at_index(k), b.l0.spans_at_index(k));
        }
        for (la, lb) in a.levels.iter().zip(&b.levels) {
            assert_eq!(la.h, lb.h);
            assert_eq!(la.water, lb.water);
            assert_eq!(la.mat, lb.mat);
            assert_eq!(la.albedo, lb.albedo);
        }
    }

    /// Two builds whose chunk / tile / lattice-cell borders fall at different places must agree
    /// exactly on their overlap (away from the level-0 edges where features are clipped):
    /// proves seamlessness across chunk borders.
    #[test]
    fn seams_independent_of_chunking() {
        for seed in [42u64, 2026] {
            let gen = WorldGen::new(seed);
            let (cx, cz) = land_centre(&gen);
            // 160 = one full 128-column tile + a partial one: tile, strip, chunk and lattice
            // borders all fall at different world positions in the two builds
            let pa = params(cx, cz, 160, 100_000, 100_000, 16, 0);
            let pb = params(cx + 37, cz - 21, 160, 100_000, 100_000, 16, 0);
            let a = build_world(&gen, &pa);
            let b = build_world(&gen, &pb);
            let m = MAX_FEATURE_RADIUS + 1;
            let (x0, x1) = (a.l0.origin_x.max(b.l0.origin_x) + m, (a.l0.origin_x + 160).min(b.l0.origin_x + 160) - m);
            let (z0, z1) = (a.l0.origin_z.max(b.l0.origin_z) + m, (a.l0.origin_z + 160).min(b.l0.origin_z + 160) - m);
            let mut n = 0;
            for z in z0..z1 {
                for x in x0..x1 {
                    assert_eq!(a.l0.spans(x, z), b.l0.spans(x, z), "seed {seed}: column ({x},{z}) differs");
                    n += 1;
                }
            }
            eprintln!("seed {seed}: {n} columns identical (x {x0}..{x1}, z {z0}..{z1})");
            assert!(n > 3000);
        }
    }

    #[test]
    fn water_never_above_water_level() {
        for seed in [1u64, 42] {
            let gen = WorldGen::new(seed);
            // a coastal / lake-ish centre: first wet column of a scan
            let mut c0 = (0, 0);
            'scan: for j in -10..=10 {
                for i in -10..=10 {
                    let c = gen.column(i as f64 * 300.0 + 0.5, j as f64 * 300.0 + 0.5);
                    if c.water_level != i32::MIN && (c.water_level as f32 - c.height) < 4.0 && c.water_level as f32 > c.height {
                        c0 = (i * 300, j * 300);
                        break 'scan;
                    }
                }
            }
            let w = build_world(&gen, &params(c0.0, c0.1, 128, 64, 64, 16, 0));
            let mut wet_cols = 0;
            for j in 0..128 {
                for i in 0..128 {
                    let (x, z) = (w.l0.origin_x + i, w.l0.origin_z + j);
                    let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                    let s = w.l0.spans(x, z);
                    // highest terrain (non-liquid, non-feature) block
                    let terrain_top = s
                        .iter()
                        .filter(|sp| !matches!(sp.mat, Material::Water | Material::Ice | Material::Lava) && !sp.mat.is_leaves() && !sp.mat.is_log())
                        .map(|sp| sp.y1 as i32)
                        .max()
                        .unwrap_or(0);
                    for sp in s {
                        if matches!(sp.mat, Material::Water | Material::Ice) {
                            if sp.y0 as i32 >= terrain_top {
                                wet_cols += 1;
                                assert!(col.water_level != i32::MIN, "surface water in dry column ({x},{z}): {s:?}");
                                assert!(sp.y1 as i32 <= col.water_level, "water above water_level at ({x},{z}): {s:?} wl {}", col.water_level);
                            } else {
                                // cave / aquifer water: below terrain and below sea level
                                assert!((sp.y1 as i32) <= col.water_level.max(SEA_LEVEL), "cave water too high at ({x},{z}): {s:?}");
                            }
                        }
                    }
                }
            }
            assert!(wet_cols > 0, "seed {seed}: test centre has no surface water");
        }
    }

    /// Without caves/features and where the band is inactive, level 0 is the exact heightfield
    /// ⌈H − ½⌉; inside the band the residual lattice matches the exact density almost everywhere.
    #[test]
    fn band_matches_exact_density() {
        let gen = WorldGen::new(42);
        // find a jagged area
        let mut c0 = (0, 0, 0.0f32);
        for j in -15..=15 {
            for i in -15..=15 {
                let c = gen.column(i as f64 * 500.0 + 0.5, j as f64 * 500.0 + 0.5);
                if c.jaggedness > c0.2 {
                    c0 = (i * 500, j * 500, c.jaggedness);
                }
            }
        }
        let w = build_world(&gen, &params(c0.0, c0.1, 64, 0, 0, 16, 0));
        let (mut tot, mut bad, mut band_blocks) = (0usize, 0usize, 0usize);
        for j in 0..64 {
            for i in 0..64 {
                let (x, z) = (w.l0.origin_x + i, w.l0.origin_z + j);
                let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
                let (nx, nz) = (x.div_euclid(4) * 4, z.div_euclid(4) * 4);
                let corner_jag = [(0, 0), (4, 0), (0, 4), (4, 4)]
                    .iter()
                    .map(|&(a, b)| gen.column((nx + a) as f64 + 0.5, (nz + b) as f64 + 0.5).jaggedness)
                    .fold(0.0f32, f32::max);
                if corner_jag <= 0.0 {
                    let solid_top = w.l0.spans(x, z).iter().filter(|s| s.mat.is_opaque()).map(|s| s.y1 as i32).max().unwrap();
                    assert_eq!(solid_top, ((col.height - 0.5).ceil() as i32).clamp(1, WORLD_HEIGHT), "({x},{z})");
                }
                let j = col.jaggedness.max(0.0) + 2.0;
                for y in ((col.height - j) as i32).max(1)..((col.height + j) as i32 + 1).min(WORLD_HEIGHT) {
                    let exact = gen.density(x as f64 + 0.5, y as f64 + 0.5, z as f64 + 0.5, &col) > 0.0;
                    let got = w.l0.block(x, y, z);
                    let got_solid = got.is_opaque() && got != Material::Lava;
                    tot += 1;
                    if col.jaggedness > 0.0 {
                        band_blocks += 1;
                    }
                    if exact != got_solid && got != Material::Water && got != Material::Ice {
                        bad += 1;
                    }
                }
            }
        }
        eprintln!("band check: {bad}/{tot} mismatches ({band_blocks} band blocks, max J {})", c0.2);
        assert!((bad as f64) < 0.04 * tot as f64, "{bad}/{tot}");
    }

    #[test]
    fn features_respect_terrain() {
        let gen = WorldGen::new(42);
        // a forested centre: highest canopy coverage of a scan
        let mut c0 = (0, 0, 0.0f32);
        for j in -12..=12 {
            for i in -12..=12 {
                let (x, z) = (i as f64 * 400.0 + 0.5, j as f64 * 400.0 + 0.5);
                let c = gen.column(x, z);
                let cov = gen.canopy_lod(x, z, &c, 1.0).coverage;
                if cov > c0.2 {
                    c0 = (i * 400, j * 400, cov);
                }
            }
        }
        let p1 = params(c0.0, c0.1, 96, 0, 1000, 16, 0);
        let p0 = BuildParams { feature_radius: 0, ..p1 };
        let w1 = build_world(&gen, &p1);
        let w0 = build_world(&gen, &p0);
        // replace_solid runs, recomputed independently from the bare terrain
        let (ox, oz) = (w0.l0.origin_x, w0.l0.origin_z);
        let m = MAX_FEATURE_RADIUS;
        let feats = gen.features_in(ox - m, oz - m, ox + 96 + m, oz + 96 + m);
        let mut repl: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut any_feature_blocks = 0usize;
        for f in &feats {
            let base = if w0.l0.contains(f.root_x, f.root_z) {
                let s = w0.l0.spans(f.root_x, f.root_z);
                let Some(top) = s.last() else { continue };
                if !top.mat.is_opaque() {
                    continue;
                }
                top.y1 as i32
            } else {
                exact_root_y(&gen, f.root_x, f.root_z) as i32
            };
            for c in &f.columns {
                if c.replace_solid {
                    repl.push((f.root_x + c.dx as i32, f.root_z + c.dz as i32, base + c.y0 as i32, base + c.y1 as i32));
                }
            }
        }
        for j in 0..96 {
            for i in 0..96 {
                let (x, z) = (ox + i, oz + j);
                for y in 0..WORLD_HEIGHT {
                    let a = w0.l0.block(x, y, z);
                    let b = w1.l0.block(x, y, z);
                    if a == b {
                        continue;
                    }
                    any_feature_blocks += 1;
                    if a != Material::Air {
                        let ok = repl.iter().any(|&(rx, rz, y0, y1)| rx == x && rz == z && y >= y0 && y < y1);
                        assert!(ok, "feature overwrote {a:?} with {b:?} at ({x},{y},{z}) without replace_solid");
                    }
                }
            }
        }
        eprintln!("feature blocks: {any_feature_blocks} (coverage {})", c0.2);
        assert!(any_feature_blocks > 0 || feats.is_empty());
    }

    /// Level 1 continues level 0 without a systematic step: the inner level-1 cells (box-filtered
    /// voxels, unbiased surface ⌈H − ½⌉) agree in the mean with what the outer cells use
    /// (`column_lod` at the same cell). Canopy is excluded (it is the features' statistical model).
    #[test]
    fn heightfield_continuity_at_level0_boundary() {
        for seed in [1u64, 2026] {
            let gen = WorldGen::new(seed);
            let (cx, cz) = land_centre(&gen);
            let w = build_world(&gen, &params(cx, cz, 128, 0, 0, 128, 2));
            let lv = &w.levels[0];
            let (mut sum, mut sum2, mut n) = (0.0f64, 0.0f64, 0usize);
            let (mut all_sum, mut all_n) = (0.0f64, 0usize);
            let (mut wet_mismatch, mut wet) = (0usize, 0usize);
            for j in 0..lv.size as i32 {
                for i in 0..lv.size as i32 {
                    let (x, z) = (lv.origin_x + 2 * i, lv.origin_z + 2 * j);
                    if !(w.l0.contains(x, z) && w.l0.contains(x + 1, z + 1)) {
                        continue;
                    }
                    let q = lv.index(i, j);
                    let col = gen.column_lod(x as f64 + 1.0, z as f64 + 1.0, 2.0);
                    let lod_wet = col.water_level != i32::MIN && col.water_level as f32 > col.height;
                    if lod_wet || lv.water[q] > lv.h[q] {
                        wet += 1;
                        if lod_wet != (lv.water[q] > lv.h[q]) {
                            wet_mismatch += 1;
                        } else {
                            assert_eq!(lv.water[q], col.water_level as f32, "water surface mismatch at ({x},{z})");
                        }
                        continue;
                    }
                    // (3-D band columns legitimately differ: overhangs are not a heightfield)
                    let d = (lv.h[q] - col.height) as f64;
                    all_sum += d;
                    all_n += 1;
                    if col.jaggedness > 0.0 {
                        continue;
                    }
                    sum += d;
                    sum2 += d * d;
                    n += 1;
                }
            }
            let mean = sum / n.max(1) as f64;
            let rms = (sum2 / n.max(1) as f64).sqrt();
            let all_mean = all_sum / all_n.max(1) as f64;
            eprintln!(
                "seed {seed}: voxel-cells − lod: mean {mean:.3}, rms {rms:.3} (n {n}); incl. band {all_mean:.3} (n {all_n}); wet {wet}, wet mismatch {wet_mismatch}"
            );
            assert!(all_n > 1000);
            assert!(all_mean.abs() < 0.5, "biased level-0/level-1 heights: {all_mean}");
            if n > 200 {
                assert!(mean.abs() < 0.2, "biased level-0/level-1 heights: {mean}");
                assert!(rms < 1.0, "level-0/level-1 heights disagree: rms {rms}");
            }
            assert!(wet_mismatch * 10 <= wet.max(10), "{wet_mismatch}/{wet}");
        }
    }
    /// Sizes that are not multiples of the chunk / lattice / tile size, no heightfield levels.
    #[test]
    fn odd_sizes_and_voxels_only() {
        let gen = WorldGen::new(2026);
        let (cx, cz) = land_centre(&gen);
        for (l0, hf, lv) in [(100u32, 50u32, 1u32), (37, 16, 0), (130, 64, 2)] {
            let w = build_world(&gen, &params(cx + 3, cz - 5, l0, l0 / 2, l0, hf, lv));
            assert_eq!(w.l0.size, l0);
            assert_eq!(w.levels.len(), lv as usize);
            check_spans(&w.l0);
        }
    }

    /// Caves are carved inside the cave radius only; outside it the columns are bit-identical to
    /// a cave-free build.
    #[test]
    fn caves_only_inside_radius() {
        let mut carved_total = 0usize;
        for seed in [1u64, 42, 2026] {
            let gen = WorldGen::new(seed);
            let (cx, cz) = land_centre(&gen);
            let r = 56i32;
            let a = build_world(&gen, &params(cx, cz, 160, r as u32, 0, 16, 0));
            let b = build_world(&gen, &params(cx, cz, 160, 0, 0, 16, 0));
            for j in 0..160 {
                for i in 0..160 {
                    let (x, z) = (a.l0.origin_x + i, a.l0.origin_z + j);
                    let d2 = (x as f64 + 0.5 - cx as f64).powi(2) + (z as f64 + 0.5 - cz as f64).powi(2);
                    let (sa, sb) = (a.l0.spans(x, z), b.l0.spans(x, z));
                    if d2 > (r * r) as f64 {
                        assert_eq!(sa, sb, "seed {seed}: column ({x},{z}) outside the cave radius changed");
                        continue;
                    }
                    // carved = opaque in the cave-free build, liquid/air with caves
                    let top = sb.iter().filter(|s| s.mat.is_opaque()).map(|s| s.y1 as i32).max().unwrap_or(0);
                    for y in 0..top {
                        let (ma, mb) = (a.l0.block(x, y, z), b.l0.block(x, y, z));
                        if mb.is_opaque() && !ma.is_opaque() {
                            carved_total += 1;
                            assert!(y >= MIN_CARVE_Y, "carved below y = {MIN_CARVE_Y}");
                        }
                    }
                }
            }
        }
        eprintln!("carved blocks: {carved_total}");
        assert!(carved_total > 1000, "no caves carved at all");
    }
}
