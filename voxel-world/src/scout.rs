//! Automatic location scouting and shot planning (DESIGN §8). OWNER: scout-main.
//!
//! [`plan_shots`] returns exactly eight [`Shot`]s — `panorama`, `river_valley`, `coast_sunset`,
//! `drylands`, `alpine`, `forest`, `cutaway`, `orbital` — chosen without any human input from
//! the pure generator functions of [`WorldGen`]. Everything is a deterministic function of the
//! seed (no randomness at all; parallel loops only evaluate pure functions and are collected in
//! index order).
//!
//! # Mathematics
//!
//! ## 1. Coarse scan
//! A square lattice `Λ = {(x₀ + i·s, z₀ + j·s)}`, `s = 48`, of radius 12 288 blocks around the
//! origin (513² ≈ 263 k nodes) is filled in parallel with band-limited columns
//! `column_lod(·, s)`. Every node keeps its column, the visible surface
//! `S = max(H, water level)` and a category bit-mask ([`cat`]: water, ocean, river, snow,
//! forest, badlands, …).
//!
//! ## 2. Derived lattice fields
//! * **Window fractions** of every category through summed-area tables
//!   `I(i,j) = Σ_{i'<i, j'<j} 1_cat`: a box count is `I(i₁,j₁) − I(i₀,j₁) − I(i₁,j₀) + I(i₀,j₀)`,
//!   O(1) per query.
//! * **Local max/min filters** (separable: the max over a square is the max of row maxima) →
//!   local relief `R_r = max_r S − min_r S`, valley depth `max_r S − S`, cliff height, …
//! * **Topographic prominence** of every summit, exact on the lattice, by a union–find sweep in
//!   decreasing `S` (the merge tree of the super-level sets `{S ≥ t}`): when a node joins two
//!   components it is the *key col* of the lower component's peak `p`, whose prominence is
//!   `S(p) − S(col)`. Summits are ranked by height and prominence, not by height alone, so a
//!   panorama is taken from a mountain that really stands out.
//!
//! ## 3. Viewsheds (what a camera would actually see)
//! From a camera `c`, a ray at azimuth θ marches outward with geometric steps
//! `Δd = max(3, 0.022·d)` (constant angular precision), sampling `S` (exact full-resolution
//! columns for `d < 128`, the lattice bilinearly, `column_lod` with footprint ∝ d beyond it),
//! lowered by Earth curvature `d²/2R`. A sample is visible iff its elevation angle
//! `α = atan((S − d²/2R − c_y)/d)` exceeds the running maximum of all previous ones (the 2-D
//! horizon algorithm); it then occupies the angular interval `[α_max, α]` of that image column.
//! The pinhole projection `s_y = tan(α − pitch) / (cos φ · tan(fov_y/2))` (φ = azimuth offset of
//! the image column) maps these intervals to exact screen extents, which gives frame-area
//! coverages of every category, the sky fraction, the area-weighted variance of the visible
//! heights (*relief*), the number of ridge layers (hidden → visible transitions — layered ridges
//! are what aerial perspective turns into depth), near occluders, and the Shannon entropy of
//! the land-cover classes in the frame (variety). 128 azimuths are cast once per camera and
//! reused for every candidate yaw (a 360° viewshed). The pitch is solved so that the mean
//! horizon (or a target summit) lands on a chosen screen line (rule of thirds).
//!
//! ## 4. Scoring, refinement, separation
//! Each shot type ranks lattice candidates by a terrain score (prominence, valley depth, cliff
//! height, biome window fractions, cave volume…), refines the best ones at full resolution
//! (multi-scale hill climbing for summits, river centre-lines from the river field, principal
//! axis of the river cells by 2×2 PCA for the valley direction), places the camera with an exact
//! clearance test (3-D density band resolved per column, real tree tops from the feature point
//! process) and picks the yaw maximising a view score. Shots are then chosen greedily so that
//! cameras are ≥ 1500 blocks apart when possible, with graceful fallbacks (relaxed distance,
//! substitute biomes, a generic "best view" planner) so that exactly eight shots always exist.
//!
//! ## 5. Lighting choices
//! Side light for the panorama, golden hour for the coast (sun 3–6° high, ±22° off the view
//! direction over the sea so its glitter path is in frame), grazing light (≈ 9°) for drylands,
//! low morning light for alpine peaks; the side is chosen so that the sun is not hidden behind
//! the camera's own horizon (the viewshed gives the horizon angle at the sun azimuth).
use crate::gen::{Biome, ColumnInfo, WorldGen, MAX_FEATURE_RADIUS, SEA_LEVEL};
use crate::noise::{derive_seed, hash_f01, mix64, salt};
use crate::render::{Camera, Sun, ViewMode};
use crate::shots::Shot;
use crate::world::BuildParams;
use rayon::prelude::*;
use std::f64::consts::PI;

/// Scan radius around the origin (blocks).
pub const SCOUT_RADIUS: f64 = 12_288.0;
/// Coarse lattice step (blocks).
pub const SCOUT_STEP: f64 = 48.0;
/// Preferred minimum horizontal distance between two shot cameras.
pub const MIN_SEPARATION: f64 = 1500.0;
/// Planet radius used for the curvature drop d²/2R.
pub const EARTH_RADIUS: f64 = 6_371_000.0;
/// Image aspect ratio assumed while scouting.
pub const ASPECT: f64 = 16.0 / 9.0;

/// Default world-build parameters of a normal shot.
pub const L0_SIZE: u32 = 2048;
pub const HF_SIZE: u32 = 2048;
pub const HF_LEVELS: u32 = 5;
pub const CAVE_RADIUS: u32 = 448;
pub const FEATURE_RADIUS: u32 = 1024;
/// Orbital shot: heightfield levels (cell 64 at level 6 → 65 536-block radius).
pub const ORBITAL_HF_LEVELS: u32 = 6;
pub const ORBITAL_L0_SIZE: u32 = 512;
pub const ORBITAL_ALTITUDE: f64 = 3000.0;
pub const ORBITAL_PITCH_DEG: f64 = -27.0;
/// Side of the cut-away diorama box.
pub const CUTAWAY_SIZE: i32 = 176;
/// Fraction of the level-0 half-size by which the clipmap centre is pushed ahead of the camera
/// (more exact voxels in front of the camera, where they are seen).
pub const FORWARD_SHIFT: f64 = 0.3;

const SEA: f32 = SEA_LEVEL as f32;
const N_AZ: usize = 128;
const NEAR_EXACT: f64 = 128.0;
const VIEW_RANGE: f64 = 22_000.0;
const DEG: f64 = PI / 180.0;

/// Shot names in index order (index = position + 1).
pub const SHOT_NAMES: [&str; 8] =
    ["panorama", "river_valley", "coast_sunset", "drylands", "alpine", "forest", "cutaway", "orbital"];

/// Land-cover category bits stored per lattice node / visible piece.
pub mod cat {
    pub const WATER: u16 = 1 << 0;
    pub const OCEAN: u16 = 1 << 1;
    pub const RIVER: u16 = 1 << 2;
    pub const LAND: u16 = 1 << 3;
    pub const SNOW: u16 = 1 << 4;
    pub const PEAK: u16 = 1 << 5;
    pub const RICH_FOREST: u16 = 1 << 6;
    pub const FOREST: u16 = 1 << 7;
    pub const BADLANDS: u16 = 1 << 8;
    pub const DESERT: u16 = 1 << 9;
    pub const SAVANNA: u16 = 1 << 10;
    pub const GRASS: u16 = 1 << 11;
    pub const SHORE: u16 = 1 << 12;
    pub const ROCK: u16 = 1 << 13;
    pub const VOLCANIC: u16 = 1 << 14;
    pub const HOODOOS: u16 = 1 << 15;
    pub const COUNT: usize = 16;
    pub const DRY: u16 = BADLANDS | DESERT | SAVANNA;
    /// Bit index of a single-bit mask.
    pub const fn bit(m: u16) -> usize {
        m.trailing_zeros() as usize
    }
}

#[inline]
fn is_wet(c: &ColumnInfo) -> bool {
    c.water_level != i32::MIN && c.water_level as f32 > c.height
}

/// Visible surface: water surface when submerged, else terrain height.
#[inline]
pub fn surface_of(c: &ColumnInfo) -> f32 {
    if is_wet(c) {
        c.water_level as f32
    } else {
        c.height
    }
}

/// Category bit-mask of a column.
pub fn column_mask(c: &ColumnInfo) -> u16 {
    use Biome::*;
    let mut m = if is_wet(c) { cat::WATER } else { cat::LAND };
    let b = c.biome;
    if b.is_ocean() {
        m |= cat::OCEAN | cat::WATER;
    }
    m |= match b {
        River | FrozenRiver => cat::RIVER,
        SnowySlopes | SnowyPeaks | Glacier | SnowyPlains | SnowyBeach => cat::SNOW,
        SnowyTaiga | AlpineGrove => cat::SNOW | cat::FOREST,
        Jungle | DarkForest | OldGrowthTaiga => cat::RICH_FOREST | cat::FOREST,
        Forest | BirchForest | Taiga | Swamp => cat::FOREST,
        Badlands => cat::BADLANDS,
        Hoodoos => cat::BADLANDS | cat::HOODOOS,
        Desert => cat::DESERT,
        Savanna => cat::SAVANNA,
        Plains | Meadow => cat::GRASS,
        Beach | StonyShore => cat::SHORE,
        StonyPeaks => cat::ROCK,
        Volcanic => cat::VOLCANIC,
        _ => 0,
    };
    if c.height > 380.0 {
        m |= cat::PEAK;
    }
    m
}

/// Exclusive land-cover class for the entropy measure: water, forest, dry, snow/ice, open
/// grass/shore, bare rock & other.
#[inline]
fn cover_class(m: u16) -> usize {
    if m & cat::WATER != 0 {
        0
    } else if m & cat::SNOW != 0 {
        3
    } else if m & cat::FOREST != 0 {
        1
    } else if m & cat::DRY != 0 {
        2
    } else if m & (cat::GRASS | cat::SHORE) != 0 {
        4
    } else {
        5
    }
}

// =====================================================================================
// 1. Coarse lattice
// =====================================================================================

/// The coarse scouting lattice (row-major, node (i, j) at (x0 + i·step, z0 + j·step)).
pub struct ScoutGrid {
    pub x0: f64,
    pub z0: f64,
    pub step: f64,
    pub n: usize,
    pub cols: Vec<ColumnInfo>,
    pub surf: Vec<f32>,
    pub mask: Vec<u16>,
}

impl ScoutGrid {
    /// Parallel scan of `column_lod(·, step)` over a square of half-side `radius`.
    pub fn scan(gen: &WorldGen, cx: f64, cz: f64, radius: f64, step: f64) -> ScoutGrid {
        let n = (2.0 * radius / step).round() as usize + 1;
        let (x0, z0) = (cx - radius, cz - radius);
        let cols: Vec<ColumnInfo> = (0..n * n)
            .into_par_iter()
            .map(|k| gen.column_lod(x0 + (k % n) as f64 * step, z0 + (k / n) as f64 * step, step))
            .collect();
        let surf = cols.iter().map(surface_of).collect();
        let mask = cols.iter().map(column_mask).collect();
        ScoutGrid { x0, z0, step, n, cols, surf, mask }
    }

    #[inline]
    pub fn pos(&self, k: usize) -> (f64, f64) {
        (self.x0 + (k % self.n) as f64 * self.step, self.z0 + (k / self.n) as f64 * self.step)
    }

    #[inline]
    pub fn ij(&self, k: usize) -> (usize, usize) {
        (k % self.n, k / self.n)
    }

    /// Nearest node index.
    pub fn nearest(&self, x: f64, z: f64) -> Option<usize> {
        let i = ((x - self.x0) / self.step).round();
        let j = ((z - self.z0) / self.step).round();
        if i < 0.0 || j < 0.0 || i >= self.n as f64 || j >= self.n as f64 {
            return None;
        }
        Some(j as usize * self.n + i as usize)
    }

    /// Bilinear surface + nearest category mask; `None` outside the lattice.
    #[inline]
    pub fn sample(&self, x: f64, z: f64) -> Option<(f32, u16)> {
        let gx = (x - self.x0) / self.step;
        let gz = (z - self.z0) / self.step;
        let lim = (self.n - 1) as f64;
        if !(gx >= 0.0 && gz >= 0.0 && gx <= lim && gz <= lim) {
            return None;
        }
        let i0 = (gx.floor() as usize).min(self.n - 2);
        let j0 = (gz.floor() as usize).min(self.n - 2);
        let fx = (gx - i0 as f64) as f32;
        let fz = (gz - j0 as f64) as f32;
        let k = j0 * self.n + i0;
        let s = &self.surf;
        let a = s[k] + (s[k + 1] - s[k]) * fx;
        let b = s[k + self.n] + (s[k + self.n + 1] - s[k + self.n]) * fx;
        let km = (gz.round() as usize) * self.n + gx.round() as usize;
        Some((a + (b - a) * fz, self.mask[km]))
    }
}

/// Summed-area table of a boolean lattice field.
struct Sat {
    n: usize,
    s: Vec<u32>,
}

impl Sat {
    fn new(n: usize, f: impl Fn(usize) -> bool) -> Sat {
        let w = n + 1;
        let mut s = vec![0u32; w * w];
        for j in 0..n {
            let mut row = 0u32;
            for i in 0..n {
                row += f(j * n + i) as u32;
                s[(j + 1) * w + i + 1] = s[j * w + i + 1] + row;
            }
        }
        Sat { n, s }
    }
    /// Fraction of true nodes in the (clipped) window of radius r around (i, j).
    fn frac(&self, i: usize, j: usize, r: usize) -> f32 {
        let w = self.n + 1;
        let (i0, j0) = (i.saturating_sub(r), j.saturating_sub(r));
        let (i1, j1) = ((i + r + 1).min(self.n), (j + r + 1).min(self.n));
        let sum = self.s[j1 * w + i1] + self.s[j0 * w + i0] - self.s[j0 * w + i1] - self.s[j1 * w + i0];
        sum as f32 / ((i1 - i0) * (j1 - j0)) as f32
    }
}

/// Separable square-window max (or min) filter of radius r.
fn window_reduce(src: &[f32], n: usize, r: usize, max: bool) -> Vec<f32> {
    let pick = |a: f32, b: f32| if max { a.max(b) } else { a.min(b) };
    let init = if max { f32::NEG_INFINITY } else { f32::INFINITY };
    let mut tmp = vec![0f32; n * n];
    tmp.par_chunks_mut(n).enumerate().for_each(|(j, row)| {
        for i in 0..n {
            let mut v = init;
            for ii in i.saturating_sub(r)..(i + r + 1).min(n) {
                v = pick(v, src[j * n + ii]);
            }
            row[i] = v;
        }
    });
    let mut out = vec![0f32; n * n];
    out.par_chunks_mut(n).enumerate().for_each(|(j, row)| {
        for i in 0..n {
            let mut v = init;
            for jj in j.saturating_sub(r)..(j + r + 1).min(n) {
                v = pick(v, tmp[jj * n + i]);
            }
            row[i] = v;
        }
    });
    out
}

/// Exact lattice prominence by a union–find sweep in decreasing height (8-connectivity).
/// Returns, for every node, the prominence of the summit at that node (0 for non-summits).
pub fn prominence(surf: &[f32], n: usize) -> Vec<f32> {
    const NONE: u32 = u32::MAX;
    let mut order: Vec<u32> = (0..(n * n) as u32).collect();
    order.sort_unstable_by(|&a, &b| surf[b as usize].total_cmp(&surf[a as usize]).then(a.cmp(&b)));
    let mut parent = vec![NONE; n * n];
    let mut peak = vec![0u32; n * n];
    let mut prom = vec![0f32; n * n];
    fn find(parent: &mut [u32], mut a: u32) -> u32 {
        let mut root = a;
        while parent[root as usize] != root {
            root = parent[root as usize];
        }
        while parent[a as usize] != root {
            let next = parent[a as usize];
            parent[a as usize] = root;
            a = next;
        }
        root
    }
    let min_s = surf.iter().cloned().fold(f32::INFINITY, f32::min);
    for &k in &order {
        let ku = k as usize;
        parent[ku] = k;
        peak[ku] = k;
        let (i, j) = ((ku % n) as i64, (ku / n) as i64);
        for (di, dj) in [(-1i64, -1i64), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (ii, jj) = (i + di, j + dj);
            if ii < 0 || jj < 0 || ii >= n as i64 || jj >= n as i64 {
                continue;
            }
            let nb = (jj as usize * n + ii as usize) as u32;
            if parent[nb as usize] == NONE {
                continue;
            }
            let ra = find(&mut parent, k);
            let rb = find(&mut parent, nb);
            if ra == rb {
                continue;
            }
            let (pa, pb) = (peak[ra as usize], peak[rb as usize]);
            let a_higher = surf[pa as usize] > surf[pb as usize]
                || (surf[pa as usize] == surf[pb as usize] && pa < pb);
            let (hi, lo) = if a_higher { (ra, rb) } else { (rb, ra) };
            let lp = peak[lo as usize] as usize;
            prom[lp] = surf[lp] - surf[ku];
            parent[lo as usize] = hi;
        }
    }
    // the global summit(s) of each remaining component: prominence relative to the lowest node
    for k in 0..n * n {
        if parent[k] == k as u32 && peak[k] as usize == k {
            prom[k] = surf[k] - min_s;
        }
    }
    prom
}

// =====================================================================================
// 2. Exact local probes (full resolution)
// =====================================================================================

/// Exclusive top of the highest solid block of a column (3-D density band resolved exactly), or
/// the liquid surface if higher.
pub fn solid_top(gen: &WorldGen, x: f64, z: f64) -> f32 {
    let c = gen.column(x, z);
    let mut top = (c.height - 0.5).ceil();
    if c.jaggedness > 0.25 {
        let hi = (c.height + c.jaggedness + 2.0).ceil() as i32;
        let lo = (c.height - c.jaggedness - 2.0).floor() as i32;
        let mut y = hi;
        while y >= lo {
            if gen.density(x, y as f64 + 0.5, z, &c) > 0.0 {
                top = top.max(y as f32 + 1.0);
                break;
            }
            y -= 1;
        }
    }
    if c.water_level != i32::MIN {
        top = top.max(c.water_level as f32);
    }
    top
}

/// Max of [`solid_top`] over a disc of radius r (rings of samples ≈ 3 blocks apart).
pub fn clearance(gen: &WorldGen, x: f64, z: f64, r: f64) -> f32 {
    let mut top = solid_top(gen, x, z);
    let rings = (r / 3.0).ceil().max(1.0) as usize;
    for ring in 1..=rings {
        let rr = r * ring as f64 / rings as f64;
        let m = ((2.0 * PI * rr / 3.0).ceil() as usize).max(6);
        for q in 0..m {
            let a = 2.0 * PI * (q as f64 + 0.5 * (ring % 2) as f64) / m as f64;
            top = top.max(solid_top(gen, x + rr * a.sin(), z + rr * a.cos()));
        }
    }
    top
}

/// Highest feature (tree, boulder…) block top over a disc of radius r, exact from the feature
/// point process (`f32::MIN` when there is none).
pub fn feature_top(gen: &WorldGen, x: f64, z: f64, r: f64) -> f32 {
    let ri = r.ceil() as i32 + MAX_FEATURE_RADIUS;
    let (xi, zi) = (x.floor() as i32, z.floor() as i32);
    let mut top = f32::MIN;
    for f in gen.features_in(xi - ri, zi - ri, xi + ri + 1, zi + ri + 1) {
        let mut ground: Option<f32> = None;
        for c in &f.columns {
            let dx = (f.root_x + c.dx as i32) as f64 + 0.5 - x;
            let dz = (f.root_z + c.dz as i32) as f64 + 0.5 - z;
            if dx * dx + dz * dz > (r + 0.75) * (r + 0.75) {
                continue;
            }
            let g = *ground.get_or_insert_with(|| solid_top(gen, f.root_x as f64 + 0.5, f.root_z as f64 + 0.5));
            top = top.max(g + c.y1 as f32);
        }
    }
    top
}

/// Safe camera height over (x, z): above terrain, liquids and trees within radius r.
pub fn camera_floor(gen: &WorldGen, x: f64, z: f64, r: f64) -> f32 {
    clearance(gen, x, z, r).max(feature_top(gen, x, z, r))
}

/// Multi-scale hill climb to the full-resolution summit near (x, z). Returns block-centre coords.
pub fn refine_summit(gen: &WorldGen, x: f64, z: f64) -> (f64, f64, f32) {
    let (x, z) = (x.floor() + 0.5, z.floor() + 0.5);
    let mut best = (x, z, gen.column(x, z).height);
    for &(step, r) in &[(12.0f64, 5i32), (4.0, 3), (1.0, 3)] {
        let (bx, bz, _) = best;
        for j in -r..=r {
            for i in -r..=r {
                let (px, pz) = (bx + i as f64 * step, bz + j as f64 * step);
                let h = gen.column(px, pz).height;
                if h > best.2 {
                    best = (px, pz, h);
                }
            }
        }
    }
    best
}

// =====================================================================================
// 3. Viewsheds
// =====================================================================================

/// A visible piece of terrain in one image column: elevation interval [a0, a1] (radians).
#[derive(Clone, Copy, Debug)]
struct Piece {
    a0: f32,
    a1: f32,
    d: f32,
    h: f32,
    mask: u16,
}

#[derive(Clone, Debug, Default)]
struct RayProfile {
    pieces: Vec<Piece>,
    /// Final horizon elevation angle (radians).
    horizon: f32,
    /// Hidden → visible transitions beyond 300 m (ridge layers).
    layers: f32,
    /// Distance of the last visible piece (the horizon-forming terrain).
    hdist: f32,
}

/// Terrain oracle for the ray marcher.
struct Terrain<'a> {
    gen: &'a WorldGen,
    grid: &'a ScoutGrid,
}

impl Terrain<'_> {
    #[inline]
    fn sample(&self, x: f64, z: f64, d: f64) -> (f32, u16) {
        if d < NEAR_EXACT {
            // exact column + expected canopy (near trees are the commonest occluders)
            let c = self.gen.column(x, z);
            let mut h = surface_of(&c);
            if !is_wet(&c) {
                let cl = self.gen.canopy_lod(x, z, &c, 1.0);
                if cl.coverage > 0.15 {
                    h += cl.height * (cl.coverage * 1.6).min(1.0);
                }
            }
            return (h, column_mask(&c));
        }
        if let Some(s) = self.grid.sample(x, z) {
            return s;
        }
        let c = self.gen.column_lod(x, z, (d * 0.012).max(SCOUT_STEP));
        (surface_of(&c), column_mask(&c))
    }

    /// Horizon-algorithm ray march (see module docs).
    fn cast(&self, o: [f64; 3], az: f64, d0: f64, dmax: f64) -> RayProfile {
        let (sa, ca) = az.sin_cos();
        let mut amax = -1.5f32;
        let mut d = d0;
        let mut hidden = false;
        let mut layers = 0.0f32;
        let mut hdist = d0 as f32;
        let mut pieces = Vec::with_capacity(96);
        while d < dmax {
            let (h, m) = self.sample(o[0] + sa * d, o[2] + ca * d, d);
            let he = h as f64 - d * d / (2.0 * EARTH_RADIUS);
            let a = ((he - o[1]) / d).atan() as f32;
            if a > amax {
                if hidden && d > 300.0 && a - amax > 0.002 {
                    layers += 1.0;
                }
                pieces.push(Piece { a0: amax, a1: a, d: d as f32, h, mask: m });
                amax = a;
                hdist = d as f32;
                hidden = false;
            } else {
                hidden = true;
            }
            d += (d * 0.022).max(3.0);
        }
        RayProfile { pieces, horizon: amax, layers, hdist }
    }
}

/// How the pitch of a view is chosen.
#[derive(Clone, Copy, Debug)]
pub enum PitchRule {
    /// Fixed pitch (radians).
    Fixed(f64),
    /// Mean horizon lands on screen line `sy` (−1 bottom … +1 top); clamped to [min, max] (rad).
    Horizon { sy: f64, min: f64, max: f64 },
    /// A target elevation angle (rad) lands on screen line `sy`.
    Target { angle: f64, sy: f64, min: f64, max: f64 },
}

/// Frame statistics of a candidate view (coverages are fractions of the image area).
#[derive(Clone, Debug, Default)]
pub struct ViewStats {
    pub pitch: f64,
    pub sky: f32,
    pub terrain: f32,
    pub cov: [f32; cat::COUNT],
    /// Coverage restricted to the central third of the image width.
    pub center: [f32; cat::COUNT],
    /// Area-weighted standard deviation of visible surface heights (blocks).
    pub relief: f32,
    pub mean_h: f32,
    /// Mean number of ridge layers per image column.
    pub layers: f32,
    /// Area covered by terrain closer than 150 blocks in the upper ~60 % of the frame.
    pub near_block: f32,
    /// Area covered by terrain closer than 700 blocks in the upper ~40 % of the frame (a wall).
    pub wall: f32,
    /// Coverage of pieces at mid distance (250 … 8000 blocks).
    pub mid: [f32; cat::COUNT],
    /// Mean log₁₀ of the horizon distance over the central third of the image.
    pub center_depth: f32,
    /// Degrees by which the pitch rule had to be clamped.
    pub clamped_deg: f32,
    pub far: f32,
    /// Mean horizon elevation (radians).
    pub horizon: f32,
    /// Normalised Shannon entropy of the six land-cover classes (0 … 1).
    pub entropy: f32,
}

impl ViewStats {
    #[inline]
    pub fn c(&self, m: u16) -> f32 {
        self.cov[cat::bit(m)]
    }
    /// Coverage of any of the bits in `m` (upper bound: sum of single-bit coverages, clamped).
    pub fn any(&self, m: u16) -> f32 {
        let mut s = 0.0;
        for b in 0..cat::COUNT {
            if m & (1 << b) != 0 {
                s += self.cov[b];
            }
        }
        s.min(1.0)
    }
}

fn frame_stats(cols: &[(&RayProfile, f64)], vfov: f64, rule: PitchRule) -> ViewStats {
    let t = (vfov * 0.5).tan();
    let k = cols.len() as f32;
    let horizon = cols.iter().map(|c| c.0.horizon as f64).sum::<f64>() / cols.len() as f64;
    let want = match rule {
        PitchRule::Fixed(p) => p,
        PitchRule::Horizon { sy, .. } => horizon - (sy * t).atan(),
        PitchRule::Target { angle, sy, .. } => angle - (sy * t).atan(),
    };
    let pitch = match rule {
        PitchRule::Fixed(p) => p,
        PitchRule::Horizon { min, max, .. } | PitchRule::Target { min, max, .. } => want.clamp(min, max),
    };
    let mut st = ViewStats { pitch, horizon: horizon as f32, ..Default::default() };
    st.clamped_deg = ((want - pitch).abs() / DEG) as f32;
    let mut ncenter = 0.0f32;
    let (mut sw, mut swh, mut swh2) = (0.0f64, 0.0f64, 0.0f64);
    let mut classes = [0.0f32; 6];
    for (idx, (ray, phi)) in cols.iter().enumerate() {
        let sx = (2 * idx + 1) as f32 / k - 1.0;
        let center = sx.abs() < 0.34;
        let sc = 1.0 / (phi.cos() * t);
        let proj = |a: f32| -> f32 { (((a as f64 - pitch).clamp(-1.5, 1.5)).tan() * sc).clamp(-1.0, 1.0) as f32 };
        for p in &ray.pieces {
            let s0 = proj(p.a0);
            let s1 = proj(p.a1);
            if s1 <= s0 {
                continue;
            }
            let w = (s1 - s0) * 0.5 / k;
            st.terrain += w;
            for b in 0..cat::COUNT {
                if p.mask & (1 << b) != 0 {
                    st.cov[b] += w;
                    if center {
                        st.center[b] += w * 3.0;
                    }
                    if p.d > 250.0 && p.d < 8000.0 {
                        st.mid[b] += w;
                    }
                }
            }
            classes[cover_class(p.mask)] += w;
            sw += w as f64;
            swh += (w * p.h) as f64;
            swh2 += (w * p.h * p.h) as f64;
            if p.d > 4000.0 {
                st.far += w;
            }
            if p.d < 150.0 {
                let (c0, c1) = (s0.max(-0.25), s1);
                if c1 > c0 {
                    st.near_block += (c1 - c0) * 0.5 / k;
                }
            }
            if p.d < 700.0 {
                let (c0, c1) = (s0.max(0.15), s1);
                if c1 > c0 {
                    st.wall += (c1 - c0) * 0.5 / k;
                }
            }
        }
        if center {
            st.center_depth += ray.hdist.max(1.0).log10();
            ncenter += 1.0;
        }
        st.sky += (1.0 - proj(ray.horizon)) * 0.5 / k;
        st.layers += ray.layers / k;
    }
    if ncenter > 0.0 {
        st.center_depth /= ncenter;
    }
    if sw > 0.0 {
        let m = swh / sw;
        st.mean_h = m as f32;
        st.relief = ((swh2 / sw - m * m).max(0.0)).sqrt() as f32;
    }
    let tot: f32 = classes.iter().sum();
    if tot > 0.0 {
        let mut h = 0.0f32;
        for c in classes {
            let p = c / tot;
            if p > 1e-6 {
                h -= p * p.ln();
            }
        }
        st.entropy = h / (6f32).ln();
    }
    st
}

/// Horizontal field of view (radians) for a vertical one at [`ASPECT`].
#[inline]
pub fn hfov(vfov: f64) -> f64 {
    2.0 * ((vfov * 0.5).tan() * ASPECT).atan()
}

/// 360° viewshed of a camera: `N_AZ` ray profiles reused for every candidate yaw.
pub struct Viewshed {
    pub cam: [f64; 3],
    rays: Vec<RayProfile>,
}

impl Viewshed {
    fn new(t: &Terrain, cam: [f64; 3], dmax: f64) -> Viewshed {
        let rays = (0..N_AZ).map(|i| t.cast(cam, i as f64 * 2.0 * PI / N_AZ as f64, 3.0, dmax)).collect();
        Viewshed { cam, rays }
    }

    #[inline]
    fn ray_at(&self, az: f64) -> &RayProfile {
        let f = az.rem_euclid(2.0 * PI) / (2.0 * PI) * N_AZ as f64;
        &self.rays[(f.round() as usize) % N_AZ]
    }

    /// Frame statistics for a view direction (16 image columns).
    pub fn view(&self, yaw: f64, vfov: f64, rule: PitchRule) -> ViewStats {
        let th = (hfov(vfov) * 0.5).tan();
        const K: usize = 16;
        let cols: Vec<(&RayProfile, f64)> = (0..K)
            .map(|i| {
                let sx = (2 * i + 1) as f64 / K as f64 - 1.0;
                let phi = (sx * th).atan();
                (self.ray_at(yaw + phi), phi)
            })
            .collect();
        frame_stats(&cols, vfov, rule)
    }

    /// Horizon elevation (degrees) seen from the camera at an azimuth (radians).
    pub fn horizon_deg(&self, az: f64) -> f64 {
        self.ray_at(az).horizon as f64 / DEG
    }

    /// Fraction of the visible terrain of one azimuth (within the frame of the given pitch) that
    /// belongs to category `m` — used for the sun-glitter path over the sea.
    fn column_fraction(&self, az: f64, pitch: f64, vfov: f64, m: u16) -> f32 {
        let st = frame_stats(&[(self.ray_at(az), 0.0)], vfov, PitchRule::Fixed(pitch));
        if st.terrain <= 1e-4 {
            0.0
        } else {
            st.c(m) / st.terrain
        }
    }
}

// =====================================================================================
// 4. Scouting context and shot planners
// =====================================================================================

/// Lattice + derived fields shared by all planners.
pub struct Scout<'a> {
    pub gen: &'a WorldGen,
    pub grid: ScoutGrid,
    pub prom: Vec<f32>,
    max3: Vec<f32>,
    max8: Vec<f32>,
    min2: Vec<f32>,
    max2: Vec<f32>,
    min6: Vec<f32>,
    max6: Vec<f32>,
    max16: Vec<f32>,
    max32: Vec<f32>,
    sat: Vec<Option<Sat>>,
}

impl<'a> Scout<'a> {
    pub fn new(gen: &'a WorldGen) -> Scout<'a> {
        Self::with_area(gen, 0.0, 0.0, SCOUT_RADIUS, SCOUT_STEP)
    }

    pub fn with_area(gen: &'a WorldGen, cx: f64, cz: f64, radius: f64, step: f64) -> Scout<'a> {
        let grid = ScoutGrid::scan(gen, cx, cz, radius, step);
        let n = grid.n;
        let s = &grid.surf;
        let prom = prominence(s, n);
        let w = |r: usize, mx: bool| window_reduce(s, n, r, mx);
        let (max3, max8, min2, max2, min6, max6, max16, max32) =
            (w(3, true), w(8, true), w(2, false), w(2, true), w(6, false), w(6, true), w(16, true), w(32, true));
        let needed = [
            cat::WATER,
            cat::OCEAN,
            cat::RIVER,
            cat::LAND,
            cat::SNOW,
            cat::RICH_FOREST,
            cat::FOREST,
            cat::BADLANDS,
            cat::DESERT,
            cat::SAVANNA,
            cat::GRASS,
            cat::SHORE,
            cat::HOODOOS,
        ];
        let sat: Vec<Option<Sat>> = (0..cat::COUNT)
            .into_par_iter()
            .map(|b| {
                let m = 1u16 << b;
                if needed.contains(&m) {
                    Some(Sat::new(n, |k| grid.mask[k] & m != 0))
                } else {
                    None
                }
            })
            .collect();
        Scout { gen, grid, prom, max3, max8, min2, max2, min6, max6, max16, max32, sat }
    }

    #[inline]
    fn frac(&self, m: u16, k: usize, r: usize) -> f32 {
        let (i, j) = self.grid.ij(k);
        self.sat[cat::bit(m)].as_ref().expect("SAT not built for category").frac(i, j, r)
    }

    fn terrain(&self) -> Terrain<'_> {
        Terrain { gen: self.gen, grid: &self.grid }
    }

    fn viewshed(&self, cam: [f64; 3]) -> Viewshed {
        Viewshed::new(&self.terrain(), cam, VIEW_RANGE)
    }

    #[inline]
    fn land(&self, k: usize) -> bool {
        self.grid.mask[k] & cat::LAND != 0
    }
}

/// A fully specified candidate shot.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub score: f32,
    pub camera: Camera,
    pub sun: Sun,
    pub mode: ViewMode,
    pub build: BuildParams,
    pub exposure: f32,
    pub haze: f32,
    pub title: String,
    /// What the shot is about (summit, river point, box centre…), world xz.
    pub target: [f64; 2],
    pub note: String,
}

/// A planned shot plus scouting diagnostics.
#[derive(Clone, Debug)]
pub struct PlannedShot {
    pub shot: Shot,
    pub score: f32,
    pub target: [f64; 2],
    pub ground_y: f32,
    pub biome: Biome,
    pub note: String,
    /// True if the planner had to fall back (missing biome, separation relaxed …).
    pub fallback: bool,
}

/// World-build parameters for a normal view: clipmap centred slightly ahead of the camera.
pub fn normal_build(cam: &Camera, l0_size: u32, hf_levels: u32) -> BuildParams {
    let shift = FORWARD_SHIFT * l0_size as f64 * 0.5;
    let (sy, cy) = cam.yaw.sin_cos();
    let fr = (FEATURE_RADIUS).min(l0_size / 2);
    BuildParams {
        center_x: (cam.pos[0] + sy * shift).round() as i32,
        center_z: (cam.pos[2] + cy * shift).round() as i32,
        l0_size,
        hf_size: HF_SIZE,
        hf_levels,
        cave_radius: CAVE_RADIUS.min(l0_size / 2),
        feature_radius: fr,
    }
}

/// Re-derive a shot's build parameters for another level-0 size (e.g. `--fast`); keeps the
/// number of heightfield levels. Diorama shots are returned unchanged.
pub fn rebuild_params(shot: &Shot, l0_size: u32) -> BuildParams {
    match shot.mode {
        ViewMode::Diorama { .. } => shot.build,
        ViewMode::Normal if shot.build.l0_size == l0_size => shot.build,
        ViewMode::Normal if shot.name == "orbital" => orbital_build(&shot.camera, l0_size, shot.build.hf_levels),
        ViewMode::Normal => normal_build(&shot.camera, l0_size, shot.build.hf_levels),
    }
}

fn orbital_build(cam: &Camera, l0_size: u32, hf_levels: u32) -> BuildParams {
    let (sy, cy) = cam.yaw.sin_cos();
    let ahead = 2500.0;
    BuildParams {
        center_x: (cam.pos[0] + sy * ahead).round() as i32,
        center_z: (cam.pos[2] + cy * ahead).round() as i32,
        l0_size,
        hf_size: HF_SIZE,
        hf_levels,
        cave_radius: 0,
        feature_radius: (l0_size / 2).min(256),
    }
}

/// Per-seed deterministic coin (variety in sun sides etc.).
fn coin(gen: &WorldGen, name: &str) -> bool {
    hash_f01(mix64(derive_seed(gen.seed, salt(name)))) < 0.5
}

/// Greedy non-maximum suppression: keep the best items at least `dist` apart.
fn nms<T: Clone>(mut items: Vec<(f32, usize, T)>, grid: &ScoutGrid, dist: f64, max_n: usize) -> Vec<(f32, usize, T)> {
    items.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<(f32, usize, T)> = Vec::new();
    for it in items {
        let (x, z) = grid.pos(it.1);
        if out.iter().all(|o| {
            let (ox, oz) = grid.pos(o.1);
            (ox - x).hypot(oz - z) >= dist
        }) {
            out.push(it);
            if out.len() >= max_n {
                break;
            }
        }
    }
    out
}

/// Generic landscape quality of a frame: relief, ridge layers, depth, variety; penalises near
/// occluders and a sky fraction far from `sky_target`.
fn landscape_score(v: &ViewStats, sky_target: f32) -> f32 {
    let relief = (v.relief / 70.0).min(1.5);
    let layers = (v.layers / 3.0).min(1.0);
    let far = (v.far / 0.25).min(1.0);
    0.9 * relief + 0.5 * layers + 0.5 * far + 0.6 * v.entropy
        - 3.0 * v.near_block
        - 2.0 * v.wall
        - 0.12 * v.clamped_deg.min(15.0)
        - 2.2 * (v.sky - sky_target).abs()
}

/// Evaluate yaws and return the best (yaw, stats, score).
fn best_yaw(
    vs: &Viewshed,
    yaws: &[f64],
    vfov: f64,
    rule: impl Fn(f64) -> PitchRule,
    score: impl Fn(f64, &ViewStats) -> f32,
) -> (f64, ViewStats, f32) {
    let mut best: Option<(f64, ViewStats, f32)> = None;
    for &y in yaws {
        let st = vs.view(y, vfov, rule(y));
        let s = score(y, &st);
        if best.as_ref().map_or(true, |b| s > b.2) {
            best = Some((y, st, s));
        }
    }
    best.expect("no yaws")
}

fn all_yaws(n: usize) -> Vec<f64> {
    (0..n).map(|i| i as f64 * 2.0 * PI / n as f64).collect()
}

/// Choose a sun azimuth among `offsets` (degrees from the view yaw), in the given preference
/// order, so that the sun is not hidden behind the camera's own horizon.
fn pick_sun(vs: &Viewshed, yaw: f64, offsets: &[f64], elevation: f64) -> Sun {
    let mut best = (f64::NEG_INFINITY, offsets[0]);
    for (i, &o) in offsets.iter().enumerate() {
        let az = yaw + o * DEG;
        let margin = elevation - vs.horizon_deg(az);
        // prefer earlier entries unless they are blocked
        let s = margin.min(3.0) - i as f64 * 0.01;
        if s > best.0 {
            best = (s, o);
        }
    }
    Sun { azimuth_deg: (yaw / DEG + best.1).rem_euclid(360.0), elevation_deg: elevation }
}

fn cam_at(x: f64, y: f64, z: f64, yaw: f64, pitch: f64, fov: f64) -> Camera {
    Camera { pos: [x, y, z], yaw: yaw.rem_euclid(2.0 * PI), pitch, fov_y_deg: fov }
}

fn normal_candidate(
    score: f32,
    camera: Camera,
    sun: Sun,
    exposure: f32,
    haze: f32,
    title: String,
    target: [f64; 2],
    note: String,
) -> Candidate {
    let build = normal_build(&camera, L0_SIZE, HF_LEVELS);
    Candidate { score, camera, sun, mode: ViewMode::Normal, build, exposure, haze, title, target, note }
}

// ------------------------------------------------------------------ 1. panorama

fn plan_panorama(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .filter(|&k| s.prom[k] > 20.0 && s.land(k) && g.surf[k] > SEA + 40.0)
        .map(|k| (0.6 * (g.surf[k] - SEA) + 1.2 * s.prom[k].min(300.0), k, ()))
        .collect();
    let peaks = nms(items, g, 1500.0, 12);
    let vfov = 60.0 * DEG;
    let yaws = all_yaws(32);
    let mut out: Vec<Candidate> = peaks
        .par_iter()
        .map(|&(s0, k, _)| {
            let (x, z) = g.pos(k);
            let (sx, sz, h) = refine_summit(s.gen, x, z);
            let y = camera_floor(s.gen, sx, sz, 8.0) as f64 + 12.0;
            let vs = s.viewshed([sx, y, sz]);
            let rule = |_| PitchRule::Horizon { sy: 0.36, min: -22.0 * DEG, max: -2.0 * DEG };
            let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| {
                landscape_score(v, 0.33)
                    + 0.8 * (v.any(cat::SNOW | cat::PEAK) / 0.25).min(1.0)
                    + 0.4 * (v.c(cat::WATER) / 0.15).min(1.0)
            });
            let side = if coin(s.gen, "scout.panorama.sun") { 1.0 } else { -1.0 };
            let sun = pick_sun(&vs, yaw, &[75.0 * side, -75.0 * side, 110.0 * side, -110.0 * side], 25.0);
            let score = s0 / 300.0 + vscore;
            let cam = cam_at(sx, y, sz, yaw, st.pitch, 60.0);
            let title = format!("Панорама с вершины (y={})", h.round() as i32);
            let note = format!("prominence {:.0}, relief {:.0}, sky {:.2}", s.prom[k], st.relief, st.sky);
            normal_candidate(score, cam, sun, 1.0, 1.0, title, [sx, sz], note)
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ 2. river valley

/// Principal axis (yaw, radians) of the river nodes around k (2×2 covariance → eigenvector).
fn river_axis(s: &Scout, k: usize, r: i64) -> Option<f64> {
    let g = &s.grid;
    let (i, j) = g.ij(k);
    let (mut sxx, mut szz, mut sxz, mut cnt) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let (mut mx, mut mz) = (0.0f64, 0.0f64);
    let mut pts = Vec::new();
    for dj in -r..=r {
        for di in -r..=r {
            let (ii, jj) = (i as i64 + di, j as i64 + dj);
            if ii < 0 || jj < 0 || ii >= g.n as i64 || jj >= g.n as i64 {
                continue;
            }
            if g.mask[jj as usize * g.n + ii as usize] & cat::RIVER != 0 {
                pts.push((di as f64, dj as f64));
                mx += di as f64;
                mz += dj as f64;
                cnt += 1;
            }
        }
    }
    if cnt < 4 {
        return None;
    }
    mx /= cnt as f64;
    mz /= cnt as f64;
    for (x, z) in pts {
        sxx += (x - mx) * (x - mx);
        szz += (z - mz) * (z - mz);
        sxz += (x - mx) * (z - mz);
    }
    // principal direction angle θ from the x axis: tan 2θ = 2 s_xz / (s_xx − s_zz)
    let th = 0.5 * (2.0 * sxz).atan2(sxx - szz);
    let (dx, dz) = (th.cos(), th.sin());
    Some(dx.atan2(dz))
}

fn plan_river(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .filter(|&k| g.mask[k] & cat::RIVER != 0 && s.frac(cat::OCEAN, k, 10) < 0.03)
        .map(|k| {
            let depth = (s.max8[k] - g.surf[k]).max(0.0);
            let cont = (s.frac(cat::RIVER, k, 6) * 13.0).min(1.5);
            (depth.min(220.0) / 220.0 + 0.5 * cont, k, ())
        })
        .collect();
    let picks = nms(items, g, 1500.0, 12);
    let vfov = 55.0 * DEG;
    let mut out: Vec<Candidate> = picks
        .par_iter()
        .map(|&(s0, k, _)| {
            let (x, z) = g.pos(k);
            // river centre-line at full resolution: max of the river field among river columns
            let mut best = (f32::NEG_INFINITY, x.floor() + 0.5, z.floor() + 0.5, g.surf[k]);
            for j in -5..=5 {
                for i in -5..=5 {
                    let (px, pz) = (x.floor() + 0.5 + i as f64 * 6.0, z.floor() + 0.5 + j as f64 * 6.0);
                    let c = s.gen.column(px, pz);
                    if is_wet(&c) && c.climate.river > best.0 {
                        best = (c.climate.river, px, pz, surface_of(&c));
                    }
                }
            }
            let (_, rx, rz, water) = best;
            let depth = (s.max8[k] - water).max(0.0);
            let y = (water + (0.3 * depth).clamp(22.0, 60.0)).max(camera_floor(s.gen, rx, rz, 14.0) + 8.0) as f64;
            let vs = s.viewshed([rx, y, rz]);
            let yaws: Vec<f64> = match river_axis(s, k, 6) {
                Some(a) => (-7..=7).flat_map(|q| [a + q as f64 * 5.0 * DEG, a + PI + q as f64 * 5.0 * DEG]).collect(),
                None => all_yaws(32),
            };
            let rule = |_| PitchRule::Horizon { sy: 0.3, min: -18.0 * DEG, max: 0.0 };
            let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| {
                landscape_score(v, 0.3)
                    + 2.0 * (v.center[cat::bit(cat::WATER)] / 0.12).min(1.0)
                    + 1.2 * ((v.center_depth - 2.7) / 0.8).clamp(0.0, 1.0)
            });
            let side = if coin(s.gen, "scout.river.sun") { 1.0 } else { -1.0 };
            let sun = pick_sun(&vs, yaw, &[110.0 * side, -110.0 * side, 70.0 * side, -70.0 * side], 30.0);
            let score = s0 + vscore;
            let cam = cam_at(rx, y, rz, yaw, st.pitch, 55.0);
            let d = depth.round() as i32;
            let title = if d >= 90 {
                format!("Каньон реки (глубина {} м)", d)
            } else {
                format!("Речная долина (склоны {} м)", d)
            };
            let note = format!("valley depth {:.0}, water centre {:.3}", depth, st.center[cat::bit(cat::WATER)]);
            normal_candidate(score, cam, sun, 1.0, 1.1, title, [rx, rz], note)
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ 3. coast at sunset

fn plan_coast(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .filter(|&k| {
            s.land(k) && {
                let o2 = s.frac(cat::OCEAN, k, 2);
                let l6 = s.frac(cat::LAND, k, 6);
                o2 > 0.1 && o2 < 0.65 && l6 > 0.3 && l6 < 0.8
            }
        })
        .map(|k| {
            let cliff = (s.max3[k] - SEA).max(0.0);
            let s0 = 0.6 * cliff.min(80.0) / 80.0
                + 0.3 * s.frac(cat::SHORE, k, 2).min(0.5) / 0.5
                + 0.3 * (s.max16[k] - SEA).clamp(0.0, 250.0) / 250.0;
            (s0, k, ())
        })
        .collect();
    let picks = nms(items, g, 1500.0, 14);
    let vfov = 60.0 * DEG;
    let yaws = all_yaws(32);
    let elev = 4.5;
    let mut out: Vec<Candidate> = picks
        .par_iter()
        .map(|&(s0, k, _)| {
            let (x, z) = g.pos(k);
            let (x, z) = (x.floor() + 0.5, z.floor() + 0.5);
            let y = (camera_floor(s.gen, x, z, 10.0) + 14.0).max(SEA + 24.0) as f64;
            let vs = s.viewshed([x, y, z]);
            let rule = |_| PitchRule::Horizon { sy: 0.2, min: -14.0 * DEG, max: -2.0 * DEG };
            // per yaw: choose the sun side over the sea (glitter path)
            let side_score = |yaw: f64, v: &ViewStats| -> (f32, f64) {
                let mut best = (f32::NEG_INFINITY, 22.0);
                for o in [22.0, -22.0] {
                    let az = yaw + o * DEG;
                    let glint = vs.column_fraction(az, v.pitch, vfov, cat::OCEAN);
                    let blocked = vs.horizon_deg(az) > elev - 1.0;
                    let sc = 1.2 * glint - if blocked { 1.5 } else { 0.0 };
                    if sc > best.0 {
                        best = (sc, o);
                    }
                }
                best
            };
            let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |y, v| {
                landscape_score(v, 0.42)
                    + 1.4 * (v.c(cat::OCEAN) / 0.3).min(1.0)
                    + 1.2 * (v.c(cat::LAND) / 0.3).min(1.0)
                    + side_score(y, v).0
            });
            let (_, off) = side_score(yaw, &st);
            let sun = Sun { azimuth_deg: (yaw / DEG + off).rem_euclid(360.0), elevation_deg: elev };
            let cliff = (s.max3[k] - SEA).max(0.0).round() as i32;
            let shore = s.frac(cat::SHORE, k, 2);
            let title = if cliff >= 35 {
                format!("Закат над морскими утёсами ({} м)", cliff)
            } else if shore > 0.2 {
                "Закат на песчаном берегу".to_string()
            } else {
                "Закат на побережье".to_string()
            };
            let cam = cam_at(x, y, z, yaw, st.pitch, 60.0);
            let note = format!("cliff {}, ocean {:.2}, land {:.2}", cliff, st.c(cat::OCEAN), st.c(cat::LAND));
            normal_candidate(s0 + vscore, cam, sun, 1.1, 1.3, title, [x, z], note)
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ 4. drylands

fn plan_drylands(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let classes: [(u16, &str); 5] = [
        (cat::BADLANDS, "badlands"),
        (cat::DESERT, "desert"),
        (cat::SAVANNA, "savanna"),
        (cat::GRASS, "grass"),
        (cat::LAND, "land"),
    ];
    for (ci, &(m, cname)) in classes.iter().enumerate() {
        let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
            .filter(|&k| s.land(k) && g.mask[k] & m != 0 && s.frac(m, k, 6) >= 0.45)
            .map(|k| {
                let relief = (s.max6[k] - s.min6[k]).min(120.0) / 120.0;
                let hoodoo = if m == cat::BADLANDS { s.frac(cat::HOODOOS, k, 6) } else { 0.0 };
                (s.frac(m, k, 6) + 0.5 * relief + 0.5 * hoodoo, k, ())
            })
            .collect();
        if items.len() < 6 && ci + 1 < classes.len() {
            continue;
        }
        let picks = nms(items, g, 1500.0, 10);
        let vfov = 55.0 * DEG;
        let yaws = all_yaws(32);
        let mut out: Vec<Candidate> = picks
            .par_iter()
            .map(|&(s0, k, _)| {
                let (x, z) = g.pos(k);
                let (x, z) = (x.floor() + 0.5, z.floor() + 0.5);
                let y = camera_floor(s.gen, x, z, 14.0) as f64 + 28.0;
                let vs = s.viewshed([x, y, z]);
                let rule = |_| PitchRule::Horizon { sy: 0.33, min: -20.0 * DEG, max: -2.0 * DEG };
                let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| {
                    landscape_score(v, 0.33) + 2.0 * (v.any(m) / 0.45).min(1.0)
                });
                let side = if coin(s.gen, "scout.dry.sun") { 1.0 } else { -1.0 };
                let sun = pick_sun(&vs, yaw, &[80.0 * side, -80.0 * side, 120.0 * side, -120.0 * side], 9.0);
                let hood = s.frac(cat::HOODOOS, k, 8);
                let title = match cname {
                    "badlands" if hood > 0.05 => "Бедленды: столовые горы и худу".to_string(),
                    "badlands" => "Бедленды: террасы столовых гор".to_string(),
                    "desert" => "Пустыня: дюнное море в косом свете".to_string(),
                    "savanna" => "Саванна в косом вечернем свете".to_string(),
                    _ => "Засушливые равнины в косом свете".to_string(),
                };
                let cam = cam_at(x, y, z, yaw, st.pitch, 55.0);
                let note = format!("{} cover {:.2}, relief {:.0}", cname, st.any(m), st.relief);
                let mut c = normal_candidate(s0 + vscore, cam, sun, 1.0, 0.8, title, [x, z], note);
                if ci >= 3 {
                    c.score -= 5.0;
                }
                c
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

// ------------------------------------------------------------------ 5. alpine

fn plan_alpine(s: &Scout, avoid: &[[f64; 2]]) -> Vec<Candidate> {
    let g = &s.grid;
    let tiers: [(f32, f32, f32); 3] = [(380.0, 40.0, 0.12), (320.0, 30.0, 0.0), (SEA + 60.0, 20.0, 0.0)];
    let mut peaks = Vec::new();
    for &(hmin, pmin, snow) in &tiers {
        let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
            .filter(|&k| g.surf[k] > hmin && s.prom[k] > pmin && (snow == 0.0 || s.frac(cat::SNOW, k, 3) > snow))
            .map(|k| {
                let (x, z) = g.pos(k);
                let near_taken = avoid.iter().any(|a| (a[0] - x).hypot(a[1] - z) < 2000.0);
                ((g.surf[k] - 300.0) / 150.0 + s.prom[k].min(300.0) / 300.0 - if near_taken { 2.0 } else { 0.0 }, k, ())
            })
            .collect();
        if items.len() >= 2 {
            peaks = nms(items, g, 2000.0, 6);
            break;
        }
    }
    let vfov = 50.0 * DEG;
    let t = s.terrain();
    let mut out: Vec<Candidate> = peaks
        .par_iter()
        .flat_map_iter(|&(s0, k, _)| {
            let (x, z) = g.pos(k);
            let (px, pz, hp) = refine_summit(s.gen, x, z);
            // camera positions on rings around the peak, judged on the lattice first
            let mut pos: Vec<(f32, f64, f64, f64, f64)> = Vec::new();
            for &r in &[1500.0f64, 2100.0, 2800.0, 3600.0] {
                for q in 0..24 {
                    let az = q as f64 * 2.0 * PI / 24.0;
                    let (cx, cz) = (px + r * az.sin(), pz + r * az.cos());
                    let Some((gs, gm)) = g.sample(cx, cz) else { continue };
                    if gm & cat::OCEAN != 0 || (hp - gs) < 150.0 {
                        continue;
                    }
                    let cy = gs as f64 + 14.0;
                    let a = ((hp as f64 - cy) / r).atan();
                    // line of sight to the summit over the lattice
                    let mut los = true;
                    for st in 1..40 {
                        let d = r * st as f64 / 40.0;
                        if d > r - 250.0 {
                            break;
                        }
                        let (sx, sz) = (cx + (px - cx) * d / r, cz + (pz - cz) * d / r);
                        if let Some((h, _)) = g.sample(sx, sz) {
                            if ((h as f64 - cy) / d).atan() > a - 0.006 {
                                los = false;
                                break;
                            }
                        }
                    }
                    let sc = -((a / DEG - 10.0).abs() / 4.0) + if los { 1.0 } else { -3.0 }
                        + if gm & cat::WATER == 0 { 0.3 } else { 0.0 };
                    pos.push((sc as f32, cx, cz, a, az));
                }
            }
            pos.sort_by(|a, b| b.0.total_cmp(&a.0));
            pos.truncate(4);
            let mut res = Vec::new();
            for (ps, cx, cz, _, az) in pos {
                let (cx, cz) = (cx.floor() + 0.5, cz.floor() + 0.5);
                let cy = camera_floor(s.gen, cx, cz, 10.0) as f64 + 10.0;
                let r = (px - cx).hypot(pz - cz);
                let a = ((hp as f64 - cy) / r).atan();
                let vs = Viewshed::new(&t, [cx, cy, cz], VIEW_RANGE);
                let to_peak = az + PI;
                let yaws: Vec<f64> = [-16.0, -8.0, 0.0, 8.0, 16.0].iter().map(|o| to_peak + o * DEG).collect();
                let rule = |_| PitchRule::Target { angle: a, sy: 0.42, min: -6.0 * DEG, max: 12.0 * DEG };
                let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| {
                    landscape_score(v, 0.4)
                        + 2.0 * (v.c(cat::SNOW) / 0.3).min(1.0)
                        + 0.6 * (v.c(cat::PEAK) / 0.15).min(1.0)
                        + 0.5 * (1.0 - ((a / DEG - 10.0).abs() / 6.0).min(1.0)) as f32
                });
                let side = if coin(s.gen, "scout.alpine.sun") { 1.0 } else { -1.0 };
                let sun = pick_sun(&vs, yaw, &[115.0 * side, -115.0 * side, 75.0 * side, -75.0 * side], 17.0);
                let snowy = st.c(cat::SNOW) > 0.03 || hp > 400.0;
                let title = if snowy {
                    format!("Альпийский хребет: снежный пик y={}", hp.round() as i32)
                } else {
                    format!("Горный хребет: пик y={}", hp.round() as i32)
                };
                let cam = cam_at(cx, cy, cz, yaw, st.pitch, 50.0);
                let note = format!(
                    "peak ({:.0},{:.0}) y={:.0}, dist {:.0}, elev {:.1}°, snow {:.2}",
                    px,
                    pz,
                    hp,
                    r,
                    a / DEG,
                    st.c(cat::SNOW)
                );
                let score = s0 + 0.3 * ps + vscore;
                res.push(normal_candidate(score, cam, sun, 1.0, 0.7, title, [px, pz], note));
            }
            res
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ 6. forest

fn plan_forest(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    for (ci, &m) in [cat::RICH_FOREST, cat::FOREST].iter().enumerate() {
        let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
            .filter(|&k| s.land(k) && g.mask[k] & m != 0 && s.frac(m, k, 6) >= 0.6)
            .map(|k| {
                let relief = (s.max8[k] - g.surf[k]).min(150.0) / 150.0;
                let water = if s.frac(cat::WATER, k, 12) > 0.01 { 0.3 } else { 0.0 };
                (s.frac(m, k, 6) + 0.4 * relief + water, k, ())
            })
            .collect();
        if items.len() < 4 && ci == 0 {
            continue;
        }
        let picks = nms(items, g, 1500.0, 10);
        let vfov = 60.0 * DEG;
        let yaws = all_yaws(32);
        let mut out: Vec<Candidate> = picks
            .par_iter()
            .map(|&(s0, k, _)| {
                let (x, z) = g.pos(k);
                let (x, z) = (x.floor() + 0.5, z.floor() + 0.5);
                let col = s.gen.column(x, z);
                let canopy = s.gen.canopy_lod(x, z, &col, 1.0);
                let y = camera_floor(s.gen, x, z, 14.0).max(col.height + canopy.height) as f64 + 20.0;
                let vs = s.viewshed([x, y, z]);
                let rule = |_| PitchRule::Horizon { sy: 0.4, min: -25.0 * DEG, max: -3.0 * DEG };
                let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| {
                    landscape_score(v, 0.3)
                        + 1.0 * (v.c(m) / 0.5).min(1.0)
                        + 1.5 * (v.mid[cat::bit(m)] / 0.3).min(1.0)
                        + 0.4 * (v.c(cat::WATER) / 0.08).min(1.0)
                });
                let side = if coin(s.gen, "scout.forest.sun") { 1.0 } else { -1.0 };
                let sun = pick_sun(&vs, yaw, &[120.0 * side, -120.0 * side, 80.0 * side, -80.0 * side], 34.0);
                let title = match col.biome {
                    Biome::Jungle => "Джунгли: полог тропического леса".to_string(),
                    Biome::DarkForest => "Тёмный лес: сомкнутый полог".to_string(),
                    Biome::OldGrowthTaiga => "Древняя тайга: исполинские ели".to_string(),
                    Biome::Taiga | Biome::SnowyTaiga => "Тайга сверху".to_string(),
                    Biome::BirchForest => "Берёзовый лес сверху".to_string(),
                    _ => "Лесной массив сверху".to_string(),
                };
                let cam = cam_at(x, y, z, yaw, st.pitch, 60.0);
                let note = format!("{} cover {:.2}, canopy {:.0} m", col.biome.name(), st.c(m), canopy.height);
                let mut c = normal_candidate(s0 + vscore, cam, sun, 1.0, 1.2, title, [x, z], note);
                if ci > 0 {
                    c.score -= 3.0;
                }
                c
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

// ------------------------------------------------------------------ 7. cut-away diorama

/// Project a world point with a camera (screen coords in [-1,1]², None if behind).
fn project(cam: &Camera, p: [f64; 3], aspect: f64) -> Option<(f64, f64)> {
    let (r, u, f) = cam.basis();
    let v = [p[0] - cam.pos[0], p[1] - cam.pos[1], p[2] - cam.pos[2]];
    let dot = |a: crate::math::Vec3| a.x as f64 * v[0] + a.y as f64 * v[1] + a.z as f64 * v[2];
    let zf = dot(f);
    if zf <= 1e-6 {
        return None;
    }
    let t = (cam.fov_y_deg.to_radians() * 0.5).tan();
    Some((dot(r) / (zf * t * aspect), dot(u) / (zf * t)))
}

/// Frame a box: camera orientation fixed by (yaw, pitch); find the position whose projection of
/// the 8 corners is centred on screen and just fits inside `margin` (NDC). Alternates a
/// bisection on the distance along the view axis with a lateral shift (in the camera's right/up
/// plane) that cancels the off-centre of the projected bounding rectangle — a fixed-point
/// iteration that converges in a few steps because the projection is locally affine.
fn frame_box(center: [f64; 3], yaw: f64, pitch: f64, corners: &[[f64; 3]], fov: f64, margin: f64) -> Camera {
    let probe = Camera { pos: center, yaw, pitch, fov_y_deg: fov };
    let (r, u, f) = probe.basis();
    let v3 = |a: crate::math::Vec3| [a.x as f64, a.y as f64, a.z as f64];
    let (r, u, f) = (v3(r), v3(u), v3(f));
    let t = (fov.to_radians() * 0.5).tan();
    let place = |d: f64, lx: f64, ly: f64| -> Camera {
        let pos = [0, 1, 2].map(|i| center[i] - f[i] * d + r[i] * lx + u[i] * ly);
        Camera { pos, yaw, pitch, fov_y_deg: fov }
    };
    let bounds = |cam: &Camera| -> Option<(f64, f64, f64, f64)> {
        let mut b = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for &c in corners {
            let (x, y) = project(cam, c, ASPECT)?;
            b = (b.0.min(x), b.1.max(x), b.2.min(y), b.3.max(y));
        }
        Some(b)
    };
    let fits = |cam: &Camera| bounds(cam).is_some_and(|b| b.0 >= -margin && b.1 <= margin && b.2 >= -margin && b.3 <= margin);
    let (mut lx, mut ly) = (0.0f64, 0.0f64);
    let mut d = 400.0;
    for _ in 0..6 {
        let (mut lo, mut hi) = (20.0, 200.0);
        while !fits(&place(hi, lx, ly)) && hi < 1e5 {
            lo = hi;
            hi *= 1.6;
        }
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if fits(&place(mid, lx, ly)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        d = hi;
        if let Some(b) = bounds(&place(d, lx, ly)) {
            lx += 0.5 * (b.0 + b.1) * d * t * ASPECT;
            ly += 0.5 * (b.2 + b.3) * d * t;
        }
    }
    let mut cam = place(d, lx, ly);
    let mut k = 0;
    while !fits(&cam) && k < 60 {
        d *= 1.02;
        cam = place(d, lx, ly);
        k += 1;
    }
    cam
}

fn plan_cutaway(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .filter(|&k| {
            let relief = s.max2[k] - s.min2[k];
            s.land(k)
                && (30.0..=170.0).contains(&relief)
                && g.surf[k] < 390.0
                && s.min2[k] > SEA + 4.0
                && s.frac(cat::WATER, k, 2) == 0.0
        })
        .map(|k| {
            let relief = s.max2[k] - s.min2[k];
            let s0 = crate::math::smoothstep(30.0, 100.0, relief)
                + 0.4 * s.frac(cat::BADLANDS, k, 3)
                + 0.3 * s.frac(cat::FOREST, k, 3)
                + 0.2 * s.frac(cat::GRASS, k, 3);
            (s0, k, ())
        })
        .collect();
    let picks = nms(items, g, 1500.0, 20);
    let half = CUTAWAY_SIZE / 2;
    let mut out: Vec<Candidate> = picks
        .par_iter()
        .map(|&(s0, k, _)| {
            let (x, z) = g.pos(k);
            let (cx, cz) = (x.round() as i32, z.round() as i32);
            let (x0, x1, z0, z1) = (cx - half, cx + half, cz - half, cz + half);
            // full-res columns along the four faces (every 4 blocks)
            let face_pts = |f: usize, t: i32| -> (f64, f64) {
                match f {
                    0 => (x0 as f64 + 0.5, (z0 + t) as f64 + 0.5), // −x face
                    1 => ((x1 - 1) as f64 + 0.5, (z0 + t) as f64 + 0.5), // +x face
                    2 => ((x0 + t) as f64 + 0.5, z0 as f64 + 0.5), // −z face
                    _ => ((x0 + t) as f64 + 0.5, (z1 - 1) as f64 + 0.5), // +z face
                }
            };
            let mut faces: Vec<Vec<ColumnInfo>> = vec![Vec::new(); 4];
            let (mut tmin, mut tmax) = (f32::INFINITY, f32::NEG_INFINITY);
            for (f, face) in faces.iter_mut().enumerate() {
                for t in (0..CUTAWAY_SIZE).step_by(4) {
                    let (px, pz) = face_pts(f, t);
                    let c = s.gen.column(px, pz);
                    tmin = tmin.min(c.height);
                    tmax = tmax.max(c.height + c.jaggedness * 0.5);
                    face.push(c);
                }
            }
            for j in 0..=8 {
                for i in 0..=8 {
                    let c = s.gen.column((x0 + i * 22) as f64, (z0 + j * 22) as f64);
                    tmax = tmax.max(c.height + c.jaggedness * 0.5);
                }
            }
            let min_y = ((tmin - 100.0).floor() as i32).max(8);
            // cave area on each face (samples every 4 × 4 blocks)
            let mut area = [0.0f32; 4];
            let mut tot = [0.0f32; 4];
            for (f, face) in faces.iter().enumerate() {
                for c in face {
                    let top = c.height as i32 - 3;
                    let mut y = min_y + 2;
                    while y < top {
                        tot[f] += 1.0;
                        if s.gen.cave_field(c.x, y as f64 + 0.5, c.z, c) > 0.0 {
                            area[f] += 1.0;
                        }
                        y += 4;
                    }
                }
            }
            // camera corner: faces seen from (sx, sz) are −x if sx<0 else +x, −z if sz<0 else +z
            let mut best = (f32::NEG_INFINITY, -1.0f64, -1.0f64);
            for (sx, sz) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let fx = if sx < 0.0 { 0 } else { 1 };
                let fz = if sz < 0.0 { 2 } else { 3 };
                let frac = (area[fx] + area[fz]) / (tot[fx] + tot[fz]).max(1.0);
                if frac > best.0 {
                    best = (frac, sx, sz);
                }
            }
            let (cave_frac, sx, sz) = best;
            let top = tmax.ceil() as i32;
            let center = [cx as f64, (min_y as f64 + top as f64) * 0.5, cz as f64];
            let el = 35.0 * DEG;
            // azimuth from the box centre toward the camera corner; the camera looks back at it
            let az = (sx as f64).atan2(sz as f64);
            let mut corners = Vec::new();
            for &xx in &[x0 as f64, x1 as f64] {
                for &yy in &[min_y as f64, top as f64] {
                    for &zz in &[z0 as f64, z1 as f64] {
                        corners.push([xx, yy, zz]);
                    }
                }
            }
            let fov = 32.0;
            let mut camera = frame_box(center, (az + PI).rem_euclid(2.0 * PI), -el, &corners, fov, 0.84);
            // never inside terrain (irrelevant in diorama mode, but keep the invariant)
            let floor = camera_floor(s.gen, camera.pos[0], camera.pos[2], 4.0) as f64 + 4.0;
            camera.pos[1] = camera.pos[1].max(floor);
            let sun_az = (camera.yaw / DEG + 180.0 + 38.0).rem_euclid(360.0);
            let sun = Sun { azimuth_deg: sun_az, elevation_deg: 42.0 };
            let mode = ViewMode::Diorama { min_x: x0, max_x: x1, min_z: z0, max_z: z1, min_y };
            let build = BuildParams {
                center_x: cx,
                center_z: cz,
                l0_size: 256,
                hf_size: HF_SIZE,
                hf_levels: 0,
                cave_radius: 256,
                feature_radius: 128,
            };
            let any_cave = area.iter().any(|&a| a > 0.0);
            let score = s0 + 2.0 * (cave_frac / 0.08).min(1.0) - if any_cave { 0.0 } else { 3.0 };
            let title = format!("Разрез {0}×{0}: пещеры и пласты (y {1}–{2})", CUTAWAY_SIZE, min_y, top);
            let note = format!("cave fraction on visible faces {:.3}, relief {:.0}", cave_frac, tmax - tmin);
            Candidate {
                score,
                camera,
                sun,
                mode,
                build,
                exposure: 1.0,
                haze: 0.6,
                title,
                target: [cx as f64, cz as f64],
                note,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ 8. orbital

fn plan_orbital(s: &Scout) -> Vec<Candidate> {
    let g = &s.grid;
    let mut items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .filter(|&k| {
            let (i, j) = g.ij(k);
            let o = s.frac(cat::OCEAN, k, 2);
            s.land(k) && o > 0.1 && o < 0.7 && i % 2 == 0 && j % 2 == 0
        })
        .map(|k| {
            let big = s.frac(cat::OCEAN, k, 48);
            let s0 = 1.0 - (big - 0.45).abs() * 2.0 + (s.max32[k] - SEA).clamp(0.0, 350.0) / 350.0;
            (s0, k, ())
        })
        .collect();
    if items.is_empty() {
        items = (0..g.n * g.n).step_by(97).map(|k| (s.max32[k] - g.surf[k], k, ())).collect();
    }
    let picks = nms(items, g, 3000.0, 10);
    let t = s.terrain();
    let vfov = 60.0 * DEG;
    let pitch = ORBITAL_PITCH_DEG * DEG;
    let alt = ORBITAL_ALTITUDE;
    let back = (alt - 160.0) / (-pitch).tan();
    let th = (hfov(vfov) * 0.5).tan();
    let mut out: Vec<Candidate> = picks
        .par_iter()
        .map(|&(s0, k, _)| {
            let (tx, tz) = g.pos(k);
            let mut best: Option<(f32, f64, ViewStats)> = None;
            for yaw in all_yaws(16) {
                let (cx, cz) = (tx - yaw.sin() * back, tz - yaw.cos() * back);
                let cam = [cx, alt, cz];
                const K: usize = 9;
                let rays: Vec<(RayProfile, f64)> = (0..K)
                    .map(|i| {
                        let sx = (2 * i + 1) as f64 / K as f64 - 1.0;
                        let phi = (sx * th).atan();
                        (t.cast(cam, yaw + phi, 1500.0, 70_000.0), phi)
                    })
                    .collect();
                let cols: Vec<(&RayProfile, f64)> = rays.iter().map(|(r, p)| (r, *p)).collect();
                let st = frame_stats(&cols, vfov, PitchRule::Fixed(pitch));
                let land = st.c(cat::LAND);
                let ocean = st.c(cat::OCEAN);
                let sc = 1.5 * st.entropy
                    + 1.5 * (ocean / 0.35).min(1.0)
                    + 1.5 * (land / 0.4).min(1.0)
                    + 0.8 * (st.c(cat::SNOW) / 0.05).min(1.0)
                    + 0.6 * (st.relief / 60.0).min(1.5);
                if best.as_ref().map_or(true, |b| sc > b.0) {
                    best = Some((sc, yaw, st));
                }
            }
            let (vscore, yaw, st) = best.unwrap();
            let (cx, cz) = (tx - yaw.sin() * back, tz - yaw.cos() * back);
            let camera = cam_at(cx, alt, cz, yaw, pitch, 60.0);
            let side = if coin(s.gen, "scout.orbital.sun") { 1.0 } else { -1.0 };
            let sun = Sun { azimuth_deg: (yaw / DEG + 70.0 * side).rem_euclid(360.0), elevation_deg: 22.0 };
            let build = orbital_build(&camera, ORBITAL_L0_SIZE, ORBITAL_HF_LEVELS);
            let title = format!("Орбитальный вид с высоты {} км", (alt / 1000.0).round() as i32);
            let note = format!(
                "ocean {:.2}, land {:.2}, snow {:.3}, entropy {:.2}",
                st.c(cat::OCEAN),
                st.c(cat::LAND),
                st.c(cat::SNOW),
                st.entropy
            );
            Candidate {
                score: s0 + vscore,
                camera,
                sun,
                mode: ViewMode::Normal,
                build,
                exposure: 1.0,
                haze: 1.0,
                title,
                target: [tx, tz],
                note,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

// ------------------------------------------------------------------ generic fallback

/// Best generic landscape view from high, rugged land far from the taken positions.
fn plan_generic(s: &Scout, taken: &[[f64; 2]], sun_elev: f64) -> Candidate {
    let g = &s.grid;
    let items: Vec<(f32, usize, ())> = (0..g.n * g.n)
        .step_by(7)
        .map(|k| {
            let (x, z) = g.pos(k);
            let far = taken.iter().map(|t| (t[0] - x).hypot(t[1] - z)).fold(f64::INFINITY, f64::min);
            let sep = if far >= MIN_SEPARATION { 0.0 } else { -5.0 };
            ((s.max6[k] - s.min6[k]) / 100.0 + if s.land(k) { 0.5 } else { 0.0 } + sep, k, ())
        })
        .collect();
    let picks = nms(items, g, 1500.0, 4);
    let yaws = all_yaws(32);
    let vfov = 60.0 * DEG;
    let mut best: Option<Candidate> = None;
    for (s0, k, _) in picks {
        let (x, z) = g.pos(k);
        let (x, z) = (x.floor() + 0.5, z.floor() + 0.5);
        let y = camera_floor(s.gen, x, z, 12.0).max(SEA) as f64 + 40.0;
        let vs = s.viewshed([x, y, z]);
        let rule = |_| PitchRule::Horizon { sy: 0.33, min: -20.0 * DEG, max: 0.0 };
        let (yaw, st, vscore) = best_yaw(&vs, &yaws, vfov, rule, |_, v| landscape_score(v, 0.33));
        let sun = pick_sun(&vs, yaw, &[80.0, -80.0], sun_elev);
        let cam = cam_at(x, y, z, yaw, st.pitch, 60.0);
        let c = normal_candidate(s0 + vscore, cam, sun, 1.0, 1.0, String::new(), [x, z], "generic".into());
        if best.as_ref().map_or(true, |b| c.score > b.score) {
            best = Some(c);
        }
    }
    best.unwrap_or_else(|| {
        let cam = cam_at(0.5, SEA as f64 + 60.0, 0.5, 0.0, -8.0 * DEG, 60.0);
        let sun = Sun { azimuth_deg: 80.0, elevation_deg: sun_elev };
        normal_candidate(0.0, cam, sun, 1.0, 1.0, String::new(), [0.5, 0.5], "origin".into())
    })
}

// =====================================================================================
// 5. Selection
// =====================================================================================

fn min_dist(p: [f64; 2], taken: &[[f64; 2]]) -> f64 {
    taken.iter().map(|t| (t[0] - p[0]).hypot(t[1] - p[1])).fold(f64::INFINITY, f64::min)
}

/// Pick the best candidate whose camera is far enough from the taken ones (relaxing the
/// distance in steps). Returns (candidate, relaxed?).
fn select(cands: &[Candidate], taken: &[[f64; 2]]) -> Option<(Candidate, bool)> {
    for (i, &d) in [MIN_SEPARATION, 1000.0, 500.0, 0.0].iter().enumerate() {
        if let Some(c) = cands.iter().find(|c| min_dist([c.camera.pos[0], c.camera.pos[2]], taken) >= d) {
            return Some((c.clone(), i > 0));
        }
    }
    None
}

/// Plan exactly 8 shots for this world.
pub fn plan_shots(gen: &WorldGen) -> Vec<Shot> {
    plan_shots_detailed(gen).into_iter().map(|p| p.shot).collect()
}

/// [`plan_shots`] with scouting diagnostics.
pub fn plan_shots_detailed(gen: &WorldGen) -> Vec<PlannedShot> {
    let s = Scout::new(gen);
    let mut taken: Vec<[f64; 2]> = Vec::new();
    let mut chosen: Vec<Option<(Candidate, bool)>> = vec![None; 8];
    // order: most constrained / most important first
    let order = [0usize, 4, 2, 1, 3, 5, 6, 7];
    for &idx in &order {
        let cands = match idx {
            0 => plan_panorama(&s),
            1 => plan_river(&s),
            2 => plan_coast(&s),
            3 => plan_drylands(&s),
            4 => {
                let mut avoid = taken.clone();
                if let Some(Some((p, _))) = chosen.first() {
                    avoid.push(p.target);
                }
                plan_alpine(&s, &avoid)
            }
            5 => plan_forest(&s),
            6 => plan_cutaway(&s),
            _ => plan_orbital(&s),
        };
        let pick = match select(&cands, &taken) {
            Some(p) => p,
            None => {
                let sun_elev = match idx {
                    2 => 4.5,
                    3 => 9.0,
                    4 => 17.0,
                    _ => 25.0,
                };
                (plan_generic(&s, &taken, sun_elev), true)
            }
        };
        taken.push([pick.0.camera.pos[0], pick.0.camera.pos[2]]);
        chosen[idx] = Some(pick);
    }
    chosen
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let (mut c, fallback) = c.expect("all shots planned");
            if c.title.is_empty() {
                c.title = default_title(i, &c);
            }
            let col = gen.column(c.camera.pos[0], c.camera.pos[2]);
            PlannedShot {
                shot: Shot {
                    index: i + 1,
                    name: SHOT_NAMES[i].to_string(),
                    title: c.title,
                    camera: c.camera,
                    sun: c.sun,
                    mode: c.mode,
                    build: c.build,
                    exposure: c.exposure,
                    haze: c.haze,
                },
                score: c.score,
                target: c.target,
                ground_y: surface_of(&col),
                biome: col.biome,
                note: c.note,
                fallback,
            }
        })
        .collect()
}

fn default_title(i: usize, c: &Candidate) -> String {
    let y = c.camera.pos[1].round() as i32;
    match i {
        0 => format!("Панорама (y={})", y),
        1 => "Долина".to_string(),
        2 => "Закат над водой".to_string(),
        3 => "Равнины в косом свете".to_string(),
        4 => "Горный вид".to_string(),
        5 => "Лес".to_string(),
        6 => "Разрез".to_string(),
        _ => "Вид с высоты".to_string(),
    }
}

// =====================================================================================
// 6. Map (shaded relief + biomes + water + optional shot overlay)
// =====================================================================================

/// Shaded-relief biome map rendering.
pub mod map {
    use super::*;
    use crate::materials::{linear_to_srgb, srgb8};
    use crate::math::Vec3;

    /// Distinct overlay colours per shot index (sRGB).
    pub const SHOT_COLORS: [[u8; 3]; 8] = [
        [255, 80, 60],
        [40, 200, 255],
        [255, 170, 30],
        [230, 60, 230],
        [255, 255, 255],
        [90, 255, 90],
        [255, 230, 0],
        [0, 255, 210],
    ];

    /// Map parameters: pixel (i, j) ↦ world (cx + (i + ½ − size/2)·scale, cz + (j + ½ − size/2)·scale).
    /// +x to the right, +z downward.
    #[derive(Clone, Copy, Debug)]
    pub struct MapParams {
        pub cx: f64,
        pub cz: f64,
        pub size: u32,
        pub scale: f64,
    }

    impl MapParams {
        #[inline]
        pub fn to_px(&self, x: f64, z: f64) -> (f64, f64) {
            (
                (x - self.cx) / self.scale + self.size as f64 * 0.5,
                (z - self.cz) / self.scale + self.size as f64 * 0.5,
            )
        }
    }

    /// Smallest scale (≥ `min_scale`, integer) for which every shot camera and target lies at
    /// least `margin` blocks inside the map.
    pub fn fit_scale(shots: &[PlannedShot], cx: f64, cz: f64, size: u32, min_scale: f64, margin: f64) -> f64 {
        let mut ext = 0.0f64;
        for p in shots {
            for q in [[p.shot.camera.pos[0], p.shot.camera.pos[2]], p.target] {
                ext = ext.max((q[0] - cx).abs()).max((q[1] - cz).abs());
            }
        }
        min_scale.max(((ext + margin) * 2.0 / size as f64).ceil())
    }

    /// Render the map as RGB8 (row-major, top row first).
    pub fn render_map(gen: &WorldGen, mp: MapParams, shots: &[PlannedShot]) -> Vec<u8> {
        let n = mp.size as usize;
        let cols: Vec<ColumnInfo> = (0..n * n)
            .into_par_iter()
            .map(|k| {
                let (i, j) = (k % n, k / n);
                let x = mp.cx + (i as f64 + 0.5 - n as f64 * 0.5) * mp.scale;
                let z = mp.cz + (j as f64 + 0.5 - n as f64 * 0.5) * mp.scale;
                gen.column_lod(x, z, mp.scale)
            })
            .collect();
        let surf: Vec<f32> = cols.iter().map(surface_of).collect();
        let ht: Vec<f32> = cols.iter().map(|c| c.height).collect();
        let at = |v: &Vec<f32>, i: isize, j: isize| -> f32 {
            v[(j.clamp(0, n as isize - 1) as usize) * n + i.clamp(0, n as isize - 1) as usize]
        };
        let sc = mp.scale as f32;
        // light directions (unit), weights: multi-directional oblique hillshade
        let lights = [
            (Vec3::new(-1.0, 1.2, -1.0).normalize(), 0.6f32),
            (Vec3::new(-1.0, 1.2, 0.0).normalize(), 0.2),
            (Vec3::new(0.0, 1.2, -1.0).normalize(), 0.2),
        ];
        let mut rgb = vec![0u8; n * n * 3];
        rgb.par_chunks_mut(n * 3).enumerate().for_each(|(j, row)| {
            for i in 0..n {
                let k = j * n + i;
                let c = &cols[k];
                let (ii, jj) = (i as isize, j as isize);
                // gradient of the visible surface (central differences, vertical exaggeration)
                let ex = 2.0 + 0.04 * sc;
                let gx = (at(&surf, ii + 1, jj) - at(&surf, ii - 1, jj)) / (2.0 * sc) * ex;
                let gz = (at(&surf, ii, jj + 1) - at(&surf, ii, jj - 1)) / (2.0 * sc) * ex;
                let nrm = Vec3::new(-gx, 1.0, -gz).normalize();
                let mut shade = 0.0;
                for (l, w) in lights {
                    shade += w * nrm.dot(l).max(0.0);
                }
                let shade = 0.32 + 0.95 * shade / lights[0].0.y;
                let wet = surf[k] > c.height + 0.01;
                let bc = c.biome.map_color();
                let mut col = srgb8(bc[0], bc[1], bc[2]);
                if wet {
                    let depth = surf[k] - c.height;
                    let shallow = srgb8(70, 160, 205);
                    let deep = srgb8(12, 32, 92);
                    let t = 1.0 - (-depth / 28.0).exp();
                    col = shallow.lerp(deep, t);
                    if c.water_level > SEA_LEVEL {
                        col = col.lerp(srgb8(60, 140, 215), 0.35); // lakes slightly brighter
                    }
                    // subdued bathymetric shading
                    let bx = (at(&ht, ii + 1, jj) - at(&ht, ii - 1, jj)) / (2.0 * sc) * ex;
                    let bz = (at(&ht, ii, jj + 1) - at(&ht, ii, jj - 1)) / (2.0 * sc) * ex;
                    let bn = Vec3::new(-bx, 1.0, -bz).normalize();
                    col = col * (0.85 + 0.25 * bn.dot(lights[0].0).max(0.0));
                } else {
                    // hypsometric brightening with altitude + hillshade
                    let alt = ((c.height - SEA) / 350.0).clamp(0.0, 1.0);
                    col = col.lerp(Vec3::splat(0.75), 0.15 * alt) * shade;
                    // anti-aliased contour lines: |frac(h/Δ) − ½| measured in pixels via |∇h|
                    let grad = ((at(&ht, ii + 1, jj) - at(&ht, ii - 1, jj)).powi(2)
                        + (at(&ht, ii, jj + 1) - at(&ht, ii, jj - 1)).powi(2))
                    .sqrt()
                        * 0.5;
                    for (step, strength) in [(50.0f32, 0.12f32), (250.0, 0.28)] {
                        let v = (c.height - SEA) / step;
                        let dpx = (v - v.round()).abs() * step / grad.max(1e-3);
                        let a = (1.0 - dpx / 0.8).clamp(0.0, 1.0);
                        col = col * (1.0 - strength * a);
                    }
                }
                // coastline outline
                let edge = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(di, dj)| {
                    let kk = (jj + dj).clamp(0, n as isize - 1) as usize * n + (ii + di).clamp(0, n as isize - 1) as usize;
                    (surf[kk] > cols[kk].height + 0.01) != wet
                });
                if edge && !wet {
                    col = col * 0.55;
                }
                let o = i * 3;
                row[o] = (linear_to_srgb(col.x).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                row[o + 1] = (linear_to_srgb(col.y).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                row[o + 2] = (linear_to_srgb(col.z).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
        let mut canvas = Canvas { w: n, h: n, rgb };
        // scout radius (dashed circle) + scale bar
        let (ox, oz) = mp.to_px(0.0, 0.0);
        let rr = SCOUT_RADIUS / mp.scale;
        let segs = 180;
        for q in 0..segs {
            if q % 2 == 1 {
                continue;
            }
            let a0 = q as f64 / segs as f64 * 2.0 * PI;
            let a1 = (q + 1) as f64 / segs as f64 * 2.0 * PI;
            canvas.line(ox + rr * a0.sin(), oz + rr * a0.cos(), ox + rr * a1.sin(), oz + rr * a1.cos(), 1.2, [255, 255, 255], 0.5);
        }
        canvas.scale_bar(mp.scale);
        for p in shots {
            draw_shot(&mut canvas, &mp, p);
        }
        canvas.rgb
    }

    fn draw_shot(cv: &mut Canvas, mp: &MapParams, p: &PlannedShot) {
        let s = &p.shot;
        let col = SHOT_COLORS[(s.index - 1) % 8];
        let (px, pz) = mp.to_px(s.camera.pos[0], s.camera.pos[2]);
        if let ViewMode::Diorama { min_x, max_x, min_z, max_z, .. } = s.mode {
            let (ax, az) = mp.to_px(min_x as f64, min_z as f64);
            let (bx, bz) = mp.to_px(max_x as f64, max_z as f64);
            let (ax, az, bx, bz) = (ax - 2.0, az - 2.0, bx + 2.0, bz + 2.0);
            for &(x0, z0, x1, z1) in &[(ax, az, bx, az), (bx, az, bx, bz), (bx, bz, ax, bz), (ax, bz, ax, az)] {
                cv.line(x0, z0, x1, z1, 2.5, [0, 0, 0], 0.8);
                cv.line(x0, z0, x1, z1, 1.4, col, 1.0);
            }
            let (tx, tz) = mp.to_px(p.target[0], p.target[1]);
            cv.line(px, pz, tx, tz, 1.2, col, 0.8);
        } else {
            // view cone: wedge of half-angle hfov/2, length by shot type
            let half = hfov(s.camera.fov_y_deg.to_radians()) * 0.5;
            let len_world = if s.name == "orbital" { 9000.0 } else { 3000.0 };
            let len = (len_world / mp.scale).max(25.0);
            cv.wedge(px, pz, s.camera.yaw, half, len, col, 0.22);
            for sgn in [-1.0, 1.0] {
                let a = s.camera.yaw + sgn * half;
                let (ex, ez) = (px + len * a.sin(), pz + len * a.cos());
                cv.line(px, pz, ex, ez, 2.6, [0, 0, 0], 0.6);
                cv.line(px, pz, ex, ez, 1.3, col, 1.0);
            }
            let (ex, ez) = (px + len * s.camera.yaw.sin(), pz + len * s.camera.yaw.cos());
            cv.line(px, pz, ex, ez, 1.0, col, 0.6);
        }
        cv.disc(px, pz, 6.5, [0, 0, 0], 1.0);
        cv.disc(px, pz, 5.0, col, 1.0);
        cv.digit_label(px + 9.0, pz - 9.0, s.index as u32, col);
    }

    struct Canvas {
        w: usize,
        h: usize,
        rgb: Vec<u8>,
    }

    impl Canvas {
        #[inline]
        fn blend(&mut self, x: i64, y: i64, c: [u8; 3], a: f32) {
            if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || a <= 0.0 {
                return;
            }
            let o = (y as usize * self.w + x as usize) * 3;
            for q in 0..3 {
                let v = self.rgb[o + q] as f32 * (1.0 - a) + c[q] as f32 * a;
                self.rgb[o + q] = v.round().clamp(0.0, 255.0) as u8;
            }
        }

        /// Anti-aliased thick segment: coverage = clamp(w/2 + ½ − dist(p, segment)).
        fn line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, width: f64, c: [u8; 3], alpha: f32) {
            let r = width * 0.5 + 1.0;
            let (bx0, bx1) = ((x0.min(x1) - r).floor() as i64, (x0.max(x1) + r).ceil() as i64);
            let (by0, by1) = ((y0.min(y1) - r).floor() as i64, (y0.max(y1) + r).ceil() as i64);
            let (dx, dy) = (x1 - x0, y1 - y0);
            let l2 = (dx * dx + dy * dy).max(1e-9);
            for y in by0.max(0)..=by1.min(self.h as i64 - 1) {
                for x in bx0.max(0)..=bx1.min(self.w as i64 - 1) {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let t = (((px - x0) * dx + (py - y0) * dy) / l2).clamp(0.0, 1.0);
                    let d = (px - x0 - t * dx).hypot(py - y0 - t * dy);
                    let cov = (width * 0.5 + 0.5 - d).clamp(0.0, 1.0) as f32;
                    self.blend(x, y, c, cov * alpha);
                }
            }
        }

        fn disc(&mut self, cx: f64, cy: f64, r: f64, c: [u8; 3], alpha: f32) {
            for y in (cy - r - 1.0).floor() as i64..=(cy + r + 1.0).ceil() as i64 {
                for x in (cx - r - 1.0).floor() as i64..=(cx + r + 1.0).ceil() as i64 {
                    let d = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy);
                    self.blend(x, y, c, (r + 0.5 - d).clamp(0.0, 1.0) as f32 * alpha);
                }
            }
        }

        /// Filled circular sector (view cone) around direction `yaw` (map: x = sin, y = cos).
        fn wedge(&mut self, cx: f64, cy: f64, yaw: f64, half: f64, len: f64, c: [u8; 3], alpha: f32) {
            let (dx, dy) = (yaw.sin(), yaw.cos());
            for y in (cy - len - 1.0).floor() as i64..=(cy + len + 1.0).ceil() as i64 {
                for x in (cx - len - 1.0).floor() as i64..=(cx + len + 1.0).ceil() as i64 {
                    let (vx, vy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                    let d = vx.hypot(vy);
                    if d > len || d < 1e-9 {
                        continue;
                    }
                    let cosang = (vx * dx + vy * dy) / d;
                    if cosang >= half.cos() {
                        let fade = 1.0 - 0.6 * (d / len);
                        self.blend(x, y, c, alpha * fade as f32);
                    }
                }
            }
        }

        fn glyph(&mut self, x: f64, y: f64, g: &[u8; 7], px: i64, c: [u8; 3]) {
            for (r, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0b10000 >> col) != 0 {
                        for yy in 0..px {
                            for xx in 0..px {
                                self.blend(x as i64 + col * px + xx, y as i64 + r as i64 * px + yy, c, 1.0);
                            }
                        }
                    }
                }
            }
        }

        fn digit_label(&mut self, x: f64, y: f64, d: u32, c: [u8; 3]) {
            let s = d.to_string();
            let px = 2i64;
            let w = s.len() as f64 * 12.0 + 4.0;
            for yy in 0..(7 * px + 6) {
                for xx in 0..w as i64 {
                    self.blend(x as i64 - 3 + xx, y as i64 - 3 + yy, [0, 0, 0], 0.65);
                }
            }
            for (q, ch) in s.chars().enumerate() {
                let g = &FONT[ch.to_digit(10).unwrap_or(0) as usize];
                self.glyph(x + q as f64 * 12.0, y, g, px, c);
            }
        }

        /// 5 km scale bar with 1 km ticks in the lower-left corner, labelled "5 km".
        fn scale_bar(&mut self, scale: f64) {
            let km = if scale * self.w as f64 > 40_000.0 { 10.0 } else { 5.0 };
            let len = km * 1000.0 / scale;
            let (x0, y0) = (20.0, self.h as f64 - 28.0);
            for yy in -16..10i64 {
                for xx in -8..(len as i64 + 70) {
                    self.blend(x0 as i64 + xx, y0 as i64 + yy, [0, 0, 0], 0.45);
                }
            }
            self.line(x0, y0, x0 + len, y0, 3.0, [255, 255, 255], 1.0);
            for t in 0..=(km as i64) {
                let x = x0 + len * t as f64 / km;
                self.line(x, y0 - 6.0, x, y0 + 4.0, 2.0, [255, 255, 255], 1.0);
            }
            let label_x = x0 + len + 10.0;
            let digits = (km as u32).to_string();
            let mut xx = label_x;
            for ch in digits.chars() {
                self.glyph(xx, y0 - 7.0, &FONT[ch.to_digit(10).unwrap() as usize], 2, [255, 255, 255]);
                xx += 12.0;
            }
            self.glyph(xx + 4.0, y0 - 7.0, &FONT_K, 2, [255, 255, 255]);
            self.glyph(xx + 16.0, y0 - 7.0, &FONT_M, 2, [255, 255, 255]);
        }
    }

    /// 5×7 bitmap digits (MSB = leftmost column).
    const FONT: [[u8; 7]; 10] = [
        [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
    ];
    const FONT_K: [u8; 7] = [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010];
    const FONT_M: [u8; 7] = [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10101, 0b10101];
}

// =====================================================================================
// Tests
// =====================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prominence_two_peaks() {
        // 1-D-like ridge: peaks 10 and 7 separated by a col at 3, on a 5×5 lattice
        let n = 5;
        let mut s = vec![0.0f32; n * n];
        let row = [10.0, 5.0, 3.0, 6.0, 7.0];
        for j in 0..n {
            for i in 0..n {
                s[j * n + i] = if j == 2 { row[i] } else { 0.0 };
            }
        }
        let p = prominence(&s, n);
        assert_eq!(p[2 * n], 10.0); // global summit: relative to the lowest node (0)
        assert_eq!(p[2 * n + 4], 4.0); // 7 − col 3
        assert_eq!(p[2 * n + 1], 0.0);
    }

    #[test]
    fn sat_fraction() {
        let n = 10;
        let sat = Sat::new(n, |k| (k % n) < 5);
        assert!((sat.frac(2, 5, 2) - 1.0).abs() < 1e-6);
        assert!((sat.frac(5, 5, 1) - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn horizon_algorithm_flat_plane() {
        // On a flat plane below the camera everything is visible and the horizon is ≈ 0 (minus curvature).
        let st = {
            let pieces: Vec<Piece> = (1..200)
                .map(|i| {
                    let d = i as f32 * 10.0;
                    let a1 = (-(100.0f32) / d).atan();
                    let a0 = (-(100.0f32) / (d - 10.0).max(1.0)).atan();
                    Piece { a0, a1, d, h: 0.0, mask: cat::LAND | cat::GRASS }
                })
                .collect();
            let ray = RayProfile { pieces, horizon: (-100.0f32 / 1990.0).atan(), layers: 0.0, hdist: 1990.0 };
            frame_stats(&[(&ray, 0.0)], 60.0 * DEG, PitchRule::Fixed(0.0))
        };
        // horizon at atan(−100/1990) ≈ −2.9° → s_y ≈ −0.088 → sky ≈ 0.544
        assert!(st.sky > 0.5 && st.sky < 0.6, "sky {}", st.sky);
        assert!((st.sky + st.terrain - 1.0).abs() < 0.02);
    }

    fn check_plan(seed: u64) -> Vec<PlannedShot> {
        let gen = WorldGen::new(seed);
        let t = std::time::Instant::now();
        let plan = plan_shots_detailed(&gen);
        eprintln!("seed {seed}: planned in {:.2}s", t.elapsed().as_secs_f64());
        assert_eq!(plan.len(), 8);
        let mut names: Vec<&str> = plan.iter().map(|p| p.shot.name.as_str()).collect();
        for (i, p) in plan.iter().enumerate() {
            assert_eq!(p.shot.index, i + 1);
            assert_eq!(p.shot.name, SHOT_NAMES[i]);
            assert!(!p.shot.title.is_empty());
            let c = &p.shot.camera;
            assert!(c.pos.iter().all(|v| v.is_finite()) && c.yaw.is_finite() && c.pitch.is_finite());
            assert!(c.pitch.abs() < 1.2 && c.fov_y_deg > 10.0 && c.fov_y_deg < 100.0);
            assert!(p.shot.sun.elevation_deg > 0.0 && p.shot.sun.elevation_deg < 90.0);
            eprintln!(
                "  {} {:<13} cam ({:.0},{:.0},{:.0}) yaw {:.0} pitch {:.1} | {} | {}",
                p.shot.index,
                p.shot.name,
                c.pos[0],
                c.pos[1],
                c.pos[2],
                c.yaw.to_degrees(),
                c.pitch.to_degrees(),
                p.shot.title,
                p.note
            );
            match p.shot.mode {
                ViewMode::Normal => {
                    // camera above terrain (incl. 3-D band) and water within a few blocks around it
                    let floor = clearance(&gen, c.pos[0], c.pos[2], 3.0);
                    assert!(c.pos[1] as f32 > floor + 1.0, "{}: camera y {} below floor {}", p.shot.name, c.pos[1], floor);
                    let col = gen.column(c.pos[0], c.pos[2]);
                    assert!(c.pos[1] as f32 > col.height + col.jaggedness + 1.0);
                    let b = p.shot.build;
                    let (ox, oz) = b.l0_origin();
                    let inside = |v: f64, o: i32| v >= o as f64 && v < (o + b.l0_size as i32) as f64;
                    assert!(p.shot.name == "orbital" || (inside(c.pos[0], ox) && inside(c.pos[2], oz)));
                }
                ViewMode::Diorama { min_x, max_x, min_z, max_z, min_y } => {
                    assert_eq!(max_x - min_x, CUTAWAY_SIZE);
                    assert_eq!(max_z - min_z, CUTAWAY_SIZE);
                    assert!(min_y >= 4);
                    let outside = c.pos[0] < min_x as f64 || c.pos[0] > max_x as f64 || c.pos[2] < min_z as f64 || c.pos[2] > max_z as f64;
                    assert!(outside, "cutaway camera inside the box");
                    assert_eq!(p.shot.build.hf_levels, 0);
                    assert_eq!(p.shot.build.l0_size, 256);
                }
            }
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8, "names must be distinct");
        plan
    }

    #[test]
    fn plan_has_eight_valid_shots_and_is_deterministic() {
        let a = check_plan(1);
        let b = plan_shots(&WorldGen::new(1));
        for (p, q) in a.iter().zip(b.iter()) {
            assert_eq!(format!("{:?}", p.shot), format!("{:?}", q));
        }
        // well separated where possible
        let mut close = 0;
        for i in 0..8 {
            for j in i + 1..8 {
                let (p, q) = (&a[i].shot.camera.pos, &a[j].shot.camera.pos);
                if (p[0] - q[0]).hypot(p[2] - q[2]) < MIN_SEPARATION {
                    close += 1;
                }
            }
        }
        assert!(close <= 1, "{close} pairs of shots closer than {MIN_SEPARATION}");
    }

    #[test]
    fn plan_other_seeds() {
        check_plan(42);
        check_plan(2026);
    }
}
