//! Procedural 16×16 block textures (one per material × {top, side, bottom}) with a box-filtered
//! mip chain (16, 8, 4, 2, 1).
//!
//! Every texel is a pure function of (material, face, texel coords): integer hashes give per-texel
//! grain, *tileable* value noise (lattice period dividing 16) gives soft blotches, and simple
//! analytic structure gives the recognisable patterns: radial rings `|p − c|` on log tops,
//! vertical bark grooves, a jagged grass fringe (per-column depth 2–5), Voronoi pebbles (nearest of
//! a toroidally wrapped point set) for gravel, layered sandstone, columnar basalt, molten lava
//! (bright where the noise is high → also the emission mask)…
//!
//! A texel stores `a + b·tint` (untinted part, biome-tinted part) and an emission factor, so the
//! mip chain can be averaged linearly while biome tints are still applied per hit.
use crate::materials::{srgb8, Material, MATERIAL_COUNT};
use crate::math::{lerp, saturate, smoothstep, Vec3};
use crate::noise::{hash3, hash_f01};

/// Ore overlays (coal, iron, copper, gold, lapis, diamond), stored after the material textures.
pub const ORE_COUNT: usize = 6;
pub const ORE_COLORS: [(u8, u8, u8); ORE_COUNT] =
    [(28, 28, 30), (216, 175, 147), (196, 110, 70), (250, 214, 60), (40, 70, 190), (90, 230, 220)];

pub const FACE_TOP: usize = 0;
pub const FACE_SIDE: usize = 1;
pub const FACE_BOTTOM: usize = 2;

#[derive(Clone, Copy, Debug, Default)]
pub struct Texel {
    pub a: Vec3,
    pub b: Vec3,
    pub e: f32,
}

impl Texel {
    #[inline]
    fn plain(c: Vec3) -> Texel {
        Texel { a: c, b: Vec3::ZERO, e: 0.0 }
    }
    #[inline]
    fn tinted(c: Vec3) -> Texel {
        Texel { a: Vec3::ZERO, b: c, e: 0.0 }
    }
    #[inline]
    pub fn lerp(self, o: Texel, t: f32) -> Texel {
        Texel { a: self.a.lerp(o.a, t), b: self.b.lerp(o.b, t), e: lerp(self.e, o.e, t) }
    }
    #[inline]
    pub fn color(&self, tint: Vec3) -> Vec3 {
        self.a + self.b * tint
    }
}

const MIPS: [usize; 5] = [16, 8, 4, 2, 1];
const MIP_OFF: [usize; 5] = [0, 256, 320, 336, 340];
const PER_TEX: usize = 341;

pub struct Textures {
    data: Vec<Texel>,
}

/// Generation context for one texture.
struct Gen {
    seed: u64,
}

impl Gen {
    #[inline]
    fn h(&self, x: i32, y: i32, k: i32) -> f32 {
        hash_f01(hash3(self.seed ^ (k as u64).wrapping_mul(0x9E37_79B9), x & 15, y & 15, k))
    }
    /// Tileable value noise with lattice period `p` (divides 16): smooth blobs of size 16/p.
    fn vn(&self, x: f32, y: f32, p: i32, k: i32) -> f32 {
        let s = p as f32 / 16.0;
        let (fx, fy) = (x * s, y * s);
        let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let (tx, ty) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
        let g = |a: i32, b: i32| hash_f01(hash3(self.seed ^ 0xABCD ^ k as u64, a.rem_euclid(p), b.rem_euclid(p), k));
        let a = lerp(g(ix, iy), g(ix + 1, iy), tx);
        let b = lerp(g(ix, iy + 1), g(ix + 1, iy + 1), tx);
        lerp(a, b, ty)
    }
    /// Voronoi on the 16-torus: (F1 distance, cell id) for `n` hashed points.
    fn voronoi(&self, x: f32, y: f32, n: i32, k: i32) -> (f32, i32) {
        let mut best = (1e9f32, 0i32);
        for i in 0..n {
            let px = self.h(i, 0, k + 1000) * 16.0;
            let py = self.h(i, 1, k + 1000) * 16.0;
            let mut dx = (x - px).abs();
            let mut dy = (y - py).abs();
            dx = dx.min(16.0 - dx);
            dy = dy.min(16.0 - dy);
            let d = (dx * dx + dy * dy).sqrt();
            if d < best.0 {
                best = (d, i);
            }
        }
        best
    }
}

fn c8(r: u8, g: u8, b: u8) -> Vec3 {
    srgb8(r, g, b)
}

/// Texel (x, y) of material `m` on face kind `f`; y = 0 is the TOP row for side faces.
fn texel(m: Material, f: usize, x: i32, y: i32) -> Texel {
    use Material::*;
    let g = Gen { seed: (m as u64) * 0x1000_0000_01B3 + f as u64 * 77 + 0x5EED };
    let (xf, yf) = (x as f32 + 0.5, y as f32 + 0.5);
    let grain = |amp: f32, k: i32| 1.0 + amp * (g.h(x, y, k) * 2.0 - 1.0);
    let blot = |amp: f32, p: i32, k: i32| 1.0 + amp * (g.vn(xf, yf, p, k) * 2.0 - 1.0);
    let base = m.albedo();
    let speckled = |c: Vec3, dark: f32, light: f32| {
        let s = g.h(x, y, 9);
        let k = if s < dark {
            0.72
        } else if s > 1.0 - light {
            1.18
        } else {
            1.0
        };
        c * (blot(0.10, 4, 1) * grain(0.07, 2) * k)
    };
    let dirt = |gg: &Gen| {
        let s = gg.h(x, y, 31);
        let c = c8(134, 96, 67);
        let k = if s < 0.12 {
            0.70
        } else if s > 0.9 {
            1.2
        } else {
            1.0
        };
        c * (k * (1.0 + 0.08 * (gg.vn(xf, yf, 4, 32) * 2.0 - 1.0)) * (1.0 + 0.06 * (gg.h(x, y, 33) * 2.0 - 1.0)))
    };
    match m {
        Air | Water => Texel::plain(base),
        Stone | Andesite => {
            let mut c = speckled(base, 0.08, 0.05);
            if m == Andesite {
                let sw = g.vn(xf + yf * 0.5, yf, 4, 5);
                c = c * (0.9 + 0.25 * smoothstep(0.55, 0.8, sw));
            }
            Texel::plain(c)
        }
        Deepslate => {
            let row = g.h(0, y, 3);
            let mut c = base * (0.88 + 0.2 * row) * grain(0.06, 2);
            if f == FACE_SIDE && g.h(x / 3, y, 4) < 0.15 {
                c = c * 0.75;
            }
            if f != FACE_SIDE && (x % 8 == 0 || y % 8 == 0) {
                c = c * 0.8;
            }
            Texel::plain(c)
        }
        Bedrock => {
            let v = g.vn(xf, yf, 8, 1) * 0.7 + g.h(x, y, 2) * 0.3;
            Texel::plain(if v > 0.5 { c8(105, 105, 105) } else { c8(38, 38, 40) } * grain(0.1, 3))
        }
        Granite => {
            let s = g.h(x, y, 5);
            let c = if s < 0.18 {
                c8(110, 72, 60)
            } else if s > 0.86 {
                c8(190, 150, 132)
            } else {
                base
            };
            Texel::plain(c * blot(0.08, 4, 1) * grain(0.05, 2))
        }
        Calcite => Texel::plain(base * blot(0.05, 4, 1) * grain(0.03, 2) * if g.h(x, y, 7) < 0.06 { 0.85 } else { 1.0 }),
        Dirt => Texel::plain(dirt(&g)),
        Grass => {
            let blade = |k: i32| {
                let s = g.h(x, y, k);
                (0.78 + 0.22 * s) * blot(0.08, 4, k + 1) * if s < 0.1 { 0.8 } else { 1.0 }
            };
            match f {
                FACE_TOP => Texel::tinted(Vec3::splat(blade(11))),
                FACE_SIDE => {
                    let depth = 2 + (g.h(x, 0, 12) * 3.2) as i32 + if g.h(x, 1, 13) < 0.25 { 2 } else { 0 };
                    if y < depth {
                        Texel::tinted(Vec3::splat(blade(14)))
                    } else if y == depth && g.h(x, y, 15) < 0.5 {
                        Texel::plain(dirt(&g) * 0.8)
                    } else {
                        Texel::plain(dirt(&g))
                    }
                }
                _ => Texel::plain(dirt(&g)),
            }
        }
        Podzol => match f {
            FACE_TOP => {
                let s = g.h(x, y, 3);
                let c = if s < 0.25 { c8(122, 82, 36) } else if s > 0.85 { c8(70, 46, 18) } else { base };
                Texel::plain(c * blot(0.1, 4, 1))
            }
            FACE_SIDE => {
                let depth = 2 + (g.h(x, 0, 12) * 2.5) as i32;
                if y < depth {
                    Texel::plain(base * grain(0.12, 4))
                } else {
                    Texel::plain(dirt(&g))
                }
            }
            _ => Texel::plain(dirt(&g)),
        },
        Mud => Texel::plain(base * blot(0.12, 4, 1) * grain(0.05, 2)),
        Moss => Texel::tinted(Vec3::splat(0.75 + 0.25 * g.vn(xf, yf, 8, 3)) * grain(0.08, 4)),
        Sand | RedSand => {
            let s = g.h(x, y, 3);
            let k = if s < 0.07 { 0.8 } else if s > 0.94 { 1.08 } else { 1.0 };
            Texel::plain(base * (k * grain(0.05, 2) * blot(0.04, 4, 1)))
        }
        Sandstone | RedSandstone => {
            let c = match f {
                FACE_SIDE => {
                    let band = if y < 3 {
                        1.06
                    } else if y == 3 || y == 12 {
                        0.86
                    } else {
                        0.96 + 0.06 * g.h(0, y, 5)
                    };
                    base * band * grain(0.03, 2)
                }
                _ => base * grain(0.03, 2) * blot(0.04, 4, 1),
            };
            Texel::plain(c)
        }
        Gravel => {
            let (d, id) = g.voronoi(xf, yf, 14, 1);
            let tone = 0.55 + 0.75 * g.h(id, 7, 2);
            let edge = smoothstep(1.4, 2.6, d);
            Texel::plain(base * tone * (1.0 - 0.35 * edge) * grain(0.05, 3))
        }
        Clay => Texel::plain(base * grain(0.03, 2) * blot(0.04, 4, 1)),
        Snow => Texel::plain(base * if g.h(x, y, 2) < 0.08 { 0.93 } else { 1.0 } * blot(0.02, 4, 1)),
        Ice | PackedIce => {
            let streak = ((xf + yf * 0.7 + 3.0 * g.vn(xf, yf, 4, 1)) * 0.6).sin();
            let crack = smoothstep(0.93, 0.99, streak.abs());
            let c = base * (0.95 + 0.08 * g.vn(xf, yf, 4, 2)) + Vec3::splat(0.25) * crack;
            Texel::plain(c)
        }
        Lava => {
            let n = g.vn(xf, yf, 4, 1) * 0.65 + g.vn(xf, yf, 8, 2) * 0.35;
            let hot = smoothstep(0.35, 0.75, n);
            let c = c8(150, 40, 8).lerp(c8(255, 190, 60), hot);
            Texel { a: c, b: Vec3::ZERO, e: 0.35 + 1.4 * hot }
        }
        Terracotta => Texel::plain(Vec3::splat(0.96 + 0.05 * g.h(x, y, 2) - if g.h(x, y, 3) < 0.05 { 0.08 } else { 0.0 })),
        OakLog | BirchLog | SpruceLog | JungleLog | AcaciaLog | DarkOakLog => {
            let bark = base;
            if f == FACE_SIDE {
                if m == BirchLog {
                    let dash = g.h(x / 3, y, 4) < 0.12 || (g.h(x / 2, y / 2, 5) < 0.05);
                    Texel::plain(if dash { c8(40, 40, 38) } else { bark * grain(0.04, 2) })
                } else {
                    let col = g.vn(xf, yf * 0.25, 8, 3);
                    let groove = if g.h(x, 0, 4) < 0.3 { 0.7 } else { 1.0 };
                    Texel::plain(bark * (0.8 + 0.35 * col) * groove * grain(0.06, 2))
                }
            } else {
                let inner = match m {
                    OakLog => c8(176, 141, 87),
                    BirchLog => c8(193, 179, 135),
                    SpruceLog => c8(115, 85, 52),
                    JungleLog => c8(170, 121, 84),
                    AcaciaLog => c8(169, 92, 51),
                    _ => c8(66, 43, 20),
                };
                let (dx, dy) = (xf - 8.0, yf - 8.0);
                let r = (dx * dx + dy * dy).sqrt();
                if x == 0 || y == 0 || x == 15 || y == 15 {
                    Texel::plain(bark * grain(0.06, 2))
                } else {
                    let ring = 0.88 + 0.14 * (r * 1.9 + g.h(x, y, 3) * 0.8).sin();
                    Texel::plain(inner * ring * grain(0.03, 2))
                }
            }
        }
        OakLeaves | BirchLeaves | SpruceLeaves | JungleLeaves | AcaciaLeaves | DarkOakLeaves => {
            let s = g.h(x, y, 3);
            let clump = g.vn(xf, yf, 8, 1);
            let mut k = (0.62 + 0.5 * clump) * (0.85 + 0.3 * g.h(x, y, 4));
            if s < 0.16 {
                k *= 0.32; // gaps
            } else if s > 0.92 {
                k *= 1.2;
            }
            let c = base * k;
            if m.foliage_tinted() {
                Texel::tinted(c)
            } else {
                Texel::plain(c)
            }
        }
        Cactus => {
            if f == FACE_SIDE {
                let stripe = if x % 4 == 0 { 0.72 } else { 1.0 };
                let spine = if (x % 4 == 2) && (y % 5 == 1) { 1.6 } else { 1.0 };
                Texel::plain(base * stripe * spine * grain(0.05, 2))
            } else {
                let r = ((xf - 8.0).powi(2) + (yf - 8.0).powi(2)).sqrt();
                Texel::plain(base * (0.85 + 0.2 * (r * 1.3).sin()) * grain(0.04, 2))
            }
        }
        Crystal => {
            let facet = ((xf * 0.7 + yf * 0.9) * 0.9 + 2.0 * g.vn(xf, yf, 4, 1)).sin();
            let k = 0.7 + 0.45 * facet.abs();
            Texel { a: base * k, b: Vec3::ZERO, e: 0.5 + 0.9 * facet.abs() }
        }
        Basalt => {
            let c = if f == FACE_SIDE {
                let line = if x % 5 == 0 { 0.65 } else { 1.0 };
                base * line * (0.92 + 0.12 * g.vn(xf, yf * 0.3, 4, 1)) * grain(0.05, 2)
            } else {
                let (d, id) = g.voronoi(xf, yf, 7, 4);
                let _ = d;
                let (d2, _) = g.voronoi(xf + 0.0, yf, 7, 4);
                base * (0.9 + 0.15 * g.h(id, 1, 5)) * (1.0 - 0.3 * smoothstep(3.0, 4.5, d2))
            };
            Texel::plain(c)
        }
        Obsidian => {
            let s = g.h(x, y, 2);
            let c = if s > 0.9 { c8(62, 40, 96) } else { base * blot(0.2, 4, 1) };
            Texel::plain(c)
        }
    }
}

/// Ore overlay texel: `a` = ore colour × coverage, `e` = coverage (blend weight).
fn ore_texel(k: usize, x: i32, y: i32) -> Texel {
    let g = Gen { seed: 0x0BE0 + k as u64 * 977 };
    // A few blobs (Voronoi cells around hashed centres) of 1–3 texel radius.
    let (d, id) = g.voronoi(x as f32 + 0.5, y as f32 + 0.5, 6, 3);
    let r = 0.9 + 1.2 * g.h(id, 2, 4);
    if d < r {
        let (cr, cg, cb) = ORE_COLORS[k];
        let shade = 0.75 + 0.45 * g.h(x, y, 5);
        Texel { a: c8(cr, cg, cb) * shade, b: Vec3::ZERO, e: 1.0 }
    } else {
        Texel::default()
    }
}

impl Textures {
    pub fn new() -> Textures {
        let ntex = MATERIAL_COUNT * 3 + ORE_COUNT;
        let mut data = vec![Texel::default(); ntex * PER_TEX];
        for ti in 0..ntex {
            let (mi, f) = (ti / 3, ti % 3);
            {
                let base = ti * PER_TEX;
                for y in 0..16 {
                    for x in 0..16 {
                        data[base + y * 16 + x] = if mi < MATERIAL_COUNT {
                            texel(Material::from_u8(mi as u8), f, x as i32, y as i32)
                        } else {
                            ore_texel(ti - MATERIAL_COUNT * 3, x as i32, y as i32)
                        };
                    }
                }
                for lv in 1..5 {
                    let s = MIPS[lv];
                    let ps = MIPS[lv - 1];
                    for y in 0..s {
                        for x in 0..s {
                            let p = |xx: usize, yy: usize| data[base + MIP_OFF[lv - 1] + yy * ps + xx];
                            let (a, b, c, d) = (p(2 * x, 2 * y), p(2 * x + 1, 2 * y), p(2 * x, 2 * y + 1), p(2 * x + 1, 2 * y + 1));
                            data[base + MIP_OFF[lv] + y * s + x] = Texel {
                                a: (a.a + b.a + c.a + d.a) * 0.25,
                                b: (a.b + b.b + c.b + d.b) * 0.25,
                                e: (a.e + b.e + c.e + d.e) * 0.25,
                            };
                        }
                    }
                }
            }
        }
        Textures { data }
    }

    /// Nearest texel at mip level `lv` for face coordinates (u, v) ∈ [0,1)².
    #[inline]
    fn fetch(&self, base: usize, lv: usize, u: f32, v: f32) -> Texel {
        let s = MIPS[lv];
        let x = ((u * s as f32) as usize).min(s - 1);
        let y = ((v * s as f32) as usize).min(s - 1);
        self.data[base + MIP_OFF[lv] + y * s + x]
    }

    /// Trilinear-in-mip (nearest within a level) sample. `lod` = log2(texels per pixel).
    #[inline]
    pub fn sample(&self, m: Material, face: usize, u: f32, v: f32, lod: f32) -> Texel {
        let base = (m as usize * 3 + face) * PER_TEX;
        let l = lod.clamp(0.0, 4.0);
        let l0 = (l as usize).min(3);
        let w = saturate(l - l0 as f32);
        if lod <= 0.0 {
            return self.fetch(base, 0, u, v);
        }
        let a = self.fetch(base, l0, u, v);
        let b = self.fetch(base, l0 + 1, u, v);
        a.lerp(b, w)
    }

    /// Ore overlay sample (a = colour × coverage, e = coverage).
    #[inline]
    pub fn sample_ore(&self, k: usize, u: f32, v: f32, lod: f32) -> Texel {
        let base = (MATERIAL_COUNT * 3 + k) * PER_TEX;
        let l = lod.clamp(0.0, 4.0);
        let l0 = (l as usize).min(3);
        let w = saturate(l - l0 as f32);
        if lod <= 0.0 {
            return self.fetch(base, 0, u, v);
        }
        self.fetch(base, l0, u, v).lerp(self.fetch(base, l0 + 1, u, v), w)
    }

    /// 1×1 average of a face texture.
    #[inline]
    pub fn average(&self, m: Material, face: usize) -> Texel {
        self.data[(m as usize * 3 + face) * PER_TEX + MIP_OFF[4]]
    }
}

impl Default for Textures {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_deterministic_and_averages_sane() {
        let a = Textures::new();
        let b = Textures::new();
        for mi in 1..MATERIAL_COUNT {
            let m = Material::from_u8(mi as u8);
            for f in 0..3 {
                let ta = a.average(m, f);
                let tb = b.average(m, f);
                assert_eq!(ta.a, tb.a);
                let c = ta.color(Vec3::ONE);
                assert!(c.is_finite() && c.max_elem() < 1.5 && c.min_elem() >= 0.0, "{:?} {:?}", m, c);
                // Texture mean stays close to the material albedo (except multi-part textures).
                if !matches!(m, Material::Terracotta | Material::Grass | Material::Podzol | Material::Lava | Material::Bedrock)
                    && !m.is_log()
                    && f == FACE_TOP
                {
                    let alb = if m.grass_tinted() || m.foliage_tinted() { m.albedo() } else { m.albedo() };
                    let rel = c.luminance() / alb.luminance().max(1e-3);
                    assert!(rel > 0.6 && rel < 1.3, "{:?} mean {:?} vs {:?}", m, c, alb);
                }
            }
        }
    }
}
