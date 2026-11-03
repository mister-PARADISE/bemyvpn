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
//! Lowland biomes partition the (T_s, H) plane (Whittaker-style) so that neighbours are climatic
//! neighbours: desert ↔ savanna ↔ jungle, plains ↔ forest ↔ dark forest, taiga ↔ old-growth
//! taiga; the cold end is snow-free tundra/taiga (grass, podzol, moss, gravel).
//!
//! # Surface rules
//! `material()` costs one integer hash plus at most two noise evaluations (a 2D simplex for patches
//! at the top or the snow line above y 300, a 3D simplex for rock strata underground).
use super::climate::FastSimplex;
use super::terrain::Regions;
use super::{Biome, Climate, ColumnInfo, BEDROCK_TOP, SEA_LEVEL};
use crate::materials::Material;
use crate::noise::{derive_seed, hash2, hash3, salt, Simplex};

const SEA: f32 = SEA_LEVEL as f32;

/// Mean snow line (world y).
pub const SNOW_LINE: f32 = 330.0;
/// Height above the local snow line from which glaciers / permanent ice cover everything.
pub const GLACIER_REL: f32 = 90.0;
/// No snow line is ever below this (330 − 12 − 15).
pub const SNOW_LINE_MIN: f32 = SNOW_LINE - 27.0;

/// The local snow line `S(x,z) = 330 + 12·n(x,z) − 15·T` (see module docs).
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
        SNOW_LINE + 12.0 * n.clamp(-1.0, 1.0) - 15.0 * temperature.clamp(-1.0, 1.0)
    }
}

/// Convenience for other modules (features: treeline at ≈ S − 90 … S − 30):
/// `snow_line(gen.seed, col.x, col.z, col.climate.temperature)`.
pub fn snow_line(seed: u64, x: f64, z: f64, temperature: f32) -> f32 {
    SnowLine::new(seed).at(x, z, temperature)
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

    // ---------------- lowlands: Whittaker partition of (T_s, H), snow-free
    let hu = c.humidity;
    if st < -0.2 {
        // boreal / tundra
        if hu > 0.45 {
            return OldGrowthTaiga;
        }
        if hu > -0.35 {
            return Taiga;
        }
        return Plains;
    }
    if st < 0.42 {
        if hu > 0.5 {
            return DarkForest;
        }
        if hu > 0.05 {
            return if c.weirdness > 0.3 { BirchForest } else { Forest };
        }
        if hu > -0.35 {
            return if c.weirdness < -0.55 { Forest } else { Plains };
        }
        return if st > 0.25 { Savanna } else { Plains };
    }
    // hot
    if hu > 0.35 {
        return Jungle;
    }
    if hu > 0.1 {
        return Forest;
    }
    Savanna
}

// =====================================================================================
// Surface rules
// =====================================================================================

pub struct SurfaceRules {
    pub seed: u64,
    patch: Simplex,
    strata: Simplex,
    snow: SnowLine,
}

#[inline]
fn bit(h: u64, shift: u32) -> f32 {
    ((h >> shift) & 0xFFFF) as f32 * (1.0 / 65536.0)
}

impl SurfaceRules {
    pub fn new(seed: u64) -> Self {
        SurfaceRules {
            seed,
            patch: Simplex::new(derive_seed(seed, salt("surface.patch"))),
            strata: Simplex::new(derive_seed(seed, salt("surface.strata"))),
            snow: SnowLine::new(seed),
        }
    }

    /// The local snow line of a column (world y).
    #[inline]
    pub fn snow_line(&self, col: &ColumnInfo) -> f32 {
        self.snow.at(col.x, col.z, col.climate.temperature)
    }

    /// Patchiness field in [-1, 1] at ~14-block scale (one 2D simplex).
    #[inline]
    fn patch(&self, x: i32, z: i32) -> f32 {
        self.patch.n2(x as f64 * 0.071, z as f64 * 0.071)
    }

    /// Snow cover of an exposed block at world y (None below the snow line or where the slope
    /// exceeds the altitude-dependent limit). Dithered ±3 blocks so the line is not a contour.
    #[inline]
    fn snow_cover(&self, y: i32, col: &ColumnInfo, jit: f32) -> Option<Material> {
        if (y as f32) < SNOW_LINE_MIN - 4.0 {
            return None;
        }
        let rel = y as f32 + 0.5 - self.snow_line(col) + 6.0 * (jit - 0.5);
        if rel < 0.0 || col.slope >= snow_slope_limit(rel) {
            return None;
        }
        if rel > GLACIER_REL && (col.slope > 1.1 || jit < 0.3) {
            return Some(Material::PackedIce);
        }
        Some(Material::Snow)
    }

    /// Underground rock: deepslate below y≈64 (dithered), bedrock floor, strata lenses
    /// (granite/andesite/calcite) from a squashed 3D simplex.
    #[inline]
    fn rock(&self, x: i32, y: i32, z: i32, h: u64) -> Material {
        if y < BEDROCK_TOP {
            let p = (BEDROCK_TOP - y) as f32 / (BEDROCK_TOP as f32 + 1.0);
            if y == 0 || bit(h, 0) < p {
                return Material::Bedrock;
            }
        }
        let deep = if y < 56 {
            true
        } else if y < 72 {
            bit(h, 16) < (72 - y) as f32 / 16.0
        } else {
            false
        };
        let v = self.strata.n3(x as f64 * 0.021, y as f64 * 0.065, z as f64 * 0.021);
        if deep {
            if v > 0.62 {
                return Material::Granite;
            }
            return Material::Deepslate;
        }
        if v > 0.52 {
            if v > 0.78 {
                return Material::Calcite;
            }
            Material::Granite
        } else if (-0.84..-0.52).contains(&v) {
            Material::Andesite
        } else if v < -0.84 {
            Material::Calcite
        } else {
            Material::Stone
        }
    }

    pub fn material(&self, x: i32, y: i32, z: i32, depth: i32, col: &ColumnInfo) -> Material {
        use Material::*;
        let h = hash3(self.seed, x, y, z);
        let below_surface = col.height - y as f32;
        let b = col.biome;
        // Deep blocks, cave floors/walls far below the 2D surface band → rock. Badlands and
        // hoodoo columns expose their whole side down to the neighbour (mesa cliffs, 36-block
        // spires), so their banded terracotta must reach much deeper below the column top.
        let cut = match b {
            Biome::Badlands | Biome::Hoodoos => 64.0,
            Biome::Volcanic => 28.0,
            _ => col.jaggedness + 10.0 + 4.0 * bit(h, 32),
        };
        if depth > 48 || below_surface > cut || y < BEDROCK_TOP {
            return self.rock(x, y, z, h);
        }
        let st = col.surface_temperature;
        let slope = col.slope;
        let underwater = y < col.water_level && b != Biome::Volcanic;
        let jit = bit(h, 48);
        let hc = hash2(self.seed ^ 0x51, x, z);
        let dirt_depth = 3 + (hc & 1) as i32 + ((hc >> 1) & 1) as i32;

        // ---------- snow: altitude-driven only (nothing below the local snow line)
        if depth == 0 && !underwater {
            if let Some(m) = self.snow_cover(y, col, jit) {
                return m;
            }
        }

        // ---------- badlands / hoodoos: red sand on flats, banded terracotta
        if matches!(b, Biome::Badlands | Biome::Hoodoos) {
            if depth == 0 && slope < 0.75 && !(b == Biome::Hoodoos && below_surface < -3.0) {
                return if self.patch(x, z) > 0.8 { Sand } else { RedSand };
            }
            if depth < 2 && slope < 0.5 {
                return RedSand;
            }
            if depth < 56 {
                return Terracotta;
            }
            return self.rock(x, y, z, h);
        }
        // ---------- volcanic: basalt with radial obsidian flows, ash (gravel)
        if b == Biome::Volcanic {
            if depth < 6 {
                let p = self.patch(x, z);
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
            return if depth < 20 { Basalt } else { self.rock(x, y, z, h) };
        }

        // ---------- underwater floors (sea, rivers, lakes)
        if underwater {
            let wd = col.water_level - y; // water depth above this block
            if depth >= 4 {
                return match b {
                    Biome::Beach | Biome::WarmOcean if depth < 7 => Sandstone,
                    _ => self.rock(x, y, z, h),
                };
            }
            let p = self.patch(x, z);
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

        // ---------- steep rock faces (any biome without its own rock)
        let steep = slope > 1.25 + 0.25 * jit;

        match b {
            Biome::Beach | Biome::Desert | Biome::SnowyBeach => {
                let sand = if b == Biome::Desert { 4 + (hc & 3) as i32 } else { 3 + (hc & 1) as i32 };
                if depth < sand {
                    return Sand;
                }
                if depth < sand + 8 {
                    return Sandstone;
                }
                return self.rock(x, y, z, h);
            }
            Biome::StonyShore => {
                if depth < 2 && self.patch(x, z) > 0.3 {
                    return Gravel;
                }
                return self.rock(x, y, z, h);
            }
            Biome::Glacier => {
                // firn / glacier ice under the (snow-line-driven) surface
                if depth == 0 {
                    return if steep { self.rock(x, y, z, h) } else { PackedIce };
                }
                if depth < 10 + (hc & 3) as i32 && !steep {
                    return if self.patch(x, z) > 0.6 { Ice } else { PackedIce };
                }
                return self.rock(x, y, z, h);
            }
            Biome::SnowyPeaks | Biome::SnowySlopes | Biome::SnowyPlains => {
                // the top block was handled by `snow_cover`; a snowpack lies below it where
                // the surface is snowy, otherwise bare rock / scree / alpine turf
                let top_snow = self.snow_cover(y + depth, col, 0.5).is_some();
                if top_snow && depth < 3 {
                    return if b == Biome::SnowyPeaks && depth > 0 { PackedIce } else { Snow };
                }
                if b == Biome::SnowyPlains && !steep && depth < dirt_depth {
                    return if depth == 0 { if self.patch(x, z) > 0.2 { Gravel } else { Grass } } else { Dirt };
                }
                if depth < 2 && slope < 0.9 && self.patch(x, z) > 0.3 {
                    return Gravel;
                }
                return self.rock(x, y, z, h);
            }
            Biome::StonyPeaks => {
                if depth < 2 && slope < 0.8 && self.patch(x, z) > 0.35 {
                    return Gravel;
                }
                return self.rock(x, y, z, h);
            }
            _ => {}
        }

        // ---------- vegetated / soil biomes
        if steep {
            return self.rock(x, y, z, h);
        }
        if depth >= dirt_depth {
            return self.rock(x, y, z, h);
        }
        if depth > 0 {
            return match b {
                Biome::Swamp if depth < 2 => Mud,
                Biome::River | Biome::FrozenRiver => Sand,
                _ => Dirt,
            };
        }
        // depth == 0: the exposed top block (never snow below the snow line)
        // scree on moderately steep mountain ground
        if slope > 0.95 && self.patch(x, z) > 0.25 {
            return Gravel;
        }
        let p = self.patch(x, z);
        match b {
            Biome::River | Biome::FrozenRiver => {
                if p > 0.3 {
                    Gravel
                } else {
                    Sand
                }
            }
            Biome::Taiga | Biome::SnowyTaiga | Biome::AlpineGrove => {
                if p > 0.25 {
                    Podzol
                } else if p < -0.65 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::OldGrowthTaiga => {
                if p > -0.1 {
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
                if p > 0.6 {
                    Dirt
                } else {
                    Grass
                }
            }
            Biome::Plains | Biome::Meadow if st < -0.35 => {
                // tundra: grass with gravel, moss and podzol patches — but no snow
                if p > 0.55 {
                    Gravel
                } else if p < -0.5 {
                    Moss
                } else if p > 0.3 {
                    Podzol
                } else {
                    Grass
                }
            }
            Biome::Meadow if slope > 0.6 && p > 0.4 => Gravel,
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
        assert!(matches!(m, Material::Stone | Material::Granite | Material::Andesite | Material::Calcite));
    }

    /// Mesa cliffs and hoodoo spires expose a column's side down to its neighbour's top; those
    /// blocks must be banded terracotta / red sand, never grey stone (previously the generic
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
                        if !matches!(m, Material::Terracotta | Material::RedSand | Material::Sand | Material::Snow) {
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
