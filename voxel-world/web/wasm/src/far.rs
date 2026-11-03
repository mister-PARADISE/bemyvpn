//! Far-field heightfield tiles, material table, point queries and spawn search.
use crate::chunk::terracotta_base;
use crate::terrain::{column_top, Liquids, Nodes, WH};
use voxel_world::gen::{Biome, WorldGen, SEA_LEVEL};
use voxel_world::materials::{far_albedo, linear_to_srgb, Material, MATERIAL_COUNT};
use voxel_world::math::Vec3;

/// Bytes per far-tile vertex.
pub const FAR_VB: usize = 16;
pub const DRY: f32 = -1.0e9;

#[inline]
fn srgb_u8(v: f32) -> u8 {
    ((linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32).min(255) as u8
}

/// One far sample (same rules as the world builder's `hf_direct`, at a point).
pub struct FarSample {
    pub top: f32,
    pub water: f32,
    pub albedo: Vec3,
    pub mat: Material,
    /// Expected canopy coverage 0..1 and mean canopy height (blocks above ground).
    pub cov: f32,
    pub canopy_h: f32,
    pub biome: Biome,
}

pub fn far_sample(gen: &WorldGen, liq: &Liquids, x: f64, z: f64, cell: f64) -> FarSample {
    let fp = cell.max(1.0);
    let col = gen.column_lod(x, z, fp);
    let h = col.height;
    let wet = col.water_level != i32::MIN && col.water_level as f32 > h;
    let ty = ((h - 0.5).ceil() as i32 - 1).clamp(0, WH - 1);
    let (temp, hum) = (col.surface_temperature, col.climate.humidity);
    let q = fp * 0.25;
    let mut mats = [Material::Air; 4];
    let mut alb = Vec3::ZERO;
    for (s, (sx, sz)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)].iter().enumerate() {
        let bx = (x + sx * q).floor() as i32;
        let bz = (z + sz * q).floor() as i32;
        let m = gen.surface_material(bx, ty, bz, 0, &col);
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
        let mut l = liq.liquid(&col);
        if l == Material::Water && liq.freezes(gen, x, z, &col) {
            l = Material::Ice;
        }
        return FarSample { top: h, water: col.water_level as f32, albedo: alb, mat: l, cov: 0.0, canopy_h: 0.0, biome: col.biome };
    }
    let can = gen.canopy_lod(x, z, &col, fp);
    let cov = can.coverage.clamp(0.0, 1.0);
    if cov <= 0.0 || !can.height.is_finite() {
        return FarSample { top: h, water: DRY, albedo: alb, mat: ground, cov: 0.0, canopy_h: 0.0, biome: col.biome };
    }
    let leaf = far_albedo(can.leaf, temp, hum, h);
    FarSample {
        top: h + cov * can.height.max(0.0),
        water: DRY,
        albedo: alb.lerp(leaf, cov),
        mat: if cov >= 0.5 { can.leaf } else { ground },
        cov,
        canopy_h: can.height.max(0.0),
        biome: col.biome,
    }
}

/// (n+1)² vertices at (x0 + i·cell, z0 + j·cell), row-major in j (z), i (x) fastest.
/// 16 bytes each: f32 top (incl. expected canopy), f32 water level (−1e9 dry), sRGB8 albedo,
/// u8 material, then extras: u8 canopy coverage (0..255), u8 biome, u8 canopy height
/// (blocks), u8 0.
pub fn build_far(gen: &WorldGen, liq: &Liquids, cell: u32, x0: i32, z0: i32, n: u32, out: &mut Vec<u8>) {
    out.clear();
    let m = n as usize + 1;
    out.reserve(m * m * FAR_VB);
    let c = cell.max(1) as f64;
    for j in 0..m {
        for i in 0..m {
            let x = x0 as f64 + i as f64 * c;
            let z = z0 as f64 + j as f64 * c;
            let s = far_sample(gen, liq, x, z, c);
            out.extend_from_slice(&s.top.to_le_bytes());
            out.extend_from_slice(&s.water.to_le_bytes());
            out.extend_from_slice(&[srgb_u8(s.albedo.x), srgb_u8(s.albedo.y), srgb_u8(s.albedo.z), s.mat as u8]);
            out.extend_from_slice(&[(s.cov * 255.0 + 0.5) as u8, s.biome as u8, s.canopy_h.round().clamp(0.0, 255.0) as u8, 0]);
        }
    }
}

/// Material flag bits (stored as f32 numbers in the table).
pub const F_GRASS: u32 = 1;
pub const F_FOLIAGE: u32 = 2;
pub const F_EMISSIVE: u32 = 4;
pub const F_LEAVES: u32 = 8;
/// Tint carries an absolute colour ratio (terracotta strata), not a biome tint.
pub const F_BANDED: u32 = 16;

pub fn material_table() -> Vec<f32> {
    let mut t = Vec::with_capacity(MATERIAL_COUNT * 4);
    for i in 0..MATERIAL_COUNT {
        let m = Material::from_u8(i as u8);
        let a = if m == Material::Terracotta { terracotta_base() } else { m.albedo() };
        let mut f = 0u32;
        if m.grass_tinted() {
            f |= F_GRASS;
        }
        if m.foliage_tinted() {
            f |= F_FOLIAGE;
        }
        if m.emission().max_elem() > 0.0 {
            f |= F_EMISSIVE;
        }
        if m.is_leaves() {
            f |= F_LEAVES;
        }
        if m == Material::Terracotta {
            f |= F_BANDED;
        }
        t.extend_from_slice(&[a.x, a.y, a.z, f as f32]);
    }
    t
}

/// Top of terrain + liquid at a world position (lattice-consistent with the chunks).
pub fn height_at(gen: &WorldGen, nodes: &mut Nodes, x: f64, z: f64) -> f32 {
    let ct = column_top(nodes, gen, x.floor() as i32, z.floor() as i32);
    ct.surface() as f32
}

/// A pleasant dry viewpoint near (x, z): (x, y, z, yaw, pitch, 0).
pub fn find_spawn(gen: &WorldGen, nodes: &mut Nodes, x: f64, z: f64) -> [f32; 6] {
    // coarse search for dry, moderately elevated, not-too-steep land with relief around it
    let step = 96.0;
    let rad = 14i32;
    let mut best = (f32::MIN, x, z);
    for j in -rad..=rad {
        for i in -rad..=rad {
            let (px, pz) = (x + i as f64 * step, z + j as f64 * step);
            let c = gen.column_lod(px, pz, 16.0);
            if c.water_level != i32::MIN && c.water_level as f32 > c.height - 1.0 {
                continue;
            }
            if c.biome.is_watery() {
                continue;
            }
            let rel = c.height - SEA_LEVEL as f32;
            // relief: how much lower the surroundings are (a lookout), sampled on a ring
            let mut lower = 0.0;
            for k in 0..8 {
                let a = k as f64 * std::f64::consts::FRAC_PI_4;
                let q = gen.column_lod(px + 260.0 * a.cos(), pz + 260.0 * a.sin(), 32.0);
                lower += (c.height - q.height.max(SEA_LEVEL as f32)).clamp(-40.0, 80.0);
            }
            let dist = ((i * i + j * j) as f32).sqrt() * step as f32;
            let score = lower / 8.0 * 1.2 + rel.clamp(0.0, 160.0) * 0.15 - (rel - 220.0).max(0.0) * 0.5 - c.slope * 25.0 - c.jaggedness * 1.5
                - dist * 0.004
                + if matches!(c.biome, Biome::Glacier | Biome::SnowyPeaks) { -20.0 } else { 0.0 };
            if score > best.0 {
                best = (score, px, pz);
            }
        }
    }
    let (_, sx, sz) = best;
    // view direction: the most open, most varied direction
    let ground = height_at(gen, nodes, sx, sz);
    let mut tmax = ground;
    for (dx, dz) in [(-6.0, -6.0), (6.0, -6.0), (-6.0, 6.0), (6.0, 6.0)] {
        tmax = tmax.max(height_at(gen, nodes, sx + dx, sz + dz));
    }
    let eye = tmax + 32.0;
    // view direction: march 24 azimuths; reward visible land (with relief), give water a
    // little credit (a coast in view is nice, an empty sea is not), punish nearby walls
    let mut best_yaw = (f32::MIN, 0.0f64, -0.12f32);
    for k in 0..24 {
        let yaw = k as f64 * std::f64::consts::TAU / 24.0;
        let (s, c) = yaw.sin_cos();
        let mut horizon = f32::MIN;
        let mut score = 0.0f32;
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        let mut mean_ang = 0.0f32;
        let n = 24;
        for d in 1..=n {
            let dist = 40.0 + d as f64 * d as f64 * 6.0; // 46 … 3496, denser near the eye
            let q = gen.column_lod(sx + s * dist, sz + c * dist, (dist / 24.0).clamp(4.0, 64.0));
            let wet = q.water_level != i32::MIN && q.water_level as f32 > q.height;
            let top = if wet { q.water_level as f32 } else { q.height };
            let ang = (top - eye) / dist as f32;
            mean_ang += ang;
            if dist < 400.0 && ang > 0.08 {
                score -= 3.0; // a slope right in front of the camera
            }
            if ang > horizon {
                // visible sample
                score += if wet { 0.3 } else { 1.0 };
                horizon = ang;
            }
            if !wet {
                lo = lo.min(q.height);
                hi = hi.max(q.height);
            }
        }
        let relief = if hi > lo { (hi - lo).min(200.0) } else { 0.0 };
        score += relief * 0.02;
        if score > best_yaw.0 {
            // look slightly below the mean terrain elevation angle of that direction
            let pitch = ((mean_ang / n as f32).atan() - 0.04).clamp(-0.35, 0.05);
            best_yaw = (score, yaw, pitch);
        }
    }
    [sx as f32, eye, sz as f32, best_yaw.1 as f32, best_yaw.2, 0.0]
}
