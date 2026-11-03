//! Trees and other surface features. OWNER: caves-features.
//!
//! # Point process (placement)
//!
//! Three **marked Matérn type-II hard-core processes** on jittered grids (big / medium / small
//! layer: cell 7 / 5 / 4 blocks, hard-core radius 5.6 / 3.6 / 2.6). Every grid cell owns one
//! candidate whose position, priority, thinning variable `u` and shape seed are pure hashes of
//! (seed, layer, cell). A candidate survives the hard core iff no candidate of the same layer
//! within the hard-core radius has a higher priority — a purely hash-geometric test, so the
//! result is **independent of the query rectangle** and needs no terrain evaluation.
//! Survivors are then **independently thinned** (`u < p`) by the local vegetation density
//! `p(biome, grove, treeline, slope)`; independent thinning of a hard-core process keeps the
//! minimum spacing (blue noise) while the intensity follows the biome. Cross-layer exclusion: a
//! medium/small candidate yields to an accepted bigger tree within 4.5 / 3.5 / 2.2 blocks.
//! Retention of the hard core is measured once (Monte Carlo over 4096 cells) and used by the LOD.
//!
//! Densities are modulated by
//! * a **grove field** `g = (n(x/170) + ½ n(x/63)) / 1.5`: clearings inside closed forests
//!   (`0.12 + 0.88·smoothstep(−0.48, −0.26, g)`), groves / oases inside open land
//!   (`p → mix(p, p_grove, smoothstep(g₀, g₀ + 0.22, g))`),
//! * a **mix field** (λ ≈ 95) that tilts species weights `wᵢ·exp(2 bᵢ m)` → birch stands inside
//!   oak forests, pine patches inside spruce taiga …,
//! * the **treeline** relative to the local snow line `S` (`r = H − S`): trees fade over
//!   `r ∈ [−95, −30]`, shrink (`scale = 0.5 + 0.5·(1 − smoothstep(−110, −30, r))`), bushes go up
//!   to `r ≈ −12`; mega trees need `scale > 0.8`; slope fades trees over `[0.45, 0.9]`.
//!
//! # Shapes (rasterised analytically into a local voxel grid, then run-length encoded)
//! * Leaf canopies are **metaball** iso-surfaces `Σ (1 − |Mᵢ(p − cᵢ)|²)₊² ≥ τ` (ellipsoidal
//!   balls, τ = ¼) with the threshold dithered per voxel by a hash → lumpy cumulus crowns with
//!   leafy edges. Oak, swamp oak, birch, jungle, bushes, big-oak branch clusters.
//! * Conifers are **layered cones with ring modulation**:
//!   `r(y) = R·(0.12 + 0.88·u^0.9)·(1 − ρ(1 − frac(Δy/P + φ)))·(1 + a·cos(4θ + ψ_tier))` — the
//!   sawtooth `frac` makes tiers (narrow at the top of each tier, flaring at its base) and the
//!   4-lobed azimuthal term rotates per tier (star-shaped whorls). Spruce, pine, mega spruce (2×2).
//! * Branches are 3D segments rasterised as face-connected voxel paths.
//! * Acacia: straight bole, a bent (diagonal staircase) leader + optional fork, flat
//!   superellipse discs `|x/r|^p + |z/r|^p ≤ 1` (p ≈ 2.6). Dark oak: 2×2 bole and a stack of
//!   wide superellipse discs (a flat dome). Mega jungle: 2×2, buttress roots, a ring of flattened
//!   metaballs (broad canopy) and side limbs. Palm: quadratic-curve trunk, parabolic drooping
//!   fronds. Cactus (saguaro arms), boulders (union of dithered ellipsoids, embedded, mossy or
//!   snow-capped), ice spires (`r(y) = r₀(1 − y/H)^1.25`, leaning), fallen logs.
//!
//! # LOD
//! `canopy_lod` is the expectation of the same process (Boolean model):
//! `coverage = 1 − exp(−x − 0.6x²)`, `x = Σ_L ρ_L Σ_s f_s·π r_s²` (second-order term: hard-core
//! crowns overlap less than Poisson ones), with `ρ_L = retention_L/cell_L²·p_L·excl_L`
//! (`excl` = Poisson void probability of the bigger layers within the exclusion radius),
//! mean canopy height `Σ ρA h / Σ ρA`, representative leaf of the dominant species. The grove and
//! mix fields are band-limited and blend to their measured expectation as the footprint grows.
use super::biome::SnowLine;
use super::{Biome, CanopyLod, ColumnInfo, Feature, FeatureColumn, FeatureKind, WorldGen, MAX_FEATURE_RADIUS};
use crate::materials::Material;
use crate::noise::{derive_seed, hash2, hash3, hash_f01, mix64, salt, Simplex};

// =====================================================================================
// Species
// =====================================================================================

/// Concrete shape recipes (finer than `FeatureKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Species {
    Oak,
    SwampOak,
    BigOak,
    Birch,
    TallBirch,
    Spruce,
    Pine,
    MegaSpruce,
    Jungle,
    MegaJungle,
    Acacia,
    DarkOak,
    Bush,
    Cactus,
    Boulder,
    IceSpire,
    Palm,
    FallenLog,
}

pub const ALL_SPECIES: [Species; 18] = [
    Species::Oak,
    Species::SwampOak,
    Species::BigOak,
    Species::Birch,
    Species::TallBirch,
    Species::Spruce,
    Species::Pine,
    Species::MegaSpruce,
    Species::Jungle,
    Species::MegaJungle,
    Species::Acacia,
    Species::DarkOak,
    Species::Bush,
    Species::Cactus,
    Species::Boulder,
    Species::IceSpire,
    Species::Palm,
    Species::FallenLog,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Tree,
    MegaTree,
    Bush,
    Cactus,
    Rock,
    Ice,
    Log,
}

impl Species {
    pub fn kind(self) -> FeatureKind {
        use Species::*;
        match self {
            Oak | SwampOak => FeatureKind::Oak,
            BigOak => FeatureKind::BigOak,
            Birch | TallBirch => FeatureKind::Birch,
            Spruce | Pine => FeatureKind::Spruce,
            MegaSpruce => FeatureKind::MegaSpruce,
            Jungle => FeatureKind::Jungle,
            MegaJungle => FeatureKind::MegaJungle,
            Acacia => FeatureKind::Acacia,
            DarkOak => FeatureKind::DarkOak,
            Bush => FeatureKind::Bush,
            Cactus => FeatureKind::Cactus,
            Boulder => FeatureKind::Boulder,
            IceSpire => FeatureKind::IceSpire,
            Palm | FallenLog => FeatureKind::Other,
        }
    }
    pub fn name(self) -> &'static str {
        use Species::*;
        match self {
            Oak => "oak",
            SwampOak => "swamp_oak",
            BigOak => "big_oak",
            Birch => "birch",
            TallBirch => "tall_birch",
            Spruce => "spruce",
            Pine => "pine",
            MegaSpruce => "mega_spruce",
            Jungle => "jungle",
            MegaJungle => "mega_jungle",
            Acacia => "acacia",
            DarkOak => "dark_oak",
            Bush => "bush",
            Cactus => "cactus",
            Boulder => "boulder",
            IceSpire => "ice_spire",
            Palm => "palm",
            FallenLog => "fallen_log",
        }
    }
    fn class(self) -> Class {
        use Species::*;
        match self {
            MegaSpruce | MegaJungle | DarkOak | BigOak => Class::MegaTree,
            Bush => Class::Bush,
            Cactus => Class::Cactus,
            Boulder => Class::Rock,
            IceSpire => Class::Ice,
            FallenLog => Class::Log,
            _ => Class::Tree,
        }
    }
    /// (mean canopy radius, mean top height above ground) at scale 1 — used by the LOD
    /// expectation; validated against rasterised shapes by the test `lod_shape_constants`.
    fn lod_shape(self) -> (f32, f32) {
        use Species::*;
        match self {
            Oak => (3.4, 8.4),
            SwampOak => (4.05, 8.4),
            BigOak => (4.9, 14.5),
            Birch => (2.42, 9.0),
            TallBirch => (2.43, 13.3),
            Spruce => (2.72, 12.0),
            Pine => (2.33, 14.2),
            MegaSpruce => (5.26, 31.5),
            Jungle => (3.62, 15.2),
            MegaJungle => (6.54, 29.5),
            Acacia => (4.13, 9.1),
            DarkOak => (5.43, 11.4),
            Bush => (1.8, 2.5),
            Cactus => (0.62, 3.4),
            Boulder => (2.55, 2.1),
            IceSpire => (3.48, 14.6),
            Palm => (3.3, 10.6),
            FallenLog => (1.4, 1.6),
        }
    }
}

// =====================================================================================
// Vegetation tables
// =====================================================================================

type Mix = &'static [(Species, f32, f32)]; // (species, weight, mix-field bias)

#[derive(Clone, Copy)]
struct Veg {
    /// Base acceptance probability per layer (big, medium, small).
    p: [f32; 3],
    /// Closed canopy biome: clearings where the grove field is low.
    closed: bool,
    /// Open biome: medium-layer probability inside groves and the grove threshold g₀.
    grove: Option<(f32, f32)>,
    mix: [Mix; 3],
}

mod tables {
    use super::Species::*;
    use super::{Mix, Veg};
    use crate::gen::{Biome, ColumnInfo};
    const NONE: Mix = &[];
    const S_BUSH: Mix = &[(Bush, 1.0, 0.0)];
    const S_ROCK: Mix = &[(Boulder, 1.0, 0.0)];

    pub(super) fn veg(b: Biome, col: &ColumnInfo) -> Veg {
        use Biome::*;
        let v = |p: [f32; 3], closed: bool, grove: Option<(f32, f32)>, mix: [Mix; 3]| Veg { p, closed, grove, mix };
        match b {
            Plains => v(
                [0.0, 0.012, 0.035],
                false,
                Some((0.5, 0.38)),
                [NONE, &[(Oak, 0.72, 0.0), (BigOak, 0.14, 0.6), (Birch, 0.14, -0.9)], &[(Bush, 0.8, 0.0), (Boulder, 0.12, 0.0), (FallenLog, 0.08, 0.0)]],
            ),
            Meadow => v(
                [0.0, 0.006, 0.035],
                false,
                Some((0.25, 0.42)),
                [NONE, &[(Oak, 0.45, 0.0), (Birch, 0.25, 0.5), (Spruce, 0.3, -0.6)], &[(Bush, 0.45, 0.0), (Boulder, 0.55, 0.0)]],
            ),
            Forest => v(
                [0.10, 0.70, 0.12],
                true,
                None,
                [
                    &[(BigOak, 1.0, 0.0)],
                    &[(Oak, 0.66, 0.0), (Birch, 0.22, 1.1), (BigOak, 0.12, -0.8)],
                    &[(Bush, 0.7, 0.0), (FallenLog, 0.22, 0.0), (Boulder, 0.08, 0.0)],
                ],
            ),
            BirchForest => v(
                [0.0, 0.74, 0.10],
                true,
                None,
                [NONE, &[(TallBirch, 0.55, 0.8), (Birch, 0.41, -0.3), (Oak, 0.04, -1.0)], &[(Bush, 0.6, 0.0), (FallenLog, 0.4, 0.0)]],
            ),
            DarkForest => v(
                [0.88, 0.38, 0.12],
                true,
                None,
                [&[(DarkOak, 1.0, 0.0)], &[(Oak, 0.45, 0.0), (BigOak, 0.35, 0.7), (Birch, 0.2, -0.7)], &[(Bush, 0.75, 0.0), (FallenLog, 0.25, 0.0)]],
            ),
            Taiga => v(
                [0.05, 0.66, 0.07],
                true,
                None,
                [&[(MegaSpruce, 1.0, 0.0)], &[(Spruce, 0.75, 0.0), (Pine, 0.25, 1.2)], &[(Bush, 0.45, 0.0), (Boulder, 0.35, 0.0), (FallenLog, 0.2, 0.0)]],
            ),
            OldGrowthTaiga => v(
                [0.50, 0.48, 0.09],
                true,
                None,
                [&[(MegaSpruce, 1.0, 0.0)], &[(Spruce, 0.82, 0.0), (Pine, 0.18, 1.0)], &[(Boulder, 0.5, 0.0), (Bush, 0.3, 0.0), (FallenLog, 0.2, 0.0)]],
            ),
            SnowyTaiga => v(
                [0.0, 0.45, 0.05],
                true,
                None,
                [NONE, &[(Spruce, 0.85, 0.0), (Pine, 0.15, 1.0)], &[(Bush, 0.55, 0.0), (Boulder, 0.45, 0.0)]],
            ),
            AlpineGrove => v(
                [0.0, 0.36, 0.07],
                true,
                None,
                [NONE, &[(Spruce, 0.7, 0.0), (Pine, 0.3, 1.0)], &[(Bush, 0.5, 0.0), (Boulder, 0.5, 0.0)]],
            ),
            SnowyPlains => v([0.0, 0.0, 0.016], false, None, [NONE, NONE, &[(IceSpire, 0.45, 0.0), (Boulder, 0.55, 0.0)]]),
            SnowySlopes | SnowyPeaks | Glacier => v([0.0, 0.0, 0.004], false, None, [NONE, NONE, S_ROCK]),
            Jungle => v([0.32, 0.78, 0.45], true, None, [&[(MegaJungle, 1.0, 0.0)], &[(super::Species::Jungle, 0.9, 0.0), (Palm, 0.1, 1.0)], S_BUSH]),
            Savanna => v(
                [0.0, 0.065, 0.035],
                false,
                Some((0.2, 0.4)),
                [NONE, &[(Acacia, 0.86, 0.0), (Oak, 0.14, -1.0)], &[(Bush, 0.7, 0.0), (Boulder, 0.3, 0.0)]],
            ),
            Desert => v([0.0, 0.0, 0.075], false, Some((0.14, 0.52)), [NONE, &[(Palm, 1.0, 0.0)], &[(Cactus, 0.92, 0.0), (Boulder, 0.08, 0.0)]]),
            Badlands => v([0.0, 0.0, 0.035], false, None, [NONE, NONE, &[(Cactus, 0.45, 0.0), (Bush, 0.55, 0.0)]]),
            Hoodoos => v([0.0, 0.0, 0.012], false, None, [NONE, NONE, &[(Cactus, 1.0, 0.0)]]),
            Swamp => v([0.0, 0.32, 0.06], true, None, [NONE, &[(SwampOak, 1.0, 0.0)], S_BUSH]),
            Beach if col.climate.temperature > 0.3 => v([0.0, 0.03, 0.0], false, None, [NONE, &[(Palm, 1.0, 0.0)], NONE]),
            StonyShore => v([0.0, 0.0, 0.05], false, None, [NONE, NONE, S_ROCK]),
            StonyPeaks => v([0.0, 0.0, 0.025], false, None, [NONE, NONE, S_ROCK]),
            Volcanic => v([0.0, 0.0, 0.02], false, None, [NONE, NONE, S_ROCK]),
            _ => v([0.0; 3], false, None, [NONE, NONE, NONE]),
        }
    }
}

// =====================================================================================
// Point process
// =====================================================================================

#[derive(Clone, Copy)]
struct LayerSpec {
    cell: i32,
    hard_core: f32,
}

const LAYERS: [LayerSpec; 3] = [
    LayerSpec { cell: 7, hard_core: 5.6 },
    LayerSpec { cell: 5, hard_core: 3.6 },
    LayerSpec { cell: 4, hard_core: 2.6 },
];
const BIG: usize = 0;
const MED: usize = 1;
const SMALL: usize = 2;
const EXCL_MED_BIG: f32 = 4.5;
const EXCL_SMALL_BIG: f32 = 3.5;
const EXCL_SMALL_MED: f32 = 2.2;
/// Global upper bound of the acceptance probability per layer (cheap pre-rejection).
const P_MAX: [f32; 3] = [0.92, 0.82, 0.5];

#[derive(Clone, Copy, Debug)]
struct Cand {
    x: i32,
    z: i32,
    prio: u64,
    u: f32,
    u2: f32,
    var: u64,
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Local environment of a root column.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    pub biome: Biome,
    /// Effective acceptance probability per layer.
    pub p: [f32; 3],
    /// Mix field value (tilts species weights).
    pub mix: f32,
    /// Height relative to the local snow line.
    pub rel: f32,
    /// Size scale from the treeline.
    pub scale: f32,
    pub slope: f32,
}

/// Build context for a single feature.
#[derive(Clone, Copy, Debug)]
pub struct TreeCtx {
    pub biome: Biome,
    /// Size scale (1 = full size; < 1 stunted near the treeline).
    pub scale: f32,
}

pub struct FeaturePlacer {
    pub seed: u64,
    layer_seed: [u64; 3],
    grove1: Simplex,
    grove2: Simplex,
    mixn: Simplex,
    snow: SnowLine,
    /// Fraction of grid candidates surviving the hard core, per layer.
    retention: [f32; 3],
    /// E[clearing modulation] and E[grove(g₀)] for the grove thresholds in use (far LOD).
    e_clear: f32,
    e_grove: [(f32, f32); 4],
}

const GROVE_L1: f64 = 170.0;
const GROVE_L2: f64 = 63.0;
const MIX_L: f64 = 95.0;

impl FeaturePlacer {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        let mut fp = FeaturePlacer {
            seed,
            layer_seed: [s("features.big"), s("features.medium"), s("features.small")],
            grove1: Simplex::new(s("features.grove1")),
            grove2: Simplex::new(s("features.grove2")),
            mixn: Simplex::new(s("features.mix")),
            snow: SnowLine::new(seed),
            retention: [0.0; 3],
            e_clear: 0.0,
            e_grove: [(0.38, 0.0), (0.40, 0.0), (0.42, 0.0), (0.52, 0.0)],
        };
        // Hard-core retention (Monte Carlo over 64×64 cells per layer).
        for l in 0..3 {
            let mut alive = 0;
            for cz in 0..64 {
                for cx in 0..64 {
                    if fp.survivor(l, cx, cz).is_some() {
                        alive += 1;
                    }
                }
            }
            fp.retention[l] = alive as f32 / 4096.0;
        }
        // Expectations of the grove modulations (for far LOD).
        let n = 4096;
        let mut ec = 0.0;
        let mut eg = [0.0f32; 4];
        for i in 0..n {
            let h = mix64(i as u64 ^ 0x6A0F);
            let x = hash_f01(h) as f64 * 40_000.0 - 20_000.0;
            let z = hash_f01(mix64(h)) as f64 * 40_000.0 - 20_000.0;
            let g = fp.grove(x, z, 0.0);
            ec += clearing(g);
            for (k, e) in eg.iter_mut().enumerate() {
                *e += smoothstep(fp.e_grove[k].0, fp.e_grove[k].0 + 0.22, g);
            }
        }
        fp.e_clear = ec / n as f32;
        for (e, sum) in fp.e_grove.iter_mut().zip(eg) {
            e.1 = sum / n as f32;
        }
        fp
    }

    #[inline]
    fn cand(&self, l: usize, cx: i32, cz: i32) -> Cand {
        let c = LAYERS[l].cell;
        let h = hash2(self.layer_seed[l], cx, cz);
        let ox = ((hash_f01(h) * c as f32) as i32).min(c - 1);
        let oz = ((hash_f01(mix64(h ^ 0x51)) * c as f32) as i32).min(c - 1);
        Cand {
            x: cx * c + ox,
            z: cz * c + oz,
            prio: mix64(h ^ 0xA7A7),
            u: hash_f01(mix64(h ^ 0x3C3C)),
            u2: hash_f01(mix64(h ^ 0x9191)),
            var: mix64(h ^ 0x7777),
        }
    }

    /// Matérn-II hard-core survivor of cell (cx, cz) in layer `l` (hash-only test).
    fn survivor(&self, l: usize, cx: i32, cz: i32) -> Option<Cand> {
        let c = self.cand(l, cx, cz);
        let hc = LAYERS[l].hard_core;
        let hc2 = hc * hc;
        let k = (hc / LAYERS[l].cell as f32).ceil() as i32;
        for dz in -k..=k {
            for dx in -k..=k {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let o = self.cand(l, cx + dx, cz + dz);
                let d2 = ((o.x - c.x) * (o.x - c.x) + (o.z - c.z) * (o.z - c.z)) as f32;
                if d2 < hc2 && (o.prio > c.prio || (o.prio == c.prio && (cx + dx, cz + dz) > (cx, cz))) {
                    return None;
                }
            }
        }
        Some(c)
    }

    /// Is there an accepted (under `p`) survivor of layer `l` within `r` of (x, z)?
    fn blocked_by(&self, l: usize, x: i32, z: i32, r: f32, p: f32) -> bool {
        if p <= 0.0 {
            return false;
        }
        let cell = LAYERS[l].cell;
        let ri = r.ceil() as i32;
        let (cx0, cx1) = ((x - ri).div_euclid(cell), (x + ri).div_euclid(cell));
        let (cz0, cz1) = ((z - ri).div_euclid(cell), (z + ri).div_euclid(cell));
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let o = self.cand(l, cx, cz);
                if o.u >= p {
                    continue;
                }
                let d2 = ((o.x - x) * (o.x - x) + (o.z - z) * (o.z - z)) as f32;
                if d2 < r * r && self.survivor(l, cx, cz).is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// Grove field g ∈ ≈[-1, 1] (band-limited for `footprint` > 0).
    #[inline]
    fn grove(&self, x: f64, z: f64, footprint: f64) -> f32 {
        let w1 = if footprint > 0.0 { (((GROVE_L1 / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let w2 = if footprint > 0.0 { (((GROVE_L2 / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let mut g = 0.0;
        if w1 > 0.0 {
            g += w1 * self.grove1.n2(x / GROVE_L1, z / GROVE_L1);
        }
        if w2 > 0.0 {
            g += 0.5 * w2 * self.grove2.n2(x / GROVE_L2 + 17.1, z / GROVE_L2 - 3.3);
        }
        g / 1.5
    }

    /// Environment at a column. `footprint` > 0 → band-limited fields blended to expectations.
    pub fn env(&self, col: &ColumnInfo, footprint: f64) -> Env {
        let veg = tables::veg(col.biome, col);
        let (x, z) = (col.x, col.z);
        let g = self.grove(x, z, footprint);
        // How much of the grove field survives the band limit (1 = full detail).
        let wg = if footprint > 0.0 { (((GROVE_L1 / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let mut p = veg.p;
        if veg.closed {
            let m = clearing(g) * wg + self.e_clear * (1.0 - wg);
            let boost = 1.0 + 0.15 * smoothstep(0.3, 0.6, g) * wg;
            p[BIG] = (p[BIG] * m * boost).min(P_MAX[BIG]);
            p[MED] = (p[MED] * m * boost).min(P_MAX[MED]);
        }
        if let Some((pg, g0)) = veg.grove {
            let e = self
                .e_grove
                .iter()
                .min_by(|a, b| (a.0 - g0).abs().total_cmp(&(b.0 - g0).abs()))
                .map(|e| e.1)
                .unwrap_or(0.1);
            let t = smoothstep(g0, g0 + 0.22, g) * wg + e * (1.0 - wg);
            p[MED] = p[MED] + (pg - p[MED]) * t;
        }
        let wm = if footprint > 0.0 { (((MIX_L / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let mix = if wm > 0.0 { wm * self.mixn.n2(x / MIX_L - 7.7, z / MIX_L + 2.9) } else { 0.0 };
        let rel = col.height - self.snow.at(x, z, col.climate.temperature);
        let scale = 0.5 + 0.5 * (1.0 - smoothstep(-110.0, -30.0, rel));
        Env { biome: col.biome, p, mix, rel, scale, slope: col.slope }
    }

    /// Survival factor of a species class in an environment (treeline, slope).
    fn class_factor(&self, sp: Species, env: &Env) -> f32 {
        let tree = (1.0 - smoothstep(-95.0, -30.0, env.rel)) * (1.0 - smoothstep(0.45, 0.9, env.slope));
        match sp.class() {
            Class::Tree => tree,
            Class::MegaTree => tree * smoothstep(0.78, 0.92, env.scale),
            Class::Bush => (1.0 - smoothstep(-45.0, -12.0, env.rel)) * (1.0 - smoothstep(0.6, 0.95, env.slope)),
            Class::Cactus => 1.0 - smoothstep(0.5, 0.9, env.slope),
            Class::Log => tree * (1.0 - smoothstep(0.2, 0.4, env.slope)),
            Class::Rock | Class::Ice => 1.0,
        }
    }

    /// Species weights of a layer at an environment (normalised, with the mix tilt).
    fn weights(&self, mix: Mix, env: &Env, out: &mut [(Species, f32); 8]) -> usize {
        let mut n = 0;
        let mut sum = 0.0;
        for &(sp, w, b) in mix.iter().take(8) {
            let we = w * (2.0 * b * env.mix).exp();
            out[n] = (sp, we);
            sum += we;
            n += 1;
        }
        if sum > 0.0 {
            for o in out[..n].iter_mut() {
                o.1 /= sum;
            }
        }
        n
    }

    /// All features whose root lies in [x0, x1) × [z0, z1).
    pub fn features_in(&self, gen: &WorldGen, x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<Feature> {
        let mut out = Vec::new();
        if x1 <= x0 || z1 <= z0 {
            return out;
        }
        for (l, spec) in LAYERS.iter().enumerate() {
            let cell = spec.cell;
            for cz in z0.div_euclid(cell)..=(z1 - 1).div_euclid(cell) {
                for cx in x0.div_euclid(cell)..=(x1 - 1).div_euclid(cell) {
                    let c = self.cand(l, cx, cz);
                    if c.x < x0 || c.x >= x1 || c.z < z0 || c.z >= z1 || c.u >= P_MAX[l] {
                        continue;
                    }
                    if self.survivor(l, cx, cz).is_none() {
                        continue;
                    }
                    if let Some(f) = self.try_place(gen, l, &c) {
                        out.push(f);
                    }
                }
            }
        }
        out
    }

    /// Thinning + species choice + shape for one hard-core survivor.
    fn try_place(&self, gen: &WorldGen, l: usize, c: &Cand) -> Option<Feature> {
        let col = gen.column(c.x as f64 + 0.5, c.z as f64 + 0.5);
        if col.water_level != i32::MIN && col.water_level as f32 > col.height {
            return None; // under water
        }
        if col.slope > 0.9 || col.jaggedness > 6.0 {
            return None;
        }
        let veg = tables::veg(col.biome, &col);
        let env = self.env(&col, 0.0);
        if c.u >= env.p[l] || veg.mix[l].is_empty() {
            return None;
        }
        if l == MED && self.blocked_by(BIG, c.x, c.z, EXCL_MED_BIG, env.p[BIG]) {
            return None;
        }
        if l == SMALL
            && (self.blocked_by(BIG, c.x, c.z, EXCL_SMALL_BIG, env.p[BIG])
                || self.blocked_by(MED, c.x, c.z, EXCL_SMALL_MED, env.p[MED]))
        {
            return None;
        }
        let mut w = [(Species::Oak, 0.0f32); 8];
        let n = self.weights(veg.mix[l], &env, &mut w);
        let mut r = hash_f01(c.var ^ 0x5EED);
        let mut sp = w[n - 1].0;
        for &(s, wi) in &w[..n] {
            if r < wi {
                sp = s;
                break;
            }
            r -= wi;
        }
        if c.u2 >= self.class_factor(sp, &env) {
            return None;
        }
        let ctx = TreeCtx { biome: col.biome, scale: env.scale };
        Some(build_feature(sp, c.x, c.z, c.var, &ctx))
    }

    /// Expected canopy (Boolean-model expectation of the same point process).
    pub fn canopy_lod(&self, _x: f64, _z: f64, col: &ColumnInfo, footprint: f64) -> CanopyLod {
        let none = CanopyLod { coverage: 0.0, height: 0.0, leaf: Material::OakLeaves };
        if col.water_level != i32::MIN && col.water_level as f32 > col.height {
            return none;
        }
        let veg = tables::veg(col.biome, col);
        if veg.p == [0.0; 3] && veg.grove.is_none() {
            return none;
        }
        let env = self.env(col, footprint.max(1e-3));
        let ground = (1.0 - smoothstep(0.8, 0.95, col.slope)) * (1.0 - smoothstep(5.0, 7.0, col.jaggedness));
        let pi = std::f32::consts::PI;
        let rho_hc = |l: usize| self.retention[l] / (LAYERS[l].cell * LAYERS[l].cell) as f32;
        let excl = [
            1.0,
            (-rho_hc(BIG) * env.p[BIG] * pi * EXCL_MED_BIG * EXCL_MED_BIG).exp(),
            (-(rho_hc(BIG) * env.p[BIG] * pi * EXCL_SMALL_BIG * EXCL_SMALL_BIG
                + rho_hc(MED) * env.p[MED] * pi * EXCL_SMALL_MED * EXCL_SMALL_MED))
                .exp(),
        ];
        let mut sum_a = 0.0f32;
        let mut sum_ah = 0.0f32;
        let mut best = (0.0f32, Material::OakLeaves);
        #[allow(clippy::needless_range_loop)]
        for l in 0..3 {
            if env.p[l] <= 0.0 {
                continue;
            }
            let rho = rho_hc(l) * env.p[l] * excl[l] * ground;
            let mut w = [(Species::Oak, 0.0f32); 8];
            let n = self.weights(veg.mix[l], &env, &mut w);
            for &(sp, wi) in &w[..n] {
                let (r, h) = sp.lod_shape();
                let s = if matches!(sp.class(), Class::Rock | Class::Ice | Class::Cactus) { 1.0 } else { env.scale };
                let a = rho * wi * self.class_factor(sp, &env) * pi * (r * s) * (r * s);
                sum_a += a;
                sum_ah += a * h * s;
                if a > best.0 {
                    if let Some(m) = species_leaf(sp, col.biome) {
                        best = (a, m);
                    }
                }
            }
        }
        if sum_a <= 0.0 {
            return none;
        }
        // Boolean model 1 − e^(−x) assumes Poisson overlap; the hard-core process is more
        // regular (crowns overlap less), which the second-order term β·x² (β = 0.6, fitted by
        // `canopy_lod_matches_features`) accounts for.
        let x = sum_a + 0.6 * sum_a * sum_a;
        CanopyLod { coverage: 1.0 - (-x).exp(), height: sum_ah / sum_a, leaf: best.1 }
    }
}

#[inline]
fn clearing(g: f32) -> f32 {
    0.12 + 0.88 * smoothstep(-0.48, -0.26, g)
}

/// Representative "leaf" material of a species in a biome (None for non-vegetation).
fn species_leaf(sp: Species, biome: Biome) -> Option<Material> {
    use Species::*;
    Some(match sp {
        Oak | SwampOak | BigOak => Material::OakLeaves,
        Birch | TallBirch => Material::BirchLeaves,
        Spruce | Pine | MegaSpruce => Material::SpruceLeaves,
        Jungle | MegaJungle | Palm => Material::JungleLeaves,
        Acacia => Material::AcaciaLeaves,
        DarkOak => Material::DarkOakLeaves,
        Bush => bush_mats(biome).0,
        Cactus => Material::Cactus,
        _ => return None,
    })
}

// =====================================================================================
// Rasteriser
// =====================================================================================

const REPLACE: u8 = 0x80;

/// Local voxel grid around the root: dx, dz ∈ [−r, r+1], y ∈ [ylo, yhi).
/// Cell = material id (| REPLACE). Logs override leaves; leaves only fill empty cells.
pub struct Raster {
    r: i32,
    ylo: i32,
    ny: i32,
    w: i32,
    cells: Vec<u8>,
    seed: u64,
}

impl Raster {
    pub fn new(r: i32, ylo: i32, yhi: i32, seed: u64) -> Self {
        let r = r.clamp(1, MAX_FEATURE_RADIUS);
        let w = 2 * r + 2;
        let ny = (yhi - ylo).max(1);
        Raster { r, ylo, ny, w, cells: vec![0; (w * w * ny) as usize], seed }
    }
    #[inline]
    fn idx(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if x.abs() > MAX_FEATURE_RADIUS || z.abs() > MAX_FEATURE_RADIUS {
            return None;
        }
        let (ix, iy, iz) = (x + self.r, y - self.ylo, z + self.r);
        if ix < 0 || iz < 0 || iy < 0 || ix >= self.w || iz >= self.w || iy >= self.ny {
            return None;
        }
        Some(((iy * self.w + iz) * self.w + ix) as usize)
    }
    #[inline]
    fn get(&self, x: i32, y: i32, z: i32) -> u8 {
        self.idx(x, y, z).map(|i| self.cells[i]).unwrap_or(0)
    }
    #[inline]
    fn leaf(&mut self, x: i32, y: i32, z: i32, m: Material) {
        if let Some(i) = self.idx(x, y, z) {
            if self.cells[i] == 0 {
                self.cells[i] = m as u8;
            }
        }
    }
    #[inline]
    fn wood(&mut self, x: i32, y: i32, z: i32, m: Material) {
        if let Some(i) = self.idx(x, y, z) {
            let c = self.cells[i];
            if c == 0 || Material::from_u8(c & 0x7F).is_leaves() {
                self.cells[i] = m as u8;
            }
        }
    }
    #[inline]
    fn solid(&mut self, x: i32, y: i32, z: i32, m: Material, replace: bool) {
        if let Some(i) = self.idx(x, y, z) {
            self.cells[i] = m as u8 | if replace { REPLACE } else { 0 };
        }
    }
    /// Uniform [0,1) hash of a voxel (for dithering).
    #[inline]
    fn h(&self, x: i32, y: i32, z: i32) -> f32 {
        hash_f01(hash3(self.seed, x, y, z))
    }

    /// Vertical trunk of `size`×`size` from y0 to y1 (exclusive) at (x0, z0).
    fn trunk(&mut self, x0: i32, z0: i32, size: i32, y0: i32, y1: i32, m: Material) {
        for y in y0..y1 {
            for dz in 0..size {
                for dx in 0..size {
                    self.wood(x0 + dx, y, z0 + dz, m);
                }
            }
        }
    }
    /// Roots below ground (replace solid) — fills gaps under 2×2 trunks on slopes.
    fn roots(&mut self, size: i32, depth: i32, m: Material) {
        for y in -depth..0 {
            for dz in 0..size {
                for dx in 0..size {
                    self.solid(dx, y, dz, m, true);
                }
            }
        }
    }
    /// Face-connected voxel path along the segment a→b (continuous local coordinates).
    fn line(&mut self, a: [f32; 3], b: [f32; 3], m: Material) {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let n = (len * 3.0).ceil().max(1.0) as i32;
        let mut prev: Option<[i32; 3]> = None;
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let v = [
                (a[0] + d[0] * t).floor() as i32,
                (a[1] + d[1] * t).floor() as i32,
                (a[2] + d[2] * t).floor() as i32,
            ];
            if let Some(mut c) = prev {
                if c != v {
                    // step one axis at a time → face-connected (no edge-only contacts)
                    for ax in [1usize, 0, 2] {
                        if c[ax] != v[ax] {
                            c[ax] = v[ax];
                            if c != v {
                                self.wood(c[0], c[1], c[2], m);
                            }
                        }
                    }
                }
            }
            self.wood(v[0], v[1], v[2], m);
            prev = Some(v);
        }
    }
    /// Metaball canopy: Σ (1 − |q|²)₊² ≥ τ (τ = ¼, threshold dithered by `rough`).
    /// Balls: (cx, cy, cz, effective horizontal radius, effective vertical radius).
    fn canopy(&mut self, balls: &[[f32; 5]], m: Material, rough: f32) {
        const TAU: f32 = 0.25;
        const K: f32 = std::f32::consts::SQRT_2; // r_eff = r·sqrt(1 − sqrt τ) = r/√2
        if balls.is_empty() {
            return;
        }
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for b in balls {
            let (rx, ry) = (b[3] * K, b[4] * K);
            lo = [lo[0].min(b[0] - rx), lo[1].min(b[1] - ry), lo[2].min(b[2] - rx)];
            hi = [hi[0].max(b[0] + rx), hi[1].max(b[1] + ry), hi[2].max(b[2] + rx)];
        }
        let inv: Vec<[f32; 5]> = balls.iter().map(|b| [b[0], b[1], b[2], 1.0 / (b[3] * K), 1.0 / (b[4] * K)]).collect();
        for y in lo[1].floor() as i32..=hi[1].floor() as i32 {
            for z in lo[2].floor() as i32..=hi[2].floor() as i32 {
                for x in lo[0].floor() as i32..=hi[0].floor() as i32 {
                    let p = [x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5];
                    let mut s = 0.0;
                    for b in &inv {
                        let qx = (p[0] - b[0]) * b[3];
                        let qy = (p[1] - b[1]) * b[4];
                        let qz = (p[2] - b[2]) * b[3];
                        let d2 = qx * qx + qy * qy + qz * qz;
                        if d2 < 1.0 {
                            let t = 1.0 - d2;
                            s += t * t;
                        }
                    }
                    if s > TAU * 0.4 {
                        let thr = TAU * (1.0 + rough * (self.h(x, y, z) - 0.5) * 2.0);
                        if s >= thr {
                            self.leaf(x, y, z, m);
                        }
                    }
                }
            }
        }
    }
    /// Superellipse disc |x/r|^p + |z/r|^p ≤ 1 (dithered) at layer y, centre (cx, cz).
    #[allow(clippy::too_many_arguments)]
    fn disc(&mut self, cx: f32, cz: f32, y: i32, r: f32, p: f32, m: Material, rough: f32) {
        let ri = (r + 1.0).ceil() as i32;
        let (bx, bz) = (cx.floor() as i32, cz.floor() as i32);
        for z in bz - ri..=bz + ri {
            for x in bx - ri..=bx + ri {
                let qx = ((x as f32 + 0.5 - cx) / r).abs();
                let qz = ((z as f32 + 0.5 - cz) / r).abs();
                let q = qx.powf(p) + qz.powf(p);
                if q <= 1.0 + rough * (self.h(x, y, z) - 0.5) * 2.0 {
                    self.leaf(x, y, z, m);
                }
            }
        }
    }
    /// Layered cone with ring (tier) modulation and rotating 4-lobed whorls (see module docs).
    #[allow(clippy::too_many_arguments)]
    fn cone(&mut self, cx: f32, cz: f32, y_bot: i32, y_top: i32, r_max: f32, period: f32, ring: f32, lobes: f32, phase: f32, r_min: f32, m: Material) {
        let span = (y_top - y_bot).max(1) as f32;
        for y in y_bot..=y_top {
            let u = (y_top - y) as f32 / span;
            let tp = (y_top - y) as f32 / period + phase;
            let fr = tp - tp.floor();
            let tier = tp.floor() as i32;
            let mut r = (r_max * (0.12 + 0.88 * u.powf(0.9)) * (1.0 - ring * (1.0 - fr)) + 0.45).max(r_min);
            if y == y_top {
                r = r.min(r_min.max(0.6));
            }
            let psi = hash_f01(hash2(self.seed ^ 0xC0E, tier, 7)) * std::f32::consts::TAU;
            let (spsi, cpsi) = psi.sin_cos();
            let ri = (r * (1.0 + lobes) + 1.0).ceil() as i32;
            let (bx, bz) = (cx.floor() as i32, cz.floor() as i32);
            for z in bz - ri..=bz + ri {
                for x in bx - ri..=bx + ri {
                    let px = x as f32 + 0.5 - cx;
                    let pz = z as f32 + 0.5 - cz;
                    let d2 = px * px + pz * pz;
                    let d = d2.sqrt();
                    // cos(4θ + ψ) with cos 4θ = 8c⁴ − 8c² + 1, sin 4θ = 4sc(2c² − 1), c = cos θ, s = sin θ
                    let (c, sn) = if d > 1e-4 { (px / d, pz / d) } else { (1.0, 0.0) };
                    let c2 = c * c;
                    let cos4 = 8.0 * c2 * c2 - 8.0 * c2 + 1.0;
                    let sin4 = 4.0 * sn * c * (2.0 * c2 - 1.0);
                    let rr = r * (1.0 + lobes * (cos4 * cpsi - sin4 * spsi));
                    if d <= rr + 0.22 * (self.h(x, y, z) - 0.5) * 2.0 {
                        self.leaf(x, y, z, m);
                    }
                }
            }
        }
    }

    /// Run-length encode into feature columns (sorted by (dx, dz), runs sorted by y).
    pub fn columns(&self) -> Vec<FeatureColumn> {
        let mut out = Vec::new();
        for iz in 0..self.w {
            for ix in 0..self.w {
                let mut run: Option<(i32, u8)> = None;
                for iy in 0..=self.ny {
                    let c = if iy < self.ny { self.cells[((iy * self.w + iz) * self.w + ix) as usize] } else { 0 };
                    match run {
                        Some((y0, v)) if v == c => {
                            let _ = y0;
                        }
                        _ => {
                            if let Some((y0, v)) = run {
                                if v != 0 {
                                    out.push(FeatureColumn {
                                        dx: (ix - self.r) as i16,
                                        dz: (iz - self.r) as i16,
                                        y0: (y0 + self.ylo) as i16,
                                        y1: (iy + self.ylo) as i16,
                                        mat: Material::from_u8(v & 0x7F),
                                        replace_solid: v & REPLACE != 0,
                                    });
                                }
                            }
                            run = Some((iy, c));
                        }
                    }
                }
            }
        }
        out
    }
}

/// Tiny deterministic stream of uniforms from a seed.
struct Rng(u64);
impl Rng {
    #[inline]
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        hash_f01(mix64(self.0))
    }
    #[inline]
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    #[inline]
    fn int(&mut self, a: i32, b: i32) -> i32 {
        (a + (self.f() * (b - a + 1) as f32) as i32).min(b)
    }
    #[inline]
    fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
}

const TAU: f32 = std::f32::consts::TAU;

/// (leaves, log) of bushes per biome.
fn bush_mats(b: Biome) -> (Material, Material) {
    use Biome::*;
    match b {
        Jungle => (Material::JungleLeaves, Material::JungleLog),
        Taiga | OldGrowthTaiga | SnowyTaiga | AlpineGrove | Meadow | SnowyPlains => (Material::SpruceLeaves, Material::SpruceLog),
        Savanna | Badlands | Hoodoos | Desert => (Material::AcaciaLeaves, Material::AcaciaLog),
        DarkForest => (Material::DarkOakLeaves, Material::DarkOakLog),
        BirchForest => (Material::BirchLeaves, Material::BirchLog),
        _ => (Material::OakLeaves, Material::OakLog),
    }
}

/// Build one feature of a given species at a root (deterministic in `var`).
pub fn build_feature(sp: Species, root_x: i32, root_z: i32, var: u64, ctx: &TreeCtx) -> Feature {
    let mut rng = Rng(var);
    let s = ctx.scale.clamp(0.4, 1.0);
    let rs = build_shape(sp, &mut rng, s, ctx.biome, var);
    Feature { root_x, root_z, kind: sp.kind(), columns: rs.columns() }
}

fn build_shape(sp: Species, rng: &mut Rng, s: f32, biome: Biome, var: u64) -> Raster {
    use Material as M;
    match sp {
        Species::Oak | Species::SwampOak => {
            let swamp = sp == Species::SwampOak;
            let t = (((4.0 + rng.range(0.0, 2.8)) * s).round() as i32).max(3) + swamp as i32;
            let mut rs = Raster::new(7, -1, t + 7, var);
            rs.trunk(0, 0, 1, 0, t + 1, M::OakLog);
            let mut r0 = rng.range(2.5, 3.2) * s.sqrt();
            let mut ry = r0 * rng.range(0.78, 0.92);
            if swamp {
                r0 *= 1.3;
                ry *= 0.62;
            }
            let cy = t as f32 + 0.6;
            let mut balls = vec![[0.5, cy, 0.5, r0, ry]];
            let n = rng.int(3, 5);
            let a0 = rng.range(0.0, TAU);
            for k in 0..n {
                let a = a0 + k as f32 * TAU / n as f32 + rng.range(-0.4, 0.4);
                let d = rng.range(1.1, 1.9) * s.sqrt() * if swamp { 1.4 } else { 1.0 };
                let rr = rng.range(1.6, 2.2) * s.sqrt();
                balls.push([0.5 + d * a.cos(), cy + rng.range(-1.2, 0.5), 0.5 + d * a.sin(), rr, rr * if swamp { 0.6 } else { 0.85 }]);
            }
            rs.canopy(&balls, M::OakLeaves, 0.4);
            if swamp {
                // drooping fringe: a few leaf strands hanging from the rim
                for _ in 0..rng.int(4, 8) {
                    let a = rng.range(0.0, TAU);
                    let d = r0 * rng.range(0.85, 1.05);
                    let (x, z) = ((0.5 + d * a.cos()).floor() as i32, (0.5 + d * a.sin()).floor() as i32);
                    let len = rng.int(1, 3);
                    for y in (t - len)..=t {
                        rs.leaf(x, y, z, M::OakLeaves);
                    }
                }
            }
            rs
        }
        Species::BigOak => {
            let t = (((8.5 + rng.range(0.0, 4.5)) * s).round() as i32).max(6);
            let mut rs = Raster::new(11, -1, t + 8, var);
            let (lx, lz) = (rng.range(-1.0, 1.0), rng.range(-1.0, 1.0));
            let top = [0.5 + lx, t as f32 + 0.5, 0.5 + lz];
            rs.line([0.5, 0.0, 0.5], top, M::OakLog);
            let mut balls = vec![[top[0], top[1] + 0.8, top[2], 2.9 * s.sqrt(), 2.2 * s.sqrt()]];
            let n = rng.int(3, 5);
            let a0 = rng.range(0.0, TAU);
            for k in 0..n {
                let a = a0 + k as f32 * TAU / n as f32 + rng.range(-0.5, 0.5);
                let f = rng.range(0.45, 0.82);
                let start = [0.5 + lx * f, t as f32 * f, 0.5 + lz * f];
                let len = rng.range(3.0, 5.2) * s;
                let el = rng.range(0.3, 0.8);
                let end = [start[0] + len * a.cos() * el.cos(), start[1] + len * el.sin(), start[2] + len * a.sin() * el.cos()];
                rs.line(start, end, M::OakLog);
                let rr = rng.range(2.0, 2.7) * s.sqrt();
                balls.push([end[0], end[1] + 0.6, end[2], rr, rr * 0.78]);
            }
            rs.canopy(&balls, M::OakLeaves, 0.42);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                if rng.chance(0.5) {
                    rs.wood(dx, 0, dz, M::OakLog);
                }
            }
            rs
        }
        Species::Birch | Species::TallBirch => {
            let extra = if sp == Species::TallBirch { rng.range(3.0, 5.5) } else { 0.0 };
            let t = (((5.0 + rng.range(0.0, 2.5) + extra) * s).round() as i32).max(4);
            let mut rs = Raster::new(5, -1, t + 6, var);
            rs.trunk(0, 0, 1, 0, t + 1, M::BirchLog);
            let r0 = rng.range(1.9, 2.4) * s.sqrt();
            let ry = rng.range(2.5, 3.3) * s.sqrt();
            let cy = t as f32 - 0.2;
            let mut balls = vec![[0.5, cy, 0.5, r0, ry]];
            for _ in 0..rng.int(1, 3) {
                let a = rng.range(0.0, TAU);
                balls.push([0.5 + 1.1 * a.cos(), cy + rng.range(-1.5, 1.2), 0.5 + 1.1 * a.sin(), 1.5, 1.6]);
            }
            rs.canopy(&balls, M::BirchLeaves, 0.38);
            rs
        }
        Species::Spruce | Species::Pine => {
            let pine = sp == Species::Pine;
            let h = if pine {
                (((10.0 + rng.range(0.0, 5.0)) * s).round() as i32).max(5)
            } else {
                (((8.0 + rng.range(0.0, 6.0)) * s).round() as i32).max(4)
            };
            let mut rs = Raster::new(5, -1, h + 4, var);
            rs.trunk(0, 0, 1, 0, h - 1, M::SpruceLog);
            let (y_bot, r_max, ring, period) = if pine {
                ((h as f32 * rng.range(0.55, 0.68)) as i32, rng.range(1.7, 2.3) * s.sqrt(), 0.3, rng.range(2.0, 2.6))
            } else {
                (rng.int(1, 3).min(h / 3).max(1), rng.range(2.3, 3.3) * s.sqrt(), 0.58, if rng.chance(0.5) { 2.0 } else { 3.0 })
            };
            rs.cone(0.5, 0.5, y_bot, h, r_max, period, ring, 0.12, rng.f(), 0.5, M::SpruceLeaves);
            rs.leaf(0, h + 1, 0, M::SpruceLeaves);
            if pine {
                // a few dead stubs on the bare bole
                for _ in 0..rng.int(0, 2) {
                    let (dx, dz) = [(1, 0), (-1, 0), (0, 1), (0, -1)][rng.int(0, 3) as usize];
                    rs.wood(dx, rng.int(3, (y_bot - 1).max(3)), dz, M::SpruceLog);
                }
            }
            rs
        }
        Species::MegaSpruce => {
            let h = (((25.0 + rng.range(0.0, 10.0)) * s).round() as i32).max(16);
            let mut rs = Raster::new(8, -4, h + 4, var);
            rs.roots(2, 3, M::SpruceLog);
            rs.trunk(0, 0, 2, 0, h - 3, M::SpruceLog);
            for (dx, dz) in [(-1, 0), (-1, 1), (2, 0), (2, 1), (0, -1), (1, -1), (0, 2), (1, 2)] {
                if rng.chance(0.45) {
                    rs.wood(dx, 0, dz, M::SpruceLog);
                    rs.solid(dx, -1, dz, M::SpruceLog, true);
                    if rng.chance(0.4) {
                        rs.wood(dx, 1, dz, M::SpruceLog);
                    }
                }
            }
            let y_bot = (h as f32 * rng.range(0.3, 0.45)) as i32;
            let r_max = rng.range(4.4, 6.0) * s.sqrt();
            rs.cone(1.0, 1.0, y_bot, h, r_max, rng.range(3.0, 4.0), 0.55, 0.16, rng.f(), 1.05, M::SpruceLeaves);
            for y in h - 1..=h + 1 {
                rs.leaf(0, y, 0, M::SpruceLeaves);
            }
            rs
        }
        Species::Jungle => {
            let h = (((9.0 + rng.range(0.0, 7.0)) * s).round() as i32).max(6);
            let mut rs = Raster::new(7, -1, h + 6, var);
            rs.trunk(0, 0, 1, 0, h + 1, M::JungleLog);
            let r0 = rng.range(2.8, 3.4) * s.sqrt();
            let mut balls = vec![[0.5, h as f32 + 0.8, 0.5, r0, 1.8]];
            for _ in 0..rng.int(2, 3) {
                let a = rng.range(0.0, TAU);
                let d = rng.range(1.4, 2.2);
                balls.push([0.5 + d * a.cos(), h as f32 + rng.range(0.0, 1.2), 0.5 + d * a.sin(), 2.0, 1.4]);
            }
            for _ in 0..rng.int(1, 3) {
                let a = rng.range(0.0, TAU);
                let y = h as f32 * rng.range(0.45, 0.8);
                let len = rng.range(2.0, 3.2);
                let end = [0.5 + len * a.cos(), y + 0.9, 0.5 + len * a.sin()];
                rs.line([0.5, y, 0.5], end, M::JungleLog);
                balls.push([end[0], end[1] + 0.5, end[2], 1.6, 1.1]);
            }
            rs.canopy(&balls, M::JungleLeaves, 0.4);
            rs
        }
        Species::MegaJungle => {
            let h = (((22.0 + rng.range(0.0, 10.0)) * s).round() as i32).max(14);
            let mut rs = Raster::new(11, -4, h + 6, var);
            rs.roots(2, 3, M::JungleLog);
            rs.trunk(0, 0, 2, 0, h + 1, M::JungleLog);
            // buttress roots: staircases fanning out of each face
            for (ox, oz, dx, dz) in [(-1, 0, -1, 0), (2, 1, 1, 0), (1, -1, 0, -1), (0, 2, 0, 1)] {
                if rng.chance(0.8) {
                    let len = rng.int(1, 3);
                    for k in 0..len {
                        let (x, z) = (ox + dx * k, oz + dz * k);
                        rs.solid(x, -1, z, M::JungleLog, true);
                        for y in 0..(len - k).min(3) {
                            rs.wood(x, y, z, M::JungleLog);
                        }
                    }
                }
            }
            let c = [1.0, h as f32 + 0.8, 1.0];
            let mut balls = vec![[c[0], c[1], c[2], rng.range(3.8, 4.4) * s.sqrt(), 2.3]];
            let n = rng.int(5, 7);
            let a0 = rng.range(0.0, TAU);
            for k in 0..n {
                let a = a0 + k as f32 * TAU / n as f32 + rng.range(-0.3, 0.3);
                let d = rng.range(3.2, 4.4) * s.sqrt();
                balls.push([c[0] + d * a.cos(), c[1] + rng.range(-1.0, 0.6), c[2] + d * a.sin(), rng.range(2.6, 3.2) * s.sqrt(), 1.7]);
            }
            for _ in 0..rng.int(2, 4) {
                let a = rng.range(0.0, TAU);
                let y = h as f32 * rng.range(0.5, 0.8);
                let len = rng.range(4.0, 6.0) * s;
                let el = rng.range(0.25, 0.5);
                let st = [1.0, y, 1.0];
                let end = [st[0] + len * a.cos() * el.cos(), y + len * el.sin(), st[2] + len * a.sin() * el.cos()];
                rs.line(st, end, M::JungleLog);
                balls.push([end[0], end[1] + 0.7, end[2], rng.range(2.2, 2.7), 1.5]);
            }
            rs.canopy(&balls, M::JungleLeaves, 0.42);
            rs
        }
        Species::Acacia => {
            let t1 = (((3.0 + rng.range(0.0, 2.0)) * s).round() as i32).max(2);
            let mut rs = Raster::new(9, -1, t1 + 10, var);
            rs.trunk(0, 0, 1, 0, t1, M::AcaciaLog);
            let az = rng.range(0.0, TAU);
            let k = rng.range(2.0, 4.0) * s.sqrt();
            let base = [0.5, t1 as f32 - 0.5, 0.5];
            let mut tops = Vec::new();
            let ta = [base[0] + k * az.cos(), base[1] + k, base[2] + k * az.sin()];
            rs.line(base, ta, M::AcaciaLog);
            tops.push(ta);
            if rng.chance(0.65) {
                let az2 = az + std::f32::consts::PI + rng.range(-0.7, 0.7);
                let k2 = rng.range(1.5, 3.0) * s.sqrt();
                let b2 = [0.5, t1 as f32 - rng.range(0.5, 1.5), 0.5];
                let tb = [b2[0] + k2 * az2.cos(), b2[1] + k2 * 0.9 + 0.5, b2[2] + k2 * az2.sin()];
                rs.line(b2, tb, M::AcaciaLog);
                tops.push(tb);
            }
            for (i, tp) in tops.iter().enumerate() {
                let ty = tp[1].floor() as i32;
                let r1 = rng.range(3.2, 4.2) * s.sqrt() * if i == 0 { 1.0 } else { 0.85 };
                rs.disc(tp[0], tp[2], ty + 1, r1, 2.6, M::AcaciaLeaves, 0.35);
                rs.disc(tp[0], tp[2], ty + 2, r1 * rng.range(0.5, 0.65), 2.4, M::AcaciaLeaves, 0.3);
            }
            rs
        }
        Species::DarkOak => {
            let t = (((6.0 + rng.range(0.0, 3.0)) * s).round() as i32).max(4);
            let mut rs = Raster::new(8, -3, t + 6, var);
            rs.roots(2, 2, M::DarkOakLog);
            rs.trunk(0, 0, 2, 0, t + 1, M::DarkOakLog);
            for (dx, dz) in [(-1, 0), (2, 1), (1, -1), (0, 2)] {
                if rng.chance(0.5) {
                    rs.wood(dx, 0, dz, M::DarkOakLog);
                }
            }
            // limbs under the canopy
            for _ in 0..rng.int(2, 4) {
                let a = rng.range(0.0, TAU);
                let y = t as f32 - rng.range(1.0, 2.5);
                let len = rng.range(2.0, 3.2);
                rs.line([1.0, y, 1.0], [1.0 + len * a.cos(), y + 1.2, 1.0 + len * a.sin()], M::DarkOakLog);
            }
            let r = rng.range(4.6, 5.4) * s.sqrt();
            let layers = [(t - 1, 0.74), (t, 1.0), (t + 1, 0.98), (t + 2, 0.74), (t + 3, 0.4)];
            for (y, f) in layers {
                rs.disc(1.0, 1.0, y, r * f, 2.3, M::DarkOakLeaves, 0.45);
            }
            let mut bumps = Vec::new();
            for _ in 0..rng.int(2, 4) {
                let a = rng.range(0.0, TAU);
                let d = rng.range(1.5, 3.2);
                bumps.push([1.0 + d * a.cos(), t as f32 + 3.0, 1.0 + d * a.sin(), 1.7, 1.1]);
            }
            rs.canopy(&bumps, M::DarkOakLeaves, 0.4);
            rs
        }
        Species::Bush => {
            let (leaf, log) = bush_mats(biome);
            let mut rs = Raster::new(4, -1, 5, var);
            let stem = rng.chance(0.6) as i32;
            if stem > 0 {
                rs.wood(0, 0, 0, log);
            }
            let r0 = rng.range(1.3, 2.1) * s.sqrt();
            let ry = rng.range(1.0, 1.5);
            let a = rng.range(0.0, TAU);
            let balls = [
                [0.5, 0.7 + stem as f32 * 0.6, 0.5, r0, ry],
                [0.5 + 0.9 * a.cos(), 0.6, 0.5 + 0.9 * a.sin(), r0 * 0.75, ry * 0.8],
            ];
            let dry = matches!(biome, Biome::Badlands | Biome::Desert | Biome::Hoodoos);
            rs.canopy(&balls, leaf, if dry { 0.9 } else { 0.45 });
            rs
        }
        Species::Cactus => {
            let h = rng.int(2, 6);
            let mut rs = Raster::new(3, -1, h + 2, var);
            for y in 0..h {
                rs.wood(0, y, 0, M::Cactus);
            }
            if h >= 4 && rng.chance(0.65) {
                let dirs = [(1, 0), (-1, 0), (0, 1), (0, -1)];
                let d0 = rng.int(0, 3) as usize;
                let arms = if rng.chance(0.4) { 2 } else { 1 };
                for k in 0..arms {
                    let (dx, dz) = dirs[(d0 + 2 * k) % 4];
                    let ya = rng.int(1, h - 3);
                    rs.wood(dx, ya, dz, M::Cactus);
                    for y in ya + 1..=ya + rng.int(1, 2) {
                        rs.wood(dx, y, dz, M::Cactus);
                    }
                }
            }
            rs
        }
        Species::Boulder => boulder(rng, biome, var),
        Species::IceSpire => {
            let h = rng.int(9, 22);
            let mut rs = Raster::new(6, -2, h + 2, var);
            let r0 = rng.range(1.6, 2.6);
            let (lx, lz) = (rng.range(-0.15, 0.15), rng.range(-0.15, 0.15));
            spire(&mut rs, 0.5, 0.5, h, r0, lx, lz);
            if rng.chance(0.5) {
                let a = rng.range(0.0, TAU);
                let d = rng.range(2.2, 3.2);
                spire(&mut rs, 0.5 + d * a.cos(), 0.5 + d * a.sin(), (h as f32 * rng.range(0.4, 0.6)) as i32, r0 * 0.7, lx * 1.5, lz * 1.5);
            }
            rs
        }
        Species::Palm => {
            let h = (((7.0 + rng.range(0.0, 3.5)) * s).round() as i32).max(5);
            let mut rs = Raster::new(8, -1, h + 4, var);
            let az = rng.range(0.0, TAU);
            let amp = rng.range(1.2, 2.8);
            let pt = |y: f32| {
                let f = (y / h as f32) * (y / h as f32) * amp;
                [0.5 + f * az.cos(), y, 0.5 + f * az.sin()]
            };
            for y in 0..h {
                rs.line(pt(y as f32), pt(y as f32 + 1.0), M::JungleLog);
            }
            let top = pt(h as f32);
            rs.canopy(&[[top[0], top[1] + 0.6, top[2], 1.2, 1.0]], M::JungleLeaves, 0.2);
            let n = rng.int(6, 8);
            let a0 = rng.range(0.0, TAU);
            for k in 0..n {
                let a = a0 + k as f32 * TAU / n as f32 + rng.range(-0.25, 0.25);
                let len = rng.range(3.4, 4.8);
                let mut t = 0.0;
                while t <= len {
                    let y = top[1] + 0.6 + 0.62 * t - 0.2 * t * t;
                    let x = top[0] + t * a.cos();
                    let z = top[2] + t * a.sin();
                    rs.leaf(x.floor() as i32, y.floor() as i32, z.floor() as i32, M::JungleLeaves);
                    t += 0.4;
                }
            }
            rs
        }
        Species::FallenLog => {
            let (log, moss) = match biome {
                Biome::Taiga | Biome::OldGrowthTaiga | Biome::SnowyTaiga | Biome::AlpineGrove => (M::SpruceLog, true),
                Biome::BirchForest => (M::BirchLog, false),
                Biome::DarkForest => (M::DarkOakLog, true),
                _ => (M::OakLog, rng.chance(0.4)),
            };
            let len = rng.int(4, 7);
            let mut rs = Raster::new(5, -1, 4, var);
            let along_x = rng.chance(0.5);
            let off = rng.int(1, 2);
            for k in 0..len {
                let (x, z) = if along_x { (k - len / 2 + off, 0) } else { (0, k - len / 2 + off) };
                rs.wood(x, 0, z, log);
                if moss && rng.chance(0.45) {
                    rs.solid(x, 1, z, M::Moss, false);
                }
            }
            if rng.chance(0.5) {
                // stump at the root
                let (x, z) = if along_x { (-len / 2 + off - 2, 0) } else { (0, -len / 2 + off - 2) };
                for y in 0..rng.int(1, 2) {
                    rs.wood(x, y, z, log);
                }
            }
            rs
        }
    }
}

/// Leaning conical ice spire `r(y) = r₀(1 − y/H)^1.25`, with a flared embedded base.
fn spire(rs: &mut Raster, cx: f32, cz: f32, h: i32, r0: f32, lx: f32, lz: f32) {
    for y in -2..=h {
        let u = (y.max(0) as f32 / h as f32).min(1.0);
        // ≥ 0.72 keeps at least one voxel per level (a cell centre is within √2/2 of the axis)
        let mut r = (r0 * (1.0 - u).powf(1.25) + 0.3).max(if y < h { 0.72 } else { 0.0 });
        if y <= 1 {
            r += 0.8 * (2 - y.max(-1)) as f32 * 0.5;
        }
        let (ox, oz) = (cx + lx * y.max(0) as f32, cz + lz * y.max(0) as f32);
        let ri = (r + 1.0).ceil() as i32;
        let (bx, bz) = (ox.floor() as i32, oz.floor() as i32);
        for z in bz - ri..=bz + ri {
            for x in bx - ri..=bx + ri {
                let px = x as f32 + 0.5 - ox;
                let pz = z as f32 + 0.5 - oz;
                if px * px + pz * pz <= r * r + 0.3 * r * (rs.h(x, y, z) - 0.5) {
                    rs.solid(x, y, z, Material::PackedIce, y < 0);
                }
            }
        }
    }
}

/// Boulder: union of 2–3 dithered ellipsoids, partly embedded, top dressed by biome.
fn boulder(rng: &mut Rng, biome: Biome, var: u64) -> Raster {
    use Biome::*;
    use Material as M;
    let big = rng.chance(0.12);
    let r = if big { rng.range(3.0, 4.3) } else { rng.range(1.5, 2.9) };
    let ri = (r * 1.8).ceil() as i32 + 1;
    let mut rs = Raster::new(ri.min(7), -(r as i32) - 3, (r as i32) + 4, var);
    let (main, alt) = match biome {
        Desert => (M::Sandstone, M::Sandstone),
        Volcanic => (M::Basalt, M::Obsidian),
        StonyPeaks | SnowySlopes | SnowyPeaks | Glacier | Meadow | AlpineGrove => (M::Andesite, M::Stone),
        Savanna | Badlands => (M::Granite, M::Stone),
        _ => (M::Stone, M::Andesite),
    };
    let top = match biome {
        Taiga | OldGrowthTaiga | DarkForest | Forest | SnowyTaiga => Some(M::Moss),
        SnowySlopes | SnowyPeaks | Glacier | SnowyPlains => Some(M::Snow),
        _ => None,
    };
    let mut blobs = vec![[0.5, r * rng.range(0.25, 0.55) - 0.5, 0.5, r, r * rng.range(0.65, 0.9), r * rng.range(0.8, 1.2)]];
    for _ in 0..rng.int(1, 2) {
        let a = rng.range(0.0, TAU);
        let d = r * rng.range(0.4, 0.8);
        let rr = r * rng.range(0.5, 0.75);
        blobs.push([0.5 + d * a.cos(), blobs[0][1] + rng.range(-0.6, 0.4), 0.5 + d * a.sin(), rr, rr * 0.8, rr]);
    }
    let lo_y = rs.ylo;
    let hi_y = rs.ylo + rs.ny;
    for y in lo_y..hi_y {
        for z in -ri..=ri {
            for x in -ri..=ri {
                let p = [x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5];
                let mut inside = false;
                for b in &blobs {
                    let q = ((p[0] - b[0]) / b[3]).powi(2) + ((p[1] - b[1]) / b[4]).powi(2) + ((p[2] - b[2]) / b[5]).powi(2);
                    if q <= 1.0 + 0.35 * (rs.h(x, y, z) - 0.5) {
                        inside = true;
                        break;
                    }
                }
                if inside {
                    let m = if rs.h(x, y + 1000, z) < 0.25 { alt } else { main };
                    rs.solid(x, y, z, m, true);
                }
            }
        }
    }
    if let Some(tm) = top {
        // dress the exposed top: the highest voxel of each boulder column (above ground)
        for z in -ri..=ri {
            for x in -ri..=ri {
                for y in (0..hi_y).rev() {
                    if rs.get(x, y, z) != 0 {
                        if rs.h(x, y + 2000, z) < if tm == M::Snow { 0.85 } else { 0.4 } {
                            rs.solid(x, y, z, tm, true);
                        }
                        break;
                    }
                }
            }
        }
    }
    rs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::WorldGen;

    fn key(f: &Feature) -> (i32, i32, u8, usize) {
        (f.root_x, f.root_z, f.kind as u8, f.columns.len())
    }

    #[test]
    fn determinism_and_rect_independence() {
        let g = WorldGen::new(5);
        // find a forested spot
        let (mut x0, mut z0) = (1000, -700);
        for i in 0..5000u64 {
            let x = (hash_f01(mix64(i)) * 16000.0 - 8000.0) as i32;
            let z = (hash_f01(mix64(i ^ 0x77)) * 16000.0 - 8000.0) as i32;
            let c = g.column(x as f64 + 0.5, z as f64 + 0.5);
            if matches!(c.biome, Biome::Forest | Biome::DarkForest | Biome::Taiga | Biome::Jungle) && c.slope < 0.3 {
                (x0, z0) = (x, z);
                break;
            }
        }
        let big = g.features_in(x0, z0, x0 + 96, z0 + 96);
        let again = g.features_in(x0, z0, x0 + 96, z0 + 96);
        assert_eq!(big.len(), again.len());
        for (a, b) in big.iter().zip(&again) {
            assert_eq!(a.columns, b.columns);
        }
        let mut parts = Vec::new();
        for (sx, sz) in [(0, 0), (37, 0), (0, 53), (37, 53)] {
            let ex = if sx == 0 { 37 } else { 96 };
            let ez = if sz == 0 { 53 } else { 96 };
            parts.extend(g.features_in(x0 + sx, z0 + sz, x0 + ex, z0 + ez));
        }
        let mut a: Vec<_> = big.iter().map(key).collect();
        let mut b: Vec<_> = parts.iter().map(key).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        eprintln!("{} features in 96×96 at ({x0},{z0})", big.len());
        assert!(big.len() > 20);
    }

    #[test]
    fn shapes_well_formed_and_bounded() {
        for (i, &sp) in ALL_SPECIES.iter().enumerate() {
            for v in 0..40u64 {
                for biome in [Biome::Forest, Biome::Taiga, Biome::Jungle, Biome::Desert, Biome::SnowySlopes] {
                    let f = build_feature(sp, 0, 0, mix64(v * 977 + i as u64), &TreeCtx { biome, scale: 1.0 });
                    assert!(!f.columns.is_empty(), "{} empty", sp.name());
                    assert!(f.radius() <= MAX_FEATURE_RADIUS, "{} radius {}", sp.name(), f.radius());
                    let mut cols: Vec<_> = f.columns.clone();
                    cols.sort_by_key(|c| (c.dx, c.dz, c.y0));
                    for w in cols.windows(2) {
                        if (w[0].dx, w[0].dz) == (w[1].dx, w[1].dz) {
                            assert!(w[0].y1 <= w[1].y0, "{} overlapping runs", sp.name());
                        }
                    }
                    for c in &f.columns {
                        assert!(c.y0 < c.y1);
                        assert!(c.mat != Material::Air);
                        if c.mat.is_leaves() {
                            assert!(!c.replace_solid);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mega_spruce_height_range() {
        for v in 0..50u64 {
            let f = build_feature(Species::MegaSpruce, 0, 0, mix64(v), &TreeCtx { biome: Biome::OldGrowthTaiga, scale: 1.0 });
            let top = f.columns.iter().map(|c| c.y1).max().unwrap();
            assert!((25..=38).contains(&top), "mega spruce top {top}");
        }
    }

    /// The LOD constants (canopy radius, top height) must match the rasterised shapes.
    #[test]
    fn lod_shape_constants() {
        for &sp in ALL_SPECIES.iter() {
            let n = 60;
            let (mut area, mut top) = (0.0f32, 0.0f32);
            for v in 0..n {
                let f = build_feature(sp, 0, 0, mix64(v ^ 0x1234), &TreeCtx { biome: Biome::Forest, scale: 1.0 });
                let mut cells = std::collections::HashSet::new();
                for c in &f.columns {
                    if c.y1 > 0 {
                        cells.insert((c.dx, c.dz));
                    }
                }
                area += cells.len() as f32;
                top += f.columns.iter().map(|c| c.y1).max().unwrap() as f32;
            }
            let (r, h) = sp.lod_shape();
            let ra = (area / n as f32 / std::f32::consts::PI).sqrt();
            let hm = top / n as f32;
            eprintln!("{:12} footprint radius {:5.2} (const {:4.1}), top {:5.1} (const {:4.1})", sp.name(), ra, r, hm, h);
            assert!((ra - r).abs() < 0.25 * r + 0.4, "{} radius {ra} vs {r}", sp.name());
            assert!((hm - h).abs() < 0.2 * h + 1.0, "{} top {hm} vs {h}", sp.name());
        }
    }

    /// canopy_lod's coverage ≈ measured coverage of rasterised features over the same area.
    #[test]
    fn canopy_lod_matches_features() {
        let g = WorldGen::new(3);
        let mut checked = 0;
        let mut worst = 0.0f32;
        'outer: for i in 0..4000u64 {
            let x = (hash_f01(mix64(i)) * 20000.0 - 10000.0).floor() as i32;
            let z = (hash_f01(mix64(i ^ 0xABC)) * 20000.0 - 10000.0).floor() as i32;
            let col = g.column(x as f64 + 0.5, z as f64 + 0.5);
            let want = [Biome::Forest, Biome::DarkForest, Biome::Taiga, Biome::Jungle, Biome::Plains, Biome::Savanna, Biome::BirchForest];
            if !want.contains(&col.biome) || col.slope > 0.3 {
                continue;
            }
            // homogeneous 96×96 patch?
            let n = 96;
            let mut lod = 0.0;
            for k in 0..64 {
                let (px, pz) = (x + (k % 8) * 12 + 6, z + (k / 8) * 12 + 6);
                let c = g.column(px as f64 + 0.5, pz as f64 + 0.5);
                if c.biome != col.biome {
                    continue 'outer;
                }
                lod += g.canopy_lod(c.x, c.z, &c, 1.0).coverage;
            }
            lod /= 64.0;
            let feats = g.features_in(x - 12, z - 12, x + n + 12, z + n + 12);
            let mut cov = vec![false; (n * n) as usize];
            for f in &feats {
                for c in &f.columns {
                    if c.y1 <= 0 {
                        continue;
                    }
                    let (cx, cz) = (f.root_x + c.dx as i32 - x, f.root_z + c.dz as i32 - z);
                    if cx >= 0 && cz >= 0 && cx < n && cz < n {
                        cov[(cz * n + cx) as usize] = true;
                    }
                }
            }
            let real = cov.iter().filter(|&&b| b).count() as f32 / (n * n) as f32;
            eprintln!("{:?} at ({x},{z}): lod {lod:.3} real {real:.3}", col.biome);
            worst = worst.max((lod - real).abs());
            checked += 1;
            if checked >= 10 {
                break;
            }
        }
        assert!(checked >= 4, "only {checked} patches");
        assert!(worst < 0.15, "LOD coverage off by {worst}");
    }

    #[test]
    fn canopy_lod_performance() {
        let g = WorldGen::new(1);
        let cols: Vec<ColumnInfo> = (0..256).map(|i| g.column_lod(i as f64 * 97.0, i as f64 * -61.0, 8.0)).collect();
        let n = 400_000;
        let t0 = std::time::Instant::now();
        let mut acc = 0.0f32;
        for i in 0..n {
            let c = &cols[i % 256];
            acc += g.canopy_lod(c.x, c.z, c, 8.0).coverage;
        }
        let ns = t0.elapsed().as_secs_f64() * 1e9 / n as f64;
        eprintln!("canopy_lod: {ns:.0} ns/call (mean coverage {:.3})", acc / n as f32);
        assert!(ns < 2000.0);
    }

    #[test]
    fn features_performance() {
        let g = WorldGen::new(1);
        let t0 = std::time::Instant::now();
        let mut n = 0;
        let mut cols = 0;
        for k in 0..16 {
            let x0 = 2000 + (k % 4) * 64;
            let z0 = -1000 + (k / 4) * 64;
            let f = g.features_in(x0, z0, x0 + 64, z0 + 64);
            n += f.len();
            cols += f.iter().map(|f| f.columns.len()).sum::<usize>();
        }
        let dt = t0.elapsed().as_secs_f64();
        eprintln!("features_in 256×256: {:.1} ms, {n} features, {cols} runs ({:.2} µs/block²)", dt * 1e3, dt * 1e6 / 65536.0);
    }
}
