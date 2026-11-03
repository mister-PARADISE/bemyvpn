//! Climate fields. OWNER: gen-core.
//!
//! # Mathematics
//!
//! Every climate field is a *calibrated, domain-warped fractional Brownian motion* carried
//! together with its exact horizontal gradient (forward-mode automatic differentiation, the
//! [`D2`] dual number `v + dx·εx + dz·εz`). Gradients are needed downstream for:
//! * the terrain slope `|∇H|` (analytic, no finite differences → one column = one evaluation),
//! * the river distance estimate `d ≈ |n| / |∇n|` (first-order distance to the zero level set of
//!   the river noise — the classic implicit-surface distance bound),
//! * snow on shaded slopes (aspect = ∂H/∂z).
//!
//! **Fields** (wavelengths λ in blocks):
//! * continentalness `C = Q_C(fBm₇(λ5200) ∘ W_c)` with `W_c` a 2-octave domain warp (λ3000, ±160;
//!   kept below the fold-over limit |∇(warp)| < 1, stronger warps fold and print marbled swirls).
//! * erosion `E = Q_E(2·sabs(fBm₄(λ4800) ∘ W_c) − ½)`: low erosion (mountains) along the zero set
//!   of an fBm → long branching mountain belts (same warp, offset domain → decorrelated).
//! * weirdness/river noise `n_r = fBm₃(λ2100) ∘ W_m ∘ W_c` where `W_m` is a meander warp (λ700, ±45).
//!   `W = Q_W(n_r)`, `pv = 1 − sabs(3·sabs(W) − 2)` (smooth folds). Because the river centre-lines
//!   are the zero set `n_r = 0` and valleys are `pv → −1 ⇔ W → 0`, **rivers always run along valley
//!   floors** and ridges (`pv = 1 ⇔ |W| = ⅔`) sit between neighbouring rivers — interfluves.
//! * temperature `T = Q_T(fBm₃(λ4200))`, humidity `H = Q_H(fBm₃(λ3200))` on half-warped coords;
//!   river gate `fBm₁(λ2600)` (≈70 % of valleys carry water, the rest are dry valleys).
//!
//! All octaves run on [`FastSimplex`] (branch-free 2D simplex with analytic gradient) with
//! precomputed per-octave rotation×frequency matrices.
//!
//! **Calibration** (`Q_*`): raw fBm is bell shaped. Each `Q` is the monotone cubic Hermite
//! interpolant (Fritsch–Carlson) through `(q_raw(pᵢ), target(pᵢ))`, where `q_raw(pᵢ)` are the
//! empirical quantiles measured by the test `calibration_quantiles` (200 k samples × 3 seeds over
//! 40×40 km, constants below) and `target` is the desired quantile function — i.e. empirical CDF
//! equalisation followed by the inverse target CDF. `C`: 20 % deep ocean (< −0.45), 45 % ocean
//! (< −0.18), 8 % coast band (−0.18…−0.05), rest inland. `E, W, H`: uniform on [−1, 1];
//! `T`: warm-skewed piecewise-linear target (≈15 % below −0.58). The monotone maps are resampled
//! on a 257-node uniform table for O(1) evaluation.
//!
//! Domain warps carry their Jacobian `J = (I + A·∇f)·J_prev`, so the gradient of a warped field is
//! `∇_p n(q(p)) = Jᵀ ∇_q n` — exact.
use super::Climate;
use crate::math::saturate;
use crate::noise::{derive_seed, salt};
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

// =====================================================================================
// D2: dual number with a 2D gradient (forward-mode automatic differentiation)
// =====================================================================================

/// Value with its horizontal gradient: `v`, `∂v/∂x`, `∂v/∂z`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct D2 {
    pub v: f32,
    pub dx: f32,
    pub dz: f32,
}

impl D2 {
    pub const ZERO: D2 = D2 { v: 0.0, dx: 0.0, dz: 0.0 };
    #[inline]
    pub const fn new(v: f32, dx: f32, dz: f32) -> D2 {
        D2 { v, dx, dz }
    }
    /// A constant (zero gradient).
    #[inline]
    pub const fn c(v: f32) -> D2 {
        D2 { v, dx: 0.0, dz: 0.0 }
    }
    #[inline]
    pub fn grad_len(self) -> f32 {
        (self.dx * self.dx + self.dz * self.dz).sqrt()
    }
    /// Apply a scalar function with known derivative: f(v), f'(v)·∇v.
    #[inline]
    pub fn map(self, f: f32, df: f32) -> D2 {
        D2 { v: f, dx: df * self.dx, dz: df * self.dz }
    }
    #[inline]
    pub fn abs(self) -> D2 {
        if self.v < 0.0 {
            -self
        } else {
            self
        }
    }
    #[inline]
    pub fn sq(self) -> D2 {
        self.map(self.v * self.v, 2.0 * self.v)
    }
    #[inline]
    pub fn sqrt(self) -> D2 {
        let s = self.v.max(0.0).sqrt();
        self.map(s, if s > 1e-12 { 0.5 / s } else { 0.0 })
    }
    /// x^p for x ≥ 0 (clamped at 0).
    #[inline]
    pub fn powf(self, p: f32) -> D2 {
        if self.v <= 0.0 {
            return D2::ZERO;
        }
        let r = self.v.powf(p);
        self.map(r, p * r / self.v)
    }
    #[inline]
    pub fn sin(self) -> D2 {
        let (s, c) = self.v.sin_cos();
        self.map(s, c)
    }
    #[inline]
    pub fn exp(self) -> D2 {
        let e = self.v.exp();
        self.map(e, e)
    }
    #[inline]
    pub fn min(self, o: D2) -> D2 {
        if self.v <= o.v {
            self
        } else {
            o
        }
    }
    #[inline]
    pub fn max(self, o: D2) -> D2 {
        if self.v >= o.v {
            self
        } else {
            o
        }
    }
    #[inline]
    pub fn minf(self, o: f32) -> D2 {
        if self.v <= o {
            self
        } else {
            D2::c(o)
        }
    }
    #[inline]
    pub fn maxf(self, o: f32) -> D2 {
        if self.v >= o {
            self
        } else {
            D2::c(o)
        }
    }
    #[inline]
    pub fn clamp(self, lo: f32, hi: f32) -> D2 {
        if self.v < lo {
            D2::c(lo)
        } else if self.v > hi {
            D2::c(hi)
        } else {
            self
        }
    }
    /// Polynomial smooth minimum (C¹), blend width `k` (Quílez): min(a,b) − h²/(4k), h = max(k−|a−b|, 0).
    #[inline]
    pub fn smin(self, o: D2, k: f32) -> D2 {
        let h = (D2::c(k) - (self - o).abs()).maxf(0.0);
        self.min(o) - h.sq() * (0.25 / k)
    }
    /// Smooth maximum, see [`D2::smin`].
    #[inline]
    pub fn smax(self, o: D2, k: f32) -> D2 {
        -((-self).smin(-o, k))
    }
}

impl Add for D2 {
    type Output = D2;
    #[inline]
    fn add(self, o: D2) -> D2 {
        D2::new(self.v + o.v, self.dx + o.dx, self.dz + o.dz)
    }
}
impl Add<f32> for D2 {
    type Output = D2;
    #[inline]
    fn add(self, o: f32) -> D2 {
        D2::new(self.v + o, self.dx, self.dz)
    }
}
impl Add<D2> for f32 {
    type Output = D2;
    #[inline]
    fn add(self, o: D2) -> D2 {
        o + self
    }
}
impl AddAssign for D2 {
    #[inline]
    fn add_assign(&mut self, o: D2) {
        *self = *self + o;
    }
}
impl Sub for D2 {
    type Output = D2;
    #[inline]
    fn sub(self, o: D2) -> D2 {
        D2::new(self.v - o.v, self.dx - o.dx, self.dz - o.dz)
    }
}
impl Sub<f32> for D2 {
    type Output = D2;
    #[inline]
    fn sub(self, o: f32) -> D2 {
        D2::new(self.v - o, self.dx, self.dz)
    }
}
impl Sub<D2> for f32 {
    type Output = D2;
    #[inline]
    fn sub(self, o: D2) -> D2 {
        D2::new(self - o.v, -o.dx, -o.dz)
    }
}
impl Mul for D2 {
    type Output = D2;
    #[inline]
    fn mul(self, o: D2) -> D2 {
        D2::new(self.v * o.v, self.dx * o.v + self.v * o.dx, self.dz * o.v + self.v * o.dz)
    }
}
impl Mul<f32> for D2 {
    type Output = D2;
    #[inline]
    fn mul(self, s: f32) -> D2 {
        D2::new(self.v * s, self.dx * s, self.dz * s)
    }
}
impl Mul<D2> for f32 {
    type Output = D2;
    #[inline]
    fn mul(self, o: D2) -> D2 {
        o * self
    }
}
impl Div for D2 {
    type Output = D2;
    #[inline]
    fn div(self, o: D2) -> D2 {
        let inv = 1.0 / o.v;
        let q = self.v * inv;
        D2::new(q, (self.dx - q * o.dx) * inv, (self.dz - q * o.dz) * inv)
    }
}
impl Div<f32> for D2 {
    type Output = D2;
    #[inline]
    fn div(self, s: f32) -> D2 {
        self * (1.0 / s)
    }
}
impl Neg for D2 {
    type Output = D2;
    #[inline]
    fn neg(self) -> D2 {
        D2::new(-self.v, -self.dx, -self.dz)
    }
}

/// Hermite smoothstep on a dual number (e0 > e1 allowed).
#[inline]
pub fn smoothstep_d(e0: f32, e1: f32, x: D2) -> D2 {
    let inv = 1.0 / (e1 - e0);
    let t = (x.v - e0) * inv;
    if t <= 0.0 {
        D2::ZERO
    } else if t >= 1.0 {
        D2::c(1.0)
    } else {
        x.map(t * t * (3.0 - 2.0 * t), 6.0 * t * (1.0 - t) * inv)
    }
}

/// Smooth absolute value √(x² + ε²) − ε (C^∞, equals |x| − O(ε) away from 0).
#[inline]
pub fn sabs_d(x: D2, eps: f32) -> D2 {
    let r = (x.v * x.v + eps * eps).sqrt();
    x.map(r - eps, x.v / r)
}

/// a + (b − a)·t on duals.
#[inline]
pub fn lerp_d(a: D2, b: D2, t: D2) -> D2 {
    a + (b - a) * t
}

// =====================================================================================
// Fast 2D simplex noise with analytic gradient
// =====================================================================================

/// 2D simplex noise (Perlin/Gustavson construction, same normalisation and output range as
/// `noise::Simplex::n2d`) specialised for throughput: the lattice cell uses a truncating-cast
/// floor (no libm call), corners are hashed with a 32-bit multiply–xorshift mixer, and gradients
/// come from a 16-entry table of unit vectors (offset by half a step, none axis-aligned).
/// ≈ 2.5× faster than the generic version; every climate/terrain octave runs through it.
#[derive(Clone, Copy, Debug)]
pub struct FastSimplex {
    seed: u32,
}

const FS_F2: f64 = 0.366_025_403_784_438_6;
const FS_G2: f32 = 0.211_324_865_405_187_1;
const FS_SCALE: f32 = 99.83;

const fn grad16() -> [[f32; 2]; 16] {
    // cos/sin of (k + 0.5)·2π/16, written out (const fn cannot call trig).
    [
        [0.980_785_3, 0.195_090_32],
        [0.831_469_6, 0.555_570_24],
        [0.555_570_24, 0.831_469_6],
        [0.195_090_32, 0.980_785_3],
        [-0.195_090_32, 0.980_785_3],
        [-0.555_570_24, 0.831_469_6],
        [-0.831_469_6, 0.555_570_24],
        [-0.980_785_3, 0.195_090_32],
        [-0.980_785_3, -0.195_090_32],
        [-0.831_469_6, -0.555_570_24],
        [-0.555_570_24, -0.831_469_6],
        [-0.195_090_32, -0.980_785_3],
        [0.195_090_32, -0.980_785_3],
        [0.555_570_24, -0.831_469_6],
        [0.831_469_6, -0.555_570_24],
        [0.980_785_3, -0.195_090_32],
    ]
}
const GRAD16: [[f32; 2]; 16] = grad16();

#[inline(always)]
fn fast_floor(x: f64) -> i64 {
    let t = x as i64;
    t - ((x < t as f64) as i64)
}

impl FastSimplex {
    pub fn new(seed: u64) -> Self {
        FastSimplex { seed: (crate::noise::mix64(seed ^ 0xFA57) >> 32) as u32 }
    }

    #[inline(always)]
    fn hash(&self, i: i32, j: i32) -> usize {
        let mut h = (i as u32).wrapping_mul(0x27d4_eb2d) ^ (j as u32).wrapping_mul(0x1656_67b1) ^ self.seed;
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        h = h.wrapping_mul(0x297a_2d39);
        h ^= h >> 15;
        (h & 15) as usize
    }

    /// (value, ∂/∂x, ∂/∂y) w.r.t. the input coordinates; value ≈ [−1, 1].
    /// Branch-free: the simplex orientation and the kernel cut-offs are arithmetic selects
    /// (random branches would mispredict half of the time).
    #[inline]
    pub fn n2d(&self, x: f64, y: f64) -> (f32, f32, f32) {
        let s = (x + y) * FS_F2;
        let i = fast_floor(x + s);
        let j = fast_floor(y + s);
        let t = (i + j) as f64 * 0.211_324_865_405_187_1f64;
        let x0 = (x - (i as f64 - t)) as f32;
        let y0 = (y - (j as f64 - t)) as f32;
        let i1 = (x0 > y0) as i32;
        let j1 = 1 - i1;
        let x1 = x0 - i1 as f32 + FS_G2;
        let y1 = y0 - j1 as f32 + FS_G2;
        let x2 = x0 - 1.0 + 2.0 * FS_G2;
        let y2 = y0 - 1.0 + 2.0 * FS_G2;
        let (ii, jj) = (i as i32, j as i32);
        #[inline(always)]
        fn corner(cx: f32, cy: f32, g: [f32; 2]) -> (f32, f32, f32) {
            let tt = (0.5 - cx * cx - cy * cy).max(0.0);
            let gd = g[0] * cx + g[1] * cy;
            let t2 = tt * tt;
            let t3 = t2 * tt;
            let t4 = t2 * t2;
            (t4 * gd, -8.0 * cx * t3 * gd + t4 * g[0], -8.0 * cy * t3 * gd + t4 * g[1])
        }
        let a = corner(x0, y0, GRAD16[self.hash(ii, jj)]);
        let b = corner(x1, y1, GRAD16[self.hash(ii + i1, jj + j1)]);
        let c = corner(x2, y2, GRAD16[self.hash(ii + 1, jj + 1)]);
        ((a.0 + b.0 + c.0) * FS_SCALE, (a.1 + b.1 + c.1) * FS_SCALE, (a.2 + b.2 + c.2) * FS_SCALE)
    }
}

// =====================================================================================
// Derivative-carrying noise (built on `FastSimplex::n2d`)
// =====================================================================================

/// Golden angle — per-octave rotation, irrational w.r.t. the simplex lattice.
const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;

#[inline]
fn lod_fade(lambda: f64, fp: f64) -> f32 {
    if fp <= 1.0 {
        1.0
    } else {
        (((lambda / fp) - 2.0) * 0.5).clamp(0.0, 1.0) as f32
    }
}

/// One precomputed octave: rotation × frequency folded into a 2×2 matrix (f64 for the lattice
/// coordinates, f32 for the chain rule), amplitude and wavelength (for LOD fading).
#[derive(Clone, Copy, Debug)]
struct Octave {
    s: FastSimplex,
    cf: f64,
    sf: f64,
    dcf: f32,
    dsf: f32,
    c: f32,
    sn: f32,
    f: f32,
    amp: f32,
    lambda: f64,
    /// Per-octave lattice offset (noise-space units). Without it every octave of every field is
    /// evaluated at a lattice vertex at the world origin, where gradient noise is exactly 0 —
    /// all seeds would then share C(0,0) = Q_C(0) (a coast) and a river through the origin.
    ou: f64,
    ov: f64,
}

impl Octave {
    fn new(seed: u64, angle: f64, freq: f64, amp: f32) -> Self {
        let (sn, c) = angle.sin_cos();
        let h = crate::noise::mix64(seed ^ 0x0FF5_E7);
        Octave {
            s: FastSimplex::new(seed),
            cf: c * freq,
            sf: sn * freq,
            dcf: (c * freq) as f32,
            dsf: (sn * freq) as f32,
            c: c as f32,
            sn: sn as f32,
            f: freq as f32,
            amp,
            lambda: 1.0 / freq,
            ou: (crate::noise::hash_f01(h) as f64 - 0.5) * 2048.0,
            ov: (crate::noise::hash_f01(crate::noise::mix64(h)) as f64 - 0.5) * 2048.0,
        }
    }
    /// Noise value and world-space gradient of this octave (unscaled by `amp`).
    #[inline(always)]
    fn eval(&self, x: f64, z: f64) -> (f32, f32, f32) {
        let (n, nx, nz) = self.s.n2d(x * self.cf - z * self.sf + self.ou, x * self.sf + z * self.cf + self.ov);
        (n, nx * self.dcf + nz * self.dsf, nz * self.dcf - nx * self.dsf)
    }
    /// Noise value and the *noise-space* gradient rotated back to world orientation.
    #[inline(always)]
    fn eval_ns(&self, x: f64, z: f64) -> (f32, f32, f32, f32) {
        let (n, nx, nz) = self.s.n2d(x * self.cf - z * self.sf + self.ou, x * self.sf + z * self.cf + self.ov);
        (n, nx * self.c + nz * self.sn, nz * self.c - nx * self.sn, self.f)
    }
    #[inline(always)]
    fn fade(&self, fp: f64) -> f32 {
        lod_fade(self.lambda, fp)
    }
}

/// fBm with analytic gradient, normalised by the amplitude sum (std ≈ 0.2–0.3, range ⊂ [−1, 1]).
/// Band-limited exactly like `noise::Fbm::sample2_lod`: an octave of wavelength λ gets weight
/// `clamp((λ/footprint − 2)/2, 0, 1)`.
#[derive(Clone, Debug)]
pub struct DFbm {
    oct: Vec<Octave>,
}

impl DFbm {
    pub fn new(seed: u64, wavelength: f64, octaves: u32, lacunarity: f64, gain: f32) -> Self {
        let mut amp = 0.0;
        let mut a = 1.0f32;
        for _ in 0..octaves {
            amp += a;
            a *= gain;
        }
        let mut f = 1.0 / wavelength;
        let mut a = 1.0 / amp;
        let oct = (0..octaves)
            .map(|i| {
                let o = Octave::new(derive_seed(seed, 0xD0 + i as u64), GOLDEN_ANGLE * i as f64 + 0.37, f, a);
                f *= lacunarity;
                a *= gain;
                o
            })
            .collect();
        DFbm { oct }
    }

    #[inline]
    pub fn sample(&self, x: f64, z: f64, fp: f64) -> D2 {
        let (mut v, mut dx, mut dz) = (0.0f32, 0.0f32, 0.0f32);
        for o in &self.oct {
            let w = if fp > 1.0 {
                let w = o.fade(fp);
                if w <= 0.0 {
                    break;
                }
                w * o.amp
            } else {
                o.amp
            };
            let (n, nx, nz) = o.eval(x, z);
            v += w * n;
            dx += w * nx;
            dz += w * nz;
        }
        D2::new(v, dx, dz)
    }

    /// Value only (same cost as `sample`; convenience).
    #[inline]
    pub fn value(&self, x: f64, z: f64, fp: f64) -> f32 {
        self.sample(x, z, fp).v
    }

    /// The first `n` octaves only (same normalisation as the full sum): a cheap low-pass proxy
    /// of the field for far-field queries (rain-shadow streamlines).
    #[inline]
    pub fn sample_n(&self, x: f64, z: f64, n: usize) -> D2 {
        let (mut v, mut dx, mut dz) = (0.0f32, 0.0f32, 0.0f32);
        for o in self.oct.iter().take(n) {
            let (a, ax, az) = o.eval(x, z);
            v += o.amp * a;
            dx += o.amp * ax;
            dz += o.amp * az;
        }
        D2::new(v, dx, dz)
    }
}

/// Smooth ridged multifractal with exact gradient. Output ∈ [0, 1].
///
/// Musgrave's recurrence `sigᵢ = (1 − |nᵢ|)²·wᵢ`, `wᵢ₊₁ = clamp(g·sigᵢ, 0, 1)` produces C⁰ creases
/// at every clamp boundary and at |nᵢ| = 0 of the detail octaves ("curtain folds" in a hillshade).
/// Here the abs is a smooth abs `√(n² + ε²)` (ε ≈ 0.1: crests stay crisp at mountain scale but are C¹ over a few dozen
/// blocks, so slope-steered detail does not flip across them)
/// and the weight is the unclamped polynomial `wᵢ₊₁ = sigᵢ(1.6 − 0.6 sigᵢ) ∈ [0,1]` — detail still
/// concentrates on crests, but the field is C¹ everywhere.
#[derive(Clone, Debug)]
pub struct DRidged {
    oct: Vec<Octave>,
    inv_norm: f32,
}

impl DRidged {
    pub fn new(seed: u64, wavelength: f64, octaves: u32) -> Self {
        let mut norm = 0.0;
        let mut w = 1.0f32;
        let mut f = 1.0 / wavelength;
        let oct = (0..octaves)
            .map(|i| {
                let o = Octave::new(derive_seed(seed, 0x2D0 + i as u64), GOLDEN_ANGLE * i as f64 + 1.3, f, w);
                norm += w;
                w *= 0.5;
                f *= 2.0;
                o
            })
            .collect();
        DRidged { oct, inv_norm: 1.0 / norm }
    }

    #[inline]
    pub fn sample(&self, x: f64, z: f64, fp: f64) -> D2 {
        self.sample_with_steer(x, z, fp).0
    }

    /// Ridged value plus a *steering proxy*: the first octave alone with strongly rounded crest
    /// (ε = 0.45), on the same scale as the full field — a smooth stand-in whose gradient direction
    /// rotates slowly even across sharp crests.
    #[inline]
    pub fn sample_with_steer(&self, x: f64, z: f64, fp: f64) -> (D2, D2) {
        let mut weight = D2::c(1.0);
        let mut sum = D2::ZERO;
        let mut steer = D2::ZERO;
        for (k, o) in self.oct.iter().enumerate() {
            let fade = if fp > 1.0 { o.fade(fp) } else { 1.0 };
            if fade <= 0.0 {
                break;
            }
            let (n, nx, nz) = o.eval(x, z);
            let nd = D2::new(n, nx, nz);
            let eps = if k == 0 {
                steer = (1.0 - sabs_d(nd, 0.45)).maxf(0.0).sq() * (1.6 * self.inv_norm);
                0.10
            } else {
                0.14
            };
            let sig = (1.0 - sabs_d(nd, eps)).maxf(0.0).sq() * weight;
            weight = sig * (1.6 - sig * 0.6);
            sum += sig * (fade * o.amp);
        }
        ((sum * self.inv_norm).clamp(0.0, 1.0), steer)
    }
}

/// Derivative-damped ("eroded") fBm: octave i contributes `aᵢ nᵢ / (1 + k |Σ_{j≤i} aⱼ ∇nⱼ|²)`
/// (gradients in noise space, as `noise::ErodedFbm`). Steep places lose fine detail → gullies
/// and sharp spurs. The returned gradient treats the damping factor as locally constant (its own
/// derivative needs the Hessian); the error is a few percent of the slope (test `slope_matches_fd`).
#[derive(Clone, Debug)]
pub struct DEroded {
    steepness: f32,
    inv_amp: f32,
    oct: Vec<Octave>,
}

impl DEroded {
    pub fn new(seed: u64, wavelength: f64, octaves: u32, gain: f32, steepness: f32) -> Self {
        let mut amp = 0.0;
        let mut a = 1.0f32;
        for _ in 0..octaves {
            amp += a;
            a *= gain;
        }
        let mut f = 1.0 / wavelength;
        let mut a = 1.0f32;
        let oct = (0..octaves)
            .map(|i| {
                let o = Octave::new(derive_seed(seed, 0xE0D + i as u64), GOLDEN_ANGLE * i as f64 + 2.1, f, a);
                f *= 2.0;
                a *= gain;
                o
            })
            .collect();
        DEroded { steepness, inv_amp: 1.0 / amp, oct }
    }

    #[inline]
    pub fn sample(&self, x: f64, z: f64, fp: f64) -> D2 {
        let (mut gx, mut gz) = (0.0f32, 0.0f32);
        let (mut v, mut dx, mut dz) = (0.0f32, 0.0f32, 0.0f32);
        for o in &self.oct {
            let fade = if fp > 1.0 { o.fade(fp) } else { 1.0 };
            if fade <= 0.0 {
                break;
            }
            let (n, wdx, wdz, f) = o.eval_ns(x, z);
            gx += wdx * o.amp;
            gz += wdz * o.amp;
            let damp = fade * o.amp / (1.0 + self.steepness * (gx * gx + gz * gz));
            v += damp * n;
            dx += damp * f * wdx;
            dz += damp * f * wdz;
        }
        D2::new(v * self.inv_amp, dx * self.inv_amp, dz * self.inv_amp)
    }
}

/// A warped point with the Jacobian `J = ∂q/∂p` of the accumulated warp.
#[derive(Clone, Copy, Debug)]
pub struct P2 {
    pub x: f64,
    pub z: f64,
    pub jxx: f32,
    pub jxz: f32,
    pub jzx: f32,
    pub jzz: f32,
}

impl Default for P2 {
    fn default() -> Self {
        P2::identity(0.0, 0.0)
    }
}

impl P2 {
    #[inline]
    pub fn identity(x: f64, z: f64) -> P2 {
        P2 { x, z, jxx: 1.0, jxz: 0.0, jzx: 0.0, jzz: 1.0 }
    }
    /// Chain rule: `n` has its gradient w.r.t. q; return it w.r.t. the original p (∇ₚ = Jᵀ∇_q).
    #[inline]
    pub fn chain(&self, n: D2) -> D2 {
        D2::new(n.v, n.dx * self.jxx + n.dz * self.jzx, n.dx * self.jxz + n.dz * self.jzz)
    }
    /// Point halfway between `p` (identity) and this warped point.
    #[inline]
    pub fn half_from(&self, x: f64, z: f64) -> P2 {
        P2 {
            x: 0.5 * (x + self.x),
            z: 0.5 * (z + self.z),
            jxx: 0.5 * (1.0 + self.jxx),
            jxz: 0.5 * self.jxz,
            jzx: 0.5 * self.jzx,
            jzz: 0.5 * (1.0 + self.jzz),
        }
    }
    #[inline]
    pub fn offset(&self, ox: f64, oz: f64) -> P2 {
        P2 { x: self.x + ox, z: self.z + oz, ..*self }
    }
}

/// Domain warp `q = p + 2A·(f_x(p), f_z(p))` (same scaling as `noise::Warp2`) with Jacobian.
#[derive(Clone, Debug)]
pub struct DWarp {
    amp2: f32,
    fx: DFbm,
    fz: DFbm,
}

impl DWarp {
    pub fn new(seed: u64, wavelength: f64, octaves: u32, amplitude: f64) -> Self {
        DWarp {
            amp2: (2.0 * amplitude) as f32,
            fx: DFbm::new(derive_seed(seed, 0xA1), wavelength, octaves, 2.0, 0.5),
            fz: DFbm::new(derive_seed(seed, 0xA2), wavelength, octaves, 2.0, 0.5),
        }
    }
    #[inline]
    pub fn warp(&self, p: &P2, fp: f64) -> P2 {
        let a = self.fx.sample(p.x, p.z, fp);
        let b = self.fz.sample(p.x, p.z, fp);
        let k = self.amp2;
        // Local Jacobian L = I + k·[[a_x, a_z], [b_x, b_z]] (w.r.t. p's own coordinates).
        let l00 = 1.0 + k * a.dx;
        let l01 = k * a.dz;
        let l10 = k * b.dx;
        let l11 = 1.0 + k * b.dz;
        P2 {
            x: p.x + (k * a.v) as f64,
            z: p.z + (k * b.v) as f64,
            jxx: l00 * p.jxx + l01 * p.jzx,
            jxz: l00 * p.jxz + l01 * p.jzz,
            jzx: l10 * p.jxx + l11 * p.jzx,
            jzz: l10 * p.jxz + l11 * p.jzz,
        }
    }
}

// =====================================================================================
// Monotone calibration maps
// =====================================================================================

/// Monotone cubic Hermite interpolant (Fritsch–Carlson tangents, precomputed), clamped outside
/// the knot range. Used for empirical-CDF equalisation of the raw noise fields.
#[derive(Clone, Debug)]
pub struct MonoMap {
    xs: Vec<f32>,
    ys: Vec<f32>,
    ms: Vec<f32>,
    x0: f32,
    inv_dx: f32,
    table: Vec<(f32, f32)>,
}

impl MonoMap {
    pub fn new(xs: &[f32], ys: &[f32]) -> Self {
        assert_eq!(xs.len(), ys.len());
        let n = xs.len();
        assert!(n >= 2);
        for w in xs.windows(2) {
            assert!(w[0] < w[1], "MonoMap knots must increase: {:?}", xs);
        }
        let d: Vec<f32> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
        let mut ms = vec![0.0f32; n];
        ms[0] = d[0];
        ms[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            let (a, b) = (d[i - 1], d[i]);
            ms[i] = if a * b <= 0.0 { 0.0 } else { 2.0 * a * b / (a + b) };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                ms[i] = 0.0;
                ms[i + 1] = 0.0;
            } else {
                let lim = 3.0 * d[i].abs();
                ms[i] = ms[i].clamp(-lim, lim);
                ms[i + 1] = ms[i + 1].clamp(-lim, lim);
            }
        }
        let mut m = MonoMap { xs: xs.to_vec(), ys: ys.to_vec(), ms, x0: 0.0, inv_dx: 0.0, table: Vec::new() };
        // Resample onto a uniform grid (value + derivative at 257 nodes) → O(1) cubic Hermite
        // lookup in the hot path; the resampled curve is C¹ and still monotone to ~1e-5.
        const N: usize = 256;
        let (a, b) = (xs[0], xs[n - 1]);
        let dx = (b - a) / N as f32;
        m.table = (0..=N).map(|k| m.eval_knots(a + dx * k as f32)).collect();
        m.x0 = a;
        m.inv_dx = 1.0 / dx;
        m
    }

    /// (value, derivative) from the uniform table.
    #[inline]
    pub fn eval(&self, x: f32) -> (f32, f32) {
        let u = (x - self.x0) * self.inv_dx;
        let last = (self.table.len() - 1) as f32;
        if u <= 0.0 {
            return (self.table[0].0, 0.0);
        }
        if u >= last {
            return (self.table[self.table.len() - 1].0, 0.0);
        }
        let k = u as usize;
        let t = u - k as f32;
        let (y0, d0) = self.table[k];
        let (y1, d1) = self.table[k + 1];
        let h = 1.0 / self.inv_dx;
        let (m0, m1) = (d0 * h, d1 * h);
        let t2 = t * t;
        let t3 = t2 * t;
        let v = (2.0 * t3 - 3.0 * t2 + 1.0) * y0 + (t3 - 2.0 * t2 + t) * m0 + (3.0 * t2 - 2.0 * t3) * y1 + (t3 - t2) * m1;
        let dv = ((6.0 * t2 - 6.0 * t) * y0 + (3.0 * t2 - 4.0 * t + 1.0) * m0 + (6.0 * t - 6.0 * t2) * y1 + (3.0 * t2 - 2.0 * t) * m1) * self.inv_dx;
        (v, dv)
    }

    /// (value, derivative) evaluated on the original knots (reference implementation).
    pub fn eval_knots(&self, x: f32) -> (f32, f32) {
        let xs = &self.xs;
        let n = xs.len();
        if x <= xs[0] {
            return (self.ys[0], 0.0);
        }
        if x >= xs[n - 1] {
            return (self.ys[n - 1], 0.0);
        }
        let (mut lo, mut hi) = (0usize, n - 1);
        while hi - lo > 1 {
            let mid = (lo + hi) >> 1;
            if xs[mid] <= x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let h = xs[hi] - xs[lo];
        let t = (x - xs[lo]) / h;
        let (y0, y1, m0, m1) = (self.ys[lo], self.ys[hi], self.ms[lo], self.ms[hi]);
        let t2 = t * t;
        let t3 = t2 * t;
        let v = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * h * m0
            + (3.0 * t2 - 2.0 * t3) * y1
            + (t3 - t2) * h * m1;
        let dv = ((6.0 * t2 - 6.0 * t) * y0 + (6.0 * t - 6.0 * t2) * y1) / h
            + (3.0 * t2 - 4.0 * t + 1.0) * m0
            + (3.0 * t2 - 2.0 * t) * m1;
        (v, dv)
    }

    #[inline]
    pub fn eval_d(&self, x: D2) -> D2 {
        let (v, d) = self.eval(x.v);
        x.map(v, d)
    }
}

/// Probability levels at which raw quantiles were measured.
pub const P_LEVELS: [f32; 25] = [
    0.001, 0.01, 0.03, 0.06, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65,
    0.70, 0.75, 0.80, 0.85, 0.90, 0.94, 0.97, 0.99, 0.999,
];

// Measured raw quantiles (test `calibration_quantiles`, seeds 1/42/2026, 200k samples each,
// 40×40 km). Order: continentalness, erosion, river/weirdness, temperature, humidity, river gate.
const Q_C: [f32; 25] = [
    -0.7650, -0.6559, -0.5702, -0.4974, -0.4256, -0.3541, -0.2947, -0.2417, -0.1918, -0.1437,
    -0.0963, -0.0493, -0.0019, 0.0448, 0.0925, 0.1404, 0.1903, 0.2417, 0.2978, 0.3581,
    0.4286, 0.4987, 0.5732, 0.6596, 0.7637,
];
const Q_E: [f32; 25] = [
    -0.5000, -0.4998, -0.4984, -0.4939, -0.4835, -0.4642, -0.4390, -0.4091, -0.3747, -0.3367,
    -0.2957, -0.2521, -0.2061, -0.1575, -0.1064, -0.0520, 0.0064, 0.0699, 0.1399, 0.2219,
    0.3211, 0.4261, 0.5459, 0.6861, 0.8665,
];
const Q_W: [f32; 25] = [
    -0.8517, -0.7474, -0.6578, -0.5782, -0.5007, -0.4223, -0.3544, -0.2921, -0.2322, -0.1730,
    -0.1135, -0.0552, 0.0042, 0.0625, 0.1205, 0.1780, 0.2366, 0.2956, 0.3570, 0.4232,
    0.5005, 0.5780, 0.6569, 0.7444, 0.8536,
];
const Q_T: [f32; 25] = [
    -0.8393, -0.7236, -0.6319, -0.5520, -0.4739, -0.3967, -0.3317, -0.2722, -0.2164, -0.1621,
    -0.1096, -0.0577, -0.0045, 0.0495, 0.1036, 0.1579, 0.2125, 0.2684, 0.3285, 0.3947,
    0.4716, 0.5493, 0.6303, 0.7250, 0.8457,
];
const Q_H: [f32; 25] = [
    -0.8386, -0.7290, -0.6357, -0.5538, -0.4740, -0.3956, -0.3295, -0.2695, -0.2129, -0.1593,
    -0.1065, -0.0534, -0.0000, 0.0525, 0.1050, 0.1586, 0.2117, 0.2674, 0.3264, 0.3921,
    0.4695, 0.5484, 0.6315, 0.7259, 0.8424,
];
const Q_G: [f32; 25] = [
    -0.9773, -0.9237, -0.8794, -0.8235, -0.7499, -0.6573, -0.5649, -0.4716, -0.3780, -0.2843,
    -0.1894, -0.0947, 0.0005, 0.0952, 0.1903, 0.2848, 0.3786, 0.4724, 0.5671, 0.6610,
    0.7537, 0.8254, 0.8802, 0.9246, 0.9775,
];

// Effective humidity (rain shadow + orographic + maritime terms), before re-calibration.
const Q_H2: [f32; 25] = [
    -2.1984, -1.5849, -1.2192, -1.0266, -0.9035, -0.7881, -0.6876, -0.5874, -0.4838, -0.3808,
    -0.2779, -0.1738, -0.0725, 0.0287, 0.1315, 0.2332, 0.3370, 0.4397, 0.5452, 0.6568,
    0.7753, 0.8850, 0.9972, 1.1359, 1.4424,
];

/// Target quantile function of continentalness (piecewise linear in p).
fn target_c(p: f32) -> f32 {
    const K: [(f32, f32); 6] = [(0.0, -1.0), (0.20, -0.45), (0.45, -0.18), (0.53, -0.05), (0.78, 0.35), (1.0, 1.0)];
    for w in K.windows(2) {
        if p <= w[1].0 {
            let t = (p - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + (w[1].1 - w[0].1) * t;
        }
    }
    1.0
}

/// Temperature: slightly warm-skewed (≈15 % frozen T < −0.58, ≈25 % hot T > 0.45).
fn target_t(p: f32) -> f32 {
    const K: [(f32, f32); 4] = [(0.0, -1.0), (0.15, -0.6), (0.5, 0.1), (1.0, 1.0)];
    for w in K.windows(2) {
        if p <= w[1].0 {
            let t = (p - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + (w[1].1 - w[0].1) * t;
        }
    }
    1.0
}

fn uniform_target(p: f32) -> f32 {
    2.0 * p - 1.0
}

fn build_map(q: &[f32; 25], target: fn(f32) -> f32) -> MonoMap {
    // End knots half a unit beyond the extreme measured quantiles (beyond that: clamped).
    let mut xs = vec![q[0] - 0.5];
    let mut ys = vec![-1.0f32];
    for (i, &p) in P_LEVELS.iter().enumerate() {
        xs.push(q[i]);
        ys.push(target(p));
    }
    xs.push(q[24] + 0.5);
    ys.push(1.0);
    MonoMap::new(&xs, &ys)
}

/// Gradient floor of the river distance estimate (≈ ½ the median |∇n_r| = 1.36·10⁻³).
pub const RIVER_GRAD_FLOOR: f32 = 7.0e-4;

/// Distance (blocks) to the river centre-line `n = 0`, regularised first-order estimate
/// `d = |n| / √(|∇n|² + f²)`.
///
/// The plain estimate `|n|/|∇n|` is exact for a linear field but not Lipschitz: its true
/// gradient `sgn(n)∇n/|∇n| − |n|∇|∇n|/|∇n|²` blows up where |∇n| is small (interfluves), so
/// valley walls `V(d)` printed 12-block single-column steps there while the dual-number slope
/// (which cannot see the Hessian term) still reported ≈ 2. The floor f bounds the Hessian term
/// by ≈ d·|∇²n|/f; near rivers (|∇n| ≫ f) the estimate is unchanged. The returned gradient is
/// the first (dominant) term.
#[inline]
pub fn river_distance(n: D2) -> D2 {
    let s = (n.dx * n.dx + n.dz * n.dz + RIVER_GRAD_FLOOR * RIVER_GRAD_FLOOR).sqrt();
    let sgn = if n.v < 0.0 { -1.0 } else { 1.0 };
    D2::new((n.v.abs() / s).min(1.0e5), sgn * n.dx / s, sgn * n.dz / s)
}

/// Raw erosion: a smooth |·| of an fBm, so that low erosion (= mountains) concentrates along
/// the zero set of the noise — long, branching orogenic belts instead of round blobs.
#[inline]
fn erosion_raw(n: D2) -> D2 {
    sabs_d(n, 0.15) * 2.0 - 0.5
}

// =====================================================================================
// Climate sampler
// =====================================================================================

/// Orogeny belts: `|b|` thresholds of the tubular neighbourhood of the zero set of the belt
/// field `b`. With the median `|∇b| ≈ 1.0·10⁻⁴` (λ = 30 km) they correspond to a flat core of
/// half-width ≈ 600 blocks and foothills out to ≈ 2 200 blocks from the crest line. Plus the raw
/// gate thresholds that cut the belts into ranges of finite length.
pub const OROGENY_BAND: (f32, f32) = (0.06, 0.22);
pub const OROGENY_GATE: (f32, f32) = (0.02, 0.16);
/// Continentalness added in the core of a belt (crustal thickening).
const OROGENY_LAND: f32 = 0.28;

/// Rain shadow: streamline lattice spacing, step count and step length (blocks).
const SHADOW_SPACING: f64 = 256.0;
const SHADOW_STEPS: usize = 8;
const SHADOW_STEP: f64 = 700.0;
/// Regularisation of the wind normalisation `w = curl ψ / max(|∇ψ|, ε)`: calm cells of the
/// pressure field get weaker winds instead of an undefined direction.
const WIND_EPS: f32 = 1.2e-5;
/// Humidity response: −K_SHADOW·(upwind mountain mass) + K_ORO·(local uplift)·(1 − shadow)
/// + K_MARITIME·(upwind ocean fraction − ½).
const K_SHADOW: f32 = 1.7;
const K_ORO: f32 = 0.45;
const K_MARITIME: f32 = 0.5;

/// Climate with gradients plus the river geometry needed by the terrain shaper.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClimateD {
    pub c: D2,
    /// *Effective* erosion `E* = lerp(E_out, E_in, M)` (exported as `Climate::erosion`):
    /// outside the orogeny belts E* ∈ [−0.3, 1] (plains, hills, plateaus), inside E* ∈ [−1, −0.1].
    pub e: D2,
    /// Calibrated raw erosion (uniform on [−1, 1]); shapes the coast (sea cliffs vs beaches).
    pub e_raw: D2,
    /// Calibrated weirdness = calibrated river noise.
    pub w: D2,
    pub pv: D2,
    pub t: D2,
    /// Effective humidity (after rain shadow / orographic / maritime terms, re-calibrated).
    pub h: D2,
    /// Orogeny belt mask M ∈ [0, 1] (1 = core of a mountain range).
    pub orogeny: D2,
    /// Prevailing wind (unit-ish vector, |w| ≤ 1; weaker in calm cells of ψ).
    pub wind: (f32, f32),
    /// Upwind mountain mass (rain shadow strength, 0..1) and upwind ocean fraction.
    pub shadow: f32,
    pub maritime: f32,
    /// Raw river noise (zero set = river centre-lines).
    pub river_n: D2,
    /// Distance (blocks) to the river centre-line, [`river_distance`].
    pub river_dist: f32,
    /// River presence 0..1 (0 = dry valley / source region).
    pub river_gate: f32,
    /// Channel half-width (blocks).
    pub river_width: f32,
    /// `Climate::river` value.
    pub river: f32,
    /// Continental warp point (reused by the terrain for curved mountain ranges).
    pub q: P2,
}

impl ClimateD {
    pub fn to_climate(&self) -> Climate {
        Climate {
            continentalness: self.c.v,
            erosion: self.e.v,
            weirdness: self.w.v,
            peaks_valleys: self.pv.v,
            temperature: self.t.v,
            humidity: self.h.v,
            river: self.river,
        }
    }
}

/// One entry of the per-thread rain-shadow lattice cache (see [`ClimateSampler::shadow`]).
#[derive(Clone, Copy)]
struct ShadowEntry {
    seed: u64,
    ix: i32,
    iz: i32,
    valid: bool,
    v: [f32; 2],
}

const SHADOW_CACHE_SIZE: usize = 4096;

thread_local! {
    /// Direct-mapped memo of streamline integrals at lattice nodes. A node's value is a pure
    /// function of (seed, node), so the cache never changes results — it only avoids recomputing
    /// the 8-step streamline (≈ 70 noise octaves) for every column.
    static SHADOW_CACHE: std::cell::RefCell<Vec<ShadowEntry>> = std::cell::RefCell::new(vec![
        ShadowEntry { seed: 0, ix: 0, iz: 0, valid: false, v: [0.0; 2] };
        SHADOW_CACHE_SIZE
    ]);
}

pub struct ClimateSampler {
    pub seed: u64,
    warp_c: DWarp,
    fbm_c: DFbm,
    fbm_e: DFbm,
    warp_m: DWarp,
    fbm_r: DFbm,
    fbm_t: DFbm,
    fbm_h: DFbm,
    fbm_g: DFbm,
    fbm_o: DFbm,
    fbm_og: DFbm,
    fbm_psi: DFbm,
    cal_c: MonoMap,
    cal_e: MonoMap,
    cal_w: MonoMap,
    cal_t: MonoMap,
    cal_h: MonoMap,
    cal_g: MonoMap,
    cal_h2: MonoMap,
}

impl ClimateSampler {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        ClimateSampler {
            seed,
            warp_c: DWarp::new(s("climate.warp_c"), 3000.0, 2, 160.0),
            fbm_c: DFbm::new(s("climate.continent"), 5200.0, 7, 2.0, 0.5),
            fbm_e: DFbm::new(s("climate.erosion"), 4800.0, 4, 2.0, 0.5),
            warp_m: DWarp::new(s("climate.meander"), 700.0, 1, 45.0),
            fbm_r: DFbm::new(s("climate.river"), 2100.0, 3, 2.0, 0.42),
            fbm_t: DFbm::new(s("climate.temperature"), 4200.0, 3, 2.0, 0.5),
            fbm_h: DFbm::new(s("climate.humidity"), 3200.0, 3, 2.0, 0.5),
            fbm_g: DFbm::new(s("climate.river_gate"), 2600.0, 1, 2.0, 0.5),
            fbm_o: DFbm::new(s("climate.orogeny"), 30000.0, 2, 2.0, 0.4),
            fbm_og: DFbm::new(s("climate.orogeny_gate"), 16000.0, 2, 2.0, 0.5),
            fbm_psi: DFbm::new(s("climate.pressure"), 36000.0, 2, 2.0, 0.5),
            cal_c: build_map(&Q_C, target_c),
            cal_e: build_map(&Q_E, uniform_target),
            cal_w: build_map(&Q_W, uniform_target),
            cal_t: build_map(&Q_T, target_t),
            cal_h: build_map(&Q_H, uniform_target),
            cal_g: build_map(&Q_G, uniform_target),
            cal_h2: build_map(&Q_H2, uniform_target),
        }
    }

    /// `footprint` = blocks per sample (0 or 1 = full detail); see `Fbm::sample2_lod`.
    pub fn sample(&self, x: f64, z: f64, footprint: f64) -> Climate {
        self.sample_d(x, z, footprint).to_climate()
    }

    /// Raw (uncalibrated) field values: [C, E, river/W, T, H, gate]. Used by calibration tests.
    pub fn sample_raw(&self, x: f64, z: f64, fp: f64) -> [f32; 6] {
        let q = self.warp_c.warp(&P2::identity(x, z), fp);
        let m = self.warp_m.warp(&q, fp);
        let hp = q.half_from(x, z);
        [
            self.fbm_c.value(q.x, q.z, fp),
            erosion_raw(self.fbm_e.sample(q.x + 7313.0, q.z - 3511.0, fp)).v,
            self.fbm_r.value(m.x, m.z, fp),
            self.fbm_t.value(hp.x, hp.z, fp),
            self.fbm_h.value(hp.x - 5101.0, hp.z + 2909.0, fp),
            self.fbm_g.value(x, z, fp),
        ]
    }

    /// Raw effective humidity (before the second calibration `Q_H2`). Used by calibration tests.
    pub fn humidity_raw2(&self, x: f64, z: f64) -> f32 {
        self.sample_parts(x, z, 0.0).0.v
    }

    /// Orogeny belt mask `M = (1 − smoothstep(b₀, b₁, |b|)) · smoothstep(g₀, g₁, g)`.
    ///
    /// Mountain ranges are *tubular neighbourhoods of the zero set* of a very low-frequency field
    /// `b` (λ 30 km, on the continental warp → gently curved): `|b|/G₀` (G₀ = median |∇b|) is a
    /// globally normalised first-order distance to the curve `b = 0`, so every range has a crest
    /// line, a core and broad foothills; its width varies naturally as `1/|∇b|` (massifs where
    /// `b` lingers near 0). Unlike the local estimate `|b|/|∇b|`, this one has an *exact*
    /// gradient (the local one drops a Hessian term of relative size ≈ d·|∇²b|/|∇b| ≈ 0.6).
    /// A second field `g` (λ 16 km) gates the curves into ranges of finite length (≈ 10–30 km).
    #[inline]
    pub fn orogeny(&self, q: &P2, x: f64, z: f64, fp: f64) -> D2 {
        let b = q.chain(self.fbm_o.sample(q.x, q.z, fp));
        let g = self.fbm_og.sample(x, z, fp);
        Self::belt(b, g)
    }

    #[inline]
    fn belt(b: D2, g: D2) -> D2 {
        // |b| has a kink on the crest line, but M is flat (≡ core) there: C¹ overall.
        let core = 1.0 - smoothstep_d(OROGENY_BAND.0, OROGENY_BAND.1, b.abs());
        if core.v <= 0.0 {
            return D2::ZERO;
        }
        core * smoothstep_d(OROGENY_GATE.0, OROGENY_GATE.1, g)
    }

    /// Prevailing wind at p: `w = (∂ψ/∂z, −∂ψ/∂x) / max(|∇ψ|, ε)` — the curl of a scalar
    /// pressure potential ψ (λ 36 km). Divergence-free, circulates around highs and lows.
    #[inline]
    pub fn wind(&self, x: f64, z: f64) -> (f32, f32) {
        let psi = self.fbm_psi.sample(x, z, 0.0);
        let gl = psi.grad_len().max(WIND_EPS);
        (psi.dz / gl, -psi.dx / gl)
    }

    /// Low-pass land indicator and mountain mass at a far-field point (cheap proxies: first
    /// 3 octaves of continentalness, unwarped; orogeny on unwarped coordinates).
    #[inline]
    fn far_fields(&self, x: f64, z: f64) -> (f32, f32) {
        let c = self.cal_c.eval(self.fbm_c.sample_n(x, z, 3).v).0;
        let land = crate::math::smoothstep(-0.24, -0.04, c);
        let m = Self::belt(self.fbm_o.sample(x, z, 0.0), self.fbm_og.sample(x, z, 0.0)).v;
        (land, m * land)
    }

    /// Streamline integral at a lattice node: trace *backward* along the wind field
    /// (`p ← p − Δ·w(p)`, 8 steps of 700 blocks, explicit Euler) and average, with weights
    /// `e^{−k/6}`, the mountain mass (rain shadow) and the ocean fraction (maritime air).
    fn shadow_node(&self, x: f64, z: f64) -> [f32; 2] {
        let (mut px, mut pz) = (x, z);
        let (mut s, mut o, mut ws) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..SHADOW_STEPS {
            let (wx, wz) = self.wind(px, pz);
            px -= SHADOW_STEP * wx as f64;
            pz -= SHADOW_STEP * wz as f64;
            let wk = (-(k as f32) / 6.0).exp();
            let (land, mass) = self.far_fields(px, pz);
            s += wk * mass;
            o += wk * (1.0 - land);
            ws += wk;
        }
        [s / ws, o / ws]
    }

    fn shadow_node_cached(&self, ix: i32, iz: i32) -> [f32; 2] {
        let slot = (((ix as u32).wrapping_mul(0x9E37_79B1) ^ (iz as u32).wrapping_mul(0x85EB_CA77)) >> 20) as usize
            % SHADOW_CACHE_SIZE;
        let hit = SHADOW_CACHE.with(|c| {
            let e = c.borrow()[slot];
            (e.valid && e.seed == self.seed && e.ix == ix && e.iz == iz).then_some(e.v)
        });
        if let Some(v) = hit {
            return v;
        }
        let v = self.shadow_node(ix as f64 * SHADOW_SPACING, iz as f64 * SHADOW_SPACING);
        SHADOW_CACHE.with(|c| c.borrow_mut()[slot] = ShadowEntry { seed: self.seed, ix, iz, valid: true, v });
        v
    }

    /// Rain shadow S and maritime fraction O at (x, z): C¹ interpolation (smoothstep-weighted
    /// bilinear) of the streamline integrals on a 256-block lattice. Both are very
    /// low-frequency (they integrate over ≈ 5.6 km of fields with λ ≥ 16 km).
    pub fn shadow(&self, x: f64, z: f64) -> (D2, D2) {
        let gx = x / SHADOW_SPACING;
        let gz = z / SHADOW_SPACING;
        let (fx0, fz0) = (gx.floor(), gz.floor());
        let (ix, iz) = (fx0 as i32, fz0 as i32);
        let (fx, fz) = ((gx - fx0) as f32, (gz - fz0) as f32);
        let a = self.shadow_node_cached(ix, iz);
        let b = self.shadow_node_cached(ix + 1, iz);
        let c = self.shadow_node_cached(ix, iz + 1);
        let d = self.shadow_node_cached(ix + 1, iz + 1);
        let sx = fx * fx * (3.0 - 2.0 * fx);
        let sz = fz * fz * (3.0 - 2.0 * fz);
        let k = 1.0 / SHADOW_SPACING as f32;
        let dsx = 6.0 * fx * (1.0 - fx) * k;
        let dsz = 6.0 * fz * (1.0 - fz) * k;
        let f = |i: usize| {
            let (va, vb, vc, vd) = (a[i], b[i], c[i], d[i]);
            let top = va + (vb - va) * sx;
            let bot = vc + (vd - vc) * sx;
            D2::new(
                top + (bot - top) * sz,
                ((vb - va) + ((vd - vc) - (vb - va)) * sz) * dsx,
                (bot - top) * dsz,
            )
        };
        (f(0), f(1))
    }

    /// Effective humidity before re-calibration, plus (orogeny, shadow, maritime, wind).
    fn sample_parts(&self, x: f64, z: f64, fp: f64) -> (D2, D2, D2, D2, P2) {
        let q = self.warp_c.warp(&P2::identity(x, z), fp);
        let hp = q.half_from(x, z);
        let h_raw = hp.chain(self.fbm_h.sample(hp.x - 5101.0, hp.z + 2909.0, fp));
        let m = self.orogeny(&q, x, z, fp);
        let (sh, mar) = self.shadow(x, z);
        let he = self.cal_h.eval_d(h_raw) - sh * K_SHADOW + m * (1.0 - sh) * K_ORO + (mar - 0.5) * K_MARITIME;
        (he, m, sh, mar, q)
    }

    /// Full climate with gradients and river geometry.
    pub fn sample_d(&self, x: f64, z: f64, fp: f64) -> ClimateD {
        let (he, m, sh, mar, q) = self.sample_parts(x, z, fp);
        let c_raw = q.chain(self.fbm_c.sample(q.x, q.z, fp));
        let e_raw = q.chain(erosion_raw(self.fbm_e.sample(q.x + 7313.0, q.z - 3511.0, fp)));
        let mw = self.warp_m.warp(&q, fp);
        let n_r = mw.chain(self.fbm_r.sample(mw.x, mw.z, fp));
        let hp = q.half_from(x, z);
        let t_raw = hp.chain(self.fbm_t.sample(hp.x, hp.z, fp));
        let g_raw = self.fbm_g.value(x, z, fp);

        // Orogeny raises the crust: belts over shelf seas become mountainous islands/peninsulas.
        let c = (self.cal_c.eval_d(c_raw) + m * OROGENY_LAND).minf(1.0);
        let e_uni = self.cal_e.eval_d(e_raw);
        // Effective erosion: affine squeeze of the uniform erosion into [−0.3, 1] outside the
        // belts and [−1, −0.1] inside, blended by M → big relief only inside orogenic belts.
        let e = lerp_d(e_uni * 0.65 + 0.35, e_uni * 0.45 - 0.55, m);
        let w = self.cal_w.eval_d(n_r);
        let t = self.cal_t.eval_d(t_raw);
        let h = self.cal_h2.eval_d(he);
        let g = self.cal_g.eval(g_raw).0;
        // pv = 1 − |3|w| − 2| with smoothed folds (smooth |·| = √(x²+ε²) − ε): the kinks of the
        // fold would otherwise print creases parallel to every river.
        let pv = 1.0 - sabs_d(sabs_d(w, 0.06) * 3.0 - 2.0, 0.25);

        // River geometry: regularised distance to the zero set (see `river_distance`).
        let river_dist = river_distance(n_r).v;
        // ~70 % of valleys carry a river; the rest are dry valleys (sources fade smoothly).
        let gate = crate::math::smoothstep(-0.55, -0.15, g);
        let e01 = saturate((e.v + 1.0) * 0.5);
        let coastness = crate::math::smoothstep(0.35, -0.10, c.v);
        let river_width = (2.5 + 9.0 * e01 + 9.0 * coastness) * (0.3 + 0.7 * gate);
        let valley = 40.0 + 220.0 * e01;
        let inland = crate::math::smoothstep(-0.22, -0.10, c.v);
        let river = gate * inland * (1.0 - crate::math::smoothstep(river_width, river_width + valley, river_dist));

        ClimateD {
            c,
            e,
            e_raw: e_uni,
            w,
            pv,
            t,
            h,
            orogeny: m,
            wind: self.wind(x, z),
            shadow: sh.v,
            maritime: mar.v,
            river_n: n_r,
            river_dist,
            river_gate: gate,
            river_width,
            river,
            q,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::{hash_f01, mix64};

    fn rnd(i: u64) -> f64 {
        hash_f01(mix64(i)) as f64
    }

    fn quantiles(mut v: Vec<f32>) -> Vec<f32> {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        P_LEVELS.iter().map(|&p| v[((v.len() - 1) as f32 * p) as usize]).collect()
    }

    /// Measures raw quantiles (pooled over seeds 1, 42, 2026; 200k samples each over 40×40 km).
    /// The printed tables are the Q_* constants above. Run with `--nocapture` to see them.
    #[test]
    fn calibration_quantiles() {
        let mut fields: Vec<Vec<f32>> = vec![Vec::new(); 7];
        for &seed in &[1u64, 42, 2026] {
            let cs = ClimateSampler::new(seed);
            for i in 0..200_000u64 {
                let x = (rnd(i * 2 + seed * 7_777_777) - 0.5) * 40_000.0;
                let z = (rnd(i * 2 + 1 + seed * 7_777_777) - 0.5) * 40_000.0;
                let r = cs.sample_raw(x, z, 0.0);
                for k in 0..6 {
                    fields[k].push(r[k]);
                }
                if i % 4 == 0 {
                    fields[6].push(cs.humidity_raw2(x, z));
                }
            }
        }
        let names = ["Q_C", "Q_E", "Q_W", "Q_T", "Q_H", "Q_G", "Q_H2"];
        let consts = [&Q_C, &Q_E, &Q_W, &Q_T, &Q_H, &Q_G, &Q_H2];
        let mut worst = 0.0f32;
        for k in 0..7 {
            let q = quantiles(fields[k].clone());
            let s: Vec<String> = q.iter().map(|v| format!("{v:.4}")).collect();
            eprintln!("const {}: [f32; 25] = [{}];", names[k], s.join(", "));
            for (a, b) in q.iter().zip(consts[k].iter()) {
                worst = worst.max((a - b).abs());
            }
        }
        // The committed constants must match the measurement (otherwise re-paste them).
        assert!(worst < 0.03, "calibration constants are stale (max deviation {worst})");
    }

    #[test]
    fn calibrated_distributions() {
        let mut n = 0usize;
        let (mut ocean, mut deep, mut coast) = (0usize, 0usize, 0usize);
        let mut sums = [0.0f64; 4];
        let mut hot = 0usize;
        let mut cold = 0usize;
        for &seed in &[1u64, 42, 2026] {
            let cs = ClimateSampler::new(seed);
            for i in 0..40_000u64 {
                let x = (rnd(i * 2 + seed * 31) - 0.5) * 40_000.0;
                let z = (rnd(i * 2 + 1 + seed * 31) - 0.5) * 40_000.0;
                let c = cs.sample(x, z, 0.0);
                n += 1;
                if c.continentalness < -0.18 {
                    ocean += 1;
                }
                if c.continentalness < -0.45 {
                    deep += 1;
                }
                if (-0.18..-0.05).contains(&c.continentalness) {
                    coast += 1;
                }
                sums[0] += c.erosion.abs() as f64;
                sums[1] += c.temperature.abs() as f64;
                sums[2] += c.humidity.abs() as f64;
                sums[3] += c.weirdness.abs() as f64;
                if c.temperature > 0.5 {
                    hot += 1;
                }
                if c.temperature < -0.5 {
                    cold += 1;
                }
            }
        }
        let f = |k: usize| k as f64 / n as f64;
        eprintln!(
            "ocean {:.3} deep {:.3} coast {:.3} hot {:.3} cold {:.3} mean|E| {:.3} |T| {:.3} |H| {:.3} |W| {:.3}",
            f(ocean),
            f(deep),
            f(coast),
            f(hot),
            f(cold),
            sums[0] / n as f64,
            sums[1] / n as f64,
            sums[2] / n as f64,
            sums[3] / n as f64
        );
        assert!((0.40..0.50).contains(&f(ocean)));
        assert!((0.15..0.25).contains(&f(deep)));
        assert!((0.05..0.11).contains(&f(coast)));
        assert!((0.18..0.36).contains(&f(hot)));
        assert!((0.10..0.25).contains(&f(cold)));
        for k in [0, 2, 3] {
            let m = sums[k] / n as f64;
            assert!((0.42..0.58).contains(&m), "uniform field mean |x| {m}");
        }
    }

    #[test]
    fn gradients_match_finite_differences() {
        let cs = ClimateSampler::new(42);
        let h = 0.05;
        let mut bad = 0;
        for i in 0..2000u64 {
            let x = (rnd(i * 2) - 0.5) * 20_000.0;
            let z = (rnd(i * 2 + 1) - 0.5) * 20_000.0;
            let a = cs.sample_d(x, z, 0.0);
            let px = cs.sample_d(x + h, z, 0.0);
            let mx = cs.sample_d(x - h, z, 0.0);
            let pz = cs.sample_d(x, z + h, 0.0);
            let mz = cs.sample_d(x, z - h, 0.0);
            for (f, fx1, fx0, fz1, fz0) in [
                (a.c, px.c, mx.c, pz.c, mz.c),
                (a.e, px.e, mx.e, pz.e, mz.e),
                (a.t, px.t, mx.t, pz.t, mz.t),
                (a.river_n, px.river_n, mx.river_n, pz.river_n, mz.river_n),
            ] {
                let gx = (fx1.v - fx0.v) / (2.0 * h as f32);
                let gz = (fz1.v - fz0.v) / (2.0 * h as f32);
                let err = ((gx - f.dx).powi(2) + (gz - f.dz).powi(2)).sqrt();
                let mag = (gx * gx + gz * gz).sqrt();
                if err > 0.02 * mag + 2e-5 {
                    bad += 1;
                }
            }
        }
        assert!(bad < 40, "{bad} gradient mismatches");
    }

    #[test]
    fn monomap_is_monotone_and_interpolates() {
        let m = build_map(&Q_C, target_c);
        let mut prev = -2.0;
        for i in 0..=4000 {
            let x = -1.0 + 2.0 * i as f32 / 4000.0;
            let (v, d) = m.eval(x);
            assert!(v >= prev - 1e-5);
            assert!(d >= -1e-5);
            prev = v;
        }
        for (i, &p) in P_LEVELS.iter().enumerate() {
            assert!((m.eval_knots(Q_C[i]).0 - target_c(p)).abs() < 1e-4);
            assert!((m.eval(Q_C[i]).0 - target_c(p)).abs() < 5e-3);
        }
    }

    #[test]
    fn determinism() {
        let a = ClimateSampler::new(7);
        let b = ClimateSampler::new(7);
        for i in 0..100 {
            let x = i as f64 * 123.4 - 5000.0;
            assert_eq!(a.sample(x, -x * 0.7, 0.0), b.sample(x, -x * 0.7, 0.0));
        }
    }
}
