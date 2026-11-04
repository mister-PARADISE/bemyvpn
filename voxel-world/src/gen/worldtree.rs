//! World trees: ultra-rare colossal landmark trees (250–450 blocks tall, crowns up to ≈ 110 blocks
//! in radius) — far beyond the rasterised feature system (`MAX_FEATURE_RADIUS` = 24), so they are
//! *implicit solids* sampled per voxel by the chunk / LOD builders, like the terrain.
//!
//! # Placement
//! One candidate cell per `WT_CELL`² (8192²): with probability `WT_P` the cell owns a tree; up to
//! `WT_TRIES` hashed positions in the inner 60 % of the cell are probed and the first acceptable
//! one wins (dry, gentle ground of a forested / open biome, level surroundings). Trees of adjacent
//! cells are therefore ≥ 0.4 cells apart, and the decision is a pure function of (seed, cell).
//!
//! # Shape (all in tree-local coordinates: root column centre, y from the ground)
//! * **Trunk**: a leaning, gently wobbling spine `c(y)`; radius `R₀(1 − 0.45 (y/H_t)^1.2)` plus
//!   buttress roots `1.1 R₀ e^{−y/9} ((1 + cos(Nθ + φ))/2)^{2.5}` that flare into the ground, fine
//!   bark ridges; a hollow chamber with an arched doorway at the base (fly in!).
//! * **Broad crown** (oak / dark oak / birch): 6–8 main limbs at golden-angle azimuths, each
//!   splitting into 2–3 sub-limbs (tapered capsules), with noisy ellipsoid leaf clouds on every tip.
//! * **Umbrella crown** (jungle / acacia): a tall trunk and near-horizontal limbs carrying flat
//!   leaf discs — a parasol 160–220 blocks across.
//! * **Conifer**: trunk to the top, layered cone canopy `R(y) = R_b (1−u)^0.85 · tier · (1 +
//!   0.12 cos(5θ + ψ))`, tiers wider at their base.
//! Logs override everything; leaves fill air only (done by the caller).
use super::{Biome, WorldGen, WORLD_HEIGHT};
use crate::materials::Material;
use crate::noise::{hash2, hash_f01, mix64};
use std::cell::RefCell;
use std::rc::Rc;

/// Cell size of the world-tree process (blocks).
pub const WT_CELL: i32 = 8192;
/// Probability that a cell owns a world tree (if a suitable spot is found).
const WT_P: f32 = 0.45;
const WT_TRIES: u32 = 12;
/// Upper bound of the horizontal reach of any world tree from its root (crown radius + lean).
pub const WT_REACH: f32 = 210.0;
const GOLDEN: f32 = 2.399_963_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WtKind {
    Broad,
    Umbrella,
    Conifer,
}

#[derive(Clone, Copy, Debug)]
struct Capsule {
    a: [f32; 3],
    b: [f32; 3],
    ra: f32,
    rb: f32,
    bb: [f32; 6],
}

#[derive(Clone, Copy, Debug)]
struct Blob {
    c: [f32; 3],
    /// inverse radii (x, y, z)
    ir: [f32; 3],
    bb: [f32; 6],
}

/// One world tree (immutable once built).
#[derive(Clone, Debug)]
pub struct WorldTree {
    /// Root column (world block coordinates) and ground height.
    pub x: i32,
    pub z: i32,
    pub y0: f32,
    pub kind: WtKind,
    pub height: f32,
    pub log: Material,
    pub leaf: Material,
    /// World-space AABB [x0, y0, z0, x1, y1, z1].
    pub bbox: [f32; 6],
    seed: u64,
    r0: f32,
    trunk_top: f32,
    lean: (f32, f32),
    wob: (f32, f32, f32),
    butt_n: f32,
    butt_phi: f32,
    door: f32,
    hollow_h: f32,
    caps: Vec<Capsule>,
    blobs: Vec<Blob>,
    /// conifer canopy: (base y, base radius, tier period, tier phase, lobe phase)
    cone: Option<(f32, f32, f32, f32, f32)>,
}

/// Result of a voxel sample.
pub const WT_NONE: u8 = 0;
pub const WT_LOG: u8 = 1;
pub const WT_LEAF: u8 = 2;

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = mix64(self.0.wrapping_add(0x9E37_79B9_7F4A_7C15));
        hash_f01(self.0)
    }
    fn r(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

fn eligible(b: Biome) -> Option<(WtKind, Material, Material)> {
    use Biome::*;
    Some(match b {
        Forest | Plains | Meadow => (WtKind::Broad, Material::OakLog, Material::OakLeaves),
        DarkForest => (WtKind::Broad, Material::DarkOakLog, Material::DarkOakLeaves),
        BirchForest => (WtKind::Broad, Material::BirchLog, Material::BirchLeaves),
        Taiga | OldGrowthTaiga | SnowyTaiga => (WtKind::Conifer, Material::SpruceLog, Material::SpruceLeaves),
        Jungle => (WtKind::Umbrella, Material::JungleLog, Material::JungleLeaves),
        Savanna => (WtKind::Umbrella, Material::AcaciaLog, Material::AcaciaLeaves),
        _ => return None,
    })
}

impl WorldTree {
    /// The tree of cell (ix, iz), if any (pure function of the seed and the cell).
    fn decide(gen: &WorldGen, ix: i32, iz: i32) -> Option<WorldTree> {
        let id = hash2(gen.seed ^ 0x3017_7EE5, ix, iz);
        if hash_f01(id) >= WT_P {
            return None;
        }
        for t in 0..WT_TRIES {
            let h = mix64(id ^ (t as u64 + 1).wrapping_mul(0xA24B_AED4_963E_E407));
            let fx = 0.2 + 0.6 * hash_f01(h) as f64;
            let fz = 0.2 + 0.6 * hash_f01(mix64(h ^ 5)) as f64;
            let x = ((ix as f64 + fx) * WT_CELL as f64).floor() as i32;
            let z = ((iz as f64 + fz) * WT_CELL as f64).floor() as i32;
            let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
            let Some((kind, log, leaf)) = eligible(col.biome) else { continue };
            if col.slope > 0.3 || col.jaggedness > 2.0 || col.climate.river > 0.15 {
                continue;
            }
            if col.water_level != i32::MIN && col.height < col.water_level as f32 + 1.0 {
                continue;
            }
            // level surroundings (the buttresses must not hang over a cliff or into a lake)
            let mut ok = true;
            for k in 0..8 {
                let a = k as f64 * std::f64::consts::FRAC_PI_4;
                let c = gen.column(x as f64 + 0.5 + 26.0 * a.cos(), z as f64 + 0.5 + 26.0 * a.sin());
                if (c.height - col.height).abs() > 7.0 || (c.water_level != i32::MIN && c.water_level as f32 > c.height) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            return Some(Self::build(mix64(h ^ 0x7EE), x, z, col.height, kind, log, leaf));
        }
        None
    }

    fn build(seed: u64, x: i32, z: i32, y0: f32, kind: WtKind, log: Material, leaf: Material) -> WorldTree {
        let mut g = Rng(seed);
        let room = WORLD_HEIGHT as f32 - 14.0 - y0;
        let (h, r0, crown) = match kind {
            WtKind::Broad => (g.r(240.0, 330.0), g.r(10.0, 14.0), g.r(78.0, 105.0)),
            WtKind::Umbrella => (g.r(250.0, 340.0), g.r(8.0, 11.0), g.r(82.0, 110.0)),
            WtKind::Conifer => (g.r(330.0, 450.0), g.r(9.0, 13.0), g.r(52.0, 72.0)),
        };
        let h = h.min(room).max(120.0);
        let trunk_top = match kind {
            WtKind::Broad => h * g.r(0.42, 0.5),
            WtKind::Umbrella => h * g.r(0.74, 0.82),
            WtKind::Conifer => h * 0.97,
        };
        let la = g.r(0.0, std::f32::consts::TAU);
        let lm = g.r(0.0, 0.06);
        let mut t = WorldTree {
            x,
            z,
            y0,
            kind,
            height: h,
            log,
            leaf,
            bbox: [0.0; 6],
            seed,
            r0,
            trunk_top,
            lean: (lm * la.cos(), lm * la.sin()),
            wob: (g.r(4.0, 9.0), g.r(90.0, 160.0), g.r(0.0, 6.283)),
            butt_n: (g.r(5.0, 8.0)).floor(),
            butt_phi: g.r(0.0, 6.283),
            door: g.r(0.0, 6.283),
            hollow_h: g.r(22.0, 34.0),
            caps: Vec::new(),
            blobs: Vec::new(),
            cone: None,
        };
        let top = t.axis(trunk_top);
        let top = [top.0, trunk_top, top.1];
        let cap = |a: [f32; 3], b: [f32; 3], ra: f32, rb: f32| -> Capsule {
            let m = ra.max(rb) + 1.0;
            Capsule { a, b, ra, rb, bb: [a[0].min(b[0]) - m, a[1].min(b[1]) - m, a[2].min(b[2]) - m, a[0].max(b[0]) + m, a[1].max(b[1]) + m, a[2].max(b[2]) + m] }
        };
        let blob = |c: [f32; 3], rx: f32, ry: f32| -> Blob {
            let (ex, ey) = (rx * 1.36 + 1.0, ry * 1.36 + 1.0);
            Blob { c, ir: [1.0 / rx, 1.0 / ry, 1.0 / rx], bb: [c[0] - ex, c[1] - ey, c[2] - ex, c[0] + ex, c[1] + ey, c[2] + ex] }
        };
        match kind {
            WtKind::Broad | WtKind::Umbrella => {
                let umb = kind == WtKind::Umbrella;
                let n = if umb { g.r(7.0, 10.0) } else { g.r(6.0, 9.0) }.floor() as usize;
                let az0 = g.r(0.0, 6.283);
                let top_r = r0 * 0.58;
                for i in 0..n {
                    let az = az0 + i as f32 * GOLDEN + g.r(-0.25, 0.25);
                    let ys = trunk_top * if umb { g.r(0.93, 1.0) } else { g.r(0.72, 1.0) };
                    let sp = t.axis(ys);
                    let el = if umb { g.r(0.12, 0.38) } else { g.r(0.45, 0.95) };
                    let len = crown * if umb { g.r(0.55, 0.72) } else { g.r(0.45, 0.62) };
                    let a = [sp.0, ys, sp.1];
                    let b = [a[0] + len * el.cos() * az.cos(), a[1] + len * el.sin(), a[2] + len * el.cos() * az.sin()];
                    let (ra, rb) = (top_r * g.r(0.8, 1.0), top_r * 0.45);
                    t.caps.push(cap(a, b, ra, rb));
                    // a cloud halfway along the big limbs (fills the crown interior)
                    let mid = [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]) + 6.0, 0.5 * (a[2] + b[2])];
                    let mr = crown * g.r(0.22, 0.28);
                    t.blobs.push(blob(mid, mr, mr * if umb { 0.32 } else { 0.6 }));
                    let ns = if umb { 2 } else { 2 + (g.f() < 0.5) as usize };
                    for k in 0..ns {
                        let az2 = az + g.r(-0.85, 0.85) + if k == 0 { -0.3 } else { 0.3 };
                        let el2 = if umb { g.r(0.0, 0.22) } else { g.r(0.15, 0.7) };
                        let l2 = crown * if umb { g.r(0.3, 0.42) } else { g.r(0.3, 0.45) };
                        let s = if k == 2 { [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2])] } else { b };
                        let e = [s[0] + l2 * el2.cos() * az2.cos(), s[1] + l2 * el2.sin(), s[2] + l2 * el2.cos() * az2.sin()];
                        t.caps.push(cap(s, e, rb, g.r(1.4, 2.4)));
                        let rr = crown * g.r(0.24, 0.32);
                        t.blobs.push(blob([e[0], e[1] + rr * 0.2, e[2]], rr, rr * if umb { 0.34 } else { g.r(0.55, 0.7) }));
                    }
                }
                // crown top / centre
                let rr = crown * if umb { 0.42 } else { 0.4 };
                let cy = if umb { trunk_top + crown * 0.16 } else { trunk_top + (h - trunk_top) * 0.4 };
                t.blobs.push(blob([top[0], cy, top[2]], rr, rr * if umb { 0.3 } else { 0.75 }));
                // leader: the trunk continues into the crown centre (no floating top cloud)
                t.caps.push(cap(top, [top[0], cy, top[2]], top_r * 0.9, top_r * 0.4));
                // scale the crown so its top reaches the tree height
                let ytop = t.blobs.iter().map(|b| b.c[1] + 1.0 / b.ir[1]).fold(0.0f32, f32::max);
                if ytop > h + 4.0 {
                    let k = (h - trunk_top) / (ytop - trunk_top).max(1.0);
                    for b in &mut t.blobs {
                        b.c[1] = trunk_top + (b.c[1] - trunk_top) * k;
                        let ey = 1.36 / b.ir[1] + 1.0;
                        b.bb[1] = b.c[1] - ey;
                        b.bb[4] = b.c[1] + ey;
                    }
                    for c in &mut t.caps {
                        for p in [&mut c.a, &mut c.b] {
                            if p[1] > trunk_top {
                                p[1] = trunk_top + (p[1] - trunk_top) * k;
                            }
                        }
                        let m = c.ra.max(c.rb) + 1.0;
                        c.bb[1] = c.a[1].min(c.b[1]) - m;
                        c.bb[4] = c.a[1].max(c.b[1]) + m;
                    }
                }
            }
            WtKind::Conifer => {
                let base = h * g.r(0.22, 0.3);
                t.cone = Some((base, crown, g.r(16.0, 22.0), g.r(0.0, 1.0), g.r(0.0, 6.283)));
            }
        }
        // AABB (local → world)
        let mut bb = [-(r0 * 2.4 + 2.0), -12.0, -(r0 * 2.4 + 2.0), r0 * 2.4 + 2.0, trunk_top + 2.0, r0 * 2.4 + 2.0];
        let tl = t.axis(trunk_top);
        bb[0] = bb[0].min(tl.0 - r0);
        bb[3] = bb[3].max(tl.0 + r0);
        bb[2] = bb[2].min(tl.1 - r0);
        bb[5] = bb[5].max(tl.1 + r0);
        for c in &t.caps {
            for k in 0..3 {
                bb[k] = bb[k].min(c.bb[k]);
                bb[k + 3] = bb[k + 3].max(c.bb[k + 3]);
            }
        }
        for b in &t.blobs {
            for k in 0..3 {
                bb[k] = bb[k].min(b.bb[k]);
                bb[k + 3] = bb[k + 3].max(b.bb[k + 3]);
            }
        }
        if let Some((_, rb, _, _, _)) = t.cone {
            let e = rb * 1.15 + 2.0;
            bb[0] = bb[0].min(-e + tl.0.min(0.0));
            bb[2] = bb[2].min(-e + tl.1.min(0.0));
            bb[3] = bb[3].max(e + tl.0.max(0.0));
            bb[5] = bb[5].max(e + tl.1.max(0.0));
            bb[4] = bb[4].max(h + 2.0);
        }
        let (ox, oz) = (x as f32 + 0.5, z as f32 + 0.5);
        t.bbox = [bb[0] + ox, bb[1] + y0, bb[2] + oz, bb[3] + ox, bb[4] + y0, bb[5] + oz];
        t
    }

    /// Trunk axis offset (local x, z) at height y.
    #[inline]
    fn axis(&self, y: f32) -> (f32, f32) {
        let y = y.max(0.0);
        let w = self.wob.0 * (y / self.wob.1 + self.wob.2).sin() * (y / (self.trunk_top + 1.0)).min(1.0);
        (self.lean.0 * y + w * self.wob.2.cos(), self.lean.1 * y + w * self.wob.2.sin())
    }

    /// Does the tree's AABB (inflated by `e`) intersect the world box?
    pub fn intersects(&self, x0: f32, z0: f32, x1: f32, z1: f32, e: f32) -> bool {
        self.bbox[0] - e < x1 && self.bbox[3] + e > x0 && self.bbox[2] - e < z1 && self.bbox[5] + e > z0
    }

    /// Indices of the primitives (capsules, then blobs offset by 1<<15) whose AABB intersects the
    /// world column box [x0, x1) × [z0, z1) (inflated by `e`), and the world y range of the trunk +
    /// primitives there.
    pub fn prims_in(&self, x0: f32, z0: f32, x1: f32, z1: f32, e: f32) -> (Vec<u16>, f32, f32) {
        let (ox, oz) = (self.x as f32 + 0.5, self.z as f32 + 0.5);
        let (lx0, lz0, lx1, lz1) = (x0 - ox - e, z0 - oz - e, x1 - ox + e, z1 - oz + e);
        let mut v = Vec::new();
        let (mut ylo, mut yhi) = (f32::MAX, f32::MIN);
        let hit = |bb: &[f32; 6]| bb[0] < lx1 && bb[3] > lx0 && bb[2] < lz1 && bb[5] > lz0;
        for (i, c) in self.caps.iter().enumerate() {
            if hit(&c.bb) {
                v.push(i as u16);
                ylo = ylo.min(c.bb[1]);
                yhi = yhi.max(c.bb[4]);
            }
        }
        for (i, b) in self.blobs.iter().enumerate() {
            if hit(&b.bb) {
                v.push(i as u16 | 0x8000);
                ylo = ylo.min(b.bb[1]);
                yhi = yhi.max(b.bb[4]);
            }
        }
        // trunk / buttresses / cone
        let tr = self.r0 * 2.4 + 2.0 + self.wob.0 + self.lean.0.abs().max(self.lean.1.abs()) * self.trunk_top;
        let cr = self.cone.map_or(0.0, |c| c.1 * 1.15 + 2.0);
        let rr = tr.max(cr);
        if rr > lx0 && -rr < lx1 && rr > lz0 && -rr < lz1 {
            ylo = ylo.min(-12.0);
            yhi = yhi.max(if self.cone.is_some() { self.height + 2.0 } else { self.trunk_top + 2.0 });
        }
        (v, ylo + self.y0, yhi + self.y0)
    }

    /// Sample the solid at world point (x, y, z) (voxel centre). `e` inflates thin parts for
    /// coarse LOD voxels (≈ 0.35 × voxel size); `fine` enables the doorway and the hollow.
    pub fn sample(&self, x: f32, y: f32, z: f32, prims: &[u16], e: f32, fine: bool) -> u8 {
        let p = [x - (self.x as f32 + 0.5), y - self.y0, z - (self.z as f32 + 0.5)];
        // --- trunk
        if p[1] >= -12.0 && p[1] <= self.trunk_top + 1.0 {
            let (ax, az) = self.axis(p[1]);
            let (dx, dz) = (p[0] - ax, p[2] - az);
            let d2 = dx * dx + dz * dz;
            let rmax = self.r0 * 2.4 + 2.0 + e;
            if d2 < rmax * rmax {
                let d = d2.sqrt();
                let yy = p[1].max(0.0);
                let tt = (yy / self.trunk_top).min(1.0);
                let taper = if self.cone.is_some() { 1.0 - 0.88 * tt } else { 1.0 - 0.45 * tt.powf(1.2) };
                let th = dz.atan2(dx);
                let lobe = (0.5 + 0.5 * (self.butt_n * th + self.butt_phi + 0.012 * yy).cos()).powf(2.5);
                let butt = 1.1 * self.r0 * (-yy / 9.0).exp() * lobe;
                let bark = 0.45 * (23.0 * th + 0.07 * yy).cos();
                let r = self.r0 * taper + butt + bark + e;
                if d < r {
                    if fine && p[1] < self.hollow_h && p[1] > -1.0 {
                        // hollow chamber + arched doorway facing `door`
                        let ri = self.r0 * taper * 0.62;
                        if d < ri - 0.5 * p[1].max(0.0) / self.hollow_h * ri * 0.6 {
                            return WT_NONE;
                        }
                        let mut da = (th - self.door).rem_euclid(std::f32::consts::TAU);
                        if da > std::f32::consts::PI {
                            da -= std::f32::consts::TAU;
                        }
                        let w = 0.28 * (1.0 - (p[1] / 16.0).powi(2)).max(0.0).sqrt();
                        if da.abs() < w && d > ri * 0.5 {
                            return WT_NONE;
                        }
                    }
                    return WT_LOG;
                }
            }
        }
        // --- limbs (capsules)
        let mut leaf = false;
        for &i in prims {
            if i & 0x8000 == 0 {
                let c = &self.caps[i as usize];
                if p[0] < c.bb[0] - e || p[0] > c.bb[3] + e || p[1] < c.bb[1] - e || p[1] > c.bb[4] + e || p[2] < c.bb[2] - e || p[2] > c.bb[5] + e {
                    continue;
                }
                let ab = [c.b[0] - c.a[0], c.b[1] - c.a[1], c.b[2] - c.a[2]];
                let ap = [p[0] - c.a[0], p[1] - c.a[1], p[2] - c.a[2]];
                let l2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
                let tt = ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / l2.max(1e-6)).clamp(0.0, 1.0);
                let q = [ap[0] - ab[0] * tt, ap[1] - ab[1] * tt, ap[2] - ab[2] * tt];
                let r = c.ra + (c.rb - c.ra) * tt + e;
                if q[0] * q[0] + q[1] * q[1] + q[2] * q[2] < r * r {
                    return WT_LOG;
                }
            } else if !leaf {
                let b = &self.blobs[(i & 0x7FFF) as usize];
                if p[0] < b.bb[0] - e || p[0] > b.bb[3] + e || p[1] < b.bb[1] - e || p[1] > b.bb[4] + e || p[2] < b.bb[2] - e || p[2] > b.bb[5] + e {
                    continue;
                }
                let q = [(p[0] - b.c[0]) * b.ir[0], (p[1] - b.c[1]) * b.ir[1], (p[2] - b.c[2]) * b.ir[2]];
                let rho = q[0] * q[0] + q[1] * q[1] + q[2] * q[2];
                if rho < 1.80 {
                    // lumpy cloud surface: two cheap products of sines (n ∈ [−1.5, 1.5] → ±0.33, inside the AABB)
                    let s = hash_f01(self.seed) * 97.0;
                    let n = (p[0] * 0.21 + s).sin() * (p[1] * 0.27 + 1.3).sin() * (p[2] * 0.19 - s).sin()
                        + 0.5 * (p[0] * 0.53 - 0.7).sin() * (p[1] * 0.47 + s).sin() * (p[2] * 0.61 + 2.1).sin();
                    let lim = (1.0 + 0.22 * n).min(1.34) + e * b.ir[0] * 2.0;
                    if rho < lim * lim {
                        leaf = true;
                    }
                }
            }
        }
        // --- conifer canopy
        if !leaf {
            if let Some((base, rb, period, phase, lph)) = self.cone {
                if p[1] >= base && p[1] <= self.height + 1.0 {
                    let (ax, az) = self.axis(p[1]);
                    let (dx, dz) = (p[0] - ax, p[2] - az);
                    let d2 = dx * dx + dz * dz;
                    let u = ((p[1] - base) / (self.height - base)).clamp(0.0, 1.0);
                    let rmax = rb * 1.12 * (1.0 - u).powf(0.85) + 2.0 + e;
                    if d2 < rmax * rmax {
                        let fr = ((p[1] - base) / period + phase).rem_euclid(1.0);
                        let tier = 0.58 + 0.42 * (1.0 - fr);
                        let th = dz.atan2(dx);
                        let lobe = 1.0 + 0.12 * (5.0 * th + lph + 2.3 * ((p[1] - base) / period).floor()).cos();
                        let r = rb * (1.0 - u).powf(0.85) * tier * lobe + 1.0 + e;
                        if d2 < r * r {
                            leaf = true;
                        }
                    }
                }
            }
        }
        if leaf {
            // (no porosity holes: inside a crown this big every hole would expose six leaf faces —
            // the alpha-tested leaf texture already makes the surface lacy)
            return WT_LEAF;
        }
        WT_NONE
    }
}

thread_local! {
    static CACHE: RefCell<Vec<(u64, i32, i32, Option<Rc<WorldTree>>)>> = const { RefCell::new(Vec::new()) };
}

/// The world tree of cell (ix, iz) (memoised per thread).
pub fn tree_of_cell(gen: &WorldGen, ix: i32, iz: i32) -> Option<Rc<WorldTree>> {
    if let Some(t) = CACHE.with(|c| c.borrow().iter().find(|e| e.0 == gen.seed && e.1 == ix && e.2 == iz).map(|e| e.3.clone())) {
        return t;
    }
    let t = WorldTree::decide(gen, ix, iz).map(Rc::new);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= 64 {
            c.remove(0);
        }
        c.push((gen.seed, ix, iz, t.clone()));
    });
    t
}

/// World trees whose AABB intersects the world box [x0, x1) × [z0, z1).
pub fn world_trees_in(gen: &WorldGen, x0: f64, z0: f64, x1: f64, z1: f64) -> Vec<Rc<WorldTree>> {
    let r = WT_REACH as f64;
    let c = WT_CELL as f64;
    let mut out = Vec::new();
    for iz in ((z0 - r) / c).floor() as i32..=((z1 + r) / c).floor() as i32 {
        for ix in ((x0 - r) / c).floor() as i32..=((x1 + r) / c).floor() as i32 {
            // the candidate lies in the inner 60 % of its cell: cheap reject before deciding
            let (cx0, cx1) = ((ix as f64 + 0.2) * c - r, (ix as f64 + 0.8) * c + r);
            let (cz0, cz1) = ((iz as f64 + 0.2) * c - r, (iz as f64 + 0.8) * c + r);
            if cx1 < x0 || cx0 > x1 || cz1 < z0 || cz0 > z1 {
                continue;
            }
            if let Some(t) = tree_of_cell(gen, ix, iz) {
                if t.intersects(x0 as f32, z0 as f32, x1 as f32, z1 as f32, 0.0) {
                    out.push(t);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_trees_exist_and_fit() {
        for seed in [1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            let mut n = 0;
            let mut cells = 0;
            for iz in -3..3 {
                for ix in -3..3 {
                    cells += 1;
                    if let Some(t) = tree_of_cell(&g, ix, iz) {
                        n += 1;
                        assert!(t.bbox[4] < WORLD_HEIGHT as f32, "top {}", t.bbox[4]);
                        let reach = (t.bbox[0] - t.x as f32).abs().max((t.bbox[3] - t.x as f32).abs()).max((t.bbox[2] - t.z as f32).abs()).max((t.bbox[5] - t.z as f32).abs());
                        assert!(reach <= WT_REACH, "reach {reach}");
                        // the trunk is solid at its base and leaves exist in the crown
                        let (prims, _, _) = t.prims_in(t.bbox[0], t.bbox[2], t.bbox[3], t.bbox[5], 0.0);
                        let tx = t.x as f32 + 0.5;
                        let tz = t.z as f32 + 0.5;
                        let yh = t.hollow_h + 5.0;
                        let (ax, az) = t.axis(yh);
                        assert_eq!(t.sample(tx + ax + t.r0 * 0.5, t.y0 + yh, tz + az, &prims, 0.0, true), WT_LOG);
                        let mut leaves = 0;
                        for k in 0..4000 {
                            let h = mix64(k as u64 ^ seed);
                            let px = t.bbox[0] + (t.bbox[3] - t.bbox[0]) * hash_f01(h);
                            let py = t.bbox[1] + (t.bbox[4] - t.bbox[1]) * hash_f01(mix64(h ^ 1));
                            let pz = t.bbox[2] + (t.bbox[5] - t.bbox[2]) * hash_f01(mix64(h ^ 2));
                            leaves += (t.sample(px, py, pz, &prims, 0.0, false) == WT_LEAF) as usize;
                        }
                        assert!(leaves > 40, "{:?} leaves {leaves}", t.kind);
                        eprintln!("seed {seed} cell ({ix},{iz}): {:?} at ({}, {:.0}, {}) h {:.0} r0 {:.1} reach {:.0} leaves {leaves}", t.kind, t.x, t.y0, t.z, t.height, t.r0, reach);
                    }
                }
            }
            eprintln!("seed {seed}: {n} world trees in {cells} cells");
            assert!(n >= 1, "no world tree at all");
        }
    }
}
