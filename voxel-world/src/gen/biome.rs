//! Biome classification + surface rules. OWNER: gen-core.
//!
//! # Snow line (DESIGN §2a: snow is altitude-driven, never a lowland biome)
//! A single snow line `S(x,z) = 330 + 12·n(x,z) − 15·T` (n: 2-octave simplex, λ ≈ 2.6 km,
//! |n| ≤ 1; T: sea-level climate temperature ∈ [−1, 1]) — so S ∈ [303, 357]. Above S, exposed
//! ground whose slope is below `θ(r) = 0.55 + 1.25·smoothstep(0, 90, r)` (r = y − S) is snow:
//! gentle faces first, couloirs and gullies fill in with altitude, cliffs stay bare; from
//! r ≥ 90 (y ≈ 420) permanent snow / packed ice on everything but near-vertical faces.
//! **Below S there is no snow, ice or frozen water at all.**
//!
//! # Classification
//! A decision procedure over (C, E, PV, T, H) plus *derived* column quantities (height, slope,
//! `T_s = T − (H − sea)/250 − 0.12·aspect`, landform region weights, snow line). Altitude zones
//! relative to the snow line come first (forest → alpine meadow → bare rock → snow):
//! * r ≥ 90: `Glacier` (slope < 1.5) / `SnowyPeaks`; 0 ≤ r < 90: `SnowyPeaks` (crests, steep),
//!   `SnowySlopes`, `SnowyPlains` (flat high plateaus); lakes above S: `FrozenRiver` /
//!   `FrozenOcean` (deep), their shores `SnowyBeach`.
//! * −30 ≤ r < 0 (above the treeline): `StonyPeaks` / alpine `Meadow`.
//! * −90 ≤ r < −30 on mountain flanks (sparse stunted conifers): `AlpineGrove`, cold:
//!   `SnowyTaiga` (−60 ≤ r), steep: `StonyPeaks`.
//!
//! Lowland biomes (gameplay pass) are decided per *landform patch* (≈ 520-block warped Voronoi
//! cells, see `terrain::TerrainShaper::patch_mix`) by [`lowland_biome`]: a Whittaker partition of
//! the patch's sea-level climate (jittered per patch → contrasty mosaics along climate
//! transitions; the λ 7 km / 5.6 km temperature / humidity fields keep neighbouring patches
//! climatic neighbours: desert ↔ savanna ↔ jungle, plains ↔ forest ↔ dark forest, taiga ↔
//! old-growth taiga), plus hashed clearings and groves; the cold end is snow-free tundra/taiga
//! (grass, podzol, moss, gravel). Each biome has its own landform styles (dune seas, mesa country,
//! tower karst in jungles, crags in taiga, terraced highlands, buttes in savanna, …).
//!
//! Drylands are ordered by one scalar, the aridity `α = hot·dry` ([`aridity`]): savanna/steppe
//! ring (α > 0.2) → desert (α ≳ 0.36) → red desert → badlands mesa country (α ≳ 0.65, low/mid
//! erosion, λ 2.7 km selector) — badlands always sit inside deserts, never in green forest.
//! Open plains (`Regions::plain`, flat high-erosion lowlands) are grassland/meadow/savanna, with
//! forest only where very wet.
//!
//! # Surface rules (art pass: no per-block dithering anywhere)
//! Every choice is a threshold of *smooth* fields, so materials form coherent patches with clean
//! edges: a two-octave patch field (λ 44 + 13), a soil-depth field (λ 31), the bedding model
//! [`Strata`] for all exposed rock (horizontal, gently folded beds; soft beds = dark shale /
//! red mudstone that the terrain density undercuts into rock shelters), and for snow the
//! band-limited macro surface of the column (see [`SurfaceRules`]'s `snow_cover`). Per-column
//! values are memoised per thread, so a block costs a few hashes after the first one of its
//! column.
use super::climate::{ClimateSampler, FastSimplex};
use super::terrain::{Regions, TerrainShaper};
use super::{Biome, Climate, ColumnInfo, BEDROCK_TOP, SEA_LEVEL};
use crate::materials::Material;
use crate::math::smoothstep;
use crate::noise::{derive_seed, hash3, hash_f01, mix64, salt, Simplex};

const SEA: f32 = SEA_LEVEL as f32;

/// Mean snow line (world y).
pub const SNOW_LINE: f32 = 450.0;
/// Height above the local snow line from which glaciers / permanent ice cover everything.
pub const GLACIER_REL: f32 = 140.0;
/// No snow line is ever below this (450 − 12 − 40 − 15).
pub const SNOW_LINE_MIN: f32 = SNOW_LINE - 67.0;

/// The local snow line `S(x,z) = 450 + 12·n(x,z) − 15·T` (see module docs).
#[derive(Clone, Copy, Debug)]
pub struct SnowLine {
    a: FastSimplex,
    b: FastSimplex,
}

impl SnowLine {
    pub fn new(seed: u64) -> Self {
        let s = derive_seed(seed, salt("biome.snowline"));
        SnowLine { a: FastSimplex::new(s), b: FastSimplex::new(s ^ 0x5A0B) }
    }
    /// `temperature` = sea-level climate temperature (`Climate::temperature`).
    #[inline]
    pub fn at(&self, x: f64, z: f64, temperature: f32) -> f32 {
        const F: f64 = 1.0 / 2600.0;
        let n = (self.a.n2d(x * F, z * F).0 + 0.5 * self.b.n2d(z * 2.13 * F + 17.3, x * 2.13 * F - 4.1).0) * (1.0 / 1.5);
        // alpine pass: a mid-frequency term (λ 420 + 140, ±40) so the snow line, the treeline and
        // the bare-rock zone wander across a face (snow tongues, lower on some spurs, higher on
        // others) instead of ringing every mountain in level contour bands
        let m = 0.7 * self.a.n2d(x * (1.0 / 420.0) + 71.3, z * (1.0 / 420.0) - 9.4).0
            + 0.3 * self.b.n2d(x * (1.0 / 140.0) - 3.3, z * (1.0 / 140.0) + 57.1).0;
        SNOW_LINE + 12.0 * n.clamp(-1.0, 1.0) + 40.0 * m.clamp(-1.0, 1.0) - 15.0 * temperature.clamp(-1.0, 1.0)
    }
}

/// Convenience for other modules (features: treeline at ≈ S − 90 … S − 30):
/// `snow_line(gen.seed, col.x, col.z, col.climate.temperature)`.
pub fn snow_line(seed: u64, x: f64, z: f64, temperature: f32) -> f32 {
    SnowLine::new(seed).at(x, z, temperature)
}

/// Set the voxel footprint (blocks) for the surface rules of this thread: LOD builders pass
/// their voxel size so sub-voxel material patterns are filtered out; 0 restores exact blocks.
pub fn set_material_footprint(fp: f32) {
    MAT_FP.with(|c| c.set(fp.max(0.0)));
}

/// Slope below which snow sticks at height `rel` above the snow line (rise over run).
#[inline]
pub fn snow_slope_limit(rel: f32) -> f32 {
    0.55 + 1.25 * crate::math::smoothstep(0.0, GLACIER_REL, rel)
}

/// Everything `classify` looks at.
#[derive(Clone, Copy, Debug, Default)]
pub struct BiomeInput {
    pub climate: Climate,
    pub height: f32,
    pub slope: f32,
    pub surface_temperature: f32,
    pub regions: Regions,
    /// Distance to the river centre-line (blocks) and channel half-width.
    pub river_dist: f32,
    pub river_width: f32,
    /// Height of the mountain term above the base offset.
    pub mountain: f32,
    /// Local snow line (world y), see [`SnowLine`].
    pub snow_line: f32,
    /// `ColumnInfo::water_level` of the column (`i32::MIN` = dry).
    pub water_level: i32,
}

/// Lowland biome of a landform patch (gameplay pass): a Whittaker partition of the patch's
/// (jittered) sea-level climate, so neighbouring patches are climatic neighbours, plus a hashed
/// `u ∈ [0,1)` that drops clearings (meadows) into forests and groves into grassland.
/// `plain` = open-plains weight at the patch, `swampy`, `mesa` = swamp / mesa-country gates.
pub fn lowland_biome(t: f32, hu: f32, arid: f32, plain: f32, swampy: bool, mesa: bool, u: f32) -> Biome {
    use Biome::*;
    if mesa && arid > 0.6 {
        return Badlands;
    }
    if arid > 0.36 {
        return Desert;
    }
    if arid > 0.18 {
        return if t > 0.25 { Savanna } else { Plains };
    }
    if swampy {
        return Swamp;
    }
    let open = plain > 0.5;
    // weighted pick among the climate-compatible biomes of a zone (u ∈ [0,1))
    let pick = |opts: &[(Biome, f32)]| -> Biome {
        let total: f32 = opts.iter().map(|o| o.1).sum();
        let mut acc = 0.0;
        for &(b, w) in opts {
            acc += w / total;
            if u < acc {
                return b;
            }
        }
        opts[opts.len() - 1].0
    };
    if t < -0.22 {
        // boreal / tundra (snow-free)
        if open {
            return if hu > 0.55 { pick(&[(Taiga, 0.6), (Meadow, 0.25), (Plains, 0.15)]) } else { pick(&[(Plains, 0.55), (Meadow, 0.3), (Taiga, 0.15)]) };
        }
        if hu > 0.45 {
            return pick(&[(OldGrowthTaiga, 0.55), (Taiga, 0.3), (Meadow, 0.15)]);
        }
        if hu > -0.35 {
            return pick(&[(Taiga, 0.6), (Meadow, 0.2), (Plains, 0.1), (OldGrowthTaiga, 0.1)]);
        }
        return pick(&[(Plains, 0.6), (Taiga, 0.25), (Meadow, 0.15)]);
    }
    if t < 0.42 {
        if open {
            if hu > 0.6 {
                return pick(&[(Forest, 0.45), (Meadow, 0.35), (BirchForest, 0.2)]);
            }
            if hu > 0.1 {
                return pick(&[(Meadow, 0.4), (Plains, 0.3), (Forest, 0.18), (BirchForest, 0.12)]);
            }
            return pick(&[(Plains, 0.7), (Meadow, 0.18), (BirchForest, 0.12)]);
        }
        if hu > 0.5 {
            return pick(&[(DarkForest, 0.6), (Forest, 0.25), (Meadow, 0.15)]);
        }
        if hu > 0.05 {
            return pick(&[(Forest, 0.4), (BirchForest, 0.3), (Meadow, 0.18), (DarkForest, 0.12)]);
        }
        if hu > -0.35 {
            return pick(&[(Plains, 0.4), (Forest, 0.3), (Meadow, 0.2), (BirchForest, 0.1)]);
        }
        return if t > 0.25 { pick(&[(Savanna, 0.7), (Plains, 0.3)]) } else { pick(&[(Plains, 0.75), (Meadow, 0.25)]) };
    }
    // hot
    if open {
        return if hu > 0.55 {
            pick(&[(Jungle, 0.7), (Forest, 0.15), (Meadow, 0.15)])
        } else if hu > -0.15 {
            pick(&[(Plains, 0.45), (Savanna, 0.3), (Forest, 0.25)])
        } else {
            pick(&[(Savanna, 0.8), (Plains, 0.2)])
        };
    }
    if hu > 0.35 {
        return pick(&[(Jungle, 0.75), (Forest, 0.15), (Meadow, 0.1)]);
    }
    if hu > 0.1 {
        return pick(&[(Forest, 0.45), (Savanna, 0.35), (Jungle, 0.2)]);
    }
    pick(&[(Savanna, 0.75), (Plains, 0.25)])
}

/// Retained for compatibility: surface temperature of the *old* isotherm snow line. Snow is now
/// altitude-driven ([`SnowLine`]); this constant only separates cold from temperate lowlands.
pub const SNOW_T: f32 = -0.5;

/// Biome decision procedure (see module docs).
pub fn classify(b: &BiomeInput) -> Biome {
    use Biome::*;
    let c = &b.climate;
    let h = b.height;
    let st = b.surface_temperature;
    let alt = h - SEA;
    let r = &b.regions;
    let rel = h - b.snow_line;

    if r.volcanic > 0.5 && h >= SEA - 1.0 {
        return Volcanic;
    }

    // ---------------- water-covered
    let wet = b.water_level != i32::MIN && h < b.water_level as f32 - 0.5;
    if wet && b.water_level > SEA_LEVEL {
        if r.lake <= 0.0 {
            return Volcanic; // caldera lava lake outside the volcanic weight (defensive)
        }
        if b.water_level as f32 >= b.snow_line {
            // frozen tarn: deep ones keep the "frozen ocean" ice sheet
            return if b.water_level as f32 - h > 12.0 { FrozenOcean } else { FrozenRiver };
        }
        return if r.swamp > 0.5 { Swamp } else { River };
    }
    if wet {
        if r.channel > 0.35 && c.continentalness > -0.17 {
            return River;
        }
        if r.swamp > 0.5 {
            return Swamp;
        }
        if c.continentalness > -0.12 && h > SEA - 5.0 && b.river_dist < b.river_width * 2.5 + 6.0 {
            // shallow inland water (rivers' flanks, lagoons)
            return River;
        }
        if c.continentalness > -0.06 && h > SEA - 14.0 {
            // inland sea-level ponds in lowland hollows: fresh water, not ocean
            return River;
        }
        if h < 92.0 {
            return DeepOcean;
        }
        if c.temperature > 0.48 {
            return WarmOcean;
        }
        return Ocean;
    }

    // ---------------- lake shores
    if r.lake > 0.5 && h < r.lake_level + 2.5 && b.slope < 0.6 {
        return if rel >= 0.0 { SnowyBeach } else { Beach };
    }

    // ---------------- sea shores (all far below the snow line)
    let coastal = c.continentalness < -0.04 && alt < 4.5;
    if coastal && r.channel < 0.5 {
        if b.slope > 0.85 || r.cliff > 0.55 {
            return StonyShore;
        }
        if r.swamp > 0.5 {
            return Swamp;
        }
        return Beach;
    }
    if r.channel > 0.5 && alt < 2.0 {
        return River;
    }

    // ---------------- altitude zones relative to the snow line
    let peaky = (b.mountain > 120.0 && c.peaks_valleys > 0.25) || b.slope > 1.0;
    if rel >= GLACIER_REL {
        return if b.slope < 1.5 { Glacier } else { SnowyPeaks };
    }
    if rel >= 0.0 {
        if peaky || b.slope > 0.9 {
            return SnowyPeaks;
        }
        return if b.slope > 0.4 { SnowySlopes } else { SnowyPlains };
    }

    // ---------------- dry-land landforms with dedicated terrain
    if r.hoodoo > 0.5 && r.badlands > 0.5 {
        return Hoodoos;
    }
    if r.badlands > 0.5 {
        return Badlands;
    }

    let mountainous = b.mountain > 40.0 || alt > 150.0;
    if rel >= -30.0 && mountainous {
        // above the treeline: bare rock or alpine meadow
        return if b.slope > 0.75 || peaky { StonyPeaks } else { Meadow };
    }
    if rel >= -90.0 && mountainous {
        // treeline zone: sparse stunted conifers
        if b.slope > 1.1 {
            return StonyPeaks;
        }
        if c.temperature < -0.3 && rel >= -60.0 {
            return SnowyTaiga;
        }
        return AlpineGrove;
    }
    if alt > 85.0 && st > -0.12 && st < 0.32 && b.slope < 0.55 && c.humidity > -0.1 && b.mountain > 35.0 {
        return Meadow;
    }

    if r.desert > 0.5 {
        return Desert;
    }
    if r.swamp > 0.5 && alt < 7.0 {
        return Swamp;
    }
    // lowlands: the nearest landform patch decides (contrasty 300–1500 block patches, see
    // `lowland_biome`); its dryland/swamp labels need their landform weight to hold
    match r.patch {
        Desert | Badlands | Hoodoos => {
            if st > 0.25 {
                Savanna
            } else {
                Plains
            }
        }
        Swamp => {
            if alt < 9.0 {
                Swamp
            } else {
                Forest
            }
        }
        p => p,
    }
}

// =====================================================================================
// Geology: sedimentary bedding (shared with the terrain density)
// =====================================================================================

/// Thickness (blocks) of one lithology unit of the bedding.
pub const BED_UNIT: f32 = 4.0;
/// Thickness of one erosion unit: soft units weather back into notches (rock shelters).
pub const NOTCH_UNIT: f32 = 8.0;
/// Probability that an erosion unit is soft (recessive).
const SOFT_P: f32 = 0.2;

/// Aridity `α = smoothstep(0.2, 0.5, T)·smoothstep(−0.12, −0.7, H)` ∈ [0, 1] of the sea-level
/// climate: one scalar orders the drylands (savanna/steppe ring → desert → red desert → badlands),
/// so badlands are always nested inside deserts and never touch humid forest.
#[inline]
pub fn aridity(temperature: f32, humidity: f32) -> f32 {
    smoothstep(0.20, 0.50, temperature) * smoothstep(-0.12, -0.70, humidity)
}

/// Layered rock. Bedding coordinate `ℓ(x, y, z) = y + D(x, z)` with
/// `D = 34·n(λ 3.2 km) + 3.5·n(λ 260)` (broad folds → dips ≤ ≈ 4 %, gentle undulation), so beds
/// are nearly horizontal, laterally continuous surfaces. Along ℓ:
/// * lithology units of [`BED_UNIT`] blocks pick Stone / Andesite (/ Sandstone in arid climates)
///   from a hash of the unit index — runs of equal units give geometric bed thicknesses; the
///   thresholds shift with a lateral facies field (λ 900), so beds pinch out over hundreds of
///   blocks (large, low-contrast patches instead of blobs);
/// * erosion units of [`NOTCH_UNIT`] blocks are soft with probability 0.22: their recess profile
///   `16 f²(1 − f)²` drives the terrain density (soft beds weather back under the hard bed above
///   → rock shelters, ledges; thin fins holed from both sides → small arches) and their core is
///   dark shale (Deepslate) / red mudstone (RedSandstone in arid climates).
/// Granite appears only in deep plutons (cut-away views) — never as spots on surface rock.
#[derive(Clone, Copy, Debug)]
pub struct Strata {
    seed: u64,
    fold: FastSimplex,
    warp: FastSimplex,
    facies: FastSimplex,
}

impl Strata {
    pub fn new(seed: u64) -> Self {
        let s = derive_seed(seed, salt("geology.strata"));
        Strata {
            seed: s,
            fold: FastSimplex::new(s ^ 1),
            warp: FastSimplex::new(s ^ 2),
            facies: FastSimplex::new(s ^ 3),
        }
    }

    /// Bedding offset `D(x, z)` (ℓ = y + D).
    #[inline]
    pub fn offset(&self, x: f64, z: f64) -> f32 {
        const F1: f64 = 1.0 / 3200.0;
        const F2: f64 = 1.0 / 260.0;
        34.0 * self.fold.n2d(x * F1, z * F1).0 + 3.5 * self.warp.n2d(x * F2, z * F2).0
    }

    /// [`Strata::offset`] with its horizontal gradient (terrace treads follow the bedding).
    #[inline]
    pub fn offset_d(&self, x: f64, z: f64) -> super::climate::D2 {
        const F1: f64 = 1.0 / 3200.0;
        const F2: f64 = 1.0 / 260.0;
        let (a, ax, az) = self.fold.n2d(x * F1, z * F1);
        let (b, bx, bz) = self.warp.n2d(x * F2, z * F2);
        super::climate::D2::new(
            34.0 * a + 3.5 * b,
            34.0 * ax * F1 as f32 + 3.5 * bx * F2 as f32,
            34.0 * az * F1 as f32 + 3.5 * bz * F2 as f32,
        )
    }

    #[inline]
    fn unit_hash(&self, k: i64, salt: u64) -> f32 {
        hash_f01(mix64(self.seed ^ salt ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)))
    }

    /// Recess strength ∈ [0, 1] at bedding coordinate ℓ: `16 f²(1 − f)²` across a soft erosion
    /// unit (f = position inside the unit), 0 in hard units. C¹ in ℓ. A unit is soft where its
    /// hash is below `SOFT_P·(1 + 0.9·facies)`, so soft beds pinch out laterally (the facies field
    /// varies over ≈ 900 blocks) instead of striping every face of a range identically.
    #[inline]
    pub fn recess(&self, ell: f32, facies: f32) -> f32 {
        let u = ell * (1.0 / NOTCH_UNIT);
        let k = u.floor();
        if self.unit_hash(k as i64, 0x50F7) >= SOFT_P * (1.0 + 0.9 * facies) {
            return 0.0;
        }
        let f = u - k;
        let b = f * (1.0 - f);
        16.0 * b * b
    }

    /// Lateral facies field ≈ [−1, 1] (λ 900).
    #[inline]
    pub fn facies(&self, x: f64, z: f64) -> f32 {
        const F: f64 = 1.0 / 900.0;
        self.facies.n2d(x * F, z * F).0
    }

    /// Lithology at bedding coordinate ℓ given the facies value and the aridity.
    #[inline]
    pub fn lithology(&self, ell: f32, facies: f32, arid: f32) -> Material {
        // dark shale / red mudstone only in the ≈ 3-block core of a soft bed (thin lines)
        if self.recess(ell, facies) > 0.62 {
            return if arid > 0.45 { Material::RedSandstone } else { Material::Deepslate };
        }
        let k = (ell * (1.0 / BED_UNIT)).floor() as i64;
        let u = self.unit_hash(k, 0x11B0);
        let sand = 0.62 * smoothstep(0.15, 0.6, arid);
        if u < sand {
            return Material::Sandstone;
        }
        let andesite = 0.30 + 0.16 * facies;
        if u < sand + (1.0 - sand) * andesite {
            Material::Andesite
        } else {
            Material::Stone
        }
    }
}

// =====================================================================================
// Surface rules
// =====================================================================================

/// Footprint of the band-limited "macro" surface used by the snow rules (wavelengths below
/// ≈ 2·48 blocks removed: gullies, crags, roughness).
const MACRO_FP: f64 = 48.0;

/// Per-column geology/patch values (pure functions of (seed, x, z)), memoised per thread: the
/// world builder asks for every block of a column in a row.
#[derive(Clone, Copy)]
struct ColGeo {
    seed: u64,
    x: i32,
    z: i32,
    valid: bool,
    offset: f32,
    facies: f32,
    patch: f32,
    soil: f32,
    fp: f32,
}

thread_local! {
    static GEO: std::cell::Cell<ColGeo> = const { std::cell::Cell::new(ColGeo {
        seed: 0, x: 0, z: 0, valid: false, offset: 0.0, facies: 0.0, patch: 0.0, soil: 0.0, fp: 0.0,
    }) };
    /// Footprint (blocks) of the voxels the caller is building (LOD); 0 = exact blocks.
    static MAT_FP: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    /// (seed, x bits, z bits) → (macro slope, height − macro height) of the last column asked.
    static MACRO: std::cell::Cell<(u64, u64, u64, f32, f32)> = const { std::cell::Cell::new((u64::MAX, 0, 0, 0.0, 0.0)) };
}

pub struct SurfaceRules {
    pub seed: u64,
    patch_a: FastSimplex,
    patch_b: FastSimplex,
    lobe: FastSimplex,
    soil: FastSimplex,
    strata: Strata,
    pluton: Simplex,
    snow: SnowLine,
    climate: ClimateSampler,
    terrain: TerrainShaper,
}

#[inline]
fn bit(h: u64, shift: u32) -> f32 {
    ((h >> shift) & 0xFFFF) as f32 * (1.0 / 65536.0)
}

impl SurfaceRules {
    pub fn new(seed: u64) -> Self {
        let s = derive_seed(seed, salt("surface.patch"));
        SurfaceRules {
            seed,
            patch_a: FastSimplex::new(s),
            patch_b: FastSimplex::new(s ^ 0x77),
            lobe: FastSimplex::new(s ^ 0x10BE),
            soil: FastSimplex::new(s ^ 0x5011),
            strata: Strata::new(seed),
            pluton: Simplex::new(derive_seed(seed, salt("surface.strata"))),
            snow: SnowLine::new(seed),
            climate: ClimateSampler::new(seed),
            terrain: TerrainShaper::new(seed),
        }
    }

    /// The local snow line of a column (world y).
    #[inline]
    pub fn snow_line(&self, col: &ColumnInfo) -> f32 {
        self.snow.at(col.x, col.z, col.climate.temperature)
    }

    /// The bedding model (shared with the terrain density).
    pub fn strata(&self) -> &Strata {
        &self.strata
    }

    /// Per-column values: bedding offset, facies, patch field, soil field.
    #[inline]
    fn geo(&self, x: i32, z: i32) -> ColGeo {
        let g = GEO.with(|c| c.get());
        let fp = MAT_FP.with(|c| c.get());
        if g.valid && g.x == x && g.z == z && g.seed == self.seed && g.fp == fp {
            return g;
        }
        let (xf, zf) = (x as f64 + 0.5, z as f64 + 0.5);
        // LOD: octaves finer than the voxel footprint fade out (band-limited materials — one
        // point sample per big voxel of a λ 13 / 44 field is white noise: salt-and-pepper speckle
        // that flickers whenever the level changes)
        let fade = |l: f32| 1.0 - crate::math::smoothstep(0.2 * l, 0.5 * l, fp);
        let (fa, fb, fs) = if fp > 0.0 { (fade(44.0), fade(13.0), fade(31.0)) } else { (1.0, 1.0, 1.0) };
        // patch field: two octaves (λ 44 + λ 13) → coherent patches with fringed edges
        let p = (if fa > 0.0 { 0.8 * fa * self.patch_a.n2d(xf * (1.0 / 44.0), zf * (1.0 / 44.0)).0 } else { 0.0 }
            + if fb > 0.0 { 0.45 * fb * self.patch_b.n2d(xf * (1.0 / 13.0), zf * (1.0 / 13.0)).0 } else { 0.0 })
            * (1.0 / 0.92);
        let g = ColGeo {
            seed: self.seed,
            x,
            z,
            valid: true,
            offset: self.strata.offset(xf, zf),
            facies: self.strata.facies(xf, zf),
            patch: p,
            soil: if fs > 0.0 { fs * self.soil.n2d(xf * (1.0 / 31.0), zf * (1.0 / 31.0)).0 } else { 0.0 },
            fp,
        };
        GEO.with(|c| c.set(g));
        g
    }

    /// Band-limited ("macro") slope of the column and its height above the macro surface
    /// (negative in gullies / couloirs, positive on spurs and crags). One extra band-limited
    /// shape evaluation per column, memoised.
    fn macro_relief(&self, col: &ColumnInfo) -> (f32, f32) {
        let (kx, kz) = (col.x.to_bits(), col.z.to_bits());
        let m = MACRO.with(|c| c.get());
        if m.0 == self.seed && m.1 == kx && m.2 == kz {
            return (m.3, m.4);
        }
        let cd = self.climate.sample_d(col.x, col.z, MACRO_FP);
        let sh = self.terrain.shape(&cd, col.x, col.z, MACRO_FP);
        let r = (sh.height.grad_len(), col.height - sh.height.v);
        MACRO.with(|c| c.set((self.seed, kx, kz, r.0, r.1)));
        r
    }

    /// Snow / glacier ice on an exposed top block — smooth fields only (no per-block noise):
    /// * the edge lies `0…14·λ(x,z)` blocks above the snow line S (λ: two-octave lobe field,
    ///   λ 150 + 47) → a clean, wavy edge, and never any snow below S (DESIGN §2a);
    /// * fields: the band-limited macro slope must stay below `θ(r) + 0.75·hollow`, θ the
    ///   altitude-dependent limit and `hollow = smoothstep(−0.5, −4, H − H_macro)` — snow streaks
    ///   down couloirs and gullies of faces that are otherwise too steep;
    /// * crags and spurs: the local slope must stay below `θ + 0.9 + 0.8·hollow`.
    /// The top is always white snow; glacier ice shows only below it (step sides, ice cliffs).
    fn snow_cover(&self, x: i32, y: i32, z: i32, col: &ColumnInfo) -> Option<Material> {
        if (y as f32) < SNOW_LINE_MIN - 1.0 {
            return None;
        }
        let (xf, zf) = (x as f64 + 0.5, z as f64 + 0.5);
        let lobe = 0.5
            + 0.5 * (0.7 * self.lobe.n2d(xf * (1.0 / 150.0), zf * (1.0 / 150.0)).0
                + 0.3 * self.lobe.n2d(zf * (1.0 / 47.0) + 31.7, xf * (1.0 / 47.0) - 11.3).0);
        let rel = y as f32 + 0.5 - self.snow_line(col) - 14.0 * lobe.clamp(0.0, 1.0);
        if rel < 0.0 {
            return None;
        }
        // Far-LOD samples (the heightfield builder samples ≥ 1 block away from the centre of a
        // band-limited cell column): the column itself is already the macro surface.
        let far = (xf - col.x).abs() > 0.75 || (zf - col.z).abs() > 0.75;
        let (sm, dev) = if far { (col.slope, 0.0) } else { self.macro_relief(col) };
        let hollow = smoothstep(-0.5, -4.0, dev);
        let limit = snow_slope_limit(rel) + 0.75 * hollow;
        if sm >= limit || col.slope > limit + 0.9 + 0.8 * hollow {
            return None;
        }
        Some(Material::Snow)
    }

    /// Rock below the soil: bedrock floor, deepslate under bedding level 60 (coherent: follows
    /// the beds), deep granite plutons, otherwise the bedded lithology.
    #[inline]
    fn rock(&self, x: i32, y: i32, z: i32, col: &ColumnInfo) -> Material {
        if y < BEDROCK_TOP {
            let p = (BEDROCK_TOP - y) as f32 / (BEDROCK_TOP as f32 + 1.0);
            if y == 0 || bit(hash3(self.seed, x, y, z), 0) < p {
                return Material::Bedrock;
            }
        }
        let g = self.geo(x, z);
        let ell = y as f32 + 0.5 + g.offset;
        if y < 120 {
            let v = self.pluton.n3(x as f64 * (1.0 / 110.0), y as f64 * (1.0 / 80.0), z as f64 * (1.0 / 110.0));
            if v > 0.55 - 0.004 * (120 - y).min(60) as f32 {
                return Material::Granite;
            }
        }
        if ell < 60.0 {
            return Material::Deepslate;
        }
        self.strata.lithology(ell, g.facies, aridity(col.climate.temperature, col.climate.humidity))
    }

    pub fn material(&self, x: i32, y: i32, z: i32, depth: i32, col: &ColumnInfo) -> Material {
        use Material::*;
        let below_surface = col.height - y as f32;
        let b = col.biome;
        // Deep blocks, cave floors/walls far below the 2D surface band → rock. Badlands and
        // hoodoo columns expose their whole side down to the neighbour (mesa cliffs, 36-block
        // spires), so their banded terracotta must reach much deeper below the column top.
        let cut = match b {
            Biome::Badlands | Biome::Hoodoos => 64.0,
            Biome::Volcanic => 28.0,
            _ => col.jaggedness + 12.0,
        };
        if depth > 48 || below_surface > cut || y < BEDROCK_TOP {
            return self.rock(x, y, z, col);
        }
        let st = col.surface_temperature;
        let slope = col.slope;
        let underwater = y < col.water_level && b != Biome::Volcanic;
        let g = self.geo(x, z);
        let p = g.patch;
        // soil thickness 2–5 from a smooth field (no per-column dithering on hillside steps)
        let dirt_depth = 3 + ((g.soil + 0.75) * 1.6).floor().clamp(-1.0, 2.0) as i32;

        // ---------- snow: altitude-driven only (nothing below the local snow line), and only on
        // the open surface — not on cave floors or overhang ledges deep below the column top
        if depth == 0 && !underwater && below_surface <= col.jaggedness + 1.5 {
            if let Some(m) = self.snow_cover(x, y, z, col) {
                return m;
            }
        }

        // ---------- badlands / hoodoos: red sand flats with pale drifts, banded terracotta with
        // pale sandstone beds (caprock ledges) — fewer, larger orange masses
        if matches!(b, Biome::Badlands | Biome::Hoodoos) {
            if depth == 0 && slope < 0.75 && !(b == Biome::Hoodoos && below_surface < -3.0) {
                return if p > 0.25 {
                    Sand
                } else if p < -0.62 {
                    Terracotta
                } else {
                    RedSand
                };
            }
            if depth < 2 && slope < 0.5 {
                return RedSand;
            }
            if depth < 56 {
                let ell = y as f32 + 0.5 + 0.35 * g.offset;
                let k = (ell * (1.0 / 6.0)).floor() as i64;
                if hash_f01(mix64(self.seed ^ 0xCA9 ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))) < 0.26 {
                    return Sandstone;
                }
                return Terracotta;
            }
            return self.rock(x, y, z, col);
        }
        // ---------- volcanic: basalt with radial obsidian flows, ash (gravel)
        if b == Biome::Volcanic {
            if depth < 6 {
                if p > 0.45 && depth < 3 {
                    return Obsidian;
                }
                if p < -0.55 && depth == 0 {
                    return Gravel;
                }
                if col.water_level > SEA_LEVEL && y >= col.water_level - 3 && depth < 3 {
                    return Obsidian; // caldera rim around the lava lake
                }
                return Basalt;
            }
            return if depth < 20 { Basalt } else { self.rock(x, y, z, col) };
        }

        // ---------- underwater floors (sea, rivers, lakes)
        if underwater {
            let wd = col.water_level - y; // water depth above this block
            if depth >= 4 {
                return match b {
                    Biome::Beach | Biome::WarmOcean if depth < 7 => Sandstone,
                    _ => self.rock(x, y, z, col),
                };
            }
            return match b {
                Biome::Swamp => {
                    if p > 0.2 {
                        Clay
                    } else {
                        Mud
                    }
                }
                Biome::River | Biome::FrozenRiver | Biome::FrozenOcean => {
                    if p > 0.45 {
                        Clay
                    } else if p < -0.35 {
                        Gravel
                    } else if col.water_level > SEA_LEVEL && p < 0.1 {
                        Mud // still lake water: silt
                    } else {
                        Sand
                    }
                }
                Biome::DeepOcean => {
                    if p > 0.3 {
                        Clay
                    } else if p < -0.2 {
                        Sand
                    } else {
                        Gravel
                    }
                }
                _ => {
                    if wd > 14 {
                        if p > 0.35 {
                            Gravel
                        } else if p < -0.45 {
                            Clay
                        } else {
                            Sand
                        }
                    } else if p > 0.62 {
                        Gravel
                    } else {
                        Sand
                    }
                }
            };
        }

        // ---------- steep rock faces (any biome without its own rock); coherent threshold
        let steep = slope > 1.2 + 0.2 * p;
        // talus / scree: moderately steep rocky ground (the foot of cliffs, alpine slopes)
        let rocky = col.height > SEA + 70.0 || col.jaggedness > 4.0;
        let scree = rocky && slope > 0.8 && p > -0.15;

        match b {
            Biome::Beach | Biome::Desert | Biome::SnowyBeach => {
                let sand = if b == Biome::Desert { 5 + ((g.soil + 1.0) * 1.5) as i32 } else { 3 + (g.soil > 0.0) as i32 };
                if depth < sand && !(steep && b == Biome::Desert && depth > 0) {
                    if depth == 0 && b == Biome::Desert {
                        // red-desert ecotone toward the badlands: red sand patches grow with aridity
                        let red = 0.6 * smoothstep(0.62, 0.78, aridity(col.climate.temperature, col.climate.humidity));
                        if red > 0.0 && 0.5 + 0.5 * p < red {
                            return RedSand;
                        }
                    }
                    return Sand;
                }
                if depth < sand + 8 {
                    return Sandstone;
                }
                return self.rock(x, y, z, col);
            }
            Biome::StonyShore => {
                if depth < 2 && p > 0.3 {
                    return Gravel;
                }
                return self.rock(x, y, z, col);
            }
            Biome::Glacier => {
                // firn over glacier ice under the (snow-line-driven) surface
                if depth == 0 {
                    return if steep { self.rock(x, y, z, col) } else { Snow };
                }
                if depth < 9 + dirt_depth && !steep {
                    // snow on the sides of ledges, glacier ice only in deeper ice cliffs
                    return if depth < 6 { Snow } else { PackedIce };
                }
                return self.rock(x, y, z, col);
            }
            Biome::SnowyPeaks | Biome::SnowySlopes | Biome::SnowyPlains => {
                // the top block was handled by `snow_cover`; a snowpack lies below it where
                // the surface is snowy, otherwise bare rock / scree / alpine turf
                if depth > 0 && depth < 3 && self.snow_cover(x, y + depth, z, col).is_some() {
                    return Snow;
                }
                if b == Biome::SnowyPlains && !steep && slope < 0.5 && depth < dirt_depth {
                    return if depth == 0 { if p > 0.3 { Gravel } else { Grass } } else { Dirt };
                }
                if depth < 2 && slope < 1.2 && p > -0.1 {
                    return Gravel;
                }
                return self.rock(x, y, z, col);
            }
            Biome::StonyPeaks => {
                if depth < 2 && slope < 1.15 && p > -0.2 {
                    return Gravel;
                }
                return self.rock(x, y, z, col);
            }
            _ => {}
        }

        // ---------- vegetated / soil biomes
        if steep {
            return self.rock(x, y, z, col);
        }
        if depth >= dirt_depth {
            return self.rock(x, y, z, col);
        }
        if depth > 0 {
            return match b {
                Biome::Swamp if depth < 2 => Mud,
                Biome::River | Biome::FrozenRiver => Sand,
                _ if scree && depth < 2 => Gravel,
                _ => Dirt,
            };
        }
        // depth == 0: the exposed top block (never snow below the snow line)
        if scree {
            return Gravel;
        }
        match b {
            Biome::River | Biome::FrozenRiver => {
                if p > 0.3 {
                    Gravel
                } else {
                    Sand
                }
            }
            Biome::Taiga | Biome::SnowyTaiga | Biome::AlpineGrove => {
                if p > 0.62 {
                    Podzol
                } else if p < -0.6 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::OldGrowthTaiga => {
                if p > 0.2 {
                    Podzol
                } else if p < -0.6 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::DarkForest => {
                if p > 0.55 {
                    Podzol
                } else if p < -0.55 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::Jungle => {
                if p > 0.45 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::Swamp => {
                if p > 0.35 {
                    Mud
                } else if p < -0.6 {
                    Clay
                } else {
                    Grass
                }
            }
            Biome::Savanna => {
                if p > 0.62 {
                    Dirt
                } else {
                    Grass
                }
            }
            Biome::Plains | Biome::Meadow if st < -0.35 => {
                // tundra: grass with gravel, moss and podzol patches — but no snow
                if p > 0.6 {
                    Gravel
                } else if p < -0.55 {
                    Moss
                } else if p > 0.35 {
                    Podzol
                } else {
                    Grass
                }
            }
            Biome::Meadow if slope > 0.6 && p > 0.45 => Gravel,
            _ => Grass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::WorldGen;

    #[test]
    fn surface_rules_basic() {
        let g = WorldGen::new(1);
        let col = g.column(100.5, 200.5);
        assert_eq!(g.surface_material(0, 0, 0, 100, &col), Material::Bedrock);
        let m = g.surface_material(0, 20, 0, 100, &col);
        assert!(matches!(m, Material::Deepslate | Material::Granite));
        let m = g.surface_material(0, 100, 0, 60, &col);
        assert!(matches!(
            m,
            Material::Stone | Material::Granite | Material::Andesite | Material::Deepslate | Material::Sandstone | Material::RedSandstone
        ));
    }

    /// Mesa cliffs and hoodoo spires expose a column's side down to its neighbour's top; those
    /// blocks must be banded terracotta / red sand (or the pale sandstone caprock beds), never grey stone (previously the generic
    /// "deeper than jaggedness + 10 → rock" cut-off made the lower part of every spire stone).
    #[test]
    fn badlands_faces_are_terracotta() {
        let g = WorldGen::new(2026);
        let (mut faces, mut stone) = (0usize, 0usize);
        let mut cols = 0;
        'outer: for j in 0..700 {
            for i in 0..700 {
                let x = -20000.0 + i as f64 * 57.1 + 0.5;
                let z = -20000.0 + j as f64 * 57.1 + 0.5;
                let c = g.column(x, z);
                if !matches!(c.biome, Biome::Badlands | Biome::Hoodoos) {
                    continue;
                }
                cols += 1;
                for (ox, oz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                    let n = g.column(x + ox, z + oz);
                    let top = c.height.floor() as i32;
                    for y in (n.height.floor() as i32 + 1)..=top {
                        let m = g.surface_material(x.floor() as i32, y, z.floor() as i32, top - y, &c);
                        faces += 1;
                        if !matches!(m, Material::Terracotta | Material::RedSand | Material::Sand | Material::Sandstone | Material::Snow) {
                            stone += 1;
                        }
                    }
                }
                if cols > 4000 {
                    break 'outer;
                }
            }
        }
        eprintln!("{cols} badlands columns, {faces} exposed side blocks, {stone} non-terracotta");
        assert!(cols > 500 && faces > 1000);
        assert!(stone * 1000 < faces, "{stone}/{faces} exposed badlands blocks are not terracotta");
    }

    #[test]
    fn surface_rules_cost() {
        let g = WorldGen::new(1);
        let cols: Vec<ColumnInfo> = (0..64).map(|i| g.column(i as f64 * 97.5, i as f64 * -53.5)).collect();
        let t0 = std::time::Instant::now();
        let mut acc = 0u64;
        let n = 2_000_000;
        for i in 0..n {
            let c = &cols[i % 64];
            let y = c.height as i32 - (i % 30) as i32;
            acc += g.surface_material(i as i32, y, (i / 7) as i32, (i % 30) as i32, c) as u64;
        }
        let ns = t0.elapsed().as_secs_f64() * 1e9 / n as f64;
        eprintln!("surface_material: {ns:.1} ns/block (checksum {acc})");
    }
}
