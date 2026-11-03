//! Exact ray traversal of the clipmap world.
//!
//! ## Level 0 (exact voxels)
//! The level is a 2D grid of columns, each a sorted list of vertical spans `[y0, y1)`. A ray is
//! walked over the columns with a *hierarchical* Amanatides–Woo DDA on the max-mipmap pyramid
//! `mip[m]` (max column top over 2^m × 2^m blocks): at mip level `m` the ray's parametric interval
//! inside the coarse cell `[t, t_exit]` gives the y-interval `[min(y(t), y(t_exit)), …]` (y is
//! affine in t); if its minimum is ≥ the cell's max top the whole cell is skipped in O(1) and the
//! walker ascends one level, otherwise it descends. At a leaf the ray interval is intersected with
//! every span interval `{t : y0 ≤ oy + dy·t < y1}` → the earliest non-empty intersection is the
//! exact hit; whether it starts at the column wall (side face) or at the span's top/bottom plane
//! gives the face normal.
//!
//! ## Heightfield levels
//! Each level k is treated as a continuous surface `S(x,z) = max(B(x,z), W)`: `B` is the bilinear
//! interpolant of the cell-centre heights (a *ruled* bilinear patch per dual cell) and `W` the
//! water plane of the dual cell (max of its corner water levels). Ray–patch intersection is exact:
//! with u, v affine in t, `B(u(t), v(t)) − y(t) = A t² + B t + C` is a quadratic solved with the
//! cancellation-free formula. The same hierarchical DDA runs over the *dual* grid with the bound
//! `max` of the 2×2 primal max-mip cells that cover a coarse dual cell.
//!
//! Each level owns the part of the ray inside its square but outside every finer level's square
//! (≤ 2 intervals per level); intervals are disjoint so they are traversed in t order and the first
//! hit is the nearest one.
use crate::materials::Material;
use crate::render::types::ViewMode;
use crate::world::{HeightLevel, Level0, World};

pub const INF: f64 = f64::INFINITY;

/// Axis tags for the entry face of a cell.
pub const AXIS_X: u8 = 0;
pub const AXIS_Y: u8 = 1;
pub const AXIS_Z: u8 = 2;
pub const AXIS_NONE: u8 = 3;

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub o: [f64; 3],
    pub d: [f64; 3],
    pub inv: [f64; 3],
}

impl Ray {
    #[inline]
    pub fn new(o: [f64; 3], d: [f64; 3]) -> Ray {
        let iv = |v: f64| if v != 0.0 { 1.0 / v } else { INF };
        Ray { o, d, inv: [iv(d[0]), iv(d[1]), iv(d[2])] }
    }
    #[inline]
    pub fn at(&self, t: f64) -> [f64; 3] {
        [self.o[0] + self.d[0] * t, self.o[1] + self.d[1] * t, self.o[2] + self.d[2] * t]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitKind {
    /// Exact voxel face. `face`: 0 +x, 1 −x, 2 +y, 3 −y, 4 +z, 5 −z.
    Voxel { bx: i32, by: i32, bz: i32, mat: Material, face: u8 },
    /// Heightfield level (index into `World::levels`), terrain or water surface.
    Height { level: u8, water: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f64,
    pub p: [f64; 3],
    /// Geometric normal.
    pub n: [f64; 3],
    pub kind: HitKind,
}

#[inline]
pub fn face_normal(face: u8) -> [f64; 3] {
    match face {
        0 => [1.0, 0.0, 0.0],
        1 => [-1.0, 0.0, 0.0],
        2 => [0.0, 1.0, 0.0],
        3 => [0.0, -1.0, 0.0],
        4 => [0.0, 0.0, 1.0],
        _ => [0.0, 0.0, -1.0],
    }
}

/// Clip a ray to the axis-aligned xz rectangle [x0,x1]×[z0,z1] within [t0, t1].
/// Returns (enter, exit, entry axis).
#[inline]
pub fn clip_xz(ray: &Ray, x0: f64, z0: f64, x1: f64, z1: f64, t0: f64, t1: f64) -> Option<(f64, f64, u8)> {
    let mut a = t0;
    let mut b = t1;
    let mut axis = AXIS_NONE;
    for (k, lo, hi) in [(0usize, x0, x1), (2usize, z0, z1)] {
        let o = ray.o[k];
        let d = ray.d[k];
        if d == 0.0 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let inv = ray.inv[k];
        let (mut ta, mut tb) = ((lo - o) * inv, (hi - o) * inv);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        if ta > a {
            a = ta;
            axis = k as u8;
        }
        if tb < b {
            b = tb;
        }
    }
    if a < b {
        Some((a, b, axis))
    } else {
        None
    }
}

/// Clip a ray interval to y ≤ ymax (and y ≥ ymin).
#[inline]
fn clip_y(ray: &Ray, ymin: f64, ymax: f64, t0: f64, t1: f64) -> Option<(f64, f64)> {
    let (oy, dy) = (ray.o[1], ray.d[1]);
    let (mut a, mut b) = (t0, t1);
    if dy == 0.0 {
        if oy > ymax || oy < ymin {
            return None;
        }
    } else {
        let inv = ray.inv[1];
        let (mut ta, mut tb) = ((ymin - oy) * inv, (ymax - oy) * inv);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        a = a.max(ta);
        b = b.min(tb);
    }
    if a < b {
        Some((a, b))
    } else {
        None
    }
}

/// A 2D grid with a max-mip pyramid, traversed by `hdda`.
trait Grid {
    /// World (x, z) of the boundary of cell index 0.
    fn origin(&self) -> (f64, f64);
    fn cell(&self) -> f64;
    /// Valid cell index ranges [ilo, ihi) × [jlo, jhi).
    fn range(&self) -> (i32, i32, i32, i32);
    fn maxm(&self) -> usize;
    /// Conservative max surface height over coarse cell (ci, cj) at mip level m.
    fn bound(&self, m: usize, ci: i32, cj: i32) -> f64;
    /// Exact intersection inside leaf cell (i, j) for t ∈ [ta, tb]; `axis` = entry face axis.
    fn leaf(&self, ray: &Ray, i: i32, j: i32, ta: f64, tb: f64, axis: u8) -> Option<Hit>;
}

/// Hierarchical (max-mip) 2D DDA over `g` for the ray interval [t0, t1].
#[inline]
fn hdda<G: Grid>(g: &G, ray: &Ray, t0: f64, t1: f64, entry_axis: u8) -> Option<Hit> {
    let (gx, gz) = g.origin();
    let c = g.cell();
    let (ilo, ihi, jlo, jhi) = g.range();
    if ilo >= ihi || jlo >= jhi {
        return None;
    }
    let (ox, oy, oz) = (ray.o[0], ray.o[1], ray.o[2]);
    let (dx, dy, dz) = (ray.d[0], ray.d[1], ray.d[2]);
    let (ivx, ivz) = (ray.inv[0], ray.inv[2]);
    let p = ray.at(t0);
    let mut i = (((p[0] - gx) / c).floor() as i32).clamp(ilo, ihi - 1);
    let mut j = (((p[2] - gz) / c).floor() as i32).clamp(jlo, jhi - 1);
    let mut t = t0;
    let maxm = g.maxm();
    let mut m = maxm.min(6);
    let mut axis = entry_axis;
    loop {
        let size = 1i32 << m;
        let ci = i >> m;
        let cj = j >> m;
        let lo_i = ci << m;
        let lo_j = cj << m;
        let tx = if dx > 0.0 {
            (gx + (lo_i + size) as f64 * c - ox) * ivx
        } else if dx < 0.0 {
            (gx + lo_i as f64 * c - ox) * ivx
        } else {
            INF
        };
        let tz = if dz > 0.0 {
            (gz + (lo_j + size) as f64 * c - oz) * ivz
        } else if dz < 0.0 {
            (gz + lo_j as f64 * c - oz) * ivz
        } else {
            INF
        };
        let te = tx.min(tz);
        let tend = te.min(t1);
        let ya = oy + dy * t;
        let yb = oy + dy * tend;
        let ylo = ya.min(yb);
        if ylo < g.bound(m, ci, cj) {
            if m > 0 {
                m -= 1;
                continue;
            }
            if let Some(h) = g.leaf(ray, i, j, t, tend, axis) {
                return Some(h);
            }
        }
        if te >= t1 {
            return None;
        }
        // Step to the neighbouring coarse cell.
        if tx < tz {
            i = if dx > 0.0 { lo_i + size } else { lo_i - 1 };
            let zz = oz + dz * te;
            j = (((zz - gz) / c).floor() as i32).clamp(lo_j, lo_j + size - 1);
            axis = AXIS_X;
        } else if tz < tx {
            j = if dz > 0.0 { lo_j + size } else { lo_j - 1 };
            let xx = ox + dx * te;
            i = (((xx - gx) / c).floor() as i32).clamp(lo_i, lo_i + size - 1);
            axis = AXIS_Z;
        } else {
            i = if dx > 0.0 { lo_i + size } else { lo_i - 1 };
            j = if dz > 0.0 { lo_j + size } else { lo_j - 1 };
            axis = AXIS_X;
        }
        if i < ilo || i >= ihi || j < jlo || j >= jhi {
            return None;
        }
        t = te;
        if m < maxm {
            m += 1;
        }
    }
}

// ------------------------------------------------------------------ level 0

/// Level-0 view: column range (diorama box), floor, and whether water stops rays.
pub struct L0View<'a> {
    pub l0: &'a Level0,
    pub ilo: i32,
    pub ihi: i32,
    pub jlo: i32,
    pub jhi: i32,
    pub ymin: i32,
    pub maxm: usize,
    pub top: f64,
}

impl<'a> L0View<'a> {
    pub fn new(l0: &'a Level0, mode: ViewMode) -> L0View<'a> {
        let n = l0.size as i32;
        let maxm = l0.mip.len() - 1;
        let top = l0.mip[maxm][0] as f64;
        match mode {
            ViewMode::Normal => L0View { l0, ilo: 0, ihi: n, jlo: 0, jhi: n, ymin: 0, maxm, top },
            ViewMode::Diorama { min_x, max_x, min_z, max_z, min_y } => L0View {
                l0,
                ilo: (min_x - l0.origin_x).clamp(0, n),
                ihi: (max_x - l0.origin_x).clamp(0, n),
                jlo: (min_z - l0.origin_z).clamp(0, n),
                jhi: (max_z - l0.origin_z).clamp(0, n),
                ymin: min_y.max(0),
                maxm,
                top,
            },
        }
    }

    /// World-space box [x0,x1]×[z0,z1] of the active columns.
    #[inline]
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let (ox, oz) = (self.l0.origin_x as f64, self.l0.origin_z as f64);
        (ox + self.ilo as f64, oz + self.jlo as f64, ox + self.ihi as f64, oz + self.jhi as f64)
    }

    /// Material at an integer block (Air outside the active box / below the floor).
    #[inline]
    pub fn block(&self, x: i32, y: i32, z: i32) -> Material {
        let i = x.wrapping_sub(self.l0.origin_x);
        let j = z.wrapping_sub(self.l0.origin_z);
        if i < self.ilo || i >= self.ihi || j < self.jlo || j >= self.jhi || y < self.ymin {
            return Material::Air;
        }
        let k = j as usize * self.l0.size as usize + i as usize;
        if y >= self.l0.top[k] as i32 {
            return Material::Air;
        }
        for s in self.l0.spans_at_index(k) {
            if (s.y0 as i32) > y {
                break;
            }
            if y < s.y1 as i32 {
                return s.mat;
            }
        }
        Material::Air
    }

    /// Opaque for occlusion purposes.
    #[inline]
    pub fn opaque(&self, x: i32, y: i32, z: i32) -> bool {
        self.block(x, y, z).is_opaque()
    }

    pub fn trace(&self, ray: &Ray, t0: f64, t1: f64, water: bool) -> Option<Hit> {
        let (x0, z0, x1, z1) = self.bounds();
        let (a, b, axis) = clip_xz(ray, x0, z0, x1, z1, t0, t1)?;
        // Upper clip only on the far side (keeps top-face classification exact at entry).
        let (_, b) = clip_y(ray, self.ymin as f64, self.top, a, b).map(|(_, bb)| (a, bb))?;
        let g = L0Grid { v: self, water };
        hdda(&g, ray, a, b, axis)
    }
}

struct L0Grid<'v, 'a> {
    v: &'v L0View<'a>,
    water: bool,
}

impl<'v, 'a> Grid for L0Grid<'v, 'a> {
    #[inline]
    fn origin(&self) -> (f64, f64) {
        (self.v.l0.origin_x as f64, self.v.l0.origin_z as f64)
    }
    #[inline]
    fn cell(&self) -> f64 {
        1.0
    }
    #[inline]
    fn range(&self) -> (i32, i32, i32, i32) {
        (self.v.ilo, self.v.ihi, self.v.jlo, self.v.jhi)
    }
    #[inline]
    fn maxm(&self) -> usize {
        self.v.maxm
    }
    #[inline]
    fn bound(&self, m: usize, ci: i32, cj: i32) -> f64 {
        let s = (self.v.l0.size >> m) as usize;
        self.v.l0.mip[m][cj as usize * s + ci as usize] as f64
    }
    #[inline]
    fn leaf(&self, ray: &Ray, i: i32, j: i32, ta: f64, tb: f64, axis: u8) -> Option<Hit> {
        let l0 = self.v.l0;
        let k = j as usize * l0.size as usize + i as usize;
        let spans = l0.spans_at_index(k);
        let (oy, dy, ivy) = (ray.o[1], ray.d[1], ray.inv[1]);
        let mut best = INF;
        let mut best_s = None;
        let mut vertical = false;
        let ymin = self.v.ymin;
        for s in spans {
            if !self.water && s.mat == Material::Water {
                continue;
            }
            let y0i = (s.y0 as i32).max(ymin);
            let y1i = s.y1 as i32;
            if y1i <= y0i {
                continue;
            }
            let (y0, y1) = (y0i as f64, y1i as f64);
            let (se, sx) = if dy > 0.0 {
                ((y0 - oy) * ivy, (y1 - oy) * ivy)
            } else if dy < 0.0 {
                ((y1 - oy) * ivy, (y0 - oy) * ivy)
            } else if oy >= y0 && oy < y1 {
                (-INF, INF)
            } else {
                continue;
            };
            let enter = se.max(ta);
            let exit = sx.min(tb);
            if enter < exit && enter < best {
                best = enter;
                best_s = Some((y0i, y1i, s.mat));
                vertical = se > ta || (axis == AXIS_Y && se >= ta);
            }
        }
        let (y0i, y1i, mat) = best_s?;
        let t = best;
        let mut p = ray.at(t);
        let bx = l0.origin_x + i;
        let bz = l0.origin_z + j;
        let (face, by);
        if vertical {
            if dy < 0.0 {
                face = 2;
                by = y1i - 1;
                p[1] = y1i as f64;
            } else {
                face = 3;
                by = y0i;
                p[1] = y0i as f64;
            }
        } else {
            by = (p[1].floor() as i32).clamp(y0i, y1i - 1);
            let ax = if axis == AXIS_X || axis == AXIS_Z {
                axis
            } else if ray.d[0].abs() >= ray.d[2].abs() {
                AXIS_X
            } else {
                AXIS_Z
            };
            if ax == AXIS_X {
                if ray.d[0] > 0.0 {
                    face = 1;
                    p[0] = bx as f64;
                } else {
                    face = 0;
                    p[0] = (bx + 1) as f64;
                }
            } else if ray.d[2] > 0.0 {
                face = 5;
                p[2] = bz as f64;
            } else {
                face = 4;
                p[2] = (bz + 1) as f64;
            }
        }
        Some(Hit { t, p, n: face_normal(face), kind: HitKind::Voxel { bx, by, bz, mat, face } })
    }
}

// ------------------------------------------------------------------ heightfields

pub struct HfView<'a> {
    pub lv: &'a HeightLevel,
    pub index: u8,
    /// Covered square [x0, z0, x1, z1].
    pub square: [f64; 4],
    /// Square owned by finer levels (empty if none).
    pub hole: Option<[f64; 4]>,
    pub maxm: usize,
    pub top: f64,
}

impl<'a> HfView<'a> {
    pub fn new(lv: &'a HeightLevel, index: u8, hole: Option<[f64; 4]>) -> HfView<'a> {
        let (x0, z0, x1, z1) = lv.bounds();
        let maxm = lv.mip.len() - 1;
        let top = lv.mip[maxm][0] as f64;
        HfView { lv, index, square: [x0, z0, x1, z1], hole, maxm, top }
    }

    /// Corner data of dual cell (i, j): heights h00,h10,h01,h11 and water plane.
    #[inline]
    pub fn dual(&self, i: i32, j: i32, water: bool) -> ([f64; 4], f64) {
        let n = self.lv.size as i32 - 1;
        let (i0, i1) = (i.clamp(0, n), (i + 1).clamp(0, n));
        let (j0, j1) = (j.clamp(0, n), (j + 1).clamp(0, n));
        let s = self.lv.size as usize;
        let k00 = j0 as usize * s + i0 as usize;
        let k10 = j0 as usize * s + i1 as usize;
        let k01 = j1 as usize * s + i0 as usize;
        let k11 = j1 as usize * s + i1 as usize;
        let h = &self.lv.h;
        let hs = [h[k00] as f64, h[k10] as f64, h[k01] as f64, h[k11] as f64];
        let w = if water {
            let wt = &self.lv.water;
            wt[k00].max(wt[k10]).max(wt[k01]).max(wt[k11]) as f64
        } else {
            -INF
        };
        (hs, w)
    }

    /// Dual-cell index and local (u, v) ∈ [0,1)² of a world position.
    #[inline]
    pub fn locate(&self, x: f64, z: f64) -> (i32, i32, f64, f64) {
        let c = self.lv.cell as f64;
        let gx = (x - self.lv.origin_x as f64) / c - 0.5;
        let gz = (z - self.lv.origin_z as f64) / c - 0.5;
        let (fi, fj) = (gx.floor(), gz.floor());
        (fi as i32, fj as i32, gx - fi, gz - fj)
    }

    /// Bilinear terrain height (no water) at a world position.
    #[inline]
    pub fn terrain(&self, x: f64, z: f64) -> f64 {
        let (i, j, u, v) = self.locate(x, z);
        let (h, _) = self.dual(i, j, false);
        bilerp(h, u, v)
    }

    /// Surface S = max(terrain, water plane) at a world position (the traced surface).
    #[inline]
    pub fn surface(&self, x: f64, z: f64, water: bool) -> f64 {
        let (i, j, u, v) = self.locate(x, z);
        let (h, w) = self.dual(i, j, water);
        bilerp(h, u, v).max(w)
    }

    pub fn trace(&self, ray: &Ray, t0: f64, t1: f64, water: bool) -> Option<Hit> {
        let g = HfGrid { v: self, water };
        hdda(&g, ray, t0, t1, AXIS_NONE)
    }
}

#[inline]
pub fn bilerp(h: [f64; 4], u: f64, v: f64) -> f64 {
    let a = h[0] + (h[1] - h[0]) * u;
    let b = h[2] + (h[3] - h[2]) * u;
    a + (b - a) * v
}

struct HfGrid<'v, 'a> {
    v: &'v HfView<'a>,
    water: bool,
}

/// Smallest t ∈ (0, l] with a t² + b t + c ≥ 0, given c < 0.
#[inline]
fn first_crossing(a: f64, b: f64, c: f64, l: f64) -> Option<f64> {
    let scale = b.abs() + c.abs();
    if a.abs() <= 1e-14 * scale.max(1e-300) {
        if b > 0.0 {
            let t = -c / b;
            return if t <= l { Some(t) } else { None };
        }
        return None;
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let q = -0.5 * (b + if b >= 0.0 { sq } else { -sq });
    let (mut r1, mut r2) = (q / a, if q != 0.0 { c / q } else { q / a });
    if r1 > r2 {
        std::mem::swap(&mut r1, &mut r2);
    }
    let t = if a > 0.0 {
        r2
    } else if r1 > 0.0 {
        r1
    } else {
        return None;
    };
    if t > 0.0 && t <= l {
        Some(t)
    } else {
        None
    }
}

impl<'v, 'a> Grid for HfGrid<'v, 'a> {
    #[inline]
    fn origin(&self) -> (f64, f64) {
        let c = self.v.lv.cell as f64;
        (self.v.lv.origin_x as f64 + 0.5 * c, self.v.lv.origin_z as f64 + 0.5 * c)
    }
    #[inline]
    fn cell(&self) -> f64 {
        self.v.lv.cell as f64
    }
    #[inline]
    fn range(&self) -> (i32, i32, i32, i32) {
        let n = self.v.lv.size as i32;
        (-1, n, -1, n)
    }
    #[inline]
    fn maxm(&self) -> usize {
        self.v.maxm
    }
    #[inline]
    fn bound(&self, m: usize, ci: i32, cj: i32) -> f64 {
        let lv = self.v.lv;
        lv.mip_max(m, ci, cj)
            .max(lv.mip_max(m, ci + 1, cj))
            .max(lv.mip_max(m, ci, cj + 1))
            .max(lv.mip_max(m, ci + 1, cj + 1)) as f64
    }
    #[inline]
    fn leaf(&self, ray: &Ray, i: i32, j: i32, ta: f64, tb: f64, _axis: u8) -> Option<Hit> {
        let v = self.v;
        let c = v.lv.cell as f64;
        let (h, w) = v.dual(i, j, self.water);
        let x0 = v.lv.origin_x as f64 + (i as f64 + 0.5) * c;
        let z0 = v.lv.origin_z as f64 + (j as f64 + 0.5) * c;
        let pa = ray.at(ta);
        let ua = (pa[0] - x0) / c;
        let va = (pa[2] - z0) / c;
        let du = ray.d[0] / c;
        let dv = ray.d[2] / c;
        let e = h[1] - h[0];
        let f = h[2] - h[0];
        let g = h[0] - h[1] - h[2] + h[3];
        let cc = h[0] + e * ua + f * va + g * ua * va - pa[1];
        let l = tb - ta;
        let tt = if cc >= 0.0 {
            Some(0.0)
        } else {
            let bb = e * du + f * dv + g * (ua * dv + va * du) - ray.d[1];
            let aa = g * du * dv;
            first_crossing(aa, bb, cc, l)
        };
        let tw = if w > -INF {
            if pa[1] <= w {
                Some(0.0)
            } else if ray.d[1] < 0.0 {
                let t = (w - pa[1]) / ray.d[1];
                if t <= l {
                    Some(t)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let (lt, water) = match (tt, tw) {
            (Some(a), Some(b)) => {
                if b < a {
                    (b, true)
                } else {
                    (a, false)
                }
            }
            (Some(a), None) => (a, false),
            (None, Some(b)) => (b, true),
            (None, None) => return None,
        };
        let t = ta + lt;
        let mut p = ray.at(t);
        let n = if water {
            p[1] = w;
            [0.0, 1.0, 0.0]
        } else {
            let u = ua + du * lt;
            let vv = va + dv * lt;
            let bx = (e + g * vv) / c;
            let bz = (f + g * u) / c;
            let inv = 1.0 / (bx * bx + 1.0 + bz * bz).sqrt();
            [-bx * inv, inv, -bz * inv]
        };
        Some(Hit { t, p, n, kind: HitKind::Height { level: v.index, water } })
    }
}

// ------------------------------------------------------------------ scene

pub struct Scene<'a> {
    pub world: &'a World,
    pub l0: L0View<'a>,
    pub hf: Vec<HfView<'a>>,
    pub diorama: bool,
}

impl<'a> Scene<'a> {
    pub fn new(world: &'a World, mode: ViewMode) -> Scene<'a> {
        let l0 = L0View::new(&world.l0, mode);
        let diorama = matches!(mode, ViewMode::Diorama { .. });
        let mut hf = Vec::new();
        if !diorama {
            let (ox, oz) = (world.l0.origin_x as f64, world.l0.origin_z as f64);
            let n0 = world.l0.size as f64;
            let mut hole = [ox, oz, ox + n0, oz + n0];
            for (idx, lv) in world.levels.iter().enumerate() {
                let v = HfView::new(lv, idx as u8, Some(hole));
                let sq = v.square;
                hole = [hole[0].min(sq[0]), hole[1].min(sq[1]), hole[2].max(sq[2]), hole[3].max(sq[3])];
                hf.push(v);
            }
        }
        Scene { world, l0, hf, diorama }
    }

    /// Nearest hit along the ray in (t0, t1). `water`: water surfaces stop the ray.
    pub fn trace(&self, ray: &Ray, t0: f64, t1: f64, water: bool) -> Option<Hit> {
        // Collect disjoint segments [a, b) owned by each level, then walk them in t order.
        let mut segs: [(f64, f64, i8); 20] = [(0.0, 0.0, 0); 20];
        let mut ns = 0usize;
        if let Some((a, b, _)) = {
            let (x0, z0, x1, z1) = self.l0.bounds();
            clip_xz(ray, x0, z0, x1, z1, t0, t1)
        } {
            segs[ns] = (a, b, -1);
            ns += 1;
        }
        for (k, v) in self.hf.iter().enumerate() {
            if ns + 2 > segs.len() {
                break;
            }
            let sq = v.square;
            let Some((a, b, _)) = clip_xz(ray, sq[0], sq[1], sq[2], sq[3], t0, t1) else { continue };
            let Some((a, b)) = clip_y(ray, -INF, v.top, a, b) else { continue };
            match v.hole.and_then(|h| clip_xz(ray, h[0], h[1], h[2], h[3], a, b)) {
                None => {
                    segs[ns] = (a, b, k as i8);
                    ns += 1;
                }
                Some((ha, hb, _)) => {
                    if ha > a {
                        segs[ns] = (a, ha, k as i8);
                        ns += 1;
                    }
                    if hb < b {
                        segs[ns] = (hb, b, k as i8);
                        ns += 1;
                    }
                }
            }
        }
        let segs = &mut segs[..ns];
        segs.sort_unstable_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        for &(a, b, k) in segs.iter() {
            let hit = if k < 0 { self.l0.trace(ray, a, b, water) } else { self.hf[k as usize].trace(ray, a, b, water) };
            if hit.is_some() {
                return hit;
            }
        }
        None
    }

    /// True if anything opaque lies on the ray within (t0, t1) (water is transparent).
    #[inline]
    pub fn occluded(&self, ray: &Ray, t0: f64, t1: f64) -> bool {
        self.trace(ray, t0, t1, false).is_some()
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::Biome;
    use crate::math::Vec3;
    use crate::noise::{hash2, hash3, hash_f01, mix64};
    use crate::world::{BuildParams, HfCell, Span};

    struct Rng(u64);
    impl Rng {
        fn f(&mut self) -> f64 {
            self.0 = mix64(self.0.wrapping_add(0x9E37_79B9_7F4A_7C15));
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        fn r(&mut self, a: f64, b: f64) -> f64 {
            a + (b - a) * self.f()
        }
    }

    /// Random blocky columns with overhangs, caves, water and gaps.
    fn random_spans(x: i32, z: i32) -> Vec<Span> {
        let h = hash2(1, x, z);
        let hf = |k: u32| ((h >> (k * 8)) & 0xff) as i32;
        let base = 20 + ((x as f64 * 0.21).sin() * 8.0 + (z as f64 * 0.17).cos() * 8.0) as i32 + hf(0) % 4;
        let mut v = Vec::new();
        if hf(1) % 13 == 0 {
            return v; // hole column
        }
        if hf(2) % 3 == 0 {
            // cave: ground split
            let g = 4 + hf(3) % 8;
            v.push(Span::new(0, base - g - 3, Material::Stone));
            v.push(Span::new(base - g, base, Material::Dirt));
        } else {
            v.push(Span::new(0, base, Material::Stone));
        }
        if base < 22 {
            v.push(Span::new(base, 22, Material::Water));
        }
        if hf(4) % 7 == 0 {
            let a = base.max(22) + 3 + hf(5) % 6;
            v.push(Span::new(a, a + 1 + hf(6) % 4, Material::OakLeaves));
        }
        v
    }

    fn test_world(size: u32, hf_levels: u32, hf_size: u32, cx: i32, cz: i32) -> World {
        let p = BuildParams {
            center_x: cx,
            center_z: cz,
            l0_size: size,
            hf_size,
            hf_levels,
            cave_radius: 0,
            feature_radius: 0,
        };
        World::synthetic(p, random_spans, |k, x, z| {
            let h = 22.0 + 10.0 * (x * 0.013).sin() * (z * 0.011).cos() + 4.0 * hash_f01(hash3(5, k as i32, x as i32, z as i32)) as f64;
            let h = h as f32;
            HfCell {
                h,
                water: if h < 21.0 { 21.0 } else { f32::NEG_INFINITY },
                albedo: Vec3::splat(0.3),
                mat: Material::Grass,
                biome: Biome::Plains,
            }
        })
    }

    /// Reference: exact 3D Amanatides–Woo voxel walk testing every voxel (no hierarchy, no spans).
    fn brute_l0(v: &L0View, ray: &Ray, tmax: f64, water: bool) -> Option<(f64, [i32; 3], u8)> {
        let mut t = 0.0f64;
        let p = ray.o;
        let mut c = [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32];
        let mut tnext = [0.0f64; 3];
        let mut step = [0i32; 3];
        let mut tdelta = [INF; 3];
        for a in 0..3 {
            if ray.d[a] > 0.0 {
                step[a] = 1;
                tnext[a] = ((c[a] + 1) as f64 - p[a]) / ray.d[a];
                tdelta[a] = 1.0 / ray.d[a];
            } else if ray.d[a] < 0.0 {
                step[a] = -1;
                tnext[a] = (c[a] as f64 - p[a]) / ray.d[a];
                tdelta[a] = -1.0 / ray.d[a];
            } else {
                tnext[a] = INF;
            }
        }
        let mut face = 255u8;
        while t <= tmax {
            let m = v.block(c[0], c[1], c[2]);
            if m.is_opaque() || (water && m == Material::Water) {
                return Some((t, c, face));
            }
            let a = if tnext[0] < tnext[1] && tnext[0] < tnext[2] {
                0
            } else if tnext[1] < tnext[2] {
                1
            } else {
                2
            };
            t = tnext[a];
            c[a] += step[a];
            tnext[a] += tdelta[a];
            face = (a as u8) * 2 + if step[a] > 0 { 1 } else { 0 };
            if c[1] < -2 || c[1] > 200 {
                return None;
            }
        }
        None
    }

    fn check_l0(world: &World, mode: ViewMode, n_rays: usize, seed: u64, inside: bool) {
        let v = L0View::new(&world.l0, mode);
        let (x0, z0, x1, z1) = v.bounds();
        let mut rng = Rng(seed);
        let mut hits = 0;
        let mut mism = 0;
        let mut tested = 0;
        for k in 0..n_rays {
            // Origins: inside the box (incl. inside caves / below spans) or outside it.
            let o = if inside {
                [rng.r(x0 - 3.0, x1 + 3.0), rng.r(0.0, 45.0), rng.r(z0 - 3.0, z1 + 3.0)]
            } else {
                [rng.r(x0 - 30.0, x1 + 30.0), rng.r(10.0, 80.0), rng.r(z0 - 30.0, z1 + 30.0)]
            };
            let mut d = [rng.r(-1.0, 1.0), rng.r(-1.0, 0.3), rng.r(-1.0, 1.0)];
            // Some axis-parallel rays.
            match k % 11 {
                0 => {
                    d[0] = 0.0;
                }
                1 => {
                    d[2] = 0.0;
                }
                2 => {
                    d[1] = 0.0;
                }
                3 => {
                    d = [0.0, -1.0, 0.0];
                }
                4 => {
                    d = [1.0, 0.0, 0.0];
                }
                _ => {}
            }
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if l == 0.0 {
                continue;
            }
            let d = [d[0] / l, d[1] / l, d[2] / l];
            let ray = Ray::new(o, d);
            let water = k % 2 == 0;
            // Skip origins inside solid voxels (undefined start face).
            let ob = v.block(o[0].floor() as i32, o[1].floor() as i32, o[2].floor() as i32);
            if ob.is_opaque() || (water && ob == Material::Water) {
                continue;
            }
            tested += 1;
            let fast = v.trace(&ray, 0.0, 400.0, water);
            let slow = brute_l0(&v, &ray, 400.0, water);
            match (fast, slow) {
                (Some(h), Some((t, c, face))) => {
                    hits += 1;
                    let HitKind::Voxel { bx, by, bz, face: f, .. } = h.kind else { panic!() };
                    if (h.t - t).abs() > 1e-7 || [bx, by, bz] != c || f != face {
                        mism += 1;
                        if mism < 5 {
                            eprintln!("mismatch ray {:?} {:?}: fast t={} {:?} face {} slow t={} {:?} face {}", o, d, h.t, [bx, by, bz], f, t, c, face);
                        }
                    }
                }
                (None, None) => {}
                (a, b) => {
                    mism += 1;
                    if mism < 5 {
                        eprintln!("hit/miss mismatch {:?} {:?}: {:?} vs {:?}", o, d, a.map(|h| h.t), b.map(|x| x.0));
                    }
                }
            }
        }
        eprintln!("l0 check: {} hits of {} tested rays, {} mismatches", hits, tested, mism);
        assert!(tested > n_rays / 4 && hits > tested / 10);
        assert_eq!(mism, 0);
    }

    #[test]
    fn l0_matches_bruteforce_negative_coords() {
        let w = test_world(64, 0, 64, -37, -1000);
        check_l0(&w, ViewMode::Normal, 12000, 11, false);
        check_l0(&w, ViewMode::Normal, 12000, 12, true);
    }

    #[test]
    fn l0_matches_bruteforce_diorama() {
        let w = test_world(64, 0, 64, 5, 9);
        let mode = ViewMode::Diorama { min_x: -20, max_x: 17, min_z: -9, max_z: 30, min_y: 6 };
        check_l0(&w, mode, 6000, 13, false);
        check_l0(&w, mode, 6000, 14, true);
    }

    #[test]
    fn camera_in_cave_and_below_span() {
        // Column with floor [0,10) and roof [14, 20): camera at y=12 looking up hits roof bottom.
        let p = BuildParams { center_x: 0, center_z: 0, l0_size: 16, hf_size: 16, hf_levels: 0, cave_radius: 0, feature_radius: 0 };
        let w = World::synthetic(
            p,
            |_, _| vec![Span::new(0, 10, Material::Stone), Span::new(14, 20, Material::Stone)],
            |_, _, _| HfCell { h: 0.0, water: f32::NEG_INFINITY, albedo: Vec3::ZERO, mat: Material::Stone, biome: Biome::Plains },
        );
        let v = L0View::new(&w.l0, ViewMode::Normal);
        let up = Ray::new([0.5, 12.0, 0.5], [0.0, 1.0, 0.0]);
        let h = v.trace(&up, 0.0, 100.0, true).unwrap();
        assert!((h.t - 2.0).abs() < 1e-12);
        assert_eq!(h.kind, HitKind::Voxel { bx: 0, by: 14, bz: 0, mat: Material::Stone, face: 3 });
        let down = Ray::new([0.5, 12.0, 0.5], [0.0, -1.0, 0.0]);
        let h = v.trace(&down, 0.0, 100.0, true).unwrap();
        assert!((h.t - 2.0).abs() < 1e-12);
        assert_eq!(h.kind, HitKind::Voxel { bx: 0, by: 9, bz: 0, mat: Material::Stone, face: 2 });
        // Horizontal ray in the cave slab: escapes through the level side.
        let side = Ray::new([0.5, 12.0, 0.5], [1.0, 0.0, 0.0]);
        assert!(v.trace(&side, 0.0, 100.0, true).is_none());
        // From above the world straight down onto the roof top.
        let top = Ray::new([3.5, 50.0, -2.5], [0.0, -1.0, 0.0]);
        let h = v.trace(&top, 0.0, 100.0, true).unwrap();
        assert!((h.t - 30.0).abs() < 1e-12);
        assert_eq!(h.n, [0.0, 1.0, 0.0]);
        // From outside the level toward its side wall.
        let wall = Ray::new([-20.0, 5.5, 0.5], [1.0, 0.0, 0.0]);
        let h = v.trace(&wall, 0.0, 100.0, true).unwrap();
        assert!((h.t - 12.0).abs() < 1e-12);
        assert_eq!(h.n, [-1.0, 0.0, 0.0]);
    }

    /// Surface used by brute force: same definition as the traversal.
    fn brute_hf(v: &HfView, ray: &Ray, a: f64, b: f64, water: bool) -> Option<f64> {
        let c = v.lv.cell as f64;
        let step = c / 256.0;
        let below = |t: f64| {
            let p = ray.at(t);
            p[1] <= v.surface(p[0], p[2], water)
        };
        if below(a) {
            return Some(a);
        }
        let mut t = a;
        while t < b {
            let tn = (t + step).min(b);
            if below(tn) {
                // bisection
                let (mut lo, mut hi) = (t, tn);
                for _ in 0..60 {
                    let m = 0.5 * (lo + hi);
                    if below(m) {
                        hi = m;
                    } else {
                        lo = m;
                    }
                }
                return Some(hi);
            }
            t = tn;
        }
        None
    }

    #[test]
    fn heightfield_matches_bruteforce() {
        let w = test_world(16, 3, 32, -500, 777);
        let scene = Scene::new(&w, ViewMode::Normal);
        let mut rng = Rng(99);
        let mut n_hit = 0;
        let mut miss_close = 0;
        let mut total = 0;
        for lvl in 0..3usize {
            let v = &scene.hf[lvl];
            let sq = v.square;
            for k in 0..1500 {
                let o = [rng.r(sq[0], sq[2]), rng.r(24.0, 60.0), rng.r(sq[1], sq[3])];
                let mut d = [rng.r(-1.0, 1.0), rng.r(-0.8, 0.1), rng.r(-1.0, 1.0)];
                if k % 7 == 0 {
                    d[0] = 0.0;
                }
                if k % 9 == 0 {
                    d[2] = 0.0;
                }
                let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                let d = [d[0] / l, d[1] / l, d[2] / l];
                let ray = Ray::new(o, d);
                // Whole square of this level (ignore the hole for the comparison).
                let Some((a, b, _)) = clip_xz(&ray, sq[0], sq[1], sq[2], sq[3], 0.0, 1e6) else { continue };
                let water = k % 2 == 0;
                if o[1] <= v.surface(o[0], o[2], water) {
                    continue;
                }
                total += 1;
                let fast = v.trace(&ray, a, b, water).map(|h| h.t);
                let slow = brute_hf(v, &ray, a, b, water);
                match (fast, slow) {
                    (Some(tf), Some(ts)) => {
                        n_hit += 1;
                        let tol = 1e-6 * (1.0 + ts);
                        if (tf - ts).abs() > tol {
                            // Brute force may only step over a thin grazing crossing (later hit).
                            assert!(tf < ts, "fast later than brute: {} vs {} ray {:?} {:?}", tf, ts, o, d);
                            miss_close += 1;
                        }
                        let p = ray.at(tf);
                        let s = v.surface(p[0], p[2], water);
                        assert!((p[1] - s).abs() < 1e-6 * (1.0 + tf), "hit not on surface: {} vs {}", p[1], s);
                    }
                    (None, None) => {}
                    (Some(tf), None) => {
                        miss_close += 1;
                        let p = ray.at(tf);
                        let s = v.surface(p[0], p[2], water);
                        assert!((p[1] - s).abs() < 1e-6 * (1.0 + tf));
                    }
                    (None, Some(ts)) => panic!("traversal missed a hit at t={} ray {:?} {:?} lvl {}", ts, o, d, lvl),
                }
            }
        }
        eprintln!("hf check: {} rays, {} hits, {} grazing differences", total, n_hit, miss_close);
        assert!(n_hit > total / 3);
        assert!(miss_close * 200 < total);
    }

    #[test]
    fn scene_segments_cover_levels() {
        // A long ray from the centre crosses level 0 then all heightfield levels in order.
        let w = test_world(32, 3, 32, 3, -5);
        let scene = Scene::new(&w, ViewMode::Normal);
        let mut rng = Rng(5);
        for _ in 0..3000 {
            let o = [rng.r(-10.0, 15.0), rng.r(30.0, 45.0), rng.r(-17.0, 7.0)];
            let mut d = [rng.r(-1.0, 1.0), rng.r(-0.15, 0.0), rng.r(-1.0, 1.0)];
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            for x in d.iter_mut() {
                *x /= l;
            }
            let ray = Ray::new(o, d);
            let hit = scene.trace(&ray, 0.0, 1e6, true);
            // Reference: min over every level's own clipped segments.
            let mut best = INF;
            if let Some(h) = scene.l0.trace(&ray, 0.0, 1e6, true) {
                best = h.t;
            }
            let mut hole = scene.l0.bounds();
            for v in &scene.hf {
                let sq = v.square;
                if let Some((a, b, _)) = clip_xz(&ray, sq[0], sq[1], sq[2], sq[3], 0.0, 1e6) {
                    // brute: first hit outside hole
                    let step = v.lv.cell as f64 / 64.0;
                    let mut t = a;
                    while t < b && t < best {
                        let p = ray.at(t);
                        let in_hole = p[0] >= hole.0 && p[0] <= hole.2 && p[2] >= hole.1 && p[2] <= hole.3;
                        if !in_hole && p[1] <= v.surface(p[0], p[2], true) {
                            best = best.min(t);
                            break;
                        }
                        t += step;
                    }
                }
                hole = (hole.0.min(sq[0]), hole.1.min(sq[1]), hole.2.max(sq[2]), hole.3.max(sq[3]));
            }
            match hit {
                Some(h) => assert!(h.t <= best + 1e-6 && h.t > best - 2.0, "{} vs {}", h.t, best),
                None => assert!(best == INF),
            }
        }
    }
}
