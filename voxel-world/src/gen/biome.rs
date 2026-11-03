//! Biome classification + surface rules. OWNER: gen-core.
//!
//! # Classification
//! A decision procedure over (C, E, PV, T, H) plus the *derived* column quantities (height,
//! slope, surface temperature `T_s = T − (H − sea)/250 − 0.12·aspect`, landform region weights).
//! Using the derived quantities makes biome borders follow the terrain: snow lines are isotherms of
//! `T_s`, peaks are where altitude and convexity (pv) are high, beaches only where the coast is
//! gentle, stony shores where it is steep, rivers exactly in the carved channel. Lowland biomes
//! partition the (T_s, H) plane (Whittaker-style) so that neighbours are climatic neighbours:
//! desert ↔ savanna ↔ jungle, plains ↔ forest ↔ dark forest, taiga ↔ snowy taiga …
//!
//! # Surface rules
//! `material()` costs one integer hash plus at most two noise evaluations (a 2D simplex for patches
//! at the top, a 3D simplex for rock strata underground). Layer thicknesses vary by hash.
use super::{Biome, Climate, ColumnInfo, BEDROCK_TOP, SEA_LEVEL};
use super::terrain::Regions;
use crate::materials::Material;
use crate::noise::{derive_seed, hash2, hash3, salt, Simplex};

const SEA: f32 = SEA_LEVEL as f32;

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
}

/// Snow line: surface temperature below which exposed flat ground is snow-covered.
pub const SNOW_T: f32 = -0.5;

/// Biome decision procedure (see module docs).
pub fn classify(b: &BiomeInput) -> Biome {
    use Biome::*;
    let c = &b.climate;
    let h = b.height;
    let st = b.surface_temperature;
    let alt = h - SEA;
    let r = &b.regions;

    if r.volcanic > 0.5 && h >= SEA - 1.0 {
        return Volcanic;
    }

    // ---------------- water-covered
    if h < SEA - 0.5 {
        if r.channel > 0.35 && c.continentalness > -0.17 {
            return if st < SNOW_T { FrozenRiver } else { River };
        }
        if r.swamp > 0.5 {
            return Swamp;
        }
        if c.continentalness > -0.12 && h > SEA - 5.0 {
            // shallow inland water (rivers' flanks, lagoons)
            if b.river_dist < b.river_width * 2.5 + 6.0 {
                return if st < SNOW_T { FrozenRiver } else { River };
            }
        }
        if c.temperature < -0.6 {
            return FrozenOcean;
        }
        if h < 92.0 {
            return DeepOcean;
        }
        if c.temperature > 0.48 {
            return WarmOcean;
        }
        return Ocean;
    }

    // ---------------- shores
    let coastal = c.continentalness < -0.04 && alt < 4.5;
    if coastal && r.channel < 0.5 {
        if b.slope > 0.85 || r.cliff > 0.55 {
            return StonyShore;
        }
        if st < SNOW_T {
            return SnowyBeach;
        }
        if r.swamp > 0.5 {
            return Swamp;
        }
        return Beach;
    }
    if r.channel > 0.5 && alt < 2.0 {
        return if st < SNOW_T { FrozenRiver } else { River };
    }

    // ---------------- dry-land landforms with dedicated terrain
    if r.hoodoo > 0.5 && r.badlands > 0.5 {
        return Hoodoos;
    }
    if r.badlands > 0.5 {
        return Badlands;
    }

    // ---------------- altitude zones
    let peaky = b.mountain > 120.0 || alt > 230.0;
    if st < -0.85 && b.slope < 0.6 && alt > 150.0 {
        return Glacier;
    }
    if peaky && alt > 200.0 {
        if st < SNOW_T + 0.05 {
            return SnowyPeaks;
        }
        if alt > 260.0 || b.slope > 0.9 {
            return StonyPeaks;
        }
    }
    if st < SNOW_T + 0.1 && alt > 110.0 && b.slope > 0.75 {
        return SnowySlopes;
    }
    if st < -0.12 && alt > 95.0 && b.mountain > 40.0 {
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

    // ---------------- lowlands: Whittaker partition of (T_s, H)
    let hu = c.humidity;
    if st < -0.58 {
        return if hu > -0.05 { SnowyTaiga } else { SnowyPlains };
    }
    if st < -0.2 {
        if hu > 0.45 {
            return OldGrowthTaiga;
        }
        if hu > -0.35 {
            return Taiga;
        }
        return if st < -0.4 { SnowyPlains } else { Plains };
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
        }
    }

    /// Patchiness field in [-1, 1] at ~14-block scale (one 2D simplex).
    #[inline]
    fn patch(&self, x: i32, z: i32) -> f32 {
        self.patch.n2(x as f64 * 0.071, z as f64 * 0.071)
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
        // Deep blocks, cave floors/walls far below the 2D surface band → rock.
        if depth > 48 || below_surface > col.jaggedness + 10.0 + 4.0 * bit(h, 32) || y < BEDROCK_TOP {
            return self.rock(x, y, z, h);
        }
        let b = col.biome;
        let st = col.surface_temperature;
        let slope = col.slope;
        let underwater = y < col.water_level && col.water_level <= SEA_LEVEL + 1;
        let jit = bit(h, 48);
        let hc = hash2(self.seed ^ 0x51, x, z);
        let dirt_depth = 3 + (hc & 1) as i32 + ((hc >> 1) & 1) as i32;

        // ---------- badlands / hoodoos: red sand on flats, banded terracotta
        if matches!(b, Biome::Badlands | Biome::Hoodoos) {
            if depth == 0 && slope < 0.75 && !(b == Biome::Hoodoos && below_surface < -3.0) {
                return if self.patch(x, z) > 0.8 { Sand } else { RedSand };
            }
            if depth < 2 && slope < 0.5 {
                return RedSand;
            }
            if depth < 40 {
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

        // ---------- underwater floors
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
                    if p > 0.2 { Clay } else { Mud }
                }
                Biome::River | Biome::FrozenRiver => {
                    if p > 0.45 {
                        Clay
                    } else if p < -0.35 {
                        Gravel
                    } else {
                        Sand
                    }
                }
                Biome::DeepOcean => {
                    if p > 0.3 { Clay } else if p < -0.2 { Sand } else { Gravel }
                }
                Biome::FrozenOcean => {
                    if p > 0.4 { Clay } else { Gravel }
                }
                _ => {
                    if wd > 14 {
                        if p > 0.35 { Gravel } else if p < -0.45 { Clay } else { Sand }
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
        let cold = st < SNOW_T + 0.04 * (jit - 0.5);

        match b {
            Biome::Beach | Biome::Desert => {
                let sand = if b == Biome::Desert { 4 + (hc & 3) as i32 } else { 3 + (hc & 1) as i32 };
                if depth < sand {
                    return Sand;
                }
                if depth < sand + 8 {
                    return Sandstone;
                }
                return self.rock(x, y, z, h);
            }
            Biome::SnowyBeach => {
                if depth == 0 {
                    return Snow;
                }
                if depth < 4 {
                    return Sand;
                }
                return if depth < 7 { Sandstone } else { self.rock(x, y, z, h) };
            }
            Biome::StonyShore => {
                if depth < 2 && self.patch(x, z) > 0.3 {
                    return Gravel;
                }
                return self.rock(x, y, z, h);
            }
            Biome::Glacier => {
                if depth == 0 {
                    return if steep { PackedIce } else { Snow };
                }
                if depth < 10 + (hc & 3) as i32 {
                    return if self.patch(x, z) > 0.6 { Ice } else { PackedIce };
                }
                return self.rock(x, y, z, h);
            }
            Biome::SnowyPeaks | Biome::SnowySlopes => {
                if steep && slope > 1.6 {
                    return self.rock(x, y, z, h);
                }
                if depth == 0 {
                    return Snow;
                }
                if depth < 3 {
                    return if b == Biome::SnowyPeaks { PackedIce } else { Snow };
                }
                return self.rock(x, y, z, h);
            }
            Biome::StonyPeaks => {
                if depth == 0 && cold && slope < 0.9 {
                    return Snow;
                }
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
        // depth == 0: the exposed top block
        if cold && slope < 1.0 {
            return Snow;
        }
        // scree on moderately steep mountain ground
        if slope > 0.95 && self.patch(x, z) > 0.25 {
            return Gravel;
        }
        match b {
            Biome::River | Biome::FrozenRiver => {
                if self.patch(x, z) > 0.3 { Gravel } else { Sand }
            }
            Biome::SnowyPlains | Biome::SnowyTaiga | Biome::AlpineGrove => {
                if st < SNOW_T + 0.25 { Snow } else { Grass }
            }
            Biome::Taiga => {
                if self.patch(x, z) > 0.25 { Podzol } else { Grass }
            }
            Biome::OldGrowthTaiga => {
                let p = self.patch(x, z);
                if p > -0.1 {
                    Podzol
                } else if p < -0.6 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::DarkForest => {
                let p = self.patch(x, z);
                if p > 0.55 {
                    Podzol
                } else if p < -0.55 {
                    Moss
                } else {
                    Grass
                }
            }
            Biome::Jungle => {
                if self.patch(x, z) > 0.45 { Moss } else { Grass }
            }
            Biome::Swamp => {
                let p = self.patch(x, z);
                if p > 0.35 {
                    Mud
                } else if p < -0.6 {
                    Clay
                } else {
                    Grass
                }
            }
            Biome::Savanna => {
                if self.patch(x, z) > 0.6 { Dirt } else { Grass }
            }
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
