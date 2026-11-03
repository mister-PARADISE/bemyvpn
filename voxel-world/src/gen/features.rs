//! Trees and other surface features. OWNER: caves-features.
//!
//! # Point processes (placement)
//!
//! **Normal vegetation** — three *marked Matérn type-II hard-core processes* on jittered grids
//! (big / medium / small layer: cell 7 / 5 / 4 blocks, hard-core radius 5.6 / 3.6 / 2.6). Every
//! grid cell owns one candidate whose position, priority, thinning variables and shape seed are
//! pure hashes of (seed, layer, cell). A candidate survives the hard core iff no candidate of the
//! same layer within the hard-core radius has a higher priority (hash-geometric test). Survivors
//! are **independently thinned** (`u < p`) by the local density `p(biome, grove, treeline,
//! slope)`; independent thinning of a hard-core process keeps the minimum spacing (blue noise)
//! while the intensity follows the biome. **Cross-layer exclusion** is exact: a medium / small
//! candidate yields to an *accepted* bigger tree within 4.5 / 3.5 / 2.2 blocks, where
//! "accepted" is evaluated at the blocker's own root (its own column, biome, species and class
//! factor — memoised per query), so the result never depends on the query rectangle and trees of
//! different biomes never grow into each other at biome borders.
//!
//! **Giants** — two sparse processes with one candidate per cell (jitter keeps candidates of
//! adjacent cells ≥ 16 / 48 blocks apart): *emergent* (cell 48; colossal 3×3 spruces of
//! old-growth taiga) and *landmark* (cell 256; ancient oaks, sequoia-like conifers, jungle
//! emergents, baobabs, weeping willows by rivers/swamps, lone giant oaks in meadows). A giant
//! deterministically suppresses every normal feature (and emergent giant) whose root lies inside
//! its suppression disc; normal candidates query the giant grids (hash test first, the giant's
//! column only when a candidate is close), so placement stays independent of the query rectangle.
//!
//! Densities are modulated by
//! * a **grove field** `g = (n(x/170) + ½ n(x/63)) / 1.5`: clearings inside closed forests
//!   (`0.12 + 0.88·smoothstep(−0.48, −0.26, g)`), groves / oases inside open land,
//! * a **mix field** (λ ≈ 95) that tilts species weights `wᵢ·exp(2 bᵢ m)` (birch stands inside
//!   oak forests, pine patches inside spruce taiga …),
//! * a **stand-age field** `σ` (λ ≈ 64) that tilts the age-class distribution
//!   `P(c) ∝ w_c·exp(1.3 σ k_c)` → even-aged stands: clusters of saplings, groups of old giants,
//! * the **treeline** relative to the local snow line `S` (`r = H − S`): trees fade over
//!   `r ∈ [−95, −30]`, shrink (`scale = 0.5 + 0.5·(1 − smoothstep(−110, −30, r))`), bushes go
//!   up to `r ≈ −12`; near the treeline conifers lean and flag downwind (wind = curl of the
//!   climate pressure field, `ClimateSampler::wind`).
//!
//! # Tree genome ("one generator, endless variations")
//! Each tree = species template + a [`Genome`] drawn from its hash: an **age class**
//! (sapling / young / mature / old) with a log-normal size `s = μ_c·e^{0.09 Z}` (Z ≈ N(0,1) via
//! Irwin–Hall), trunk lean ≤ 14° and curvature (the trunk is a **quadratic Bézier spine**
//! `P(t) = (1−t)²P₀ + 2t(1−t)P₁ + t²P₂` with `P₁` at mid-height, so y is linear in t),
//! crown asymmetry, leaf holes (hash dithering), dead-branch probability and the phase of the
//! branch spiral. Allometry keeps every instance natural: crown radius ∝ s^0.8, height ∝ s,
//! trunk thickness by size, branch length ∝ crown radius. Limbs leave the trunk at **golden-angle
//! azimuths** (phyllotaxis, Δθ = π(3 − √5)) and are rasterised as face-connected DDA paths or
//! tapered capsules; leaf clusters sit on their tips, so no leaf blob floats.
//!
//! Crown families: **metaball clusters** `Σ (1 − |Mᵢ(p − cᵢ)|²)₊² ≥ τ` (oak, birch, jungle,
//! ancient oak, willow dome), **layered cones with ring modulation**
//! `r(y) = R·(0.12 + 0.88·u^k)·(1 − ρ(1 − frac(Δy/P + φ)))·(1 + a·cos(4θ + ψ_tier))·(1 + α cos(θ − θ_w))`
//! following the leaning spine (spruce, pine, mega / colossal spruce), **columnar phyllotaxis
//! whorls** (sequoia), **umbrellas** of superellipse discs `|x/r|^p + |z/r|^p ≤ 1` (acacia,
//! dark oak, jungle emergent), **weeping curtains** hanging from the lowest leaf of each crown
//! column (willow, swamp oak). Plus snags (dead standing trees), stumps, fallen logs (any
//! azimuth), bushes, saguaro cacti, boulders, ice spires and palms.
//!
//! # LOD
//! `canopy_lod` is the expectation of the same processes (Boolean model):
//! `coverage = 1 − exp(−x − 0.6x²)`, `x = Σ_L ρ_L Σ_s f_s·E[A_s]·scale^1.6 + Σ_giants ρ_g E[A_g]`
//! with `ρ_L = retention_L/cell_L²·p_L·excl_L·void_giants` (`excl` = Poisson void probability
//! of the bigger layers within the exclusion radius), `E[A_s]`, `E[top_s]` the genome
//! expectations of each species' footprint area and top height (validated against rasterised
//! instances by `lod_shape_constants`).
use super::biome::SnowLine;
use super::{Biome, CanopyLod, ColumnInfo, Feature, FeatureColumn, FeatureKind, WorldGen, MAX_FEATURE_RADIUS};
use crate::materials::Material;
use crate::noise::{derive_seed, hash2, hash3, hash_f01, mix64, salt, Simplex};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

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
    /// 3×3 spruce of old-growth taiga, 50–70 tall (emergent process).
    ColossalSpruce,
    /// Dead standing tree: broken top, a few stubs.
    Snag,
    /// Tree stump.
    Stump,
    /// Landmark: ancient oak with huge spreading limbs (crown radius ≈ 16–22).
    AncientOak,
    /// Landmark: sequoia-like giant conifer, 4×4–5×5 trunk, 60–90 tall, columnar crown.
    Sequoia,
    /// Landmark: jungle emergent towering above the canopy, umbrella crown, buttresses.
    JungleGiant,
    /// Landmark: baobab with a fat bottle trunk and stubby limbs.
    Baobab,
    /// Landmark: ancient weeping willow (by rivers and in swamps).
    Willow,
}

pub const ALL_SPECIES: [Species; 26] = [
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
    Species::ColossalSpruce,
    Species::Snag,
    Species::Stump,
    Species::AncientOak,
    Species::Sequoia,
    Species::JungleGiant,
    Species::Baobab,
    Species::Willow,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Tree,
    MegaTree,
    Giant,
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
            Oak | SwampOak | Willow => FeatureKind::Oak,
            BigOak | AncientOak => FeatureKind::BigOak,
            Birch | TallBirch => FeatureKind::Birch,
            Spruce | Pine => FeatureKind::Spruce,
            MegaSpruce | ColossalSpruce | Sequoia => FeatureKind::MegaSpruce,
            Jungle => FeatureKind::Jungle,
            MegaJungle | JungleGiant => FeatureKind::MegaJungle,
            Acacia | Baobab => FeatureKind::Acacia,
            DarkOak => FeatureKind::DarkOak,
            Bush => FeatureKind::Bush,
            Cactus => FeatureKind::Cactus,
            Boulder => FeatureKind::Boulder,
            IceSpire => FeatureKind::IceSpire,
            Palm | FallenLog | Snag | Stump => FeatureKind::Other,
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
            ColossalSpruce => "colossal_spruce",
            Snag => "snag",
            Stump => "stump",
            AncientOak => "ancient_oak",
            Sequoia => "sequoia",
            JungleGiant => "jungle_giant",
            Baobab => "baobab",
            Willow => "willow",
        }
    }
    fn class(self) -> Class {
        use Species::*;
        match self {
            MegaSpruce | MegaJungle | DarkOak | BigOak => Class::MegaTree,
            ColossalSpruce | AncientOak | Sequoia | JungleGiant | Baobab | Willow => Class::Giant,
            Bush => Class::Bush,
            Cactus => Class::Cactus,
            Boulder => Class::Rock,
            IceSpire => Class::Ice,
            FallenLog | Stump => Class::Log,
            _ => Class::Tree,
        }
    }
    /// Age classes the genome may draw (0 sapling, 1 young, 2 mature, 3 old).
    fn age_classes(self) -> (usize, usize) {
        match self.class() {
            Class::MegaTree => (2, 3),
            Class::Giant => (3, 3),
            _ => (0, 3),
        }
    }
    /// Genome expectation of (footprint area in blocks², top height above ground) at scale 1,
    /// averaged over the stand-age field — used by the LOD; validated against rasterised
    /// instances by the test `lod_shape_constants`.
    fn lod_shape(self) -> (f32, f32) {
        use Species::*;
        match self {
            Oak => (27.2, 6.6),
            SwampOak => (45.7, 7.8),
            BigOak => (74.3, 11.4),
            Birch => (12.6, 8.0),
            TallBirch => (12.5, 10.2),
            Spruce => (32.1, 16.0),
            Pine => (23.3, 16.8),
            MegaSpruce => (99.4, 37.0),
            Jungle => (26.8, 10.9),
            MegaJungle => (133.8, 24.6),
            Acacia => (49.0, 8.2),
            DarkOak => (84.2, 11.2),
            Bush => (11.1, 2.4),
            Cactus => (1.6, 4.2),
            Boulder => (17.6, 2.2),
            IceSpire => (37.2, 16.4),
            Palm => (29.9, 9.6),
            FallenLog => (9.9, 2.3),
            ColossalSpruce => (226.6, 60.8),
            Snag => (2.8, 8.2),
            Stump => (1.6, 2.0),
            AncientOak => (697.6, 19.0),
            Sequoia => (243.8, 76.2),
            JungleGiant => (347.7, 43.7),
            Baobab => (145.0, 16.4),
            Willow => (208.6, 14.2),
        }
    }

    /// Radius of the disc in which a giant suppresses other features (blocks).
    fn suppress_radius(self) -> f32 {
        use Species::*;
        match self {
            AncientOak => 17.0,
            Sequoia => 9.0,
            JungleGiant => 13.0,
            Baobab => 9.0,
            Willow => 11.0,
            ColossalSpruce => 6.0,
            _ => 0.0,
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
    use super::{Mix, Species, Veg};
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
                    &[(Bush, 0.62, 0.0), (FallenLog, 0.18, 0.0), (Stump, 0.12, 0.0), (Boulder, 0.08, 0.0)],
                ],
            ),
            BirchForest => v(
                [0.0, 0.74, 0.10],
                true,
                None,
                [
                    NONE,
                    &[(TallBirch, 0.55, 0.8), (Birch, 0.41, -0.3), (Oak, 0.04, -1.0)],
                    &[(Bush, 0.55, 0.0), (FallenLog, 0.3, 0.0), (Stump, 0.15, 0.0)],
                ],
            ),
            DarkForest => v(
                [0.88, 0.38, 0.12],
                true,
                None,
                [
                    &[(DarkOak, 1.0, 0.0)],
                    &[(Oak, 0.45, 0.0), (BigOak, 0.35, 0.7), (Birch, 0.2, -0.7)],
                    &[(Bush, 0.65, 0.0), (FallenLog, 0.22, 0.0), (Stump, 0.13, 0.0)],
                ],
            ),
            Taiga => v(
                [0.05, 0.66, 0.07],
                true,
                None,
                [
                    &[(MegaSpruce, 1.0, 0.0)],
                    &[(Spruce, 0.75, 0.0), (Pine, 0.25, 1.2)],
                    &[(Bush, 0.4, 0.0), (Boulder, 0.3, 0.0), (FallenLog, 0.18, 0.0), (Stump, 0.12, 0.0)],
                ],
            ),
            OldGrowthTaiga => v(
                [0.55, 0.45, 0.09],
                true,
                None,
                [
                    &[(MegaSpruce, 1.0, 0.0)],
                    &[(Spruce, 0.82, 0.0), (Pine, 0.18, 1.0)],
                    &[(Boulder, 0.4, 0.0), (Bush, 0.25, 0.0), (FallenLog, 0.23, 0.0), (Stump, 0.12, 0.0)],
                ],
            ),
            SnowyTaiga => v(
                [0.0, 0.45, 0.05],
                true,
                None,
                [NONE, &[(Spruce, 0.85, 0.0), (Pine, 0.15, 1.0)], &[(Bush, 0.5, 0.0), (Boulder, 0.4, 0.0), (Stump, 0.1, 0.0)]],
            ),
            AlpineGrove => v(
                [0.0, 0.36, 0.07],
                true,
                None,
                [NONE, &[(Spruce, 0.7, 0.0), (Pine, 0.3, 1.0)], &[(Bush, 0.5, 0.0), (Boulder, 0.5, 0.0)]],
            ),
            SnowyPlains => v([0.0, 0.0, 0.016], false, None, [NONE, NONE, &[(IceSpire, 0.45, 0.0), (Boulder, 0.55, 0.0)]]),
            SnowySlopes | SnowyPeaks | Glacier => v([0.0, 0.0, 0.004], false, None, [NONE, NONE, S_ROCK]),
            Jungle => v(
                [0.32, 0.78, 0.45],
                true,
                None,
                [&[(MegaJungle, 1.0, 0.0)], &[(Species::Jungle, 0.9, 0.0), (Palm, 0.1, 1.0)], S_BUSH],
            ),
            Savanna => v(
                [0.0, 0.065, 0.035],
                false,
                Some((0.2, 0.4)),
                [NONE, &[(Acacia, 0.86, 0.0), (Oak, 0.14, -1.0)], &[(Bush, 0.7, 0.0), (Boulder, 0.3, 0.0)]],
            ),
            Desert => v([0.0, 0.0, 0.075], false, Some((0.14, 0.52)), [NONE, &[(Palm, 1.0, 0.0)], &[(Cactus, 0.92, 0.0), (Boulder, 0.08, 0.0)]]),
            Badlands => v([0.0, 0.0, 0.035], false, None, [NONE, NONE, &[(Cactus, 0.45, 0.0), (Bush, 0.55, 0.0)]]),
            Hoodoos => v([0.0, 0.0, 0.012], false, None, [NONE, NONE, &[(Cactus, 1.0, 0.0)]]),
            Swamp => v([0.0, 0.32, 0.06], true, None, [NONE, &[(SwampOak, 1.0, 0.0)], &[(Bush, 0.8, 0.0), (Stump, 0.2, 0.0)]]),
            Beach if col.climate.temperature > 0.3 => v([0.0, 0.03, 0.0], false, None, [NONE, &[(Palm, 1.0, 0.0)], NONE]),
            StonyShore => v([0.0, 0.0, 0.05], false, None, [NONE, NONE, S_ROCK]),
            StonyPeaks => v([0.0, 0.0, 0.025], false, None, [NONE, NONE, S_ROCK]),
            Volcanic => v([0.0, 0.0, 0.02], false, None, [NONE, NONE, S_ROCK]),
            _ => v([0.0; 3], false, None, [NONE, NONE, NONE]),
        }
    }

    /// Giant species and acceptance probability for the landmark (`gl = 0`) and emergent
    /// (`gl = 1`) processes.
    pub(super) fn giant(gl: usize, col: &ColumnInfo) -> Option<(Species, f32)> {
        use Biome::*;
        if gl == 1 {
            return match col.biome {
                OldGrowthTaiga => Some((ColossalSpruce, 0.3)),
                _ => None,
            };
        }
        let riverside = col.climate.river > 0.2;
        match col.biome {
            Plains | Meadow | Forest | BirchForest | DarkForest if riverside => Some((Willow, 0.4)),
            Forest => Some((AncientOak, 0.35)),
            DarkForest => Some((AncientOak, 0.25)),
            BirchForest => Some((AncientOak, 0.12)),
            Plains => Some((AncientOak, 0.10)),
            Meadow => Some((AncientOak, 0.16)),
            Taiga => Some((Sequoia, 0.25)),
            OldGrowthTaiga => Some((Sequoia, 0.5)),
            SnowyTaiga => Some((Sequoia, 0.1)),
            Jungle => Some((JungleGiant, 0.6)),
            Savanna => Some((Baobab, 0.45)),
            Swamp => Some((Willow, 0.6)),
            _ => None,
        }
    }

    /// Probability that an accepted tree is a dead snag instead.
    pub(super) fn snag_prob(b: Biome) -> f32 {
        use Biome::*;
        match b {
            Taiga | OldGrowthTaiga | SnowyTaiga => 0.05,
            Swamp => 0.07,
            Forest | DarkForest | BirchForest => 0.03,
            AlpineGrove => 0.06,
            _ => 0.01,
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

/// Giant processes: 0 = landmark, 1 = emergent.
const LANDMARK: usize = 0;
const EMERGENT: usize = 1;
const GIANT_CELL: [i32; 2] = [256, 48];
/// Candidate jitter margin inside the cell (adjacent candidates ≥ 2·margin apart).
const GIANT_MARGIN: [i32; 2] = [24, 8];
const GIANT_P_MAX: [f32; 2] = [0.6, 0.3];
/// Largest suppression radius of each giant process.
const GIANT_SUPP_MAX: [f32; 2] = [17.0, 6.0];

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
    /// Stand-age field σ ∈ ≈[−1, 1] (young stands < 0 < old stands).
    pub stand: f32,
}

/// Build context for a single feature (simple form, see [`Growth`]).
#[derive(Clone, Copy, Debug)]
pub struct TreeCtx {
    pub biome: Biome,
    /// Size scale (1 = full size; < 1 stunted near the treeline).
    pub scale: f32,
}

/// Full build context of a feature: biome, treeline scale, stand age and wind.
#[derive(Clone, Copy, Debug)]
pub struct Growth {
    pub biome: Biome,
    /// Treeline size scale (1 = full size).
    pub scale: f32,
    /// Stand-age field (tilts the age-class distribution).
    pub stand: f32,
    /// Unit vector the prevailing wind blows toward (xz).
    pub wind: (f32, f32),
    /// 0..1: how strongly the wind shapes the tree (lean + flagged crown near the treeline).
    pub windy: f32,
}

impl Growth {
    pub fn simple(biome: Biome, scale: f32) -> Growth {
        Growth { biome, scale, stand: 0.0, wind: (0.0, 0.0), windy: 0.0 }
    }
}

/// An accepted feature root (pure function of the generator and the candidate).
#[derive(Clone, Copy, Debug)]
struct Accepted {
    x: i32,
    z: i32,
    sp: Species,
    var: u64,
    growth: Growth,
}

/// Cheap hasher for the per-query memo (keys are already well-mixed cell ids).
#[derive(Default)]
struct KeyHasher(u64);
impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        mix64(self.0)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ b as u64;
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 ^= v;
    }
}
type Memo = HashMap<u64, Option<Accepted>, BuildHasherDefault<KeyHasher>>;

/// Memo key: process id (0..2 normal layers, 3 + gl giants) and cell.
#[inline]
fn memo_key(l: usize, cx: i32, cz: i32) -> u64 {
    ((l as u64) << 60) | (((cx as u32) as u64 & 0x3FFF_FFFF) << 30) | ((cz as u32) as u64 & 0x3FFF_FFFF)
}

pub struct FeaturePlacer {
    pub seed: u64,
    layer_seed: [u64; 3],
    giant_seed: [u64; 2],
    grove1: Simplex,
    grove2: Simplex,
    mixn: Simplex,
    standn: Simplex,
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
const STAND_L: f64 = 64.0;

#[inline]
fn is_wet(col: &ColumnInfo) -> bool {
    col.water_level != i32::MIN && col.water_level as f32 > col.height
}

impl FeaturePlacer {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        let mut fp = FeaturePlacer {
            seed,
            layer_seed: [s("features.big"), s("features.medium"), s("features.small")],
            giant_seed: [s("features.landmark"), s("features.emergent")],
            grove1: Simplex::new(s("features.grove1")),
            grove2: Simplex::new(s("features.grove2")),
            mixn: Simplex::new(s("features.mix")),
            standn: Simplex::new(s("features.stand")),
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

    /// Giant candidate of cell (gx, gz): (x, z, u, var).
    #[inline]
    fn giant_cand(&self, gl: usize, gx: i32, gz: i32) -> (i32, i32, f32, u64) {
        let c = GIANT_CELL[gl];
        let m = GIANT_MARGIN[gl];
        let h = hash2(self.giant_seed[gl], gx, gz);
        let span = (c - 2 * m) as f32;
        let ox = m + ((hash_f01(h) * span) as i32).min(c - 2 * m - 1);
        let oz = m + ((hash_f01(mix64(h ^ 0x51)) * span) as i32).min(c - 2 * m - 1);
        (gx * c + ox, gz * c + oz, hash_f01(mix64(h ^ 0x3C3C)), mix64(h ^ 0x7777))
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
        let ws = if footprint > 0.0 { (((STAND_L / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let stand = if ws > 0.0 { ws * self.standn.n2(x / STAND_L + 3.1, z / STAND_L - 9.4).clamp(-1.0, 1.0) } else { 0.0 };
        let rel = col.height - self.snow.at(x, z, col.climate.temperature);
        let scale = 0.5 + 0.5 * (1.0 - smoothstep(-110.0, -30.0, rel));
        Env { biome: col.biome, p, mix, rel, scale, slope: col.slope, stand }
    }

    /// Survival factor of a species class in an environment (treeline, slope).
    fn class_factor(&self, sp: Species, env: &Env) -> f32 {
        let tree = (1.0 - smoothstep(-95.0, -30.0, env.rel)) * (1.0 - smoothstep(0.45, 0.9, env.slope));
        match sp.class() {
            Class::Tree => tree,
            Class::MegaTree => tree * smoothstep(0.78, 0.92, env.scale),
            Class::Giant => tree * smoothstep(0.86, 0.96, env.scale) * (1.0 - smoothstep(0.3, 0.45, env.slope)),
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

    /// Growth context at an accepted root.
    fn growth(&self, gen: &WorldGen, col: &ColumnInfo, env: &Env, sp: Species) -> Growth {
        let conifer = matches!(sp, Species::Spruce | Species::Pine | Species::MegaSpruce | Species::Snag);
        let mut windy = if conifer { smoothstep(-130.0, -55.0, env.rel) } else { 0.0 };
        if col.biome == Biome::AlpineGrove && conifer {
            windy = windy.max(0.55);
        }
        let wind = if windy > 0.0 { gen.climate.wind(col.x, col.z) } else { (0.0, 0.0) };
        Growth { biome: col.biome, scale: env.scale, stand: env.stand, wind, windy }
    }

    /// Exact acceptance of the normal candidate of cell (cx, cz) in layer `l` (memoised).
    fn accept(&self, gen: &WorldGen, l: usize, cx: i32, cz: i32, memo: &mut Memo) -> Option<Accepted> {
        let key = memo_key(l, cx, cz);
        if let Some(r) = memo.get(&key) {
            return *r;
        }
        let r = self.accept_uncached(gen, l, cx, cz, memo);
        memo.insert(key, r);
        r
    }

    fn accept_uncached(&self, gen: &WorldGen, l: usize, cx: i32, cz: i32, memo: &mut Memo) -> Option<Accepted> {
        let c = self.cand(l, cx, cz);
        if c.u >= P_MAX[l] || self.survivor(l, cx, cz).is_none() {
            return None;
        }
        let col = gen.column(c.x as f64 + 0.5, c.z as f64 + 0.5);
        if is_wet(&col) || col.slope > 0.9 || col.jaggedness > 6.0 {
            return None;
        }
        let veg = tables::veg(col.biome, &col);
        if veg.mix[l].is_empty() {
            return None;
        }
        let env = self.env(&col, 0.0);
        if c.u >= env.p[l] {
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
        if self.suppressed_by_giant(gen, c.x, c.z, [true, true], memo) {
            return None;
        }
        if l == MED && self.any_accepted(gen, BIG, c.x, c.z, EXCL_MED_BIG, memo) {
            return None;
        }
        if l == SMALL
            && (self.any_accepted(gen, BIG, c.x, c.z, EXCL_SMALL_BIG, memo) || self.any_accepted(gen, MED, c.x, c.z, EXCL_SMALL_MED, memo))
        {
            return None;
        }
        if matches!(sp.class(), Class::Tree | Class::MegaTree) && hash_f01(mix64(c.var ^ 0x5A6)) < tables::snag_prob(col.biome) {
            sp = Species::Snag;
        }
        let growth = self.growth(gen, &col, &env, sp);
        Some(Accepted { x: c.x, z: c.z, sp, var: c.var, growth })
    }

    /// Is there an accepted feature of layer `l` with its root strictly within `r` of (x, z)?
    fn any_accepted(&self, gen: &WorldGen, l: usize, x: i32, z: i32, r: f32, memo: &mut Memo) -> bool {
        let cell = LAYERS[l].cell;
        let ri = r.ceil() as i32;
        let (cx0, cx1) = ((x - ri).div_euclid(cell), (x + ri).div_euclid(cell));
        let (cz0, cz1) = ((z - ri).div_euclid(cell), (z + ri).div_euclid(cell));
        for cz in cz0..=cz1 {
            for cx in cx0..=cx1 {
                let o = self.cand(l, cx, cz);
                if o.u >= P_MAX[l] {
                    continue;
                }
                let d2 = ((o.x - x) * (o.x - x) + (o.z - z) * (o.z - z)) as f32;
                if d2 < r * r && self.accept(gen, l, cx, cz, memo).is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// Exact acceptance of the giant candidate of cell (gx, gz) of process `gl` (memoised).
    fn accept_giant(&self, gen: &WorldGen, gl: usize, gx: i32, gz: i32, memo: &mut Memo) -> Option<Accepted> {
        let key = memo_key(3 + gl, gx, gz);
        if let Some(r) = memo.get(&key) {
            return *r;
        }
        let r = self.accept_giant_uncached(gen, gl, gx, gz, memo);
        memo.insert(key, r);
        r
    }

    fn accept_giant_uncached(&self, gen: &WorldGen, gl: usize, gx: i32, gz: i32, memo: &mut Memo) -> Option<Accepted> {
        let (x, z, u, var) = self.giant_cand(gl, gx, gz);
        if u >= GIANT_P_MAX[gl] {
            return None;
        }
        let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
        if is_wet(&col) || col.slope > 0.45 || col.jaggedness > 4.0 {
            return None;
        }
        let (sp, p) = tables::giant(gl, &col)?;
        let env = self.env(&col, 0.0);
        if u >= p * self.class_factor(sp, &env) {
            return None;
        }
        if gl == EMERGENT && self.suppressed_by_giant(gen, x, z, [true, false], memo) {
            return None;
        }
        let growth = self.growth(gen, &col, &env, sp);
        Some(Accepted { x, z, sp, var, growth })
    }

    /// Does an accepted giant of the selected processes ([landmark, emergent]) suppress a root at
    /// (x, z)? Hash test first; a giant's column is evaluated only for close candidates.
    fn suppressed_by_giant(&self, gen: &WorldGen, x: i32, z: i32, which: [bool; 2], memo: &mut Memo) -> bool {
        for gl in 0..2 {
            if !which[gl] {
                continue;
            }
            let cell = GIANT_CELL[gl];
            let rm = GIANT_SUPP_MAX[gl];
            let ri = rm.ceil() as i32;
            for gz in (z - ri).div_euclid(cell)..=(z + ri).div_euclid(cell) {
                for gx in (x - ri).div_euclid(cell)..=(x + ri).div_euclid(cell) {
                    let (px, pz, u, _) = self.giant_cand(gl, gx, gz);
                    if u >= GIANT_P_MAX[gl] {
                        continue;
                    }
                    let d2 = ((px - x) * (px - x) + (pz - z) * (pz - z)) as f32;
                    if d2 > rm * rm {
                        continue;
                    }
                    if let Some(a) = self.accept_giant(gen, gl, gx, gz, memo) {
                        let rs = a.sp.suppress_radius();
                        if d2 <= rs * rs {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// All features whose root lies in [x0, x1) × [z0, z1).
    pub fn features_in(&self, gen: &WorldGen, x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<Feature> {
        let mut out = Vec::new();
        if x1 <= x0 || z1 <= z0 {
            return out;
        }
        let mut memo = Memo::default();
        for (gl, &cell) in GIANT_CELL.iter().enumerate() {
            for gz in z0.div_euclid(cell)..=(z1 - 1).div_euclid(cell) {
                for gx in x0.div_euclid(cell)..=(x1 - 1).div_euclid(cell) {
                    let (x, z, u, _) = self.giant_cand(gl, gx, gz);
                    if x < x0 || x >= x1 || z < z0 || z >= z1 || u >= GIANT_P_MAX[gl] {
                        continue;
                    }
                    if let Some(a) = self.accept_giant(gen, gl, gx, gz, &mut memo) {
                        out.push(build_feature_with(a.sp, a.x, a.z, a.var, &a.growth));
                    }
                }
            }
        }
        for (l, spec) in LAYERS.iter().enumerate() {
            let cell = spec.cell;
            for cz in z0.div_euclid(cell)..=(z1 - 1).div_euclid(cell) {
                for cx in x0.div_euclid(cell)..=(x1 - 1).div_euclid(cell) {
                    let c = self.cand(l, cx, cz);
                    if c.x < x0 || c.x >= x1 || c.z < z0 || c.z >= z1 || c.u >= P_MAX[l] {
                        continue;
                    }
                    if let Some(a) = self.accept(gen, l, cx, cz, &mut memo) {
                        out.push(build_feature_with(a.sp, a.x, a.z, a.var, &a.growth));
                    }
                }
            }
        }
        out
    }

    /// Roots, species and layer of every accepted feature in the rectangle (no rasterisation):
    /// `(x, z, species, process)` with process 0..2 = big/medium/small, 3 = landmark,
    /// 4 = emergent. Cheap input for far-LOD tree silhouettes and tests.
    pub fn roots_in(&self, gen: &WorldGen, x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<(i32, i32, Species, u8)> {
        let mut out = Vec::new();
        if x1 <= x0 || z1 <= z0 {
            return out;
        }
        let mut memo = Memo::default();
        for (gl, &cell) in GIANT_CELL.iter().enumerate() {
            for gz in z0.div_euclid(cell)..=(z1 - 1).div_euclid(cell) {
                for gx in x0.div_euclid(cell)..=(x1 - 1).div_euclid(cell) {
                    let (x, z, _, _) = self.giant_cand(gl, gx, gz);
                    if x < x0 || x >= x1 || z < z0 || z >= z1 {
                        continue;
                    }
                    if let Some(a) = self.accept_giant(gen, gl, gx, gz, &mut memo) {
                        out.push((a.x, a.z, a.sp, 3 + gl as u8));
                    }
                }
            }
        }
        for (l, spec) in LAYERS.iter().enumerate() {
            let cell = spec.cell;
            for cz in z0.div_euclid(cell)..=(z1 - 1).div_euclid(cell) {
                for cx in x0.div_euclid(cell)..=(x1 - 1).div_euclid(cell) {
                    let c = self.cand(l, cx, cz);
                    if c.x < x0 || c.x >= x1 || c.z < z0 || c.z >= z1 {
                        continue;
                    }
                    if let Some(a) = self.accept(gen, l, cx, cz, &mut memo) {
                        out.push((a.x, a.z, a.sp, l as u8));
                    }
                }
            }
        }
        out
    }

    /// Expected canopy (Boolean-model expectation of the same point processes).
    pub fn canopy_lod(&self, _x: f64, _z: f64, col: &ColumnInfo, footprint: f64) -> CanopyLod {
        let none = CanopyLod { coverage: 0.0, height: 0.0, leaf: Material::OakLeaves };
        if is_wet(col) {
            return none;
        }
        let veg = tables::veg(col.biome, col);
        let giants = [tables::giant(LANDMARK, col), tables::giant(EMERGENT, col)];
        if veg.p == [0.0; 3] && veg.grove.is_none() && giants.iter().all(|g| g.is_none()) {
            return none;
        }
        let env = self.env(col, footprint.max(1e-3));
        let ground = (1.0 - smoothstep(0.8, 0.95, col.slope)) * (1.0 - smoothstep(5.0, 7.0, col.jaggedness));
        let pi = std::f32::consts::PI;
        let mut sum_a = 0.0f32;
        let mut sum_ah = 0.0f32;
        let mut best = (0.0f32, Material::OakLeaves);
        // giants: ρ_g = P(accept)/cell², suppression void probability for everything else
        let gate = (1.0 - smoothstep(0.35, 0.45, col.slope)) * (1.0 - smoothstep(3.0, 4.0, col.jaggedness));
        let mut void_exp = 0.0f32;
        for (gl, g) in giants.iter().enumerate() {
            if let Some((sp, p)) = *g {
                let cell = GIANT_CELL[gl] as f32;
                let rho = (p * self.class_factor(sp, &env)).min(GIANT_P_MAX[gl]) * gate / (cell * cell);
                let (area, h) = sp.lod_shape();
                let a = rho * area;
                sum_a += a;
                sum_ah += a * h;
                void_exp += rho * pi * sp.suppress_radius().powi(2);
                if a > best.0 {
                    if let Some(m) = species_leaf(sp, col.biome) {
                        best = (a, m);
                    }
                }
            }
        }
        let void = (-void_exp).exp();
        let rho_hc = |l: usize| self.retention[l] / (LAYERS[l].cell * LAYERS[l].cell) as f32;
        let excl = [
            1.0,
            (-rho_hc(BIG) * env.p[BIG] * pi * EXCL_MED_BIG * EXCL_MED_BIG).exp(),
            (-(rho_hc(BIG) * env.p[BIG] * pi * EXCL_SMALL_BIG * EXCL_SMALL_BIG
                + rho_hc(MED) * env.p[MED] * pi * EXCL_SMALL_MED * EXCL_SMALL_MED))
                .exp(),
        ];
        let snag = tables::snag_prob(col.biome);
        #[allow(clippy::needless_range_loop)]
        for l in 0..3 {
            if env.p[l] <= 0.0 {
                continue;
            }
            let rho = rho_hc(l) * env.p[l] * excl[l] * ground * void;
            let mut w = [(Species::Oak, 0.0f32); 8];
            let n = self.weights(veg.mix[l], &env, &mut w);
            for &(sp, wi) in &w[..n] {
                let (area, h) = sp.lod_shape();
                let (sa, sh) = if matches!(sp.class(), Class::Rock | Class::Ice | Class::Cactus) {
                    (1.0, 1.0)
                } else {
                    (env.scale.powf(1.6), env.scale)
                };
                let live = if matches!(sp.class(), Class::Tree | Class::MegaTree) { 1.0 - snag } else { 1.0 };
                let a = rho * wi * self.class_factor(sp, &env) * live * area * sa;
                sum_a += a;
                sum_ah += a * h * sh;
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
        Oak | SwampOak | BigOak | AncientOak | Willow => Material::OakLeaves,
        Birch | TallBirch => Material::BirchLeaves,
        Spruce | Pine | MegaSpruce | ColossalSpruce | Sequoia => Material::SpruceLeaves,
        Jungle | MegaJungle | Palm | JungleGiant => Material::JungleLeaves,
        Acacia | Baobab => Material::AcaciaLeaves,
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
/// Golden angle π(3 − √5): azimuth step of successive limbs (phyllotaxis).
const GOLDEN: f32 = 2.399_963_2;

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

    /// Square `n×n` trunk following a Bézier spine over y ∈ [y0, y1). The square's corner is
    /// `round(c − n/2)` (`floor(c)` for n = 1); a 1-wide trunk that steps sideways gets a bridge
    /// voxel so successive layers stay face-connected.
    fn trunk_sq(&mut self, s: &Spine, n: i32, y0: i32, y1: i32, m: Material) {
        let mut prev: Option<(i32, i32)> = None;
        for y in y0..y1 {
            let c = s.at_y(y as f32 + 0.5);
            let (ix, iz) = if n == 1 {
                (c[0].floor() as i32, c[2].floor() as i32)
            } else {
                ((c[0] - n as f32 * 0.5).round() as i32, (c[2] - n as f32 * 0.5).round() as i32)
            };
            for dz in 0..n {
                for dx in 0..n {
                    self.wood(ix + dx, y, iz + dz, m);
                }
            }
            if let Some((px, pz)) = prev {
                if n == 1 && (px, pz) != (ix, iz) {
                    self.wood(ix, y - 1, iz, m);
                    if px != ix && pz != iz {
                        self.wood(ix, y - 1, pz, m);
                    }
                }
            }
            prev = Some((ix, iz));
        }
    }
    /// Round trunk of radius `r(t)` (t ∈ [0, 1] along the spine) over y ∈ [y0, y1).
    fn trunk_round(&mut self, s: &Spine, y0: i32, y1: i32, r: &dyn Fn(f32) -> f32, m: Material) {
        for y in y0..y1 {
            let t = ((y as f32 + 0.5) / s.p2[1].max(1.0)).clamp(0.0, 1.0);
            let c = s.at(t);
            let rr = r(t).max(0.75);
            for z in (c[2] - rr).floor() as i32..=(c[2] + rr).floor() as i32 {
                for x in (c[0] - rr).floor() as i32..=(c[0] + rr).floor() as i32 {
                    let (dx, dz) = (x as f32 + 0.5 - c[0], z as f32 + 0.5 - c[2]);
                    if dx * dx + dz * dz <= rr * rr {
                        self.wood(x, y, z, m);
                    }
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
    /// Tapered capsule (thick limb) from a (radius ra) to b (radius rb); thin limbs → `line`.
    fn capsule(&mut self, a: [f32; 3], b: [f32; 3], ra: f32, rb: f32, m: Material) {
        if ra.max(rb) < 0.75 {
            self.line(a, b, m);
            return;
        }
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let dd = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1e-6);
        let rm = ra.max(rb);
        let lo = [a[0].min(b[0]) - rm, a[1].min(b[1]) - rm, a[2].min(b[2]) - rm];
        let hi = [a[0].max(b[0]) + rm, a[1].max(b[1]) + rm, a[2].max(b[2]) + rm];
        for y in lo[1].floor() as i32..=hi[1].floor() as i32 {
            for z in lo[2].floor() as i32..=hi[2].floor() as i32 {
                for x in lo[0].floor() as i32..=hi[0].floor() as i32 {
                    let p = [x as f32 + 0.5 - a[0], y as f32 + 0.5 - a[1], z as f32 + 0.5 - a[2]];
                    let t = ((p[0] * d[0] + p[1] * d[1] + p[2] * d[2]) / dd).clamp(0.0, 1.0);
                    let q = [p[0] - d[0] * t, p[1] - d[1] * t, p[2] - d[2] * t];
                    let r = ra + (rb - ra) * t;
                    if q[0] * q[0] + q[1] * q[1] + q[2] * q[2] <= r * r {
                        self.wood(x, y, z, m);
                    }
                }
            }
        }
        // keep thin tips connected
        self.line(a, b, m);
    }
    /// Metaball canopy: Σ (1 − |q|²)₊² ≥ τ (τ = ¼, threshold dithered by `rough`); a fraction
    /// `holes` of the outer shell is left open. Balls: (cx, cy, cz, horizontal radius, vertical
    /// radius) — a lone ball's iso-surface has exactly these radii.
    fn canopy(&mut self, balls: &[[f32; 5]], m: Material, rough: f32, holes: f32) {
        const TAU: f32 = 0.25;
        const K: f32 = std::f32::consts::SQRT_2; // support radius = r·√2 ⇒ iso radius = r
        if balls.is_empty() {
            return;
        }
        // Accumulate the field ball by ball over each ball's own support box (cost Σ volumes,
        // not bbox × #balls — giants carry 50+ clusters), then threshold once.
        let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
        for b in balls {
            let (rx, ry) = (b[3] * K, b[4] * K);
            lo = [lo[0].min((b[0] - rx).floor() as i32), lo[1].min((b[1] - ry).floor() as i32), lo[2].min((b[2] - rx).floor() as i32)];
            hi = [hi[0].max((b[0] + rx).floor() as i32), hi[1].max((b[1] + ry).floor() as i32), hi[2].max((b[2] + rx).floor() as i32)];
        }
        // clip to the raster
        lo = [lo[0].max(-self.r), lo[1].max(self.ylo), lo[2].max(-self.r)];
        hi = [hi[0].min(self.r + 1), hi[1].min(self.ylo + self.ny - 1), hi[2].min(self.r + 1)];
        if lo[0] > hi[0] || lo[1] > hi[1] || lo[2] > hi[2] {
            return;
        }
        let (nx, ny, nz) = ((hi[0] - lo[0] + 1) as usize, (hi[1] - lo[1] + 1) as usize, (hi[2] - lo[2] + 1) as usize);
        let mut acc = vec![0.0f32; nx * ny * nz];
        for b in balls {
            let (rx, ry) = (b[3] * K, b[4] * K);
            let (ix, iy) = (1.0 / rx, 1.0 / ry);
            let x0 = ((b[0] - rx).floor() as i32).max(lo[0]);
            let x1 = ((b[0] + rx).floor() as i32).min(hi[0]);
            let y0 = ((b[1] - ry).floor() as i32).max(lo[1]);
            let y1 = ((b[1] + ry).floor() as i32).min(hi[1]);
            let z0 = ((b[2] - rx).floor() as i32).max(lo[2]);
            let z1 = ((b[2] + rx).floor() as i32).min(hi[2]);
            for y in y0..=y1 {
                let qy = (y as f32 + 0.5 - b[1]) * iy;
                let qy2 = qy * qy;
                if qy2 >= 1.0 {
                    continue;
                }
                for z in z0..=z1 {
                    let qz = (z as f32 + 0.5 - b[2]) * ix;
                    let qyz = qy2 + qz * qz;
                    if qyz >= 1.0 {
                        continue;
                    }
                    let row = ((y - lo[1]) as usize * nz + (z - lo[2]) as usize) * nx;
                    for x in x0..=x1 {
                        let qx = (x as f32 + 0.5 - b[0]) * ix;
                        let d2 = qyz + qx * qx;
                        if d2 < 1.0 {
                            let t = 1.0 - d2;
                            acc[row + (x - lo[0]) as usize] += t * t;
                        }
                    }
                }
            }
        }
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let row = ((y - lo[1]) as usize * nz + (z - lo[2]) as usize) * nx;
                for x in lo[0]..=hi[0] {
                    let s = acc[row + (x - lo[0]) as usize];
                    if s > TAU * 0.4 {
                        let hv = self.h(x, y, z);
                        let thr = TAU * (1.0 + rough * (hv - 0.5) * 2.0);
                        if s >= thr && !(s < 2.0 * TAU && self.h(x, y + 7777, z) < holes) {
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
    /// Layered cone with ring (tier) modulation, rotating 4-lobed whorls and a directional
    /// asymmetry `1 + α cos(θ − θ_a)`, centred on `center(y)` (a leaning spine).
    fn cone(&mut self, center: &dyn Fn(f32) -> (f32, f32), y_bot: i32, y_top: i32, c: &ConeSpec, m: Material) {
        let span = (y_top - y_bot).max(1) as f32;
        for y in y_bot..=y_top {
            let (cx, cz) = center(y as f32 + 0.5);
            let u = (y_top - y) as f32 / span;
            let tp = (y_top - y) as f32 / c.period + c.phase;
            let fr = tp - tp.floor();
            let tier = tp.floor() as i32;
            let mut r = (c.r_max * (0.12 + 0.88 * u.powf(c.shape)) * (1.0 - c.ring * (1.0 - fr)) + 0.45).max(c.r_min);
            if y == y_top {
                r = r.min(c.r_min.max(0.6));
            }
            let psi = hash_f01(hash2(self.seed ^ 0xC0E, tier, 7)) * std::f32::consts::TAU;
            let (spsi, cpsi) = psi.sin_cos();
            let ri = (r * (1.0 + c.lobes + c.asym) + 1.0).ceil() as i32;
            let (bx, bz) = (cx.floor() as i32, cz.floor() as i32);
            for z in bz - ri..=bz + ri {
                for x in bx - ri..=bx + ri {
                    let px = x as f32 + 0.5 - cx;
                    let pz = z as f32 + 0.5 - cz;
                    let d2 = px * px + pz * pz;
                    let d = d2.sqrt();
                    // cos(4θ + ψ) with cos 4θ = 8c⁴ − 8c² + 1, sin 4θ = 4sc(2c² − 1), c = cos θ, s = sin θ
                    let (co, sn) = if d > 1e-4 { (px / d, pz / d) } else { (1.0, 0.0) };
                    let c2 = co * co;
                    let cos4 = 8.0 * c2 * c2 - 8.0 * c2 + 1.0;
                    let sin4 = 4.0 * sn * co * (2.0 * c2 - 1.0);
                    let lean = 1.0 + c.asym * (co * c.asym_dir.0 + sn * c.asym_dir.1);
                    let rr = r * lean * (1.0 + c.lobes * (cos4 * cpsi - sin4 * spsi));
                    if d <= rr + 0.22 * (self.h(x, y, z) - 0.5) * 2.0 {
                        self.leaf(x, y, z, m);
                    }
                }
            }
        }
    }
    /// Weeping curtain: in the annulus r_in ≤ d ≤ r_out around (cx, cz), a fraction `density` of
    /// the columns that hold leaves gets a strand hanging from their lowest leaf down toward
    /// `y_min` (so every strand is attached to the crown).
    #[allow(clippy::too_many_arguments)]
    fn curtain(&mut self, cx: f32, cz: f32, r_in: f32, r_out: f32, y_min: i32, density: f32, m: Material) {
        let ri = r_out.ceil() as i32 + 1;
        let (bx, bz) = (cx.floor() as i32, cz.floor() as i32);
        for z in bz - ri..=bz + ri {
            for x in bx - ri..=bx + ri {
                let (dx, dz) = (x as f32 + 0.5 - cx, z as f32 + 0.5 - cz);
                let d = (dx * dx + dz * dz).sqrt();
                if d < r_in || d > r_out || self.h(x, 31337, z) >= density {
                    continue;
                }
                // lowest leaf voxel of the column
                let mut low = None;
                for y in self.ylo..self.ylo + self.ny {
                    let c = self.get(x, y, z);
                    if c != 0 {
                        if Material::from_u8(c & 0x7F).is_leaves() {
                            low = Some(y);
                        }
                        break;
                    }
                }
                let Some(yl) = low else { continue };
                // strand lengths skewed short (u^1.6): a ragged fringe, not a wall
                let len = ((yl - y_min) as f32 * (0.15 + 0.85 * self.h(x, 4242, z).powf(1.6))) as i32;
                for y in (yl - len).max(y_min)..yl {
                    self.leaf(x, y, z, m);
                }
            }
        }
    }

    /// Leaf cap on exposed wood tips inside a crown: every column whose highest voxel is wood
    /// with leaves in its 3×3 neighbourhood (same layer or the one above) gets a leaf on top, so
    /// no trunk or limb end pokes out of the crown top, whatever the dithering did.
    fn cap_wood_tips(&mut self, m: Material) {
        for z in -self.r..=self.r + 1 {
            for x in -self.r..=self.r + 1 {
                let mut y = self.ylo + self.ny - 1;
                while y >= self.ylo && self.get(x, y, z) == 0 {
                    y -= 1;
                }
                if y < self.ylo || !Material::from_u8(self.get(x, y, z) & 0x7F).is_log() {
                    continue;
                }
                let mut near = false;
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        for dy in 0..=1 {
                            let c = self.get(x + dx, y + dy, z + dz);
                            near |= c != 0 && Material::from_u8(c & 0x7F).is_leaves();
                        }
                    }
                }
                if near {
                    self.leaf(x, y + 1, z, m);
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
                        Some((_, v)) if v == c => {}
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

/// Parameters of a layered cone crown.
#[derive(Clone, Copy, Debug)]
struct ConeSpec {
    r_max: f32,
    period: f32,
    ring: f32,
    lobes: f32,
    phase: f32,
    r_min: f32,
    shape: f32,
    asym: f32,
    asym_dir: (f32, f32),
}

/// Quadratic Bézier trunk spine `P(t) = (1−t)²P₀ + 2t(1−t)P₁ + t²P₂`, with P₁ at mid-height so
/// that y(t) = t·H is linear (a layer y maps to t = y/H).
#[derive(Clone, Copy, Debug)]
struct Spine {
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
}

impl Spine {
    /// Spine of an `n×n` square trunk: thick square boles stand vertical (a 1-block shift of a
    /// 2×2 / 3×3 square reads as a notch, not as a lean); 1-wide trunks follow lean and bend.
    fn square(base: [f32; 2], h: f32, g: &Genome, n: i32) -> Spine {
        if n >= 2 {
            Spine::new(base, h, &Genome { lean: 0.0, bend: 0.0, ..*g })
        } else {
            Spine::new(base, h, g)
        }
    }
    /// Trunk of height `h` rooted at `base` (xz) with the genome's lean and bend.
    fn new(base: [f32; 2], h: f32, g: &Genome) -> Spine {
        let h = h.max(1.0);
        let off = h * g.lean.tan();
        let top = [base[0] + off * g.lean_dir.0, h, base[1] + off * g.lean_dir.1];
        let b = g.bend * h;
        let mid = [0.5 * (base[0] + top[0]) + b * g.bend_dir.0, 0.5 * h, 0.5 * (base[1] + top[2]) + b * g.bend_dir.1];
        Spine { p0: [base[0], 0.0, base[1]], p1: mid, p2: top }
    }
    #[inline]
    fn at(&self, t: f32) -> [f32; 3] {
        let (a, b, c) = ((1.0 - t) * (1.0 - t), 2.0 * t * (1.0 - t), t * t);
        [
            a * self.p0[0] + b * self.p1[0] + c * self.p2[0],
            a * self.p0[1] + b * self.p1[1] + c * self.p2[1],
            a * self.p0[2] + b * self.p1[2] + c * self.p2[2],
        ]
    }
    /// Spine point at height y (clamped to the spine; above the top it continues straight up).
    #[inline]
    fn at_y(&self, y: f32) -> [f32; 3] {
        let t = (y / self.p2[1]).clamp(0.0, 1.0);
        let mut p = self.at(t);
        p[1] = y;
        p
    }
}

/// Tiny deterministic stream of uniforms from a seed.
pub struct Rng(u64);
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
    fn rng2(&mut self, r: (f32, f32)) -> f32 {
        self.range(r.0, r.1)
    }
    #[inline]
    fn int(&mut self, a: i32, b: i32) -> i32 {
        (a + (self.f() * (b - a + 1) as f32) as i32).min(b)
    }
    #[inline]
    fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
    /// ≈ N(0, 1) (Irwin–Hall sum of four uniforms).
    #[inline]
    fn normal(&mut self) -> f32 {
        (self.f() + self.f() + self.f() + self.f() - 2.0) * 1.732
    }
}

const TAU: f32 = std::f32::consts::TAU;

// =====================================================================================
// Genome
// =====================================================================================

/// Continuous per-tree parameters drawn from its hash (see module docs).
#[derive(Clone, Copy, Debug)]
pub struct Genome {
    /// 0 sapling, 1 young, 2 mature, 3 old.
    pub age: u8,
    /// Allometric size multiplier (height ∝ size, crown radius ∝ size^0.8), treeline included.
    pub size: f32,
    /// Trunk lean (radians, ≤ 0.24) and its xz direction.
    pub lean: f32,
    pub lean_dir: (f32, f32),
    /// Spine curvature (Bézier mid-point offset as a fraction of the height) and direction.
    pub bend: f32,
    pub bend_dir: (f32, f32),
    /// Crown asymmetry (relative radius gain toward `asym_dir`).
    pub asym: f32,
    pub asym_dir: (f32, f32),
    /// Fraction of the crown shell left open.
    pub holes: f32,
    /// Probability that a limb is dead (bare).
    pub dead: f32,
    /// Azimuth of the first limb of the golden-angle spiral.
    pub phase: f32,
}

impl Genome {
    fn draw(rng: &mut Rng, sp: Species, g: &Growth) -> Genome {
        const K: [f32; 4] = [-1.2, -0.5, 0.25, 1.0];
        const W: [f32; 4] = [0.10, 0.25, 0.45, 0.20];
        const MU: [f32; 4] = [0.42, 0.66, 0.9, 1.12];
        let (lo, hi) = sp.age_classes();
        let mut w = [0.0f32; 4];
        let mut sum = 0.0;
        for c in lo..=hi {
            w[c] = W[c] * (1.3 * g.stand.clamp(-1.0, 1.0) * K[c]).exp();
            sum += w[c];
        }
        let mut r = rng.f() * sum;
        let mut age = hi;
        for c in lo..=hi {
            if r < w[c] {
                age = c;
                break;
            }
            r -= w[c];
        }
        let z = rng.normal();
        let size = match sp.class() {
            Class::Giant => (1.0 + 0.07 * z).clamp(0.86, 1.14),
            // big trees exist only as mature / old trees: a tighter, well-separated range
            Class::MegaTree => ((0.9 + 0.13 * (age == 3) as i32 as f32) * (0.06 * z).exp()).clamp(0.8, 1.2),
            _ => (MU[age] * (0.09 * z).exp()).clamp(0.36, 1.3),
        } * g.scale.clamp(0.45, 1.0);
        let az = rng.range(0.0, TAU);
        let mut lean = if rng.chance(0.18) { rng.range(0.08, 0.2) } else { rng.range(0.0, 0.07) };
        let mut lean_dir = (az.cos(), az.sin());
        let a2 = rng.range(0.0, TAU);
        let mut asym = rng.range(0.0, 0.14);
        let mut asym_dir = (a2.cos(), a2.sin());
        let wl = (g.wind.0 * g.wind.0 + g.wind.1 * g.wind.1).sqrt();
        if g.windy > 0.0 && wl > 0.05 {
            // krummholz: lean and flag downwind (the direction the wind blows toward)
            let wd = (g.wind.0 / wl, g.wind.1 / wl);
            let k = g.windy.clamp(0.0, 1.0);
            lean = lean * (1.0 - k) + k * rng.range(0.1, 0.24);
            let (lx, lz) = (lean_dir.0 * (1.0 - k) + wd.0 * k * 2.0, lean_dir.1 * (1.0 - k) + wd.1 * k * 2.0);
            let ll = (lx * lx + lz * lz).sqrt().max(1e-6);
            lean_dir = (lx / ll, lz / ll);
            asym = asym * (1.0 - k) + k * 0.32;
            asym_dir = wd;
        }
        let b = rng.range(0.0, TAU);
        // big trees stand straight: a leaning 2×2 / 3×3 bole would read as a broken staircase
        let (lean_cap, bend_cap) = match sp.class() {
            Class::MegaTree => (0.08, 0.03),
            Class::Giant => (0.05, 0.02),
            _ => (0.24, 0.06),
        };
        Genome {
            age: age as u8,
            size,
            lean: lean.min(lean_cap),
            lean_dir,
            bend: rng.range(0.0, bend_cap),
            bend_dir: (b.cos(), b.sin()),
            asym,
            asym_dir,
            holes: rng.range(0.0, 0.06),
            dead: 0.03 + if age == 3 { 0.07 } else { 0.0 },
            phase: rng.range(0.0, TAU),
        }
    }
}

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

/// Wood of dead trees / stumps / logs in a biome.
fn dead_wood(b: Biome) -> Material {
    use Biome::*;
    match b {
        Taiga | OldGrowthTaiga | SnowyTaiga | AlpineGrove | Meadow => Material::SpruceLog,
        BirchForest => Material::BirchLog,
        DarkForest => Material::DarkOakLog,
        Jungle => Material::JungleLog,
        Savanna => Material::AcaciaLog,
        _ => Material::OakLog,
    }
}

/// Build one feature of a given species at a root (deterministic in `var`).
pub fn build_feature(sp: Species, root_x: i32, root_z: i32, var: u64, ctx: &TreeCtx) -> Feature {
    build_feature_with(sp, root_x, root_z, var, &Growth::simple(ctx.biome, ctx.scale))
}

/// Build one feature with a full growth context (stand age, wind).
pub fn build_feature_with(sp: Species, root_x: i32, root_z: i32, var: u64, g: &Growth) -> Feature {
    let mut rng = Rng(var);
    let genome = Genome::draw(&mut rng, sp, g);
    let rs = build_shape(sp, &mut rng, &genome, g, var);
    Feature { root_x, root_z, kind: sp.kind(), columns: rs.columns() }
}

/// Parameters of the metaball broadleaf generator.
#[derive(Clone, Copy, Debug)]
struct Broad {
    /// Trunk length (limbs attach along it; the crown centre sits at its top).
    trunk: f32,
    /// Trunk thickness (1..3 square).
    n: i32,
    wood: Material,
    leaf: Material,
    /// Crown radius (horizontal).
    r: f32,
    limbs: (i32, i32),
    /// Fraction of the trunk where limbs start.
    limb_from: f32,
    /// Limb elevation range (radians).
    el: (f32, f32),
    /// Limb length range (× r).
    len: (f32, f32),
    /// Leaf-cluster radius range (× r).
    blob: (f32, f32),
    /// Cluster vertical / horizontal radius.
    flat: f32,
    /// Central cluster radius (× r) and its flatness.
    top: f32,
    top_flat: f32,
    /// Limb radius at the trunk (capsule; < 0.75 → 1-voxel path).
    limb_r: f32,
    /// Sub-branches per limb.
    twigs: i32,
    rough: f32,
    /// Weeping-curtain density (0 = none).
    weep: f32,
    /// Surface roots (root flare) count.
    roots: i32,
    /// Dome crown: long limbs spread flat, short ones rise (el falls as length grows).
    dome: bool,
}

/// Metaball broadleaf: Bézier trunk, golden-angle limbs with clusters on their tips.
fn broadleaf(rs: &mut Raster, rng: &mut Rng, g: &Genome, b: &Broad) -> [f32; 3] {
    let base = if b.n % 2 == 0 { [b.n as f32 * 0.5, b.n as f32 * 0.5] } else { [0.5, 0.5] };
    let spine = Spine::square(base, b.trunk, g, b.n);
    let ty = b.trunk.ceil() as i32;
    rs.trunk_sq(&spine, b.n, 0, ty, b.wood);
    let top = spine.at(1.0);
    let lim = (MAX_FEATURE_RADIUS as f32 - 0.8).min(rs.r as f32 - 0.5);
    let shift = 0.5 * g.asym * b.r;
    let c = [top[0] + shift * g.asym_dir.0, top[1] + b.top * b.r * b.top_flat * 0.3, top[2] + shift * g.asym_dir.1];
    let mut balls: Vec<[f32; 5]> = Vec::with_capacity(16);
    if b.top > 0.0 {
        balls.push([c[0], c[1], c[2], b.top * b.r, b.top * b.r * b.top_flat]);
        // the trunk top always ends inside the crown
        rs.line(top, c, b.wood);
    }
    let nl = rng.int(b.limbs.0, b.limbs.1).max(0);
    for k in 0..nl {
        let f = b.limb_from + (0.97 - b.limb_from) * ((k as f32 + rng.range(0.15, 0.85)) / nl.max(1) as f32);
        let s = spine.at(f);
        let th = g.phase + k as f32 * GOLDEN + rng.range(-0.3, 0.3);
        let (ct, st) = (th.cos(), th.sin());
        let side = 1.0 + 2.0 * g.asym * (ct * g.asym_dir.0 + st * g.asym_dir.1);
        let lr = rng.f();
        let el = if b.dome { b.el.1 + (b.el.0 - b.el.1) * lr } else { rng.rng2(b.el) };
        let rb = b.r * rng.rng2(b.blob);
        let mut len = b.r * (b.len.0 + (b.len.1 - b.len.0) * lr) * side;
        // keep the cluster inside the raster / MAX_FEATURE_RADIUS
        let hx = |len: f32| {
            let ex = s[0] + len * el.cos() * ct - 0.5;
            let ez = s[2] + len * el.cos() * st - 0.5;
            (ex * ex + ez * ez).sqrt()
        };
        while len > 0.5 && hx(len) + rb > lim {
            len *= 0.9;
        }
        let e = [s[0] + len * el.cos() * ct, s[1] + len * el.sin(), s[2] + len * el.cos() * st];
        rs.capsule(s, e, b.limb_r, b.limb_r * 0.55, b.wood);
        if rng.chance(g.dead) {
            continue; // dead limb: bare wood
        }
        balls.push([e[0], e[1] + 0.35 * rb * b.flat, e[2], rb, rb * b.flat]);
        for _ in 0..b.twigs {
            let f2 = rng.range(0.5, 0.8);
            let st0 = [s[0] + (e[0] - s[0]) * f2, s[1] + (e[1] - s[1]) * f2, s[2] + (e[2] - s[2]) * f2];
            let th2 = th + if rng.chance(0.5) { 1.0 } else { -1.0 } * rng.range(0.5, 0.9);
            let l2 = len * rng.range(0.3, 0.5);
            let e2y = st0[1] + l2 * (el + 0.25).sin();
            let mut e2 = [st0[0] + l2 * th2.cos(), e2y, st0[2] + l2 * th2.sin()];
            let rb2 = rb * rng.range(0.65, 0.85);
            let dx = e2[0] - 0.5;
            let dz = e2[2] - 0.5;
            let dh = (dx * dx + dz * dz).sqrt();
            if dh + rb2 > lim {
                let k = ((lim - rb2) / dh.max(1e-3)).max(0.0);
                e2 = [0.5 + dx * k, e2[1], 0.5 + dz * k];
            }
            rs.capsule(st0, e2, b.limb_r * 0.5, b.limb_r * 0.3, b.wood);
            balls.push([e2[0], e2[1] + 0.3 * rb2 * b.flat, e2[2], rb2, rb2 * b.flat]);
        }
    }
    rs.canopy(&balls, b.leaf, b.rough, g.holes);
    rs.cap_wood_tips(b.leaf);
    if b.weep > 0.0 {
        rs.curtain(c[0], c[2], b.r * 0.55, b.r * 1.15, (b.trunk * 0.3).ceil() as i32 + 1, b.weep, b.leaf);
    }
    for k in 0..b.roots {
        // surface roots: short logs along the ground radiating from the trunk (replace soil below)
        let th = g.phase + 0.7 + k as f32 * GOLDEN;
        let r0 = b.n as f32 * 0.5;
        let len = r0 + rng.range(1.2, 2.6) + b.n as f32 * 0.4;
        let (cx, cz) = (base[0], base[1]);
        let a = [cx + r0 * 0.8 * th.cos(), 0.4, cz + r0 * 0.8 * th.sin()];
        let e = [cx + len * th.cos(), -0.6, cz + len * th.sin()];
        rs.line(a, e, b.wood);
        rs.solid(e[0].floor() as i32, -1, e[2].floor() as i32, b.wood, true);
        rs.solid(a[0].floor() as i32, -1, a[2].floor() as i32, b.wood, true);
    }
    c
}

/// Parameters of the cone-crown conifer generator.
#[derive(Clone, Copy, Debug)]
struct Conifer {
    h: i32,
    n: i32,
    wood: Material,
    leaf: Material,
    base: i32,
    cone: ConeSpec,
    /// Leaf voxels stacked on the top (leader).
    leader: i32,
    roots: i32,
}

fn conifer(rs: &mut Raster, rng: &mut Rng, g: &Genome, c: &Conifer) {
    let base = if c.n % 2 == 0 { [c.n as f32 * 0.5, c.n as f32 * 0.5] } else { [0.5, 0.5] };
    let spine = Spine::square(base, c.h as f32, g, c.n);
    // tapering bole: n×n low down, thinner higher up, ending well inside the narrow cone top
    // (a thick trunk running to the tip shows through the crown)
    // The steps happen inside the crown (above its base), where the foliage hides them.
    let span = (c.h - c.base).max(1) as f32;
    let t_end = (c.h - 2 - 2 * (c.n - 1)).max(1);
    let mut y0 = 0;
    for (k, frac) in [(c.n, 0.18), (c.n - 1, 0.45), (1, 2.0)] {
        if k < 1 {
            continue;
        }
        let y1 = (c.base + 1 + (span * frac) as i32).min(t_end);
        if y1 > y0 {
            rs.trunk_sq(&spine, k, y0, y1, c.wood);
            y0 = y1;
        }
    }
    let center = |y: f32| {
        let p = spine.at_y(y);
        (p[0], p[2])
    };
    rs.cone(&center, c.base, c.h, &c.cone, c.leaf);
    // leader: an unbroken leaf column on the axis from the trunk top to above the cone tip
    for y in (c.h - 2).max(c.base)..=c.h + c.leader {
        let (tx, tz) = center(y as f32 + 0.5);
        rs.leaf(tx.floor() as i32, y, tz.floor() as i32, c.leaf);
    }
    if c.n >= 2 {
        // roots / root knees around the bole
        let (bx, bz) = ((base[0] - c.n as f32 * 0.5).round() as i32, (base[1] - c.n as f32 * 0.5).round() as i32);
        for y in -3..0 {
            for dz in 0..c.n {
                for dx in 0..c.n {
                    rs.solid(bx + dx, y, bz + dz, c.wood, true);
                }
            }
        }
        for k in 0..c.roots {
            let th = g.phase + k as f32 * GOLDEN;
            let r0 = c.n as f32 * 0.5;
            let len = r0 + rng.range(0.8, 1.6 + 0.4 * c.n as f32);
            let a = [base[0] + r0 * 0.7 * th.cos(), 0.5 + rng.range(0.0, 0.4 * c.n as f32), base[1] + r0 * 0.7 * th.sin()];
            let e = [base[0] + len * th.cos(), -0.5, base[1] + len * th.sin()];
            rs.line(a, e, c.wood);
            rs.solid(e[0].floor() as i32, -1, e[2].floor() as i32, c.wood, true);
        }
    }
}

fn build_shape(sp: Species, rng: &mut Rng, g: &Genome, gr: &Growth, var: u64) -> Raster {
    use Material as M;
    let s = g.size;
    let biome = gr.biome;
    match sp {
        Species::Oak | Species::SwampOak => {
            let swamp = sp == Species::SwampOak;
            let v = rng.f();
            let h = 8.6 * s;
            let mut b = Broad {
                trunk: (h * 0.55).max(2.0) + swamp as i32 as f32,
                n: 1,
                wood: M::OakLog,
                leaf: M::OakLeaves,
                r: 3.3 * s.powf(0.8),
                limbs: (3, 5),
                limb_from: 0.62,
                el: (0.45, 0.9),
                len: (0.45, 0.7),
                blob: (0.5, 0.66),
                flat: 0.8,
                top: 0.8,
                top_flat: 0.85,
                limb_r: 0.0,
                twigs: 0,
                rough: 0.4,
                weep: 0.0,
                roots: 0,
                dome: false,
            };
            if v < 0.25 {
                // round, compact crown
                b.el = (0.6, 1.1);
                b.len = (0.3, 0.5);
                b.blob = (0.55, 0.7);
                b.top = 1.0;
            } else if v < 0.45 {
                // broad spreading crown
                b.r *= 1.15;
                b.el = (0.2, 0.5);
                b.len = (0.65, 0.9);
                b.blob = (0.45, 0.58);
                b.flat = 0.68;
                b.top = 0.62;
                b.top_flat = 0.7;
            }
            if swamp {
                b.r *= 1.3;
                b.flat = 0.6;
                b.top_flat = 0.6;
                b.weep = 0.28;
                b.limb_from = 0.7;
            }
            let rr = (b.r * 1.9 + h * g.lean.tan() + 2.0).ceil() as i32;
            let mut rs = Raster::new(rr.min(12), -1, (h * 1.4) as i32 + 6, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::BigOak => {
            let h = 14.0 * s;
            let n = if s >= 1.02 { 2 } else { 1 };
            let b = Broad {
                trunk: h * 0.58,
                n,
                wood: M::OakLog,
                leaf: M::OakLeaves,
                r: 5.0 * s.powf(0.8),
                limbs: (4, 6),
                limb_from: 0.5,
                el: (0.3, 0.75),
                len: (0.55, 0.9),
                blob: (0.42, 0.55),
                flat: 0.78,
                top: 0.6,
                top_flat: 0.8,
                limb_r: if n == 2 { 0.8 } else { 0.0 },
                twigs: 1,
                rough: 0.42,
                weep: 0.0,
                roots: if n == 2 { 3 } else { 0 },
                dome: true,
            };
            let mut rs = Raster::new(14, -2, (h * 1.3) as i32 + 8, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::AncientOak => {
            let b = Broad {
                trunk: 11.0 * s,
                n: 3,
                wood: M::OakLog,
                leaf: M::OakLeaves,
                r: 17.0 * s,
                limbs: (8, 10),
                limb_from: 0.55,
                el: (0.12, 0.62),
                len: (0.45, 0.95),
                blob: (0.25, 0.3),
                flat: 0.62,
                top: 0.42,
                top_flat: 0.55,
                limb_r: 1.15,
                twigs: 2,
                rough: 0.42,
                weep: 0.0,
                roots: 7,
                dome: true,
            };
            let mut rs = Raster::new(MAX_FEATURE_RADIUS, -2, 40, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::Willow => {
            let b = Broad {
                trunk: 7.5 * s,
                n: 2,
                wood: M::OakLog,
                leaf: M::OakLeaves,
                r: 10.0 * s,
                limbs: (6, 8),
                limb_from: 0.65,
                el: (0.35, 0.95),
                len: (0.45, 0.85),
                blob: (0.32, 0.4),
                flat: 0.62,
                top: 0.5,
                top_flat: 0.6,
                limb_r: 0.8,
                twigs: 1,
                rough: 0.4,
                weep: 0.42,
                roots: 4,
                dome: true,
            };
            let mut rs = Raster::new(16, -2, 26, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::Birch | Species::TallBirch => {
            let tall = sp == Species::TallBirch;
            let h = if tall { 12.0 } else { 8.6 } * s;
            let r = 2.15 * s.powf(0.7);
            let b = Broad {
                trunk: (h * 0.66).max(3.0),
                n: 1,
                wood: M::BirchLog,
                leaf: M::BirchLeaves,
                r,
                limbs: (1, 3),
                limb_from: 0.55,
                el: (0.8, 1.25),
                len: (0.35, 0.6),
                blob: (0.62, 0.8),
                flat: 1.15,
                top: 1.0,
                top_flat: if tall { 1.5 } else { 1.3 },
                limb_r: 0.0,
                twigs: 0,
                rough: 0.38,
                weep: 0.0,
                roots: 0,
                dome: false,
            };
            let mut rs = Raster::new(7, -1, (h * 1.4) as i32 + 6, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::Jungle => {
            let h = 13.0 * s;
            let b = Broad {
                trunk: (h * 0.82).max(4.0),
                n: 1,
                wood: M::JungleLog,
                leaf: M::JungleLeaves,
                r: 3.3 * s.powf(0.8),
                limbs: (2, 4),
                limb_from: 0.45,
                el: (0.25, 0.6),
                len: (0.55, 0.85),
                blob: (0.42, 0.56),
                flat: 0.68,
                top: 0.95,
                top_flat: 0.55,
                limb_r: 0.0,
                twigs: 0,
                rough: 0.4,
                weep: 0.0,
                roots: 0,
                dome: false,
            };
            let mut rs = Raster::new(9, -1, (h * 1.3) as i32 + 6, var);
            broadleaf(&mut rs, rng, g, &b);
            rs
        }
        Species::MegaJungle | Species::JungleGiant => {
            let giant = sp == Species::JungleGiant;
            let h = if giant { 48.0 } else { 29.0 } * s;
            let n = if giant { 3 } else { 2 };
            let b = Broad {
                trunk: h * if giant { 0.84 } else { 0.8 },
                n,
                wood: M::JungleLog,
                leaf: M::JungleLeaves,
                r: if giant { 12.5 * s } else { 6.6 * s.powf(0.8) },
                limbs: if giant { (6, 8) } else { (5, 7) },
                limb_from: if giant { 0.86 } else { 0.6 },
                el: if giant { (0.12, 0.38) } else { (0.2, 0.5) },
                len: (0.62, 0.95),
                blob: if giant { (0.26, 0.33) } else { (0.36, 0.46) },
                flat: if giant { 0.42 } else { 0.58 },
                top: if giant { 0.36 } else { 0.6 },
                top_flat: 0.45,
                limb_r: if giant { 1.0 } else { 0.8 },
                twigs: 1,
                rough: 0.42,
                weep: 0.0,
                roots: 0,
                dome: giant,
            };
            let mut rs = Raster::new(if giant { MAX_FEATURE_RADIUS } else { 12 }, -4, (h * 1.15) as i32 + 8, var);
            broadleaf(&mut rs, rng, g, &b);
            // buttress roots: fins fanning out of the bole, tall at the trunk, low at the tips
            let fins = if giant { rng.int(5, 7) } else { 4 };
            // trunk axis (odd n: centred on the root cell; even n: on its corner) and half-width
            let cc = if n % 2 == 0 { n as f32 * 0.5 } else { 0.5 };
            let c0 = n as f32 * 0.5;
            let spine = Spine::square([cc, cc], b.trunk, g, n);
            for k in 0..fins {
                if !giant && rng.chance(0.25) {
                    continue;
                }
                let th = g.phase + 0.4 + k as f32 * TAU / fins as f32 + rng.range(-0.25, 0.25);
                let len = c0 + if giant { rng.range(3.0, 6.0) } else { rng.range(1.0, 3.0) };
                let hgt = if giant { rng.range(4.0, 7.0) } else { rng.range(1.5, 3.0) };
                let steps = (len * 2.0).ceil() as i32;
                for i in 0..=steps {
                    let d = c0 * 0.6 + (len - c0 * 0.6) * i as f32 / steps as f32;
                    let (x, z) = ((cc + d * th.cos()).floor() as i32, (cc + d * th.sin()).floor() as i32);
                    let top = (hgt * (1.0 - i as f32 / steps as f32).powf(1.5)).round() as i32;
                    rs.solid(x, -1, z, M::JungleLog, true);
                    for y in 0..top.max(0) {
                        rs.wood(x, y, z, M::JungleLog);
                    }
                }
            }
            if giant {
                // lianas: leaf strands down the bole
                for _ in 0..rng.int(3, 6) {
                    let th = rng.range(0.0, TAU);
                    let y1 = (b.trunk * rng.range(0.4, 0.9)) as i32;
                    let y0 = (y1 - rng.int(5, 14)).max(1);
                    for y in y0..y1 {
                        // hug the (leaning) bole: one block outside its surface
                        let p = spine.at_y(y as f32 + 0.5);
                        let (x, z) = ((p[0] + (c0 + 0.5) * th.cos()).floor() as i32, (p[2] + (c0 + 0.5) * th.sin()).floor() as i32);
                        rs.leaf(x, y, z, M::JungleLeaves);
                    }
                }
            }
            rs
        }
        Species::Spruce | Species::Pine => {
            let pine = sp == Species::Pine;
            let h = if pine { (18.0 * s).round() as i32 } else { (17.0 * s).round() as i32 }.max(4);
            let hf = h as f32;
            let (base, cone) = if pine {
                (
                    (hf * rng.range(0.55, 0.68)) as i32,
                    ConeSpec {
                        r_max: (0.11 * hf + 0.8) * rng.range(0.9, 1.1),
                        period: rng.range(2.0, 2.6),
                        ring: 0.3,
                        lobes: 0.12,
                        phase: rng.f(),
                        r_min: 0.5,
                        shape: 0.55,
                        asym: g.asym,
                        asym_dir: g.asym_dir,
                    },
                )
            } else {
                let lift = if g.age >= 3 { rng.int(0, h / 5) } else { 0 };
                (
                    (rng.int(1, 3) + lift).min(h / 3).max(1),
                    ConeSpec {
                        r_max: (0.17 * hf + 0.7) * rng.range(0.88, 1.12),
                        period: if h > 16 || rng.chance(0.4) { 3.0 } else { 2.0 },
                        ring: rng.range(0.5, 0.62),
                        lobes: 0.12,
                        phase: rng.f(),
                        r_min: 0.5,
                        shape: rng.range(0.82, 1.0),
                        asym: g.asym,
                        asym_dir: g.asym_dir,
                    },
                )
            };
            let mut rs = Raster::new(9, -1, h + 4, var);
            conifer(&mut rs, rng, g, &Conifer { h, n: 1, wood: M::SpruceLog, leaf: M::SpruceLeaves, base, cone, leader: 1, roots: 0 });
            if pine {
                // a few dead stubs on the bare bole
                let spine = Spine::new([0.5, 0.5], hf, g);
                for _ in 0..rng.int(0, 2) {
                    let y = rng.int(3, (base - 1).max(3));
                    let p = spine.at_y(y as f32 + 0.5);
                    let th = rng.range(0.0, TAU);
                    rs.line(p, [p[0] + 1.3 * th.cos(), p[1] + 0.3, p[2] + 1.3 * th.sin()], M::SpruceLog);
                }
            }
            rs
        }
        Species::MegaSpruce | Species::ColossalSpruce => {
            let colossal = sp == Species::ColossalSpruce;
            let h = if colossal { (58.0 * s).round() as i32 } else { (36.0 * s).round() as i32 }.max(16);
            let hf = h as f32;
            let cone = ConeSpec {
                r_max: if colossal { 0.12 * hf + 1.6 } else { 0.13 * hf + 1.2 } * rng.range(0.9, 1.1),
                period: if colossal { rng.range(3.6, 4.6) } else { rng.range(3.0, 4.0) },
                ring: if colossal { 0.5 } else { 0.55 },
                lobes: if colossal { 0.18 } else { 0.16 },
                phase: rng.f(),
                r_min: if colossal { 1.6 } else { 1.05 },
                shape: rng.range(0.85, 1.0),
                asym: g.asym,
                asym_dir: g.asym_dir,
            };
            let base = (hf * if colossal { rng.range(0.38, 0.5) } else { rng.range(0.3, 0.45) }) as i32;
            let n = if colossal { 3 } else { 2 };
            let mut rs = Raster::new(if colossal { 16 } else { 12 }, -4, h + 5, var);
            let leader = if colossal { 3 } else { 2 };
            conifer(&mut rs, rng, g, &Conifer { h, n, wood: M::SpruceLog, leaf: M::SpruceLeaves, base, cone, leader, roots: if colossal { 7 } else { 4 } });
            rs
        }
        Species::Sequoia => sequoia(rng, g, var),
        Species::Baobab => baobab(rng, g, var),
        Species::Acacia => {
            let t1 = ((rng.range(3.0, 5.0) * s).round() as i32).max(2);
            let mut rs = Raster::new(11, -1, t1 + 12, var);
            let spine = Spine::new([0.5, 0.5], t1 as f32, g);
            rs.trunk_sq(&spine, 1, 0, t1, M::AcaciaLog);
            let top = spine.at(1.0);
            let az = rng.range(0.0, TAU);
            let k = rng.range(2.0, 4.0) * s.sqrt();
            let base = [top[0], t1 as f32 - 0.5, top[2]];
            let mut tops = Vec::with_capacity(3);
            let ta = [base[0] + k * az.cos(), base[1] + k, base[2] + k * az.sin()];
            rs.line(base, ta, M::AcaciaLog);
            tops.push(ta);
            let forks = if rng.chance(0.7) { if rng.chance(0.3) { 2 } else { 1 } } else { 0 };
            for f in 0..forks {
                let az2 = az + std::f32::consts::PI * (0.75 + 0.5 * f as f32) + rng.range(-0.5, 0.5);
                let k2 = rng.range(1.5, 3.0) * s.sqrt();
                let b2 = [base[0], t1 as f32 - rng.range(0.5, 1.5), base[2]];
                let tb = [b2[0] + k2 * az2.cos(), b2[1] + k2 * 0.9 + 0.5, b2[2] + k2 * az2.sin()];
                rs.line(b2, tb, M::AcaciaLog);
                tops.push(tb);
            }
            for (i, tp) in tops.iter().enumerate() {
                let ty = tp[1].floor() as i32;
                let r1 = rng.range(3.0, 4.2) * s.powf(0.8) * if i == 0 { 1.0 } else { 0.82 };
                let p = rng.range(2.3, 2.9);
                rs.disc(tp[0], tp[2], ty + 1, r1, p, M::AcaciaLeaves, 0.35);
                rs.disc(tp[0], tp[2], ty + 2, r1 * rng.range(0.5, 0.65), p - 0.2, M::AcaciaLeaves, 0.3);
            }
            rs
        }
        Species::DarkOak => {
            let t = ((rng.range(6.0, 9.0) * s).round() as i32).max(4);
            let mut rs = Raster::new(10, -3, t + 7, var);
            let spine = Spine::square([1.0, 1.0], t as f32 + 1.0, g, 2);
            for y in -2..0 {
                for dz in 0..2 {
                    for dx in 0..2 {
                        rs.solid(dx, y, dz, M::DarkOakLog, true);
                    }
                }
            }
            rs.trunk_sq(&spine, 2, 0, t + 1, M::DarkOakLog);
            for (dx, dz) in [(-1, 0), (2, 1), (1, -1), (0, 2)] {
                if rng.chance(0.5) {
                    rs.wood(dx, 0, dz, M::DarkOakLog);
                }
            }
            let tp = spine.at(1.0);
            // limbs under the canopy
            for k in 0..rng.int(2, 4) {
                let a = g.phase + k as f32 * GOLDEN;
                let y = t as f32 - rng.range(1.0, 2.5);
                let p = spine.at_y(y);
                let len = rng.range(2.0, 3.2) * s.sqrt();
                rs.line(p, [p[0] + len * a.cos(), y + 1.2, p[2] + len * a.sin()], M::DarkOakLog);
            }
            let r = rng.range(4.6, 5.4) * s.powf(0.8);
            let (cx, cz) = (tp[0] + 0.4 * g.asym * r * g.asym_dir.0, tp[2] + 0.4 * g.asym * r * g.asym_dir.1);
            let layers = [(t - 1, 0.74), (t, 1.0), (t + 1, 0.98), (t + 2, 0.74), (t + 3, 0.4)];
            for (y, f) in layers {
                rs.disc(cx, cz, y, r * f, 2.3, M::DarkOakLeaves, 0.45);
            }
            let mut bumps = Vec::new();
            for _ in 0..rng.int(2, 4) {
                let a = rng.range(0.0, TAU);
                let d = rng.range(1.5, 3.2);
                bumps.push([cx + d * a.cos(), t as f32 + 3.0, cz + d * a.sin(), 1.7, 1.1]);
            }
            rs.canopy(&bumps, M::DarkOakLeaves, 0.4, 0.0);
            rs
        }
        Species::Bush => {
            let (leaf, log) = bush_mats(biome);
            let mut rs = Raster::new(4, -1, 5, var);
            let stem = rng.chance(0.6) as i32;
            if stem > 0 {
                rs.wood(0, 0, 0, log);
            }
            let k = (0.75 + 0.35 * s).min(1.15);
            let r0 = rng.range(1.3, 2.1) * k;
            let ry = rng.range(1.0, 1.5) * k.sqrt();
            let a = rng.range(0.0, TAU);
            let balls = [
                [0.5, 0.7 + stem as f32 * 0.6, 0.5, r0, ry],
                [0.5 + 0.9 * a.cos(), 0.6, 0.5 + 0.9 * a.sin(), r0 * 0.75, ry * 0.8],
            ];
            let dry = matches!(biome, Biome::Badlands | Biome::Desert | Biome::Hoodoos);
            rs.canopy(&balls, leaf, if dry { 0.9 } else { 0.45 }, 0.0);
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
            let h = ((rng.range(7.0, 10.5) * s).round() as i32).max(5);
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
            rs.canopy(&[[top[0], top[1] + 0.6, top[2], 1.2, 1.0]], M::JungleLeaves, 0.2, 0.0);
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
        Species::Snag => {
            let wood = dead_wood(biome);
            let (base_h, thick) = match biome {
                Biome::Taiga | Biome::OldGrowthTaiga | Biome::SnowyTaiga | Biome::AlpineGrove => (15.0, 0.22),
                Biome::DarkForest => (9.0, 0.3),
                Biome::Jungle => (13.0, 0.1),
                _ => (9.0, 0.08),
            };
            let n = if rng.chance(thick) { 2 } else { 1 };
            let h = ((base_h * s.max(0.6) * rng.range(0.4, 0.8)).round() as i32).max(3);
            let mut rs = Raster::new(5, -1, h + 3, var);
            let base = if n == 2 { [1.0, 1.0] } else { [0.5, 0.5] };
            let spine = Spine::square(base, h as f32, g, n);
            rs.trunk_sq(&spine, n, 0, h, wood);
            // broken top: a jagged extra voxel or two
            let tp = spine.at(1.0);
            if rng.chance(0.6) {
                rs.wood(tp[0].floor() as i32, h, tp[2].floor() as i32, wood);
            }
            // 0–2 short upturned stubs at different heights (horizontal pairs read as crosses)
            let ns = rng.int(0, 2);
            for k in 0..ns {
                let y = h as f32 * if k == 0 { rng.range(0.45, 0.6) } else { rng.range(0.75, 0.92) };
                let p = spine.at_y(y);
                let th = g.phase + k as f32 * rng.range(1.2, 1.9);
                let l = rng.range(1.0, 2.0) + (n - 1) as f32 * 0.5;
                let el = rng.range(0.55, 1.0);
                rs.line(p, [p[0] + l * el.cos() * th.cos(), p[1] + l * el.sin(), p[2] + l * el.cos() * th.sin()], wood);
            }
            rs
        }
        Species::Stump => {
            let wood = dead_wood(biome);
            let mut rs = Raster::new(3, -1, 4, var);
            let h = rng.int(1, 2);
            for y in 0..h {
                rs.wood(0, y, 0, wood);
            }
            if rng.chance(0.5) {
                let (dx, dz) = [(1, 0), (-1, 0), (0, 1), (0, -1)][rng.int(0, 3) as usize];
                rs.wood(dx, 0, dz, wood);
            }
            let mossy = matches!(biome, Biome::Taiga | Biome::OldGrowthTaiga | Biome::DarkForest | Biome::Swamp | Biome::Forest);
            if mossy && rng.chance(0.55) {
                rs.solid(0, h, 0, M::Moss, false);
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
            let old = matches!(biome, Biome::OldGrowthTaiga | Biome::DarkForest);
            // axis-aligned (diagonal logs rasterise to zig-zag staircases)
            let len = (rng.range(4.0, if old { 10.0 } else { 7.5 }) * s.sqrt().max(0.75)).round() as i32;
            let (dx, dz) = [(1, 0), (-1, 0), (0, 1), (0, -1)][rng.int(0, 3) as usize];
            let off = rng.int(0, 1);
            let (ax, az) = (-dx * off, -dz * off);
            let mut rs = Raster::new(12, -1, 5, var);
            let thick = old && rng.chance(0.35);
            let w = if thick { 2 } else { 1 };
            for k in 0..len {
                for a in 0..w {
                    for y in 0..w {
                        // the second row runs beside the first (perpendicular to the log)
                        rs.wood(ax + dx * k + dz.abs() * a, y, az + dz * k + dx.abs() * a, log);
                    }
                }
            }
            if rng.chance(0.5) {
                // a broken side branch
                let k = rng.int(1, (len - 2).max(1));
                let side = if rng.chance(0.5) { 1 } else { -1 };
                rs.wood(ax + dx * k + dz * side * w.max(1), 0, az + dz * k + dx * side * w.max(1), log);
            }
            let (dxf, dzf) = (dx as f32, dz as f32);
            let a = [ax as f32 + 0.5, 0.5, az as f32 + 0.5];
            if moss {
                // moss on the upper surface of the log
                for z in -12..=12 {
                    for x in -12..=12 {
                        for y in (0..4).rev() {
                            let c = rs.get(x, y, z);
                            if c != 0 {
                                if rs.h(x, y + 99, z) < 0.45 {
                                    rs.solid(x, y + 1, z, M::Moss, false);
                                }
                                break;
                            }
                        }
                    }
                }
            }
            if rng.chance(0.4) {
                // the stump it broke from
                let (x, z) = ((a[0] - dxf * 1.6).floor() as i32, (a[2] - dzf * 1.6).floor() as i32);
                for y in 0..rng.int(1, 2) {
                    rs.wood(x, y, z, log);
                }
            }
            rs
        }
    }
}

/// Sequoia-like giant: round tapering bole with a flared base, bare lower half, columnar crown
/// of short limbs in a golden-angle spiral carrying dense foliage clumps.
fn sequoia(rng: &mut Rng, g: &Genome, var: u64) -> Raster {
    use Material as M;
    let s = g.size;
    let h = 76.0 * s;
    let r0 = rng.range(1.9, 2.5);
    let mut rs = Raster::new(14, -3, (h as i32) + 6, var);
    let spine = Spine::new([0.5, 0.5], h, &Genome { lean: g.lean.min(0.05), bend: g.bend * 0.5, ..*g });
    let flare = rng.range(1.0, 1.6);
    let rf = move |t: f32| r0 * (1.0 - 0.55 * t) + flare * (-t * h / 3.0).exp();
    rs.trunk_round(&spine, -3, h as i32 - 1, &rf, M::OakLog);
    let u0 = rng.range(0.42, 0.52);
    let big_r = rng.range(5.6, 7.2) * s.sqrt();
    let nb = ((h * (1.0 - u0)) / 1.25) as i32;
    let mut balls: Vec<[f32; 5]> = Vec::with_capacity(nb as usize + 16);
    for k in 0..nb {
        let u = u0 + (1.0 - u0) * (k as f32 + rng.range(0.2, 0.8)) / nb as f32;
        let v = (u - u0) / (1.0 - u0);
        // columnar profile: nearly parallel sides, rounded top
        let prof = (1.0 - v.powf(2.6)).max(0.0).sqrt() * (0.82 + 0.18 * (1.0 - v));
        let p = spine.at(u);
        let th = g.phase + k as f32 * GOLDEN;
        let rt = rf(u);
        let len = rt + big_r * prof * rng.range(0.7, 1.0);
        let e = [p[0] + len * th.cos(), p[1] - rng.range(0.0, 0.8), p[2] + len * th.sin()];
        rs.line(p, e, M::OakLog);
        let cr = (1.6 + 1.0 * prof) * rng.range(0.85, 1.1);
        balls.push([e[0], e[1] + 0.3, e[2], cr, cr * 0.72]);
    }
    // inner column of foliage so the crown reads as one dense column
    let mut y = h * u0 + 1.0;
    while y < h - 1.0 {
        let v = (y / h - u0) / (1.0 - u0);
        let prof = (1.0 - v.powf(2.6)).max(0.0).sqrt();
        let p = spine.at(y / h);
        let cr = rf(y / h) + 1.4 + 0.45 * big_r * prof;
        balls.push([p[0], y, p[2], cr, 2.2]);
        y += 2.5;
    }
    let tp = spine.at(1.0);
    balls.push([tp[0], h - 0.5, tp[2], 1.6, 2.4]);
    rs.canopy(&balls, M::SpruceLeaves, 0.4, g.holes);
    // buttress flare roots
    for k in 0..rng.int(5, 8) {
        let th = g.phase + 0.3 + k as f32 * GOLDEN;
        let len = r0 + flare + rng.range(1.5, 3.5);
        let a = [0.5 + r0 * th.cos(), rng.range(1.0, 3.0), 0.5 + r0 * th.sin()];
        let e = [0.5 + len * th.cos(), -0.5, 0.5 + len * th.sin()];
        rs.capsule(a, e, 0.8, 0.5, M::OakLog);
        rs.solid(e[0].floor() as i32, -1, e[2].floor() as i32, M::OakLog, true);
    }
    rs
}

/// Baobab: fat bottle trunk `r(t) = r₀(1 + 0.22 sin πt)`, stubby thick limbs radiating from the
/// flat top, sparse small leaf clusters.
fn baobab(rng: &mut Rng, g: &Genome, var: u64) -> Raster {
    use Material as M;
    let s = g.size;
    let th_h = rng.range(10.0, 13.0) * s;
    let r0 = rng.range(2.3, 3.0) * s;
    let mut rs = Raster::new(14, -2, th_h as i32 + 12, var);
    let spine = Spine::new([0.5, 0.5], th_h, &Genome { lean: g.lean.min(0.06), ..*g });
    let rf = move |t: f32| r0 * (1.0 + 0.22 * (std::f32::consts::PI * t).sin() - 0.12 * t) + 0.5 * (-t * 8.0).exp();
    rs.trunk_round(&spine, -2, th_h.ceil() as i32, &rf, M::AcaciaLog);
    let top = spine.at(1.0);
    let nb = rng.int(5, 8);
    let mut balls: Vec<[f32; 5]> = Vec::with_capacity(16);
    for k in 0..nb {
        let th = g.phase + k as f32 * GOLDEN;
        let el = rng.range(0.25, 0.75);
        let len = rng.range(4.0, 7.0) * s;
        let st = [top[0] + 0.6 * r0 * th.cos(), top[1] - rng.range(0.0, 1.5), top[2] + 0.6 * r0 * th.sin()];
        let e = [st[0] + len * el.cos() * th.cos(), st[1] + len * el.sin(), st[2] + len * el.cos() * th.sin()];
        rs.capsule(st, e, 0.95, 0.5, M::AcaciaLog);
        let cr = rng.range(1.7, 2.5);
        balls.push([e[0], e[1] + 0.6, e[2], cr, cr * 0.6]);
        if rng.chance(0.5) {
            let th2 = th + rng.range(-0.8, 0.8);
            let e2 = [e[0] + 1.8 * th2.cos(), e[1] + 1.2, e[2] + 1.8 * th2.sin()];
            rs.line(e, e2, M::AcaciaLog);
            balls.push([e2[0], e2[1] + 0.5, e2[2], cr * 0.8, cr * 0.5]);
        }
    }
    // low flat cap of foliage over the trunk top
    balls.push([top[0], top[1] + 1.2, top[2], r0 * 0.9, 1.1]);
    rs.canopy(&balls, M::AcaciaLeaves, 0.45, 0.08 + g.holes);
    rs
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
    let mut rs = Raster::new(ri.min(9), -(r as i32) - 3, (r as i32) + 4, var);
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

    fn growth(sp: Species, v: u64) -> Growth {
        Growth { biome: biome_for(sp, v), scale: 1.0, stand: hash_f01(mix64(v ^ 0x57A)) * 2.0 - 1.0, wind: (0.0, 0.0), windy: 0.0 }
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

    /// Rectangle independence including giants, on a large area cut into odd tiles.
    #[test]
    fn roots_rect_independent_with_giants() {
        let g = WorldGen::new(9);
        let (x0, z0, n) = (-700, 1300, 600);
        let mut all = g.features.roots_in(&g, x0, z0, x0 + n, z0 + n);
        let mut parts = Vec::new();
        let cuts = [0, 113, 290, 431, n];
        for i in 0..4 {
            for j in 0..4 {
                parts.extend(g.features.roots_in(&g, x0 + cuts[i], z0 + cuts[j], x0 + cuts[i + 1], z0 + cuts[j + 1]));
            }
        }
        all.sort_by_key(|r| (r.0, r.1, r.3));
        parts.sort_by_key(|r| (r.0, r.1, r.3));
        assert_eq!(all.len(), parts.len());
        assert!(all.iter().zip(&parts).all(|(a, b)| a.0 == b.0 && a.1 == b.1 && a.2 == b.2 && a.3 == b.3));
    }

    /// Hard-core and cross-layer spacing hold exactly, and no feature roots inside a giant's
    /// suppression disc.
    #[test]
    fn spacing_and_giant_suppression() {
        let mut giants_seen = 0;
        for seed in [3u64, 4, 5, 6] {
            let g = WorldGen::new(seed);
            for k in 0..3 {
                let (x0, z0) = ((k * 1777) as i32 - 2600, (seed as i32 * 911) % 3000 - 1500);
                let roots = g.features.roots_in(&g, x0, z0, x0 + 512, z0 + 512);
                let excl = |a: u8, b: u8| -> f32 {
                    match (a.min(b), a.max(b)) {
                        (0, 0) => LAYERS[0].hard_core,
                        (1, 1) => LAYERS[1].hard_core,
                        (2, 2) => LAYERS[2].hard_core,
                        (0, 1) => EXCL_MED_BIG,
                        (0, 2) => EXCL_SMALL_BIG,
                        (1, 2) => EXCL_SMALL_MED,
                        _ => 0.0,
                    }
                };
                for (i, a) in roots.iter().enumerate() {
                    if a.3 >= 3 {
                        giants_seen += 1;
                    }
                    for b in &roots[i + 1..] {
                        let d2 = ((a.0 - b.0) * (a.0 - b.0) + (a.1 - b.1) * (a.1 - b.1)) as f32;
                        let r = excl(a.3, b.3);
                        assert!(d2 >= r * r, "{a:?} and {b:?} closer than {r}");
                        // giants suppress normal features, landmarks suppress emergent giants
                        for (gi, other) in [(a, b), (b, a)] {
                            if gi.3 >= 3 && (other.3 < 3 || (gi.3 == 3 && other.3 == 4)) {
                                let rs = gi.2.suppress_radius();
                                assert!(d2 > rs * rs, "{other:?} inside giant {gi:?}");
                            }
                        }
                    }
                }
            }
        }
        eprintln!("{giants_seen} giants checked");
        assert!(giants_seen > 0);
    }

    /// Landmark giants appear at the intended rarity (≈ one per few hundred metres in forests).
    #[test]
    fn landmark_density() {
        let mut counts = std::collections::BTreeMap::new();
        let mut forest_cells = 0;
        for seed in [1u64, 2] {
            let g = WorldGen::new(seed);
            for gz in -12..12 {
                for gx in -12..12 {
                    let mut memo = Memo::default();
                    let (x, z, _, _) = g.features.giant_cand(LANDMARK, gx, gz);
                    let col = g.column(x as f64 + 0.5, z as f64 + 0.5);
                    if matches!(col.biome, Biome::Forest | Biome::DarkForest | Biome::Taiga | Biome::OldGrowthTaiga | Biome::Jungle) {
                        forest_cells += 1;
                    }
                    if let Some(a) = g.features.accept_giant(&g, LANDMARK, gx, gz, &mut memo) {
                        *counts.entry(a.sp.name()).or_insert(0) += 1;
                    }
                }
            }
        }
        let total: i32 = counts.values().sum();
        eprintln!("landmarks over 2 × 576 cells (256²): {counts:?}, forest cells {forest_cells}");
        assert!(total > 20 && total < 600, "{total}");
    }

    #[test]
    fn shapes_well_formed_and_bounded() {
        for (i, &sp) in ALL_SPECIES.iter().enumerate() {
            let n = if sp.class() == Class::Giant { 12 } else { 40 };
            for v in 0..n as u64 {
                for scale in [1.0f32, 0.55] {
                    let mut gr = growth(sp, v);
                    gr.scale = scale;
                    if v % 3 == 0 {
                        gr.wind = (0.6, -0.8);
                        gr.windy = 1.0;
                    }
                    let f = build_feature_with(sp, 0, 0, mix64(v * 977 + i as u64), &gr);
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
                        assert!(c.y0 < c.y1 && c.y1 <= 110 && c.y0 >= -6, "{} run {c:?}", sp.name());
                        assert!(c.mat != Material::Air);
                        if c.mat.is_leaves() {
                            assert!(!c.replace_solid);
                        }
                    }
                }
            }
        }
    }

    /// Beauty guardrails, checked on rasterised instances of every species:
    /// * no floating leaves: every leaf voxel is 26-connected (through leaves) to wood or to the
    ///   ground layer;
    /// * no trunk through the crown top: the highest voxel of every tree with leaves is a leaf.
    #[test]
    fn no_floating_leaves_no_bare_tops() {
        for (i, &sp) in ALL_SPECIES.iter().enumerate() {
            let n = if sp.class() == Class::Giant { 6 } else { 24 };
            for v in 0..n as u64 {
                let f = build_feature_with(sp, 0, 0, mix64(v * 7919 + i as u64), &growth(sp, v));
                let mut cells: HashMap<(i32, i32, i32), Material> = HashMap::new();
                for c in &f.columns {
                    for y in c.y0..c.y1 {
                        cells.insert((c.dx as i32, y as i32, c.dz as i32), c.mat);
                    }
                }
                let leaves = cells.values().filter(|m| m.is_leaves()).count();
                if leaves == 0 {
                    continue;
                }
                // flood fill from wood and ground-level voxels
                let mut seen: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
                let mut stack: Vec<(i32, i32, i32)> =
                    cells.iter().filter(|(p, m)| !m.is_leaves() || p.1 <= 0).map(|(p, _)| *p).collect();
                for p in &stack {
                    seen.insert(*p);
                }
                while let Some((x, y, z)) = stack.pop() {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for dx in -1..=1 {
                                let q = (x + dx, y + dy, z + dz);
                                if cells.contains_key(&q) && seen.insert(q) {
                                    stack.push(q);
                                }
                            }
                        }
                    }
                }
                let floating = cells.keys().filter(|p| !seen.contains(p)).count();
                assert_eq!(floating, 0, "{} v{v}: {floating} floating voxels", sp.name());
                if matches!(sp.class(), Class::Tree | Class::MegaTree | Class::Giant) {
                    let top = cells.iter().max_by_key(|(p, _)| p.1).unwrap();
                    let ymax = top.0 .1;
                    let wood_top = cells.iter().filter(|(_, m)| m.is_log()).map(|(p, _)| p.1).max().unwrap_or(-99);
                    assert!(wood_top < ymax, "{} v{v}: wood reaches the crown top ({wood_top} vs {ymax})", sp.name());
                }
            }
        }
    }

    /// The genome produces variety (no two trees alike) inside natural ranges.
    #[test]
    fn genome_variety_and_ranges() {
        let tops = |sp: Species, n: u64, biome: Biome| -> Vec<i32> {
            (0..n)
                .map(|v| {
                    let gr = Growth { biome, scale: 1.0, stand: (v as f32 / n as f32) * 2.0 - 1.0, wind: (0.0, 0.0), windy: 0.0 };
                    build_feature_with(sp, 0, 0, mix64(v ^ 0xF00D), &gr).columns.iter().map(|c| c.y1 as i32).max().unwrap()
                })
                .collect()
        };
        let pct = |v: &mut Vec<i32>, p: f64| {
            v.sort();
            v[((v.len() - 1) as f64 * p) as usize]
        };
        let mut sp = tops(Species::Spruce, 200, Biome::Taiga);
        let (lo, hi, mx) = (pct(&mut sp, 0.05), pct(&mut sp, 0.95), *sp.iter().max().unwrap());
        eprintln!("spruce tops p5 {lo} p95 {hi} max {mx}");
        assert!(lo <= 10 && hi >= 20 && mx <= 27, "spruce {lo} {hi} {mx}");
        let mut ms = tops(Species::MegaSpruce, 80, Biome::OldGrowthTaiga);
        let (lo, hi) = (*ms.iter().min().unwrap(), *ms.iter().max().unwrap());
        eprintln!("mega spruce tops {lo}..{hi} (median {})", pct(&mut ms, 0.5));
        assert!(lo >= 29 && hi <= 48 && hi - lo >= 8, "mega spruce {lo}..{hi}");
        let cs = tops(Species::ColossalSpruce, 30, Biome::OldGrowthTaiga);
        let (lo, hi) = (*cs.iter().min().unwrap(), *cs.iter().max().unwrap());
        eprintln!("colossal spruce tops {lo}..{hi}");
        assert!(lo >= 50 && hi <= 72, "colossal {lo}..{hi}");
        let sq = tops(Species::Sequoia, 16, Biome::OldGrowthTaiga);
        let (lo, hi) = (*sq.iter().min().unwrap(), *sq.iter().max().unwrap());
        eprintln!("sequoia tops {lo}..{hi}");
        assert!(lo >= 60 && hi <= 92, "sequoia {lo}..{hi}");
        // no two instances identical
        for &s in &[Species::Oak, Species::Birch, Species::Spruce, Species::Acacia, Species::AncientOak] {
            let mut seen: Vec<Vec<FeatureColumn>> = Vec::new();
            for v in 0..16u64 {
                let f = build_feature_with(s, 0, 0, mix64(v ^ 0xBEEF), &growth(s, v));
                assert!(!seen.contains(&f.columns), "{} duplicate instance", s.name());
                seen.push(f.columns);
            }
        }
        // ancient oak crowns are huge
        let mut radii: Vec<i32> = (0..8u64)
            .map(|v| build_feature_with(Species::AncientOak, 0, 0, mix64(v ^ 0xA0A), &growth(Species::AncientOak, v)).radius())
            .collect();
        radii.sort();
        eprintln!("ancient oak radii {radii:?}");
        assert!(radii[0] >= 14 && radii[7] <= MAX_FEATURE_RADIUS);
    }

    /// Trunk lean stays ≤ 15° and trees near the treeline lean downwind.
    #[test]
    fn lean_bounds_and_wind() {
        let mut down = 0;
        for v in 0..200u64 {
            let gr = Growth { biome: Biome::AlpineGrove, scale: 0.8, stand: 0.0, wind: (1.0, 0.0), windy: 1.0 };
            let mut rng = Rng(mix64(v));
            let g = Genome::draw(&mut rng, Species::Spruce, &gr);
            assert!(g.lean <= 0.262, "lean {}", g.lean);
            if g.lean_dir.0 > 0.5 {
                down += 1;
            }
            let gr2 = Growth::simple(Biome::Forest, 1.0);
            let g2 = Genome::draw(&mut Rng(mix64(v)), Species::Oak, &gr2);
            assert!(g2.lean <= 0.262 && g2.asym <= 0.2 && g2.bend <= 0.06);
        }
        assert!(down > 190, "{down}");
    }

    /// The LOD constants (E[footprint area], E[top height]) match the rasterised genome.
    #[test]
    fn lod_shape_constants() {
        let mut bad = Vec::new();
        for &sp in ALL_SPECIES.iter() {
            let n = if sp.class() == Class::Giant { 16 } else { 120 };
            let (mut area, mut top) = (0.0f32, 0.0f32);
            for v in 0..n {
                let gr = Growth {
                    biome: biome_for(sp, v as u64),
                    scale: 1.0,
                    stand: (v as f32 + 0.5) / n as f32 * 2.0 - 1.0,
                    wind: (0.0, 0.0),
                    windy: 0.0,
                };
                let f = build_feature_with(sp, 0, 0, mix64(v as u64 ^ 0x1234), &gr);
                let mut cells = std::collections::HashSet::new();
                for c in &f.columns {
                    if c.y1 > 0 {
                        cells.insert((c.dx, c.dz));
                    }
                }
                area += cells.len() as f32;
                top += f.columns.iter().map(|c| c.y1).max().unwrap() as f32;
            }
            let (a, h) = sp.lod_shape();
            let am = area / n as f32;
            let hm = top / n as f32;
            eprintln!("{:16} => ({am:.1}, {hm:.1}),   const ({a:.1}, {h:.1})", sp.name());
            if (am - a).abs() >= 0.15 * a + 1.0 || (hm - h).abs() >= 0.12 * h + 0.8 {
                bad.push(sp.name());
            }
        }
        assert!(bad.is_empty(), "LOD shape constants off for {bad:?}");
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
            let want = [Biome::Forest, Biome::DarkForest, Biome::Taiga, Biome::Jungle, Biome::Plains, Biome::Savanna, Biome::BirchForest, Biome::OldGrowthTaiga];
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
            let m = MAX_FEATURE_RADIUS;
            let feats = g.features_in(x - m, z - m, x + n + m, z + n + m);
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
            if checked >= 12 {
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
        // forest-heavy area
        let (fx, fz) = (-8000, 5678);
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for k in 0..16 {
            let f = g.features_in(fx + (k % 4) * 64, fz + (k / 4) * 64, fx + (k % 4) * 64 + 64, fz + (k / 4) * 64 + 64);
            n += f.len();
        }
        let dt = t0.elapsed().as_secs_f64();
        eprintln!("features_in 256×256 at ({fx},{fz}): {:.1} ms, {n} features ({:.2} µs/block²)", dt * 1e3, dt * 1e6 / 65536.0);
    }
}
