//! Block materials + their optical properties + continuous biome tint colormaps.
//! FROZEN shared file (render agent may ADD functions in render/shade.rs, not here).
//!
//! All colours are LINEAR RGB albedo in [0,1].

use crate::math::{lerp, saturate, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum Material {
    #[default]
    Air = 0,
    Stone,
    Deepslate,
    Bedrock,
    Granite,
    Andesite,
    Calcite,
    Dirt,
    Grass,
    Podzol,
    Mud,
    Moss,
    Sand,
    RedSand,
    Sandstone,
    RedSandstone,
    Gravel,
    Clay,
    Snow,
    Ice,
    PackedIce,
    Water,
    Lava,
    /// Banded clay rock of badlands. Colour depends on world y (see `terracotta_band`).
    Terracotta,
    OakLog,
    OakLeaves,
    BirchLog,
    BirchLeaves,
    SpruceLog,
    SpruceLeaves,
    JungleLog,
    JungleLeaves,
    AcaciaLog,
    AcaciaLeaves,
    DarkOakLog,
    DarkOakLeaves,
    Cactus,
    /// Glowing crystal (geodes in caves) — emissive.
    Crystal,
    Basalt,
    Obsidian,
}

pub const MATERIAL_COUNT: usize = Material::Obsidian as usize + 1;

impl Material {
    #[inline]
    pub fn from_u8(v: u8) -> Material {
        if (v as usize) < MATERIAL_COUNT {
            // SAFETY: repr(u8), contiguous discriminants 0..MATERIAL_COUNT.
            unsafe { std::mem::transmute::<u8, Material>(v) }
        } else {
            Material::Air
        }
    }
    #[inline]
    pub fn is_air(self) -> bool {
        self == Material::Air
    }
    #[inline]
    pub fn is_liquid(self) -> bool {
        matches!(self, Material::Water | Material::Lava)
    }
    /// Solid for ray traversal = everything except air and water (lava is treated as solid surface).
    #[inline]
    pub fn is_opaque(self) -> bool {
        !matches!(self, Material::Air | Material::Water)
    }
    #[inline]
    pub fn is_leaves(self) -> bool {
        matches!(
            self,
            Material::OakLeaves
                | Material::BirchLeaves
                | Material::SpruceLeaves
                | Material::JungleLeaves
                | Material::AcaciaLeaves
                | Material::DarkOakLeaves
        )
    }
    #[inline]
    pub fn is_log(self) -> bool {
        matches!(
            self,
            Material::OakLog
                | Material::BirchLog
                | Material::SpruceLog
                | Material::JungleLog
                | Material::AcaciaLog
                | Material::DarkOakLog
        )
    }
    /// Uses the biome grass colormap.
    #[inline]
    pub fn grass_tinted(self) -> bool {
        matches!(self, Material::Grass | Material::Moss)
    }
    /// Uses the biome foliage colormap (multiplied with `albedo`).
    #[inline]
    pub fn foliage_tinted(self) -> bool {
        matches!(
            self,
            Material::OakLeaves | Material::JungleLeaves | Material::AcaciaLeaves | Material::DarkOakLeaves
        )
    }

    /// Base linear albedo (before tint / texture variation).
    pub fn albedo(self) -> Vec3 {
        use Material::*;
        let s = |r: u8, g: u8, b: u8| srgb8(r, g, b);
        match self {
            Air => Vec3::ZERO,
            Stone => s(125, 125, 125),
            Deepslate => s(80, 80, 86),
            Bedrock => s(55, 55, 55),
            Granite => s(150, 106, 88),
            Andesite => s(136, 136, 138),
            Calcite => s(222, 224, 220),
            Dirt => s(134, 96, 67),
            Grass => s(255, 255, 255), // multiplied by grass tint
            Podzol => s(91, 63, 24),
            Mud => s(60, 57, 60),
            Moss => s(255, 255, 255),
            Sand => s(219, 207, 163),
            RedSand => s(190, 102, 33),
            Sandstone => s(216, 203, 155),
            RedSandstone => s(181, 97, 31),
            Gravel => s(131, 127, 126),
            Clay => s(160, 166, 179),
            Snow => s(249, 254, 254),
            Ice => s(145, 183, 253),
            PackedIce => s(141, 180, 250),
            Water => s(63, 118, 228),
            Lava => s(207, 92, 20),
            Terracotta => s(152, 94, 67),
            OakLog => s(109, 85, 50),
            OakLeaves => s(200, 200, 200), // × foliage tint
            BirchLog => s(216, 215, 210),
            BirchLeaves => s(128, 167, 85),
            SpruceLog => s(58, 37, 16),
            SpruceLeaves => s(97, 153, 97),
            JungleLog => s(85, 67, 25),
            JungleLeaves => s(210, 210, 210),
            AcaciaLog => s(103, 96, 86),
            AcaciaLeaves => s(190, 190, 190),
            DarkOakLog => s(60, 46, 26),
            DarkOakLeaves => s(170, 170, 170),
            Cactus => s(88, 130, 44),
            Crystal => s(160, 110, 230),
            Basalt => s(73, 72, 78),
            Obsidian => s(20, 18, 30),
        }
    }

    /// Emitted radiance (linear, arbitrary HDR units; sun irradiance ≈ 10).
    pub fn emission(self) -> Vec3 {
        match self {
            Material::Lava => Vec3::new(8.0, 2.6, 0.5),
            Material::Crystal => Vec3::new(1.2, 0.6, 2.2),
            _ => Vec3::ZERO,
        }
    }

    /// Specular reflectance at normal incidence (F0) for a simple glossy term.
    pub fn specular_f0(self) -> f32 {
        match self {
            Material::Water => 0.02,
            Material::Ice | Material::PackedIce => 0.05,
            Material::Snow => 0.03,
            Material::Calcite | Material::Crystal | Material::Obsidian => 0.05,
            _ => 0.0,
        }
    }
}

/// sRGB 8-bit → linear.
#[inline]
pub fn srgb8(r: u8, g: u8, b: u8) -> Vec3 {
    Vec3::new(srgb_to_linear(r as f32 / 255.0), srgb_to_linear(g as f32 / 255.0), srgb_to_linear(b as f32 / 255.0))
}

#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.max(0.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Continuous climate colormap. `temp` and `humid` in [-1,1] (temperature already includes the
/// altitude lapse). Bilinear blend over a triangle-ish domain: humidity is scaled by temperature
/// (cold air holds little moisture), exactly the trick that makes colours vary smoothly with no
/// biome borders.
fn climate_colormap(temp: f32, humid: f32, corners: [Vec3; 4]) -> Vec3 {
    // corners: [hot-dry, hot-wet, cold-dry, cold-wet]
    let t = saturate((temp + 1.0) * 0.5); // 0 cold .. 1 hot
    let h = saturate((humid + 1.0) * 0.5) * t; // dryness forced at low temperature
    let hot = corners[0].lerp(corners[1], h);
    let cold = corners[2].lerp(corners[3], h);
    cold.lerp(hot, t)
}

/// Biome grass tint (multiply with `Material::Grass.albedo()`).
pub fn grass_tint(temp: f32, humid: f32) -> Vec3 {
    climate_colormap(
        temp,
        humid,
        [srgb8(191, 183, 85), srgb8(71, 205, 51), srgb8(128, 180, 151), srgb8(96, 161, 123)],
    )
}

/// Biome foliage tint (multiply with leaves albedo).
pub fn foliage_tint(temp: f32, humid: f32) -> Vec3 {
    climate_colormap(
        temp,
        humid,
        [srgb8(174, 164, 42), srgb8(26, 191, 0), srgb8(96, 161, 123), srgb8(72, 140, 100)],
    )
}

/// Swamp-ish water tint factor: 0 = clear blue ocean, 1 = murky green.
pub fn water_tint(murkiness: f32, temp: f32) -> Vec3 {
    let warm = srgb8(67, 213, 238);
    let cold = srgb8(57, 56, 201);
    let base = cold.lerp(warm, saturate((temp + 1.0) * 0.5));
    base.lerp(srgb8(97, 123, 100), saturate(murkiness))
}

/// Badlands terracotta stratification: colour of the band at world height `y`.
/// `phase` (blocks) lets the generator undulate the strata with low-frequency noise.
pub fn terracotta_band(y: f32, phase: f32) -> Vec3 {
    const BANDS: [(u8, u8, u8); 8] = [
        (152, 94, 67),   // plain
        (161, 83, 37),   // orange
        (210, 178, 161), // white-ish
        (186, 133, 35),  // yellow
        (142, 60, 46),   // red
        (135, 106, 97),  // light grey
        (77, 51, 35),    // brown
        (152, 94, 67),
    ];
    // Irregular band thicknesses from a deterministic hash of the band index.
    let v = (y + phase) / 3.0;
    let idx = v.floor() as i64;
    let h = crate::noise::mix64(idx as u64 ^ 0xBADA55);
    let (r, g, b) = BANDS[(h % 8) as usize];
    let c = srgb8(r, g, b);
    // Soft 1-block blend at band edges.
    let f = v - v.floor();
    let h2 = crate::noise::mix64((idx + 1) as u64 ^ 0xBADA55);
    let (r2, g2, b2) = BANDS[(h2 % 8) as usize];
    c.lerp(srgb8(r2, g2, b2), saturate((f - 0.92) / 0.08) * 0.5)
}

/// Average albedo of the top surface of a column as seen from far away (used by far LOD).
pub fn far_albedo(mat: Material, temp: f32, humid: f32, y: f32) -> Vec3 {
    let a = mat.albedo();
    if mat.grass_tinted() {
        a * grass_tint(temp, humid)
    } else if mat.foliage_tinted() {
        a * foliage_tint(temp, humid)
    } else if mat == Material::Terracotta {
        terracotta_band(y, 0.0)
    } else {
        a
    }
}

#[inline]
pub fn mix_albedo(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    Vec3::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.z, b.z, t))
}
