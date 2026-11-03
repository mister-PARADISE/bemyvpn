//! World generator: pure, deterministic functions of (seed, x, y, z).
//!
//! FROZEN shared file — public types and `WorldGen` delegation below are the contract between
//! modules. Implementation lives in:
//! * `climate.rs`  — `ClimateSampler`            (agent: gen-core)
//! * `terrain.rs`  — `TerrainShaper`             (agent: gen-core)
//! * `biome.rs`    — `classify`, `SurfaceRules`  (agent: gen-core)
//! * `caves.rs`    — `CaveCarver`                (agent: caves+features)
//! * `features.rs` — `FeaturePlacer`             (agent: caves+features)
//!
//! Coordinates: 1 unit = 1 block = 1 metre. y is up. World spans y ∈ [0, WORLD_HEIGHT).
//! The block at integer (x, y, z) occupies [x, x+1) × [y, y+1) × [z, z+1).

pub mod biome;
pub mod caves;
pub mod climate;
pub mod features;
pub mod terrain;

use crate::materials::Material;

pub use biome::SurfaceRules;
pub use caves::CaveCarver;
pub use climate::ClimateSampler;
pub use features::FeaturePlacer;
pub use terrain::TerrainShaper;

/// World height in blocks (exclusive upper bound for y).
pub const WORLD_HEIGHT: i32 = 512;
/// Water surface of oceans/rivers: water occupies y < SEA_LEVEL (the block y = SEA_LEVEL-1 is the top water block).
pub const SEA_LEVEL: i32 = 140;
/// Bottom layers y < BEDROCK_TOP are bedrock-ish floor.
pub const BEDROCK_TOP: i32 = 3;

/// Large-scale climate parameters at a horizontal position. All roughly in [-1, 1].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Climate {
    /// Ocean ↔ inland. < -0.45 deep ocean, < -0.18 ocean, ≈ -0.18..-0.05 coast, > 0 inland.
    pub continentalness: f32,
    /// -1 = rugged/mountainous … +1 = flat/eroded.
    pub erosion: f32,
    /// Raw weirdness noise (variant selector).
    pub weirdness: f32,
    /// Folded weirdness: +1 peaks, -1 valleys. pv = 1 - |3|w| - 2|.
    pub peaks_valleys: f32,
    /// Sea-level temperature: -1 frozen … +1 hot.
    pub temperature: f32,
    /// -1 arid … +1 wet.
    pub humidity: f32,
    /// River channel strength: 1 at river centre-line, falling to 0 outside the valley.
    pub river: f32,
}

/// Biomes. `repr(u8)` with contiguous discriminants so they can be stored as bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum Biome {
    DeepOcean = 0,
    #[default]
    Ocean,
    WarmOcean,
    FrozenOcean,
    Beach,
    SnowyBeach,
    StonyShore,
    River,
    FrozenRiver,
    Swamp,
    Plains,
    Meadow,
    Forest,
    BirchForest,
    DarkForest,
    Taiga,
    OldGrowthTaiga,
    SnowyTaiga,
    SnowyPlains,
    Jungle,
    Savanna,
    Desert,
    Badlands,
    /// Eroded badlands with tall spires (hoodoos).
    Hoodoos,
    /// Snowy conifer grove on mountain flanks.
    AlpineGrove,
    SnowySlopes,
    StonyPeaks,
    SnowyPeaks,
    Glacier,
    /// Basalt plateaus, calderas, obsidian flows and lava lakes.
    Volcanic,
}

pub const BIOME_COUNT: usize = Biome::Volcanic as usize + 1;

impl Biome {
    #[inline]
    pub fn from_u8(v: u8) -> Biome {
        if (v as usize) < BIOME_COUNT {
            // SAFETY: repr(u8) with contiguous discriminants 0..BIOME_COUNT.
            unsafe { std::mem::transmute::<u8, Biome>(v) }
        } else {
            Biome::Ocean
        }
    }
    pub fn is_ocean(self) -> bool {
        matches!(self, Biome::DeepOcean | Biome::Ocean | Biome::WarmOcean | Biome::FrozenOcean)
    }
    pub fn is_watery(self) -> bool {
        self.is_ocean() || matches!(self, Biome::River | Biome::FrozenRiver | Biome::Swamp)
    }
    pub fn name(self) -> &'static str {
        use Biome::*;
        match self {
            DeepOcean => "deep_ocean",
            Ocean => "ocean",
            WarmOcean => "warm_ocean",
            FrozenOcean => "frozen_ocean",
            Beach => "beach",
            SnowyBeach => "snowy_beach",
            StonyShore => "stony_shore",
            River => "river",
            FrozenRiver => "frozen_river",
            Swamp => "swamp",
            Plains => "plains",
            Meadow => "meadow",
            Forest => "forest",
            BirchForest => "birch_forest",
            DarkForest => "dark_forest",
            Taiga => "taiga",
            OldGrowthTaiga => "old_growth_taiga",
            SnowyTaiga => "snowy_taiga",
            SnowyPlains => "snowy_plains",
            Jungle => "jungle",
            Savanna => "savanna",
            Desert => "desert",
            Badlands => "badlands",
            Hoodoos => "hoodoos",
            AlpineGrove => "alpine_grove",
            SnowySlopes => "snowy_slopes",
            StonyPeaks => "stony_peaks",
            SnowyPeaks => "snowy_peaks",
            Glacier => "glacier",
            Volcanic => "volcanic",
        }
    }
    /// Distinct debug colour for biome maps (sRGB bytes).
    pub fn map_color(self) -> [u8; 3] {
        use Biome::*;
        match self {
            DeepOcean => [20, 30, 110],
            Ocean => [30, 60, 170],
            WarmOcean => [40, 140, 200],
            FrozenOcean => [140, 150, 210],
            Beach => [235, 220, 160],
            SnowyBeach => [240, 240, 220],
            StonyShore => [130, 130, 130],
            River => [60, 110, 240],
            FrozenRiver => [170, 190, 250],
            Swamp => [70, 100, 70],
            Plains => [140, 190, 90],
            Meadow => [170, 210, 120],
            Forest => [50, 130, 50],
            BirchForest => [100, 160, 90],
            DarkForest => [30, 80, 30],
            Taiga => [40, 100, 80],
            OldGrowthTaiga => [60, 80, 50],
            SnowyTaiga => [120, 150, 140],
            SnowyPlains => [230, 240, 245],
            Jungle => [40, 170, 20],
            Savanna => [190, 180, 90],
            Desert => [240, 200, 110],
            Badlands => [200, 100, 50],
            Hoodoos => [170, 80, 40],
            AlpineGrove => [150, 180, 170],
            SnowySlopes => [215, 225, 235],
            StonyPeaks => [150, 140, 130],
            SnowyPeaks => [250, 250, 255],
            Glacier => [180, 220, 255],
            Volcanic => [60, 50, 50],
        }
    }
}

/// Everything known about a column from the 2D (horizontal) fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct ColumnInfo {
    pub x: f64,
    pub z: f64,
    pub climate: Climate,
    /// Smooth 2D surface height (world y, float) — the zero level of the base density,
    /// BEFORE 3D overhang detail and caves. Range ≈ [20, 500].
    pub height: f32,
    /// Water surface: water fills [floor(height)+1 .. water_level) when water_level > height.
    /// SEA_LEVEL for oceans/rivers; local level for lakes/swamps; i32::MIN when dry.
    pub water_level: i32,
    pub biome: Biome,
    /// Temperature at the surface after the altitude lapse rate, ≈ [-1.5, 1].
    pub surface_temperature: f32,
    /// |∇height| (rise over run, dimensionless).
    pub slope: f32,
    /// Max vertical displacement (blocks) of the 3D density detail around `height`.
    /// 0 ⇒ the column is a pure heightfield. The 3D band to evaluate is
    /// [height - jaggedness - 2, height + jaggedness + 2].
    pub jaggedness: f32,
}

/// What fills a carved (cave) voxel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaveFill {
    Air,
    Water,
    Lava,
}

/// One vertical run of feature blocks, relative to the feature root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureColumn {
    pub dx: i16,
    pub dz: i16,
    /// Relative to the ground: y = 0 is the first AIR block above the root column's top solid block.
    /// Fills [y0, y1). Negative y0 allowed (roots / embedded boulders).
    pub y0: i16,
    pub y1: i16,
    pub mat: Material,
    /// If true the blocks may overwrite existing solid terrain (boulders, roots); otherwise they
    /// only fill air (and leaves never overwrite logs).
    pub replace_solid: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureKind {
    Oak,
    BigOak,
    Birch,
    Spruce,
    MegaSpruce,
    Jungle,
    MegaJungle,
    Acacia,
    DarkOak,
    Bush,
    Cactus,
    Boulder,
    IceSpire,
    Other,
}

#[derive(Clone, Debug)]
pub struct Feature {
    pub root_x: i32,
    pub root_z: i32,
    pub kind: FeatureKind,
    pub columns: Vec<FeatureColumn>,
}

impl Feature {
    /// Horizontal bounding radius (Chebyshev) of the feature around its root.
    pub fn radius(&self) -> i32 {
        self.columns.iter().map(|c| c.dx.abs().max(c.dz.abs()) as i32).max().unwrap_or(0)
    }
}

/// Expected (statistical) vegetation canopy for far LOD rendering.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanopyLod {
    /// Fraction of ground covered by canopy, 0..1.
    pub coverage: f32,
    /// Mean canopy top height above ground (blocks).
    pub height: f32,
    /// Representative leaf material (for colour).
    pub leaf: Material,
}

/// The complete generator. Cheap to share across threads (`&WorldGen` is `Sync`).
pub struct WorldGen {
    pub seed: u64,
    pub climate: ClimateSampler,
    pub terrain: TerrainShaper,
    pub surface: SurfaceRules,
    pub caves: CaveCarver,
    pub features: FeaturePlacer,
}

impl WorldGen {
    pub fn new(seed: u64) -> Self {
        WorldGen {
            seed,
            climate: ClimateSampler::new(seed),
            terrain: TerrainShaper::new(seed),
            surface: SurfaceRules::new(seed),
            caves: CaveCarver::new(seed),
            features: FeaturePlacer::new(seed),
        }
    }

    /// Climate at full resolution.
    #[inline]
    pub fn climate(&self, x: f64, z: f64) -> Climate {
        self.climate.sample(x, z, 0.0)
    }

    /// Full-resolution column.
    #[inline]
    pub fn column(&self, x: f64, z: f64) -> ColumnInfo {
        self.terrain.column(&self.climate, x, z, 0.0)
    }

    /// Band-limited column for far LOD: `footprint` = blocks covered by one sample (cell size).
    /// Detail with wavelength < ~2×footprint is filtered out (anti-aliasing).
    #[inline]
    pub fn column_lod(&self, x: f64, z: f64, footprint: f64) -> ColumnInfo {
        self.terrain.column(&self.climate, x, z, footprint)
    }

    /// 3D density (> 0 = solid). Valid everywhere; equals the heightfield sign outside the band.
    #[inline]
    pub fn density(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> f32 {
        self.terrain.density(x, y, z, col)
    }

    /// Material of a solid terrain block. `depth` = number of solid blocks above it within the
    /// same solid run (0 = exposed top block). `y` is the block's world y.
    #[inline]
    pub fn surface_material(&self, x: i32, y: i32, z: i32, depth: i32, col: &ColumnInfo) -> Material {
        self.surface.material(x, y, z, depth, col)
    }

    /// Continuous cave field: > 0 means carve (air/water/lava). Smooth on a ~4-block scale so
    /// it can be sampled on a 4×4×4 lattice and trilinearly interpolated.
    #[inline]
    pub fn cave_field(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> f32 {
        self.caves.field(x, y, z, col)
    }

    /// What fills a carved voxel at (x, y, z).
    #[inline]
    pub fn cave_fill(&self, x: i32, y: i32, z: i32, col: &ColumnInfo) -> CaveFill {
        self.caves.fill(x, y, z, col)
    }

    /// All features (trees, boulders…) whose ROOT lies in [x0, x1) × [z0, z1).
    /// Callers wanting everything that *touches* a rectangle should expand it by `MAX_FEATURE_RADIUS`.
    pub fn features_in(&self, x0: i32, z0: i32, x1: i32, z1: i32) -> Vec<Feature> {
        self.features.features_in(self, x0, z0, x1, z1)
    }

    /// Statistical canopy for far LOD.
    #[inline]
    pub fn canopy_lod(&self, x: f64, z: f64, col: &ColumnInfo, footprint: f64) -> CanopyLod {
        self.features.canopy_lod(x, z, col, footprint)
    }
}

/// Upper bound on any feature's horizontal radius (blocks).
pub const MAX_FEATURE_RADIUS: i32 = 24;
