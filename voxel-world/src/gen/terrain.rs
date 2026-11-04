//! Terrain shaping: heights, 3D density. OWNER: gen-core.
//!
//! # Mathematics
//!
//! The surface height `H(x,z)` is one closed-form expression evaluated on dual numbers
//! ([`D2`]: value + exact gradient), so `slope = |∇H|` and the aspect come for free with a single
//! evaluation (forward-mode automatic differentiation through splines, warps and fractal sums).
//!
//! ```text
//! H₀ = O(C,E,PV) + A(C,E,PV) · mix(hills, ridges, s(PV)) · v(PV) + seafloor + roughness
//! ```
//! * `O`, `A` — nested monotone cubic splines (Fritsch–Carlson; evaluated by [`spline_d`], a
//!   dual-number re-implementation of `spline::Spline::eval`). `O` encodes the hypsometric curve
//!   (deep ocean 44 → shelf 124 → coast 128–160 → lowland 145–160 → inland 160–224) with an
//!   erosion-controlled *sea-cliff step* at the coast; `A` is the mountain uplift (0 at the sea,
//!   300 for E → −1 inland).
//! * hills = derivative-damped ("eroded") fBm, ridges = smooth ridged multifractal on the
//!   continental warp (curved ranges), blended by pv-peakness and uplift; `v(pv)` lowers valleys.
//!   Because `pv` is folded from the river noise, ridges run between rivers.
//! * **Orogenic massif** (DESIGN §2a): `+130·smoothstep(0.05, 0.9, M)` under the core of an
//!   orogeny belt `M` (see `climate.rs`), on which the ridged relief (amplitude from the
//!   effective erosion E*, itself squeezed low inside belts) sits → few, large ranges
//!   (≈ 10 % of land above SEA+150, main peaks 430–496, ranges 10–20 km long).
//! * **Regional character** (§2a anti-monotony): a λ 8 km selector χ makes *open plains* where
//!   χ < 0.08…−0.10 (≈ 35 % of the land: low, calm steppe/meadow/floodplain basins at
//!   SEA + 3…6.5, ±1-block swells, rivers in broad floodplains with 2 % banks), rolling hills in
//!   the middle, *plateau
//!   country* (two-tier tablelands bounded by ≈ 50-block escarpments) where χ > 0.3, and *skerry
//!   archipelagos* on the shelf where χ < −0.1; plus mesa country (badlands), dune seas, volcanoes.
//! * **Inland lakes**: one candidate per 1.4 km cell; flat level from the pre-lake rim, basin +
//!   containment sill (see [`TerrainShaper::apply_lake`] for the no-water-wall theorem).
//! * **Gully filter** (dendritic drainage): 4 octaves of slope-aligned plane waves on a jittered
//!   lattice, wave vector ⟂ a smooth steering gradient, amplitude ∝ local slope × wavelength,
//!   each octave steered by the previous ones (see [`TerrainShaper::gullies`]).
//! * **Rivers**: with `d = min(|n|/√(|∇n|²+f²), |n|/G)` (distance to the zero set of the river
//!   noise; the min keeps it Lipschitz, see the river stage) the valley is the lower envelope
//!   `H = smin(H₀, V(d))`, `V(d) = bank − D(1 − d²/w²)` inside the channel and
//!   `bank + σ(E)·u + κ(E)·u²`, `u = d − w`, outside (σ ≈ 0.05 flood plains … 2.6 canyons), the
//!   wall slope capped C¹ at 2.2 (linear continuation beyond `u* = (2.2 − σ)/2κ`).
//! * **Mesas**: smooth terraces `τ(h) = s(⌊h/s⌋ + smoothstep(0.80, 1, frac(h/s)))` applied to a
//!   plateau field; for far LOD the terrace is *box-filtered analytically* over the height range
//!   seen by one sample (`Δh = |∇h|·footprint`) using the closed-form antiderivative of the periodic
//!   part — mesas fade into their mean instead of aliasing.
//! * **Dunes**: `A·(1 − |sin πu'|)^1.5` with `u = frac((p·ŵ)/λ + φ(p)/π)` (wind ŵ, fBm phase warp
//!   φ) and an asymmetric piecewise-linear reparametrisation `u → u'` (28 % steep slip face,
//!   72 % gentle windward side), modulated by a barchan field.
//! * **Hoodoos**: Poisson-like point process on a jittered 14-block grid; each spire is a
//!   superellipse dome `Hs(1 − (r/R)⁴)^¼` (vertical walls, rounded cap); max over the 3×3
//!   neighbourhood so spires are never clipped by Voronoi borders.
//! * **Volcanoes**: rare cells of a 3.6 km grid; concave cone `apex − K·d^0.62` (steep summit,
//!   gentle apron) joined with smooth-max, caldera `rim − D(1 − (d/r_c)⁴)` with a lava lake level.
//! * 3D density `ρ = (H − y) + J·N₃(x, 1.4y, z)` with |N₃| ≤ 1 ⇒ sign(ρ) = sign(H − y) exactly
//!   outside `[H − J, H + J]`; above the surface N₃ is biased downward to suppress floating blobs.
use super::biome::{aridity, classify, lowland_biome, BiomeInput, SnowLine, Strata};
use super::climate::{lerp_d, river_distance, sabs_d, smoothstep_d, ClimateD, ClimateSampler, DEroded, DFbm, DRidged, FastSimplex, D2};
use super::{Biome, ColumnInfo, SEA_LEVEL, WORLD_HEIGHT};
use crate::math::{lerp, saturate, smoothstep};
use crate::noise::{derive_seed, hash2, hash_f01, mix64, salt, Fbm, Worley2};
use crate::spline::{Spline, SplineValue};

const SEA: f32 = SEA_LEVEL as f32;

// =====================================================================================
// Nested spline evaluation on dual numbers
// =====================================================================================

/// Evaluate a (nested) `Spline` on dual inputs: identical value to `Spline::eval`, plus the exact
/// gradient (chain rule through the Hermite basis, the Fritsch–Carlson tangents and the nested
/// control values).
pub fn spline_d(s: &Spline, inputs: &[D2]) -> D2 {
    #[inline]
    fn val(s: &Spline, i: usize, inputs: &[D2]) -> D2 {
        match &s.points[i].1 {
            SplineValue::Const(v) => D2::c(*v),
            SplineValue::Nested(n) => spline_d(n, inputs),
        }
    }
    #[inline]
    fn tangent(a: Option<D2>, b: D2) -> D2 {
        match a {
            None => b,
            Some(a) => {
                if a.v * b.v <= 0.0 {
                    D2::ZERO
                } else {
                    (a * b * 2.0) / (a + b)
                }
            }
        }
    }
    #[inline]
    fn limit(m: D2, secant: D2) -> D2 {
        if secant.v == 0.0 || m.v * secant.v < 0.0 {
            return D2::ZERO;
        }
        let lim = secant.abs() * 3.0;
        if m.v > lim.v {
            lim
        } else if m.v < -lim.v {
            -lim
        } else {
            m
        }
    }
    let x = inputs[s.coord];
    let p = &s.points;
    let n = p.len();
    if n == 1 || x.v <= p[0].0 {
        return val(s, 0, inputs);
    }
    if x.v >= p[n - 1].0 {
        return val(s, n - 1, inputs);
    }
    let (mut lo, mut hi) = (0usize, n - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if p[mid].0 <= x.v {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let k = lo;
    let (x0, x1) = (p[k].0, p[k + 1].0);
    let y0 = val(s, k, inputs);
    let y1 = val(s, k + 1, inputs);
    let h = x1 - x0;
    let d = (y1 - y0) * (1.0 / h);
    let d_prev = if k > 0 { Some((y0 - val(s, k - 1, inputs)) * (1.0 / (x0 - p[k - 1].0))) } else { None };
    let d_next = if k + 2 < n { Some((val(s, k + 2, inputs) - y1) * (1.0 / (p[k + 2].0 - x1))) } else { None };
    let m0 = limit(tangent(d_prev, d), d);
    let m1 = limit(tangent(Some(d), d_next.unwrap_or(d)), d);
    let t = (x - x0) * (1.0 / h);
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = t3 * 2.0 - t2 * 3.0 + 1.0;
    let h10 = t3 - t2 * 2.0 + t;
    let h01 = t2 * 3.0 - t3 * 2.0;
    let h11 = t3 - t2;
    h00 * y0 + h10 * m0 * h + h01 * y1 + h11 * m1 * h
}

fn cs(coord: usize, pts: &[(f32, f32)]) -> SplineValue {
    Spline::consts(coord, pts).into()
}

/// Base offset O(C, E, PV): the hypsometric curve. Inputs: [C, E, PV].
fn offset_spline() -> Spline {
    Spline::new(
        0,
        vec![
            (-1.05, 44.0.into()),
            (-0.75, 58.0.into()),
            (-0.45, 96.0.into()),
            (-0.30, 114.0.into()),
            (-0.20, 124.0.into()),
            (-0.17, cs(1, &[(-1.0, 131.0), (-0.3, 129.5), (0.4, 127.5), (1.0, 127.0)])),
            // Low erosion → sea cliff (≈25-block step within ΔC ≈ 0.02); high erosion → beach.
            (-0.15, cs(1, &[(-1.0, 155.0), (-0.55, 149.0), (-0.3, 141.0), (0.0, 136.5), (1.0, 135.5)])),
            (-0.09, cs(1, &[(-1.0, 160.0), (-0.55, 155.0), (-0.3, 145.5), (0.0, 141.3), (1.0, 140.7)])),
            (-0.03, cs(1, &[(-1.0, 160.0), (-0.5, 155.0), (-0.2, 147.0), (0.4, 143.0), (1.0, 142.0)])),
            (0.08, cs(2, &[(-1.0, 145.0), (-0.4, 149.0), (0.0, 154.0), (1.0, 160.0)])),
            (0.25, cs(2, &[(-1.0, 149.0), (-0.4, 156.0), (0.0, 166.0), (1.0, 178.0)])),
            (0.55, cs(2, &[(-1.0, 156.0), (-0.4, 168.0), (0.0, 184.0), (1.0, 200.0)])),
            (1.0, cs(2, &[(-1.0, 164.0), (-0.4, 182.0), (0.0, 202.0), (1.0, 224.0)])),
        ],
    )
}

/// Broad orogenic uplift (blocks) under the core of a mountain belt (`U·smoothstep(M)`): the
/// elevated massif on which the ridged relief sits (valley floors of big ranges are high, too).
const OROGENIC_UPLIFT: f32 = 75.0;
/// Minimum ridged-relief amplitude in the core of an orogeny belt.
const BELT_AMP: f32 = 250.0;

/// Mountain amplitude A(C, E): uplift that multiplies the [0,1] mountain shape.
fn amp_spline() -> Spline {
    Spline::new(
        0,
        vec![
            (-0.16, 0.0.into()),
            (-0.03, cs(1, &[(-1.0, 70.0), (-0.5, 25.0), (0.0, 5.0), (1.0, 1.0)])),
            (0.12, cs(1, &[(-1.0, 220.0), (-0.65, 155.0), (-0.3, 50.0), (0.0, 18.0), (0.5, 6.0), (1.0, 2.0)])),
            (0.32, cs(1, &[(-1.0, 275.0), (-0.7, 232.0), (-0.4, 128.0), (-0.1, 40.0), (0.4, 12.0), (1.0, 4.0)])),
            (0.65, cs(1, &[(-1.0, 295.0), (-0.7, 250.0), (-0.4, 148.0), (-0.1, 50.0), (0.4, 16.0), (1.0, 5.0)])),
        ],
    )
}

/// River valley wall slope σ(E) (rise per block) and convexity κ(E).
fn wall_spline() -> Spline {
    Spline::consts(1, &[(-1.0, 0.55), (-0.6, 0.5), (-0.25, 0.38), (0.1, 0.16), (0.5, 0.06), (1.0, 0.035)])
}

// =====================================================================================
// Closed-form landform helpers
// =====================================================================================

/// Smooth terrace profile τ(u) = ⌊u⌋ + smoothstep(T0, 1, frac u) and its derivative.
const T0: f32 = 0.80;

#[inline]
fn terrace_unit(u: f32) -> (f32, f32) {
    let fl = u.floor();
    let f = u - fl;
    let t = saturate((f - T0) / (1.0 - T0));
    (fl + t * t * (3.0 - 2.0 * t), 6.0 * t * (1.0 - t) / (1.0 - T0))
}

/// Antiderivative of the periodic part g(f) = f − smoothstep(T0, 1, f), extended to all u:
/// G̃(u) = ⌊u⌋·∫₀¹g + G(frac u). ∫₀¹ g = ½ − (1 − T0)/2.
#[inline]
fn g_anti(u: f32) -> f32 {
    let fl = u.floor();
    let f = u - fl;
    let w = 1.0 - T0;
    let s = if f <= T0 {
        0.0
    } else {
        let t = (f - T0) / w;
        w * (t * t * t - 0.5 * t * t * t * t)
    };
    fl * (0.5 - 0.5 * w) + (0.5 * f * f - s)
}

#[inline]
fn g_per(u: f32) -> f32 {
    let f = u - u.floor();
    let t = saturate((f - T0) / (1.0 - T0));
    f - t * t * (3.0 - 2.0 * t)
}

/// Terrace with step `s`, box-filtered over a height window `delta` (anti-aliasing for LOD).
/// τ_s(h) = h − s·g(h/s); its box average is h − s·(G̃(b) − G̃(a))/(b − a).
fn terrace_d(h: D2, s: f32, delta: f32) -> D2 {
    let u = h.v / s;
    if delta < 0.05 * s {
        let (t, dt) = terrace_unit(u);
        return h.map(t * s, dt);
    }
    let du = delta / s;
    let (a, b) = (u - 0.5 * du, u + 0.5 * du);
    let mean_g = (g_anti(b) - g_anti(a)) / du;
    let dmean = (g_per(b) - g_per(a)) / du;
    h.map(h.v - s * mean_g, 1.0 - dmean)
}

/// sin/cos for |x| ≲ 10π: range reduction to [−π, π] + odd/even minimax polynomials
/// (|error| < 2·10⁻⁴ — plenty for gully waves, ~4× cheaper than libm).
#[inline(always)]
fn fast_sin_cos(x: f32) -> (f32, f32) {
    const INV_TAU: f32 = 1.0 / std::f32::consts::TAU;
    let x = x - std::f32::consts::TAU * (x * INV_TAU).round();
    // fold to [−π/2, π/2] for sin, keep the sign of cos
    let (xs, csign) = if x > std::f32::consts::FRAC_PI_2 {
        (std::f32::consts::PI - x, -1.0)
    } else if x < -std::f32::consts::FRAC_PI_2 {
        (-std::f32::consts::PI - x, -1.0)
    } else {
        (x, 1.0)
    };
    let x2 = xs * xs;
    let sn = xs * (1.0 + x2 * (-0.166_666_57 + x2 * (0.008_332_557 + x2 * (-0.000_198_097_6))));
    let cs = 1.0 + x2 * (-0.5 + x2 * (0.041_666_42 + x2 * (-0.001_388_397_1 + x2 * 0.000_024_390_04)));
    (sn, cs * csign)
}

/// Mean of the dune profile (1 − |sin πt|)^1.5 over a period (numerically ≈ 0.2788).
const DUNE_MEAN: f32 = 0.2788;

/// Generalised smooth terrace τ(u) = ⌊u⌋ + smoothstep(t0, 1, frac u) (riser = last 1 − t0 of
/// each period) with step `s`, box-filtered over a height window `delta` for LOD (closed-form
/// antiderivative of the periodic part, see [`terrace_d`]).
fn terrace_gen(h: D2, s: f32, delta: f32, t0: f32) -> D2 {
    let w = 1.0 - t0;
    let u = h.v / s;
    if delta < 0.05 * s {
        let fl = u.floor();
        let f = u - fl;
        let t = saturate((f - t0) / w);
        return h.map((fl + t * t * (3.0 - 2.0 * t)) * s, 6.0 * t * (1.0 - t) / w);
    }
    let anti = |u: f32| -> f32 {
        let fl = u.floor();
        let f = u - fl;
        let sm = if f <= t0 {
            0.0
        } else {
            let t = (f - t0) / w;
            w * (t * t * t - 0.5 * t * t * t * t)
        };
        fl * (0.5 - 0.5 * w) + (0.5 * f * f - sm)
    };
    let per = |u: f32| -> f32 {
        let f = u - u.floor();
        let t = saturate((f - t0) / w);
        f - t * t * (3.0 - 2.0 * t)
    };
    let du = delta / s;
    let (a, b) = (u - 0.5 * du, u + 0.5 * du);
    let mean_g = (anti(b) - anti(a)) / du;
    let dmean = (per(b) - per(a)) / du;
    h.map(h.v - s * mean_g, 1.0 - dmean)
}

// =====================================================================================
// Landform patches (gameplay pass)
// =====================================================================================

/// Patch lattice: Voronoi cells on domain-warped coordinates, nominal size 520 blocks, points
/// jittered in the middle half of their lattice cell (so the 3×3 search sees every cell whose
/// distance is within 0.19·L = 99 blocks of the nearest one — more than the blend width).
pub const PATCH_CELL: f64 = 520.0;
/// Blend widths in `d_k − d_1` (blocks): soft (relief parameters) and sharp (patch level offset →
/// escarpments between neighbouring patches).
const PATCH_SOFT: f32 = 90.0;
const PATCH_SHARP: f32 = 28.0;
const PATCH_CACHE_SIZE: usize = 512;

/// Number of blended style parameters, and their indices.
pub const NP: usize = 13;
/// Relief amplitude (blocks).
pub const P_AMP: usize = 0;
/// Spectrum: 0 = broad swells (λ 600–300) … 1 = hummocks (λ 150–75).
pub const P_FINE: usize = 1;
/// Profile: −1 billowy (rounded knolls, creased hollows) … 0 plain … +1 ridged (sharp crests).
pub const P_SHAPE: usize = 2;
/// Terrace strength 0..1, step (blocks), riser sharpness 0..1.
pub const P_TERR: usize = 3;
pub const P_STEP: usize = 4;
pub const P_SHARP: usize = 5;
/// Point-landform densities: rock tors, karst towers, buttes.
pub const P_TOR: usize = 6;
pub const P_KARST: usize = 7;
pub const P_BUTTE: usize = 8;
/// Dune sea, mesa country, hoodoo field, swamp flats.
pub const P_DUNE: usize = 9;
pub const P_MESA: usize = 10;
pub const P_HOODOO: usize = 11;
pub const P_SWAMP: usize = 12;

/// Landform style of a patch (its shape identity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Style {
    /// Flat steppe / plains: tiny swells, rare tors.
    #[default]
    Flat = 0,
    /// Rolling meadow: hummocks and hollows (ponds).
    Rolling,
    /// Forest hills: mid hills with stepped flanks and occasional outcrops.
    Hills,
    /// Crags: ridged knolls, rock tors everywhere, stepped ledges.
    Craggy,
    /// Terraced highland: grassy steps on every slope.
    Terraced,
    /// Steppe with buttes and small mesas.
    Buttes,
    /// Tower karst: steep limestone towers on a flat floor.
    Karst,
    /// Dune sea.
    Dunes,
    /// Rocky desert: inselbergs, buttes, thin dunes.
    RockyDesert,
    /// Mesa country (badlands).
    Mesa,
    /// Hoodoo field (badlands).
    Hoodoo,
    /// Swamp flats.
    Swamp,
    /// Raised tableland with escarpment edges.
    Plateau,
}

impl Style {
    /// Base parameters [amp, fine, shape, terr, step, sharp, tor, karst, butte, dune, mesa, hoodoo,
    /// swamp] and level offset.
    fn params(self) -> ([f32; NP], f32) {
        use Style::*;
        match self {
            Flat => ([2.2, 0.3, -0.3, 0.0, 6.0, 0.7, 0.10, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0),
            Rolling => ([6.5, 0.8, -0.75, 0.0, 6.0, 0.5, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 1.0),
            Hills => ([17.0, 0.35, 0.25, 0.5, 7.0, 0.75, 0.16, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 4.0),
            Craggy => ([13.0, 0.6, 0.6, 0.6, 6.0, 0.85, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 3.0),
            Terraced => ([22.0, 0.2, 0.0, 0.95, 7.0, 0.88, 0.06, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 8.0),
            Buttes => ([3.5, 0.3, -0.2, 0.35, 8.0, 0.9, 0.05, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0], 0.0),
            Karst => ([5.0, 0.5, -0.4, 0.15, 6.0, 0.8, 0.0, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0),
            Dunes => ([2.0, 0.3, 0.0, 0.0, 6.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], 0.0),
            RockyDesert => ([6.0, 0.35, 0.2, 0.45, 8.0, 0.9, 0.18, 0.0, 0.4, 0.35, 0.0, 0.0, 0.0], 2.0),
            Mesa => ([3.0, 0.3, 0.0, 0.0, 9.0, 0.8, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0.0),
            Hoodoo => ([3.0, 0.3, 0.0, 0.0, 9.0, 0.8, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0], 0.0),
            Swamp => ([1.5, 0.6, -0.3, 0.0, 6.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0], 0.0),
            Plateau => ([9.0, 0.3, 0.1, 0.75, 8.0, 0.9, 0.12, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 26.0),
        }
    }
}

/// The decision of one patch cell (pure function of (seed, cell); memoised per thread).
#[derive(Clone, Copy, Debug, Default)]
pub struct PatchCell {
    pub biome: Biome,
    pub style: Style,
    pub p: [f32; NP],
    pub off: f32,
}

/// Blended patch parameters at a column (with gradients) + the nearest cell's labels.
#[derive(Clone, Copy, Debug, Default)]
pub struct PatchMix {
    pub p: [D2; NP],
    pub off: D2,
    pub biome: Biome,
    pub style: Style,
    /// d₂ − d₁: distance-like measure to the nearest patch border.
    pub edge: f32,
}

#[derive(Clone, Copy)]
struct PatchEntry {
    seed: u64,
    ix: i32,
    iz: i32,
    valid: bool,
    cell: PatchCell,
}

thread_local! {
    static PATCH_CACHE: std::cell::RefCell<Vec<PatchEntry>> = std::cell::RefCell::new(vec![
        PatchEntry { seed: 0, ix: 0, iz: 0, valid: false, cell: PatchCell::default() };
        PATCH_CACHE_SIZE
    ]);
}

/// Rock landform point process (tors, karst towers, buttes, mountain spires): jittered grid.
const ROCK_CELL: f64 = 64.0;
const SPIRE_CELL: f64 = 90.0;

/// Size/shape of a rock landform kind: radius r0 + r1·a, height h0 + h1·b (a, b hashed), dome
/// exponents k (wall steepness) and e (crown roundness), see [`TerrainShaper::point_rocks`].
#[derive(Clone, Copy, Debug)]
struct RockKind {
    r0: f32,
    r1: f32,
    h0: f32,
    h1: f32,
    k: f32,
    e: f32,
}
/// Granite-like tor: squat, rounded, 4–12 radius, 4–14 tall.
const TOR: RockKind = RockKind { r0: 4.0, r1: 8.0, h0: 4.0, h1: 10.0, k: 2.5, e: 0.45 };
/// Tower karst: near-vertical walls, domed top, 9–24 radius, 20–58 tall.
const KARST: RockKind = RockKind { r0: 9.0, r1: 15.0, h0: 20.0, h1: 38.0, k: 6.0, e: 0.3 };
/// Butte / small mesa: flat top, cliff walls, 14–38 radius, 7–23 tall.
const BUTTE: RockKind = RockKind { r0: 14.0, r1: 24.0, h0: 7.0, h1: 16.0, k: 10.0, e: 0.15 };
/// Mountain spire / needle on high ridges: 4–10 radius, 18–50 tall.
const SPIRE: RockKind = RockKind { r0: 4.0, r1: 6.0, h0: 18.0, h1: 32.0, k: 1.8, e: 0.9 };

/// Landform style of a patch with lowland biome `b` (`u` hashed in [0,1)).
fn pick_style(b: Biome, open: bool, u: f32, hoodoo: bool) -> Style {
    use Style::*;
    let pick = |opts: &[(Style, f32)]| -> Style {
        let mut acc = 0.0;
        for &(s, w) in opts {
            acc += w;
            if u < acc {
                return s;
            }
        }
        opts[opts.len() - 1].0
    };
    match b {
        Biome::Plains if open => pick(&[(Flat, 0.5), (Rolling, 0.25), (Buttes, 0.15), (Terraced, 0.1)]),
        Biome::Plains => pick(&[(Flat, 0.2), (Rolling, 0.25), (Terraced, 0.25), (Buttes, 0.15), (Plateau, 0.15)]),
        Biome::Meadow if open => pick(&[(Rolling, 0.6), (Terraced, 0.25), (Craggy, 0.15)]),
        Biome::Meadow => pick(&[(Rolling, 0.4), (Terraced, 0.3), (Craggy, 0.15), (Plateau, 0.15)]),
        Biome::Forest => pick(&[(Hills, 0.5), (Craggy, 0.25), (Terraced, 0.15), (Plateau, 0.1)]),
        Biome::BirchForest => pick(&[(Rolling, 0.5), (Hills, 0.5)]),
        Biome::DarkForest => pick(&[(Hills, 0.5), (Craggy, 0.3), (Rolling, 0.2)]),
        Biome::Taiga => pick(&[(Craggy, 0.45), (Hills, 0.35), (Terraced, 0.1), (Plateau, 0.1)]),
        Biome::OldGrowthTaiga => pick(&[(Craggy, 0.6), (Hills, 0.4)]),
        Biome::Jungle => pick(&[(Karst, 0.6), (Hills, 0.4)]),
        Biome::Savanna => pick(&[(Buttes, 0.4), (Flat, 0.25), (Terraced, 0.2), (Plateau, 0.15)]),
        Biome::Desert => pick(&[(Dunes, 0.7), (RockyDesert, 0.3)]),
        Biome::Badlands => {
            if hoodoo {
                Hoodoo
            } else {
                Mesa
            }
        }
        Biome::Swamp => Swamp,
        _ => Hills,
    }
}

// =====================================================================================
// TerrainShaper
// =====================================================================================

/// Smooth landform/biome region weights derived from the climate (shared by terrain and biomes,
/// so special landforms and biome labels always agree).
#[derive(Clone, Copy, Debug, Default)]
pub struct Regions {
    /// Hot & dry (dunes, desert).
    pub desert: f32,
    /// Hot, very dry, low/mid erosion (terraced mesas).
    pub badlands: f32,
    /// Hoodoo spire fields (subset of badlands).
    pub hoodoo: f32,
    /// Warm-humid low flats.
    pub swamp: f32,
    /// Volcano cone dominates the surface.
    pub volcanic: f32,
    /// Sea-cliff strength (low-erosion coast).
    pub cliff: f32,
    /// Inside a river channel (0..1).
    pub channel: f32,
    /// Inside an inland lake basin (1 inside the shore ring, 0 outside).
    pub lake: f32,
    /// Flat water level of that lake (world y; meaningful when `lake > 0`).
    pub lake_level: f32,
    /// Aridity α = hot·dry ∈ [0, 1] (orders the drylands, see `biome::aridity`).
    pub arid: f32,
    /// Open-plains weight: calm lowlands (low regional character χ) outside the belts.
    pub plain: f32,
    /// 1 where a sea stack raised the column (intended vertical walls).
    pub stack: f32,
    /// Lowland biome of the nearest landform patch (see [`PatchCell`]).
    pub patch: Biome,
    /// Landform style of the nearest patch.
    pub style: Style,
    /// 1 where a rock landform (tor, karst tower, butte, spire) raised the column (intended
    /// vertical walls).
    pub rock: f32,
    /// Terrace strength applied to the column (stepped relief).
    pub terrace: f32,
}

/// Everything computed for a column (public for debug tools).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shape {
    pub height: D2,
    /// Mountain term (height above the base offset).
    pub mountain: f32,
    pub regions: Regions,
    pub jaggedness: f32,
    /// Liquid surface (water, or lava in calderas) or `i32::MIN`.
    pub water_level: i32,
    /// Gully steering gradient (debug).
    pub steer: (f32, f32),
}

pub struct TerrainShaper {
    pub seed: u64,
    offset: Spline,
    amp: Spline,
    wall: Spline,
    ridged: DRidged,
    hills: DEroded,
    rough: DFbm,
    jag: DFbm,
    select: DFbm,
    n3: Fbm,
    dune_phase: DFbm,
    dune_mod: DFbm,
    wind: (f64, f64),
    mesa: DFbm,
    /// Regional character selector χ (λ 8 km): low → plains / archipelagos, high → plateau country.
    character: DFbm,
    plateau: DFbm,
    islands: DFbm,
    swamp_n: DFbm,
    hoodoo: Worley2,
    volcano: Worley2,
    snow: SnowLine,
    /// Gentle swells of the open plains (λ 1.3 km).
    swell: DFbm,
    /// Bedding model shared with the surface rules (differential-erosion notches).
    strata: Strata,
    /// Gate of the rock-shelter notches along cliffs (λ 85).
    notch_gate: FastSimplex,
    stacks: Worley2,
    /// Climate sampler for patch decisions (climate at the patch point).
    clim: ClimateSampler,
    /// Domain warp of the patch lattice (two octaves per axis).
    pw_a: FastSimplex,
    pw_b: FastSimplex,
    /// Patch relief spectrum: 4 equal-amplitude octaves λ 600 … 75, re-weighted per patch.
    relief: DFbm,
    /// Terrace step modulation and terrace mask (λ 700 / 330).
    step_n: FastSimplex,
}

/// Inland lakes: cell size, candidate probability, max radius, rim probes.
const LAKE_CELL: f64 = 1400.0;
const LAKE_P: f32 = 0.8;
const LAKE_R_MAX: f32 = 300.0;
const LAKE_RIM_SAMPLES: usize = 12;
const LAKE_CACHE_SIZE: usize = 1024;

#[derive(Clone, Copy)]
struct LakeEntry {
    seed: u64,
    ix: i32,
    iz: i32,
    valid: bool,
    /// NaN = probed, no lake.
    level: f32,
}

thread_local! {
    /// Memo of probed lake levels (pure function of (seed, cell): never changes results).
    static LAKE_CACHE: std::cell::RefCell<Vec<LakeEntry>> = std::cell::RefCell::new(vec![
        LakeEntry { seed: 0, ix: 0, iz: 0, valid: false, level: f32::NAN };
        LAKE_CACHE_SIZE
    ]);
}

/// Basin depth of a lake of radius r.
#[inline]
fn lake_depth(r: f32) -> f32 {
    7.0 + 0.07 * r
}

/// |x| as a dual (kink at 0 is the sill crest).
#[inline]
fn sabs_tent(x: D2) -> D2 {
    x.abs()
}

/// Gradient scale G of the river noise for the valley distance min(|n|/s, |n|/G) (≈ p60 |∇n|).
const RIVER_WALL_G: f32 = 1.5e-3;

/// Plateau country where the regional character χ exceeds this (≈ 27 % of the land).
const PLATEAU_CHI: f32 = 0.3;
/// Height of the plateau-country step (two tiers: 65 % + 35 %).
const PLATEAU_HEIGHT: f32 = 46.0;

/// Maximum slope of river valley / canyon walls (≈ 61°; steeper is left to the 3D band).
const WALL_MAX: f32 = 1.8;

/// Open plains: rise of the calm surface from the coast (SEA + 3) to far inland.
const PLAIN_RISE: f32 = 3.5;
/// Soft summit ceiling: `h' = C + A·ln(1 + (h − C)/A)` above C (slope factor 1/(1 + (h − C)/A),
/// ≥ 0.2 for every reachable h: summits are compressed, never flattened into caps).
const CEIL_START: f32 = 430.0;
const CEIL_A: f32 = 45.0;
/// Rock-shelter notches: max horizontal depth (blocks) and reach factor of the 3D band.
const NOTCH_D: f32 = 6.0;
const NOTCH_REACH: f32 = 1.6;
/// Sea stacks: jittered grid on cliffed coasts.
const STACK_CELL: f64 = 64.0;

const VOLCANO_CELL: f64 = 3600.0;
const VOLCANO_P: f32 = 0.07;
const HOODOO_CELL: f64 = 14.0;
/// Fraction of hoodoo cells carrying a spire (art pass: 0.6 read as a rash of stripes).
const HOODOO_P: f32 = 0.26;
const DUNE_WAVELENGTH: f32 = 58.0;

impl TerrainShaper {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        let wa = hash_f01(s("terrain.wind")) as f64 * std::f64::consts::TAU;
        TerrainShaper {
            seed,
            offset: offset_spline(),
            amp: amp_spline(),
            wall: wall_spline(),
            ridged: DRidged::new(s("terrain.ridged"), 1500.0, 5),
            hills: DEroded::new(s("terrain.hills"), 900.0, 5, 0.5, 0.25),
            rough: DFbm::new(s("terrain.rough"), 40.0, 2, 2.0, 0.5),
            jag: DFbm::new(s("terrain.jag"), 800.0, 2, 2.0, 0.5),
            select: DFbm::new(s("terrain.select"), 1100.0, 2, 2.0, 0.5),
            n3: Fbm::new(s("terrain.n3"), 72.0, 3, 2.0, 0.5),
            dune_phase: DFbm::new(s("terrain.dune_phase"), 380.0, 2, 2.0, 0.5),
            dune_mod: DFbm::new(s("terrain.dune_mod"), 150.0, 2, 2.0, 0.5),
            wind: (wa.cos(), wa.sin()),
            mesa: DFbm::new(s("terrain.mesa"), 520.0, 4, 2.0, 0.5),
            character: DFbm::new(s("terrain.character"), 8000.0, 2, 2.0, 0.5),
            plateau: DFbm::new(s("terrain.plateau"), 2600.0, 2, 2.0, 0.45),
            islands: DFbm::new(s("terrain.islands"), 300.0, 3, 2.0, 0.5),
            swamp_n: DFbm::new(s("terrain.swamp"), 36.0, 2, 2.0, 0.5),
            hoodoo: Worley2::new(s("terrain.hoodoo"), HOODOO_CELL, 0.75),
            volcano: Worley2::new(s("terrain.volcano"), VOLCANO_CELL, 0.7),
            snow: SnowLine::new(seed),
            swell: DFbm::new(s("terrain.swell"), 1300.0, 2, 2.0, 0.4),
            strata: Strata::new(seed),
            notch_gate: FastSimplex::new(s("terrain.notch")),
            stacks: Worley2::new(s("terrain.stacks"), STACK_CELL, 0.8),
            clim: ClimateSampler::new(seed),
            pw_a: FastSimplex::new(s("terrain.patch_warp_a")),
            pw_b: FastSimplex::new(s("terrain.patch_warp_b")),
            relief: DFbm::new(s("terrain.relief"), 600.0, 4, 2.0, 1.0),
            step_n: FastSimplex::new(s("terrain.step")),
        }
    }

    // ---------------------------------------------------------------- landform patches

    /// Warped patch-lattice coordinates q(p) = p + 85·n(p/360) + 20·n(p/120) and the warp
    /// Jacobian [∂qx/∂x, ∂qx/∂z, ∂qz/∂x, ∂qz/∂z] (|∇ warp| < 1: no fold-over).
    #[inline]
    fn patch_warp(&self, x: f64, z: f64, fp: f64) -> (f64, f64, [f32; 4]) {
        const F1: f64 = 1.0 / 360.0;
        const A1: f32 = 85.0;
        const F2: f64 = 1.0 / 120.0;
        const A2: f32 = 20.0;
        let (a, ax, az) = self.pw_a.n2d(x * F1, z * F1);
        let (b, bx, bz) = self.pw_b.n2d(x * F1 + 31.7, z * F1 - 17.3);
        let k1 = A1 * F1 as f32;
        let mut qx = x + (A1 * a) as f64;
        let mut qz = z + (A1 * b) as f64;
        let mut j = [1.0 + k1 * ax, k1 * az, k1 * bx, 1.0 + k1 * bz];
        let fade = if fp > 1.0 { saturate(((120.0 / fp - 2.0) * 0.5) as f32) } else { 1.0 };
        if fade > 0.0 {
            let (c, cx, cz) = self.pw_a.n2d(x * F2 - 7.1, z * F2 + 3.3);
            let (d, dx, dz) = self.pw_b.n2d(x * F2 + 11.9, z * F2 + 5.4);
            let k2 = A2 * F2 as f32 * fade;
            qx += (A2 * fade * c) as f64;
            qz += (A2 * fade * d) as f64;
            j[0] += k2 * cx;
            j[1] += k2 * cz;
            j[2] += k2 * dx;
            j[3] += k2 * dz;
        }
        (qx, qz, j)
    }

    /// Hash and (warped-space) point of patch cell (ix, iz).
    #[inline]
    fn patch_point(&self, ix: i32, iz: i32) -> (u64, f64, f64) {
        let id = hash2(self.seed ^ 0x9A7C_4E11, ix, iz);
        let jx = 0.25 + 0.5 * hash_f01(mix64(id ^ 0x11)) as f64;
        let jz = 0.25 + 0.5 * hash_f01(mix64(id ^ 0x22)) as f64;
        (id, (ix as f64 + jx) * PATCH_CELL, (iz as f64 + jz) * PATCH_CELL)
    }

    /// Decision of patch cell (ix, iz): climate at the patch point (jittered by ±0.15 in T and
    /// ±0.2 in H → contrasty mosaics along climate transitions) → lowland biome
    /// ([`super::biome::lowland_biome`]) → landform style (hashed among the biome's styles) →
    /// parameters (style table × per-patch random scale).
    pub fn patch_decide(&self, ix: i32, iz: i32) -> PatchCell {
        let (id, px, pz) = self.patch_point(ix, iz);
        let r = |k: u64| hash_f01(mix64(id ^ k));
        let cd = self.clim.sample_d_direct(px, pz, 64.0);
        let t = cd.t.v + (r(1) - 0.5) * 0.3;
        let hu = cd.h.v + (r(2) - 0.5) * 0.4;
        let arid = aridity(t, hu);
        let chi = self.character.sample(px, pz, 64.0);
        let plain = if cd.c.v > -0.12 { self.plain_weight(&cd, chi).v } else { 0.0 };
        let (e, c) = (cd.e.v, cd.c.v);
        let swamp = (1.0 - smoothstep(0.2, 0.55, cd.orogeny.v))
            * smoothstep(0.30, 0.48, hu)
            * smoothstep(-0.20, 0.02, t)
            * smoothstep(-0.10, 0.30, e)
            * smoothstep(0.40, 0.22, c);
        // mesa country: coherent groups of patches (λ 2.7 km selector), always inside deserts
        let mesa = self.select.value(px * 0.4 + 3100.0, pz * 0.4 - 1700.0, 25.6) > -0.05 && e < 0.55;
        let biome = lowland_biome(t, hu, arid, plain, swamp > 0.5, mesa, r(3));
        let hoodoo = self.select.value(px + 3100.0, pz - 1700.0, 64.0) > 0.3;
        let style = pick_style(biome, plain > 0.5, r(4), hoodoo);
        let (mut p, mut off) = style.params();
        p[P_AMP] *= 0.65 + 0.7 * r(5);
        p[P_STEP] *= 0.8 + 0.45 * r(6);
        off *= 0.5 + r(7);
        PatchCell { biome, style, p, off }
    }

    fn patch_cell(&self, ix: i32, iz: i32) -> PatchCell {
        let slot = (((ix as u32).wrapping_mul(0x9E37_79B1) ^ (iz as u32).wrapping_mul(0x85EB_CA77)) >> 23) as usize % PATCH_CACHE_SIZE;
        let hit = PATCH_CACHE.with(|c| {
            let e = c.borrow()[slot];
            (e.valid && e.seed == self.seed && e.ix == ix && e.iz == iz).then_some(e.cell)
        });
        if let Some(c) = hit {
            return c;
        }
        let cell = self.patch_decide(ix, iz);
        PATCH_CACHE.with(|c| c.borrow_mut()[slot] = PatchEntry { seed: self.seed, ix, iz, valid: true, cell });
        cell
    }

    /// Blended patch parameters at (x, z): Voronoi distances `d_k` on the warped lattice, weights
    /// `w_k = 1 − smoothstep(0, W, d_k − d₁)` (C¹ across bisectors, compact: only cells within W
    /// of the nearest contribute), parameters `Σ w_k P_k / Σ w_k` with exact gradients. The level
    /// offset uses a sharper blend (W 28) → escarpments between patches at different levels.
    pub fn patch_mix(&self, x: f64, z: f64, fp: f64) -> PatchMix {
        let (qx, qz, j) = self.patch_warp(x, z, fp);
        let (ix, iz) = ((qx / PATCH_CELL).floor() as i32, (qz / PATCH_CELL).floor() as i32);
        let mut cand = [(0.0f32, 0.0f32, 0.0f32, 0i32, 0i32); 9];
        let mut best = 0usize;
        let mut k = 0;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (_, px, pz) = self.patch_point(ix + dx, iz + dz);
                let (ddx, ddz) = ((qx - px) as f32, (qz - pz) as f32);
                let d = (ddx * ddx + ddz * ddz).sqrt().max(1e-3);
                let (ux, uz) = (ddx / d, ddz / d);
                cand[k] = (d, ux * j[0] + uz * j[2], ux * j[1] + uz * j[3], ix + dx, iz + dz);
                if d < cand[best].0 {
                    best = k;
                }
                k += 1;
            }
        }
        let d1 = D2::new(cand[best].0, cand[best].1, cand[best].2);
        let ws_w = PATCH_SOFT.max(1.5 * fp as f32).min(95.0);
        let wh_w = PATCH_SHARP.max(1.5 * fp as f32).min(95.0);
        let mut num = [D2::ZERO; NP];
        let (mut sum, mut onum, mut osum) = (D2::ZERO, D2::ZERO, D2::ZERO);
        let mut edge = f32::MAX;
        for (i, c) in cand.iter().enumerate() {
            let delta = if i == best { D2::ZERO } else { D2::new(c.0, c.1, c.2) - d1 };
            if i != best {
                edge = edge.min(delta.v);
            }
            if delta.v >= ws_w {
                continue;
            }
            let cell = self.patch_cell(c.3, c.4);
            let ws = 1.0 - smoothstep_d(0.0, ws_w, delta);
            for (n, &pv) in num.iter_mut().zip(cell.p.iter()) {
                *n += ws * pv;
            }
            sum += ws;
            if delta.v < wh_w {
                let wh = 1.0 - smoothstep_d(0.0, wh_w, delta);
                onum += wh * cell.off;
                osum += wh;
            }
        }
        let p = num.map(|n| n / sum);
        let off = onum / osum;
        let bc = self.patch_cell(cand[best].3, cand[best].4);
        PatchMix { p, off, biome: bc.biome, style: bc.style, edge }
    }

    /// Rock landforms of a jittered point grid (cell `cell`, ≤ 1 feature per cell): each kind
    /// `(K, prob)` in priority order claims the cell when its hash `u < prob`; the feature is a
    /// superellipse dome `Ht·(1 − (r/R')^k)^e` with an irregular radius R' = R·m(θ),
    /// m = 1 + 0.2 cos(2θ+φ₁) + 0.1 cos(3θ+φ₂), shrunk by `smoothstep(0, 0.06, prob − u)` where the
    /// blended density changes (patch borders) and faded out once the footprint exceeds ≈ R/2.
    /// Returns the tallest feature at (x, z) (height above the local ground, with gradient).
    fn point_rocks(&self, x: f64, z: f64, fp: f64, cell: f64, salt: u64, kinds: &[(RockKind, f32)]) -> D2 {
        let cx = (x / cell).floor() as i32;
        let cz = (z / cell).floor() as i32;
        let mut best = D2::ZERO;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let id = hash2(self.seed ^ salt, cx + dx, cz + dz);
                for (i, &(kind, prob)) in kinds.iter().enumerate() {
                    let u = hash_f01(mix64(id ^ (0x51 + i as u64)));
                    if u >= prob {
                        continue;
                    }
                    let a = hash_f01(mix64(id ^ 0x61));
                    let b = hash_f01(mix64(id ^ 0x62));
                    let rr = kind.r0 + kind.r1 * a;
                    let fade = 1.0 - smoothstep(0.3 * rr, 0.9 * rr, fp as f32);
                    if fade <= 0.0 {
                        break;
                    }
                    let ht = (kind.h0 + kind.h1 * b * (0.4 + 0.6 * a)) * smoothstep(0.0, 0.06, prob - u) * fade;
                    let px = (cx + dx) as f64 * cell + cell * (0.2 + 0.6 * hash_f01(mix64(id ^ 0x71)) as f64);
                    let pz = (cz + dz) as f64 * cell + cell * (0.2 + 0.6 * hash_f01(mix64(id ^ 0x72)) as f64);
                    let (ddx, ddz) = ((x - px) as f32, (z - pz) as f32);
                    let d2 = ddx * ddx + ddz * ddz;
                    if d2 >= (1.3 * rr) * (1.3 * rr) {
                        break;
                    }
                    let dist = d2.sqrt().max(1e-3);
                    let (c1, s1) = (ddx / dist, ddz / dist);
                    let (p1s, p1c) = fast_sin_cos(6.283 * hash_f01(mix64(id ^ 0x81)));
                    let (p2s, p2c) = fast_sin_cos(6.283 * hash_f01(mix64(id ^ 0x82)));
                    let (c2, s2) = (2.0 * c1 * c1 - 1.0, 2.0 * s1 * c1);
                    let (c3, s3) = (4.0 * c1 * c1 * c1 - 3.0 * c1, 3.0 * s1 - 4.0 * s1 * s1 * s1);
                    let m = 1.0 + 0.2 * (c2 * p1c - s2 * p1s) + 0.1 * (c3 * p2c - s3 * p2s);
                    let r_eff = rr * m;
                    let uu = dist / r_eff;
                    if uu >= 1.0 {
                        break;
                    }
                    let uk = uu.powf(kind.k);
                    let core = (1.0 - uk).max(1e-4);
                    let v = ht * core.powf(kind.e);
                    if v > best.v {
                        let dv = (-ht * kind.e * core.powf(kind.e - 1.0) * kind.k * uk / uu.max(1e-3) / r_eff).max(-40.0);
                        best = D2::new(v, dv * c1, dv * s1);
                    }
                    break;
                }
            }
        }
        best
    }

    pub fn column(&self, climate: &ClimateSampler, x: f64, z: f64, footprint: f64) -> ColumnInfo {
        let cd = climate.sample_d(x, z, footprint);
        let sh = self.shape_full(climate, &cd, x, z, footprint);
        self.finish(&cd, &sh, x, z)
    }

    /// Assemble the `ColumnInfo` (surface temperature, biome) from a computed shape.
    pub fn finish(&self, cd: &ClimateD, sh: &Shape, x: f64, z: f64) -> ColumnInfo {
        let h = sh.height;
        let slope = h.grad_len();
        // Aspect: slopes facing north (−z, height rising toward +z) receive less sun → colder.
        let aspect = h.dz / (1.0 + slope * slope).sqrt();
        let st = cd.t.v - (h.v - SEA).max(0.0) * (1.0 / 250.0) - 0.12 * aspect;
        let climate = cd.to_climate();
        let biome = classify(&BiomeInput {
            climate,
            height: h.v,
            slope,
            surface_temperature: st,
            regions: sh.regions,
            river_dist: cd.river_dist,
            river_width: cd.river_width,
            mountain: sh.mountain,
            snow_line: self.snow.at(x, z, cd.t.v),
            water_level: sh.water_level,
        });
        ColumnInfo {
            x,
            z,
            climate,
            height: h.v,
            water_level: sh.water_level,
            biome,
            surface_temperature: st,
            slope,
            jaggedness: sh.jaggedness,
        }
    }

    /// Regional character selector χ (λ 8 km; debug / art-pass tools).
    pub fn character_at(&self, x: f64, z: f64) -> f32 {
        self.character.value(x, z, 0.0)
    }

    /// Names of the components returned by [`TerrainShaper::debug_parts`].
    pub fn debug_parts_names(&self) -> Vec<&'static str> {
        vec!["base", "ridge", "hills", "mountain", "final"]
    }

    /// Individual height components (scaled to blocks) for debug hillshades.
    pub fn debug_parts(&self, cd: &ClimateD, x: f64, z: f64) -> Vec<f32> {
        let inp = [cd.c, cd.e, cd.pv];
        let base = spline_d(&self.offset, &[cd.c, cd.e_raw, cd.pv]);
        let q = &cd.q;
        let ridge = q.chain(self.ridged.sample(q.x, q.z, 0.0));
        let hills = self.hills.sample(x, z, 0.0);
        let sh = self.shape(cd, x, z, 0.0);
        let amp = spline_d(&self.amp, &inp).maxf(0.0);
        let r2 = ((ridge - 0.12) * 1.25).clamp(0.0, 1.0);
        let hills01 = (hills * 1.3 + 0.45).clamp(0.0, 1.0);
        let s_mix = (smoothstep_d(-0.5, 0.6, cd.pv) * 0.5 + smoothstep_d(40.0, 200.0, amp) * 0.5).clamp(0.0, 1.0);
        let vfac = smoothstep_d(-0.95, -0.35, cd.pv) * 0.65 + 0.35;
        let m = amp * lerp_d(hills01, r2, s_mix) * vfac;
        vec![base.v, ridge.v * 300.0, hills.v * 300.0, sh.mountain, sh.height.v, m.v, m.dx + base.dx, m.dz + base.dz]
    }

    /// Region weights from the climate and the landform patches.
    pub fn regions(&self, cd: &ClimateD, x: f64, z: f64, fp: f64) -> Regions {
        let pm = self.patch_mix(x, z, fp);
        let chi = if cd.c.v > -0.45 { self.character.sample(x, z, fp) } else { D2::ZERO };
        self.regions_with(cd, &pm, chi)
    }

    /// Region weights: drylands, swamps and badlands are *patch styles* now (dune seas, mesa
    /// country, hoodoo fields, swamp flats — blended over the patch borders), gated to land and
    /// kept out of the orogeny-belt cores (low E* and the rain shadow make belts "hot-dry" by
    /// climate, which would terrace whole ranges and pile dunes on their flanks).
    fn regions_with(&self, cd: &ClimateD, pm: &PatchMix, chi: D2) -> Regions {
        let (t, hu, c) = (cd.t.v, cd.h.v, cd.c.v);
        let arid = aridity(t, hu);
        let land = smoothstep(-0.16, -0.08, c) * (1.0 - smoothstep(0.2, 0.55, cd.orogeny.v));
        let badlands = saturate(pm.p[P_MESA].v) * land;
        let swamp = saturate(pm.p[P_SWAMP].v) * (1.0 - smoothstep(0.2, 0.55, cd.orogeny.v)) * smoothstep(-0.14, -0.06, c);
        // sea cliffs follow the *raw* erosion (the coast-shaping input of the offset spline)
        let cliff = smoothstep(-0.25, -0.60, cd.e_raw.v) * smoothstep(-0.20, -0.15, c) * smoothstep(0.02, -0.06, c);
        let plain = if c > -0.12 { self.plain_weight(cd, chi).v } else { 0.0 };
        Regions {
            desert: saturate(pm.p[P_DUNE].v) * land,
            badlands,
            hoodoo: saturate(pm.p[P_HOODOO].v) * land,
            swamp,
            cliff,
            arid,
            plain,
            patch: pm.biome,
            style: pm.style,
            ..Default::default()
        }
    }

    /// Open-plains weight `P = w(2 − w)`, `w = s(χ; 0.08, −0.10) · (1 − s(M; 0.04, 0.25)) · s(C; −0.12, −0.02)`:
    /// the low ≈ 40 % of the regional character χ (λ 8 km) becomes big calm lowlands — the
    /// contrast to belts, rolling hills (0.08 < χ < 0.3) and plateau country
    /// (χ > 0.3). χ alone (not the erosion, whose zero-set valleys are only ≈ 100 m wide) keeps
    /// the plains' edges km-scale ramps.
    fn plain_weight(&self, cd: &ClimateD, chi: D2) -> D2 {
        let w = smoothstep_d(0.08, -0.10, chi) * (1.0 - smoothstep_d(0.04, 0.25, cd.orogeny)) * smoothstep_d(-0.12, -0.02, cd.c);
        // w(2 − w): the residual hill relief (1 − w)² vanishes quickly inside the plains
        w * (2.0 - w)
    }

    /// The height construction (see module docs) *without* inland lakes (lakes need the climate
    /// sampler to probe their rim; see [`TerrainShaper::shape_full`]).
    pub fn shape(&self, cd: &ClimateD, x: f64, z: f64, fp: f64) -> Shape {
        self.shape_impl(cd, x, z, fp, None)
    }

    /// The complete height construction including inland lakes (what `column` uses).
    pub fn shape_full(&self, climate: &ClimateSampler, cd: &ClimateD, x: f64, z: f64, fp: f64) -> Shape {
        self.shape_impl(cd, x, z, fp, Some(climate))
    }

    fn shape_impl(&self, cd: &ClimateD, x: f64, z: f64, fp: f64, lakes: Option<&ClimateSampler>) -> Shape {
        let chi = if cd.c.v > -0.45 { self.character.sample(x, z, fp) } else { D2::ZERO };
        let pm = self.patch_mix(x, z, fp);
        let mut reg = self.regions_with(cd, &pm, chi);
        // Offset (hypsometry + coast shape) from the raw erosion; uplift, valley walls etc. from
        // the effective, orogeny-gated erosion.
        let inp = [cd.c, cd.e, cd.pv];
        let base = spline_d(&self.offset, &[cd.c, cd.e_raw, cd.pv]);
        // Inside an orogeny belt the ridged relief is guaranteed: A ≥ 250·s(M; 0.15, 0.85) (smooth
        // max). Without it the massif uplift alone lifted moderate-erosion parts of a belt into
        // broad domes at the snow line — the flat snow caps of the old renders.
        let belt = smoothstep_d(0.15, 0.85, cd.orogeny) * smoothstep_d(-0.12, 0.05, cd.c);
        let amp = spline_d(&self.amp, &inp).maxf(0.0).smax(belt * BELT_AMP, 30.0).maxf(0.0);

        // --- mountains / hills
        let q = &cd.q;
        // (the ridged field only matters where there is uplift: skip it on the open sea floor)
        // (the ridged field only matters where there is real uplift: below ≈ 6 blocks of
        // amplitude the rounded hills take over smoothly and the 5 ridged octaves are skipped)
        let ridge_w = smoothstep_d(5.0, 14.0, amp);
        let (ridge, steer) = if ridge_w.v > 0.0 {
            let (r, st) = self.ridged.sample_with_steer(q.x, q.z, fp);
            (q.chain(r), q.chain(st))
        } else {
            (D2::ZERO, D2::ZERO)
        };
        let ridge = ((ridge - 0.12) * 1.25).clamp(0.0, 1.0);
        // Gameplay pass: sharper summits inside the belts (r^1.6: narrow peaks and arêtes, broad
        // walkable valley floors) instead of uniform domes.
        let ridge = lerp_d(ridge, ridge.powf(1.6), belt);
        let hills = self.hills.sample(x, z, fp);
        let hills01 = (hills * 1.3 + 0.45).clamp(0.0, 1.0);
        // Ridges dominate in high uplift and on pv-peaks; rounded eroded hills elsewhere.
        let s_mix = ((smoothstep_d(-0.5, 0.6, cd.pv) * 0.5 + smoothstep_d(40.0, 200.0, amp) * 0.5).max(belt) * ridge_w).clamp(0.0, 1.0);
        let shape = lerp_d(hills01, ridge, s_mix);
        let vfac = smoothstep_d(-0.95, -0.35, cd.pv) * 0.65 + 0.35;
        // Orogenic massif: broad uplift under the belt core (land only) — raises the valley floors
        // of big ranges; the summits come from the ridged relief above.
        let massif = smoothstep_d(0.05, 0.9, cd.orogeny) * smoothstep_d(-0.12, 0.05, cd.c) * OROGENIC_UPLIFT;
        let mountain = amp * shape * vfac + massif;
        // Smooth steering proxy for the gully filter (see `DRidged::sample_with_steer`): regional
        // slope + rounded main crests. Sharp crests must not steer the gully phase.
        let ks = amp.v * vfac.v * s_mix.v * 1.25;
        let (mut sgx, mut sgz) = (base.dx + ks * steer.dx, base.dz + ks * steer.dz);
        let mut h = base + mountain;

        // --- open plains (DESIGN §2a anti-monotony): blend toward a calm, low surface (no
        // ridge/valley relief, ±1.5-block swells) — lowland basins where rivers meander across
        // genuinely flat floodplains, steppe and meadows.
        let plain = if cd.c.v > -0.12 { self.plain_weight(cd, chi) } else { D2::ZERO };
        if plain.v > 0.0 {
            // low plains a few blocks above the (sea-level) rivers: SEA + 3 near the coast …
            // + 6.5 far inland, so rivers wind through them in broad, barely incised floodplains
            // (2 % banks, see the river stage) — relief < ≈ 4 blocks per 200 m
            let flat = smoothstep_d(-0.05, 0.6, cd.c) * PLAIN_RISE + (SEA + 3.0) + self.swell.sample(x, z, fp) * 3.0;
            h = lerp_d(h, flat, plain);
            sgx = lerp(sgx, flat.dx, plain.v);
            sgz = lerp(sgz, flat.dz, plain.v);
        }

        // --- seafloor relief
        let sea_w = 1.0 - smoothstep(-0.26, -0.16, cd.c.v);
        if sea_w > 0.0 {
            h = h + hills * (18.0 * sea_w);
        }

        // --- regional character (DESIGN §2a anti-monotony): one λ 8 km selector χ
        //   χ > 0.3 → plateau country: H += P·smoothstep(e, e + 0.035, n_p) — mesas/tablelands
        //     several km across bounded by escarpments ≈ 50 blocks wide (slope ≈ 1–1.7); rivers
        //     later cut canyons through them (lower envelope);
        //   χ < −0.1 → archipelago coasts: on the shelf the sea floor is pulled toward a skerry
        //     field `SEA + 2 + 16(n_i − 0.12)` wherever n_i > 0.12 (λ 300) → rocky islets;
        //   low χ also widens the open plains (see `plain_weight`).
        let belt_free = 1.0 - smoothstep(0.1, 0.4, cd.orogeny.v);
        if belt_free > 0.0 && cd.c.v > -0.45 {
            if chi.v > PLATEAU_CHI && cd.c.v > 0.0 {
                let wgt = smoothstep_d(PLATEAU_CHI, PLATEAU_CHI + 0.12, chi) * (belt_free * (1.0 - reg.swamp) * smoothstep(0.0, 0.12, cd.c.v));
                let np = self.plateau.sample(x, z, fp);
                let step = smoothstep_d(-0.02, 0.015, np) * 0.65 + smoothstep_d(0.16, 0.195, np) * 0.35;
                h = h + step * wgt * PLATEAU_HEIGHT;
            } else if chi.v < -0.1 && cd.c.v < -0.08 {
                let shelf = smoothstep(-0.42, -0.30, cd.c.v) * smoothstep(-0.08, -0.16, cd.c.v);
                let wgt = smoothstep_d(-0.1, -0.22, chi) * (belt_free * shelf);
                if wgt.v > 0.0 {
                    let ni = self.islands.sample(x, z, fp);
                    let isl = smoothstep_d(0.12, 0.30, ni);
                    let top = ni * 16.0 + (SEA + 2.0 - 16.0 * 0.12);
                    h = lerp_d(h, top.max(h), isl * wgt);
                }
            }
        }

        // --- landform patches (gameplay pass): mid-frequency relief with a per-patch spectrum and
        // profile, plus the patch level offset (sharp blend → escarpments). Faded toward the
        // coast and inside the orogeny belts (where the mountains own the relief).
        let g_land = smoothstep(-0.10, 0.02, cd.c.v) * (1.0 - smoothstep(0.25, 0.75, cd.orogeny.v));
        if g_land > 0.0 {
            h = h + self.patch_relief(&pm, x, z, fp) * g_land;
        }

        // --- fine roughness ±2–4 blocks (less on plains/dunes/swamps/beaches)
        let rug = smoothstep(10.0, 120.0, amp.v);
        // flat beaches / shallows: no speckle islands at the waterline
        let shore = 0.25 + 0.75 * smoothstep(1.5, 9.0, (h.v - SEA).abs());
        let rough_amp = (1.0 + 1.4 * rug) * (1.0 - 0.85 * reg.desert) * (1.0 - 0.7 * reg.swamp) * (1.0 - 0.75 * plain.v) * shore;
        h = h + self.rough.sample(x, z, fp) * rough_amp;

        // --- badlands: plateau mesas, then smooth terraces
        if reg.badlands > 0.0 {
            let m = self.mesa.sample(x, z, fp);
            let plateau = smoothstep_d(-0.02, 0.10, m) * 0.6 + smoothstep_d(0.18, 0.26, m) * 0.4;
            let mh = 34.0 + 22.0 * smoothstep(-0.2, 0.3, cd.w.v.abs());
            h = h + plateau * (mh * reg.badlands);
            let step = 9.0 + 4.0 * saturate(cd.e.v + 0.5);
            let delta = h.grad_len() * fp.max(0.0) as f32;
            let ht = terrace_d(h, step, delta);
            h = lerp_d(h, ht, D2::c(smoothstep(0.25, 0.6, reg.badlands)));
        }

        // --- desert dunes
        if reg.desert > 0.0 {
            let fade = if fp > 1.0 { saturate(((DUNE_WAVELENGTH as f64 / fp - 2.0) * 0.5) as f32) } else { 1.0 };
            let ampn = self.dune_mod.sample(x * 0.13, z * 0.13, fp * 0.13);
            let a = (6.0 + 9.0 * smoothstep(-0.3, 0.35, ampn.v)) * reg.desert;
            let mut dune = D2::c(DUNE_MEAN * 0.6);
            if fade > 0.0 {
                let (wx, wz) = self.wind;
                let along = x * wx + z * wz;
                let phi = self.dune_phase.sample(x, z, fp) * 7.0;
                let inv = 1.0 / DUNE_WAVELENGTH;
                let ph = D2::new((along as f32 * inv).rem_euclid(1.0), wx as f32 * inv, wz as f32 * inv)
                    + phi * (1.0 / std::f32::consts::PI);
                let u = ph.v.rem_euclid(1.0);
                // asymmetric reparametrisation: slip face [0, a), windward [a, 1)
                const LEE: f32 = 0.28;
                let (up, dup) = if u < LEE { (0.5 * u / LEE, 0.5 / LEE) } else { (0.5 + 0.5 * (u - LEE) / (1.0 - LEE), 0.5 / (1.0 - LEE)) };
                // f(u') = (1 − sin πu')^1.5 on u' ∈ [0,1) (sin ≥ 0 there); crest (cusp) at u' = 0.
                let (s, c) = (std::f32::consts::PI * up).sin_cos();
                let base1 = (1.0 - s).max(0.0);
                let prof = base1 * base1.sqrt();
                let dprof = -1.5 * base1.sqrt() * std::f32::consts::PI * c;
                let mut prof_d = ph.map(prof, dprof * dup);
                // barchan modulation: crests break into crescents
                let bm = self.dune_mod.sample(x, z, fp);
                let barchan = smoothstep_d(-0.30, 0.35, bm) * 0.75 + 0.25;
                prof_d = prof_d * barchan;
                dune = lerp_d(dune, prof_d, D2::c(fade));
            }
            h = h + dune * a;
        }

        // --- swamps: flatten low ground toward just above sea level, with puddles
        if reg.swamp > 0.0 {
            let low = smoothstep_d(SEA + 18.0 + 30.0 * plain.v, SEA + 5.0, h);
            let target = self.swamp_n.sample(x, z, fp) * 2.6 + (SEA + 0.35);
            h = lerp_d(h, target, low * (0.9 * reg.swamp));
        }

        // --- rivers: lower envelope with a U-channel + valley walls
        let gate = cd.river_gate;
        // (gradient of the valley wall, weight with which it defines the surface)
        let mut wall = (0.0f32, 0.0f32, 0.0f32);
        if gate > 0.0 && cd.c.v > -0.35 {
            let w = (cd.river_width * cd.river_width + 0.36 * (fp * fp) as f32).sqrt();
            // Distance coordinate across the valley: d = min(|n|/s, |n|/G), s = √(|∇n|² + f²).
            // The first-order estimate |n|/s alone is not Lipschitz where |∇n| is small (its
            // dropped Hessian term ≈ d·|∇²n|/|∇n| reached 5 blocks per block → 12-block steps on
            // canyon walls while the dual-number slope said ≈ 2); |n|/G alone overstates the
            // distance where |∇n| ≫ G. The min picks |n|/s exactly where |∇n| ≥ G (Hessian term
            // small) and |n|/G where |∇n| < G (exact gradient, magnitude |∇n|/G < 1).
            let d = river_distance(cd.river_n).min(cd.river_n.abs() * (1.0 / RIVER_WALL_G));
            let depth = 2.5 + 0.25 * w;
            let above = (h.v - SEA).max(0.0);
            let bank = SEA - 0.6 + (1.0 - gate) * above * 0.95;
            // plains: almost flat banks (σ → 0.015, no convexity) — broad floodplains
            let sigma = lerp_d(spline_d(&self.wall, &inp), D2::c(0.015), plain);
            // convexity κ: U-shaped troughs in the belts (gentle floor, walls steepening upward)
            let np = 1.0 - plain.v;
            let kappa = (0.011 * (1.0 - saturate((cd.e.v + 1.0) * 0.5)) + 0.0015) * np * np * np;
            let v = if d.v < w {
                let r = d * (1.0 / w);
                (1.0 - r.sq()) * (-depth) + bank
            } else {
                // Wall V(u) = σu + κu² until its slope σ + 2κu reaches WALL_MAX at
                // u* = (WALL_MAX − σ)/2κ, linear beyond (C¹). Gradient: WALL_MAX·∇u + u*·∇σ.
                // (The straight upper wall gets spurs and couloirs from the gully filter below.)
                let u = d - w;
                let sig = sigma.minf(WALL_MAX);
                let ustar = (WALL_MAX - sig.v) / (2.0 * kappa);
                if u.v <= ustar {
                    u * sig + u.sq() * kappa + bank
                } else {
                    u * WALL_MAX + sig * ustar + (kappa * ustar * ustar - WALL_MAX * ustar + bank)
                }
            };
            let k = 6.0 + 10.0 * saturate((cd.e.v + 1.0) * 0.5);
            // gully steering follows the wall near the river: a weight that varies over ≈ 200
            // blocks of river distance (a switch on "wall below terrain" would rotate the wave
            // vectors within a few blocks → phase jumps of the gully waves)
            let ww = smoothstep(0.0, 0.3, gate) * (1.0 - smoothstep(40.0, 220.0, d.v - w));
            wall = (ww, v.dx, v.dz);
            if v.v < h.v + k {
                h = h.smin(v, k);
            }
            reg.channel = gate * (1.0 - smoothstep(w * 0.7, w * 1.15, d.v));
        }

        // --- dendritic gullies, couloirs and spurs on mountain flanks *and* valley walls (after
        // the river envelope, steered by the wall where it defines the surface — no planar slabs).
        // Their total relief Σaₖ ≈ 88·c·|g| is kept below 0.6× the height above the river level,
        // so they fade out toward the valley floor instead of digging pits below it.
        let mut steer_dbg = (0.0, 0.0);
        let gully_gate = smoothstep(12.0, 60.0, mountain.v) * (1.0 - plain.v);
        if gully_gate > 0.0 {
            let gx = lerp(sgx, wall.1, wall.0);
            let gz = lerp(sgz, wall.2, wall.0);
            steer_dbg = (gx, gz);
            let g0 = (gx * gx + gz * gz).sqrt().clamp(1e-3, 1.5);
            let c0 = 0.65 * gully_gate;
            let floor = saturate(0.6 * (h.v - SEA - 1.0) / (88.0 * g0 * c0));
            let ga = c0 * floor * floor * (3.0 - 2.0 * floor);
            if ga > 0.0 {
                h = h + self.gullies(x, z, (gx, gz), ga, fp);
            }
        }

        // --- stepped relief (gameplay pass): smooth terraces τ_s(ℓ) on the bedding coordinate
        // ℓ = h + D(x, z) (treads follow the gently dipping beds; risers are short, steep cliff
        // bands) → bimodal slopes: long walkable shelves separated by distinct cliff steps instead
        // of uniform ramps. Lowland patches blend two fixed steps (5 and 8 blocks, by the patch
        // step parameter) with their terrace strength and riser sharpness; mountain flanks get an
        // 11-block ledge system. A λ 330 mask leaves some slopes smooth; nothing at the waterline.
        // (Fixed step sizes: a spatially varying step would make the levels k·s(x) drift through
        // the terrain.)
        let mtn_w = smoothstep(35.0, 120.0, mountain.v);
        let (mn, _, _) = self.step_n.n2d(x * (1.0 / 330.0) + 41.3, z * (1.0 / 330.0) - 12.9);
        let mask = smoothstep(-0.35, 0.15, mn);
        let water_fade = smoothstep(SEA + 1.5, SEA + 5.0, h.v) * (1.0 - reg.channel) * (1.0 - reg.badlands) * (1.0 - reg.desert);
        let tk_land = pm.p[P_TERR] * (g_land * (1.0 - mtn_w) * (0.45 + 0.55 * mask) * water_fade);
        let tk_mtn = 0.6 * mtn_w * mask * water_fade;
        if tk_land.v > 0.01 || tk_mtn > 0.01 {
            let dd = self.strata.offset_d(x, z);
            let ell = h + dd;
            let delta = ell.grad_len() * fp.max(0.0) as f32;
            let t0 = 0.55 + 0.35 * lerp(saturate(pm.p[P_SHARP].v), 0.6, mtn_w);
            let mut dh = D2::ZERO;
            if tk_land.v > 0.01 {
                let ws = ((pm.p[P_STEP] - 5.0) * (1.0 / 3.0)).clamp(0.0, 1.0);
                let t5 = terrace_gen(ell, 5.0, delta, t0);
                let t8 = if ws.v > 0.0 { terrace_gen(ell, 8.0, delta, t0) } else { t5 };
                dh += (lerp_d(t5, t8, ws) - ell) * tk_land;
            }
            if tk_mtn > 0.01 {
                dh += (terrace_gen(ell, 11.0, delta, t0) - ell) * tk_mtn;
            }
            h = h + dh;
            reg.terrace = tk_land.v + tk_mtn;
        }

        // --- rock landforms: tors, karst towers and buttes (patch densities, 64-block grid) and
        // needle spires on high ridges (90-block grid); never in water or river channels.
        if h.v > SEA + 1.0 && reg.channel < 0.5 {
            let gate = smoothstep(SEA + 1.0, SEA + 4.0, h.v) * (1.0 - reg.channel) * g_land;
            let pt = pm.p[P_TOR].v * 0.45 * gate;
            let pk = pm.p[P_KARST].v * 0.7 * gate;
            let pb = pm.p[P_BUTTE].v * 0.32 * gate;
            if pt + pk + pb > 0.0 {
                let r = self.point_rocks(x, z, fp, ROCK_CELL, 0x70C5, &[(KARST, pk), (BUTTE, pb), (TOR, pt)]);
                if r.v > 0.0 {
                    h = h + r;
                    reg.rock = smoothstep(0.5, 2.0, r.v);
                }
            }
            let ps = 0.32 * smoothstep(110.0, 230.0, mountain.v) * smoothstep(0.0, 0.5, cd.pv.v);
            if ps > 0.0 {
                let r = self.point_rocks(x, z, fp, SPIRE_CELL, 0x5917E, &[(SPIRE, ps)]);
                if r.v > 0.0 {
                    h = h + r;
                    reg.rock = reg.rock.max(smoothstep(0.5, 2.0, r.v));
                }
            }
        }

        // --- inland lakes: flat water level, basin + containment sill (see `apply_lake`)
        let mut water_level = i32::MIN;
        if let Some(cs) = lakes {
            if cd.c.v > -0.12 {
                if let Some(level) = self.apply_lake(cs, &mut h, &mut reg, x, z) {
                    water_level = level;
                }
            }
        }

        // --- volcanoes (smooth max with a concave cone + caldera)
        if let Some((cone, lava)) = self.volcano_cone(x, z) {
            let before = h;
            h = h.smax(cone, 28.0);
            // biome edge = where the cone emerges from the terrain, roughened by low-frequency
            // noise so the volcanic field is not a perfect disk
            let wob = 26.0 * self.jag.value(x * 1.7, z * 1.7, fp);
            reg.volcanic = smoothstep(-10.0, 4.0, cone.v - before.v + wob);
            if let Some(l) = lava {
                if h.v < l as f32 - 0.5 {
                    water_level = l;
                }
            }
            if water_level != i32::MIN && reg.lake > 0.0 && h.v >= reg.lake_level {
                // the cone rose above a lake level: dry there
                water_level = i32::MIN;
            }
        }

        // --- hoodoo spires
        if reg.hoodoo > 0.05 {
            let sp = self.hoodoo_spires(x, z, fp);
            if sp.v > 0.0 {
                // spires only inside the Hoodoos biome (weight > ½) and above the water
                let ground = (1.0 - reg.channel) * smoothstep(0.5, 0.7, reg.hoodoo) * smoothstep(SEA + 1.0, SEA + 6.0, h.v);
                let top = h + sp * ground;
                h = h.max(top);
            }
        }

        // --- sea stacks off cliffed coasts
        let stack_w = smoothstep(-0.25, -0.55, cd.e_raw.v) * smoothstep(-0.26, -0.21, cd.c.v) * smoothstep(-0.13, -0.17, cd.c.v);
        if stack_w > 0.05 && h.v < SEA + 4.0 && water_level <= SEA_LEVEL {
            if let Some(st) = self.sea_stack(x, z, stack_w, fp) {
                if st.v > h.v {
                    h = st;
                    reg.stack = 1.0;
                }
            }
        }

        // --- soft summit ceiling (logarithmic: compresses, never flattens; world height 512)
        if h.v > CEIL_START {
            let r = 1.0 + (h.v - CEIL_START) / CEIL_A;
            h = h.map(CEIL_START + CEIL_A * r.ln(), 1.0 / r);
        }
        if h.v > 500.0 {
            let t = ((h.v - 500.0) / 6.0).tanh();
            h = h.map(500.0 + 6.0 * t, 1.0 - t * t);
        }
        h = h.maxf(6.0);

        if water_level == i32::MIN && h.v < SEA {
            water_level = SEA_LEVEL;
        } else if water_level > SEA_LEVEL && reg.volcanic <= 0.5 && h.v >= water_level as f32 {
            // hoodoo spire / soft ceiling raised the column above its lake level
            water_level = i32::MIN;
        }

        // --- jaggedness (3D detail band) = crag noise + rock-shelter notches (see `density`)
        let slope = h.grad_len();
        let j_m = if mountain.v > 60.0 {
            let jg = smoothstep(-0.15, 0.30, self.jag.value(x, z, fp));
            11.0 * smoothstep(60.0, 230.0, mountain.v) * (0.3 + 0.7 * jg)
        } else {
            0.0
        };
        let j_c = 12.0 * reg.cliff * smoothstep(SEA + 3.0, SEA + 12.0, h.v);
        let j_s = 7.0 * smoothstep(1.2, 2.8, slope) * smoothstep(SEA + 2.0, SEA + 20.0, h.v);
        // no 3D detail on lake basins: an overhang hollow next to a water column would expose
        // a vertical water face
        let lake_free = reg.lake <= 0.0 && water_level <= SEA_LEVEL;
        let tame = (1.0 - reg.badlands) * (1.0 - reg.desert) * (1.0 - 0.8 * reg.volcanic) * (1.0 - reg.swamp) * if lake_free { 1.0 } else { 0.0 };
        let j_notch = if lake_free && water_level == i32::MIN && reg.hoodoo < 0.05 { self.notch_reach(x, z, slope) } else { 0.0 };
        let mut jag = j_m.max(j_c).max(j_s) * tame + j_notch;
        jag = jag.min(28.0).min((WORLD_HEIGHT as f32 - 3.0 - h.v).max(0.0));
        if h.v >= SEA {
            // A dry column's 3D band must stay above the sea: otherwise the overhang noise
            // hollows out air pockets below sea level next to river/sea water columns, which the
            // builder never floods (exposed vertical water faces in canyon floors).
            jag = jag.min((h.v - SEA - 1.0).max(0.0));
        }
        if jag < 0.5 {
            jag = 0.0;
        }
        Shape { height: h, mountain: mountain.v, regions: reg, jaggedness: jag, water_level, steer: steer_dbg }
    }

    /// Patch relief `R = ½A(s + 1) + off` (mostly positive: hills rise from the lowland floor):
    /// `n = 3.2·Σ g_k(f)·o_k` over 4 equal-amplitude octaves (λ 600, 300, 150, 75) whose weights
    /// slide from broad swells (1, .55, .3, .15) to hummocks (.35, .6, 1, .5) with the patch's
    /// spectrum parameter f; the profile `s = n + K⁺(ridged − n) + K⁻(billow − n)` with
    /// ridged = 1 − 2|1.2n|, billow = 2|1.2n| − 1 (smooth |·|).
    pub fn patch_relief(&self, pm: &PatchMix, x: f64, z: f64, fp: f64) -> D2 {
        const B: [f32; 4] = [1.0, 0.55, 0.3, 0.15];
        const F: [f32; 4] = [0.35, 0.6, 1.0, 0.5];
        let o = self.relief.sample_each::<4>(x, z, fp);
        let fine = pm.p[P_FINE];
        let mut n = D2::ZERO;
        for k in 0..4 {
            n += o[k] * (fine * (F[k] - B[k]) + B[k]);
        }
        let n = n * 3.2;
        let kk = pm.p[P_SHAPE];
        let a = sabs_d(n * 1.2, 0.08);
        let ridged = 1.0 - a * 2.0;
        let billow = a * 2.0 - 1.0;
        let s = n + (ridged - n) * kk.maxf(0.0) + (billow - n) * (-kk).maxf(0.0);
        pm.p[P_AMP] * (s + 1.0) * 0.5 + pm.off
    }

    /// Reach (blocks) of the rock-shelter notches of a column: `1.6·min(|∇H|, 4)·D(x, z)` with
    /// the notch depth `D = 6·gate`, `gate = s(n(λ 85); −0.1, 0.4)·s(|∇H|; 0.8, 1.4)` — only steep
    /// faces, in segments tens of blocks long. A pure function of (x, z, slope), recomputed
    /// identically by `density`.
    #[inline]
    fn notch_reach(&self, x: f64, z: f64, slope: f32) -> f32 {
        if slope <= 0.8 {
            return 0.0;
        }
        const F: f64 = 1.0 / 85.0;
        let g = smoothstep(-0.1, 0.4, self.notch_gate.n2d(x * F, z * F).0) * smoothstep(0.8, 1.4, slope);
        NOTCH_REACH * slope.min(4.0) * NOTCH_D * g
    }

    /// Sea stack of the jittered 64-block grid near (x, z), if any: a superellipse pillar
    /// `top·(1 − (r/R)⁴)^¼` standing on the shelf (vertical walls, rounded crown), R 4–9 blocks,
    /// crown SEA + 7…30. Density ∝ stack weight (cliffed coasts only).
    fn sea_stack(&self, x: f64, z: f64, w: f32, fp: f64) -> Option<D2> {
        if fp > 6.0 {
            return None;
        }
        let cx = (x / STACK_CELL).floor() as i32;
        let cz = (z / STACK_CELL).floor() as i32;
        let mut best: Option<D2> = None;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let id = self.stacks.cell_id(cx + dx, cz + dz);
                if hash_f01(id) >= 0.55 * w {
                    continue;
                }
                let (px, pz) = self.stacks.point_of(cx + dx, cz + dz);
                let r = 4.0 + 5.0 * hash_f01(mix64(id ^ 3));
                let (ddx, ddz) = ((x - px) as f32, (z - pz) as f32);
                let d2 = ddx * ddx + ddz * ddz;
                if d2 >= r * r {
                    continue;
                }
                let foot = SEA - 14.0;
                let top = SEA + 7.0 + 23.0 * hash_f01(mix64(id ^ 4)) * hash_f01(mix64(id ^ 5)).sqrt();
                let dist = d2.sqrt().max(1e-3);
                let u = dist / r;
                let core = (1.0 - u * u * u * u).max(1e-4);
                let v = foot + (top - foot) * core.powf(0.25);
                let dv = (-(top - foot) * u * u * u * core.powf(-0.75) / r).max(-40.0);
                if best.map_or(true, |b| v > b.v) {
                    best = Some(D2::new(v, dv * ddx / dist, dv * ddz / dist));
                }
            }
        }
        best
    }

    /// Slope-aligned gully noise ("erosion filter").
    ///
    /// For octave k (cell size Lₖ = 360·2⁻ᵏ, k = 0..3) every cell of a jittered lattice carries a plane wave
    /// `cos 2π((p − pⱼ)·t̂)/Lₖ` whose wave vector t = g⊥/√(|g|²+ε²) is *perpendicular* to the current
    /// terrain gradient g (regularised: ε = 0.35|g₀| keeps it from spinning where g → 0), so its crests run downhill (gullies and spurs). The waves are blended with the
    /// compact kernel `w = (1 − |p − pⱼ|²/(0.75Lₖ)²)³`, blended as Σwc/(Σw + w₀)
    /// (the background weight w₀ makes coverage gaps fade continuously). The amplitude is
    /// `aₖ = c·0.72ᵏ·|g₀|·Lₖ/2π`, i.e. octave k adds side slopes ≈ c·0.72ᵏ × the base slope (relief of a
    /// gully ∝ slope × width, as in nature; flat ground gets nothing). Each octave's derivative is
    /// added to g before the next octave → finer gullies bend into coarser ones (dendritic
    /// drainage). Exact derivative except for the dependence of t̂ on g (Hessian), see
    /// `slope_matches_fd`.
    pub fn gullies(&self, x: f64, z: f64, grad: (f32, f32), c: f32, fp: f64) -> D2 {
        const TAU: f32 = std::f32::consts::TAU;
        let (mut gx, mut gz) = grad;
        let g0 = (gx * gx + gz * gz).sqrt().min(1.5);
        if g0 < 1e-3 {
            return D2::ZERO;
        }
        let eps2 = (0.35 * g0) * (0.35 * g0);
        let mut out = D2::ZERO;
        let mut cell = 360.0f64;
        let mut taper = 1.0f32;
        for oct in 0..4u64 {
            let fade = if fp > 1.0 { saturate(((cell / fp - 2.0) * 0.5) as f32) } else { 1.0 };
            if fade <= 0.0 {
                break;
            }
            // Regularised wave vector t = g⊥/√(|g|² + ε²): where the steering gradient vanishes
            // (crests, saddles, gully floors) the waves flatten out instead of spinning around.
            let inv = 1.0 / (gx * gx + gz * gz + eps2).sqrt();
            let (tx, tz) = (gz * inv, -gx * inv);
            let px = x / cell;
            let pz = z / cell;
            let (ix, iz) = (px.floor(), pz.floor());
            let (fx, fz) = ((px - ix) as f32, (pz - iz) as f32);
            let (ix, iz) = (ix as i32, iz as i32);
            let seed = self.seed ^ (0x6011_1E5 + oct * 0x9E37);
            let (mut sw, mut swx, mut swz) = (0.0f32, 0.0f32, 0.0f32);
            let (mut sv, mut svx, mut svz) = (0.0f32, 0.0f32, 0.0f32);
            for j in -1..=1 {
                for i in -1..=1 {
                    let hh = hash2(seed, ix + i, iz + j);
                    // points jittered in [0.3, 0.7]² of their cell; kernel radius R = 0.75 cell
                    // (the 3×3 window contains every point within R; the moderate lever arm
                    // |p − pⱼ| ≤ R keeps the phase insensitive to slow steering rotations)
                    let jx = 0.3 + 0.4 * hash_f01(hh);
                    let jz = 0.3 + 0.4 * hash_f01(mix64(hh));
                    let ppx = fx - (i as f32 + jx);
                    let ppz = fz - (j as f32 + jz);
                    let d2 = (ppx * ppx + ppz * ppz) * (1.0 / (0.75 * 0.75));
                    if d2 >= 1.0 {
                        continue;
                    }
                    let q = 1.0 - d2;
                    let w = q * q * q;
                    let dw = -6.0 * q * q * (1.0 / (0.75 * 0.75)); // ∂w/∂pp = dw·pp
                    let (sn, cs) = fast_sin_cos(TAU * (ppx * tx + ppz * tz));
                    sw += w;
                    swx += dw * ppx;
                    swz += dw * ppz;
                    sv += w * cs;
                    svx += dw * ppx * cs - w * sn * TAU * tx;
                    svz += dw * ppz * cs - w * sn * TAU * tz;
                }
            }
            {
                // background weight w₀: where no point is within reach the octave fades to 0
                // continuously (a plain normalisation Σwc/Σw jumps when Σw → 0).
                let inv = 1.0 / (sw + 0.08);
                let v = sv * inv;
                let k = 1.0 / cell as f32; // ∂pp/∂x
                let vx = (svx - v * swx) * inv * k;
                let vz = (svz - v * swz) * inv * k;
                let a = c * taper * g0 * cell as f32 / TAU * fade;
                out += D2::new(v, vx, vz) * a;
                gx += vx * a;
                gz += vz * a;
            }
            cell *= 0.5;
            taper *= 0.72;
        }
        out
    }

    /// Lake candidate of lattice cell (ix, iz): centre, radius, bowl depth (no probing).
    #[inline]
    fn lake_candidate(&self, ix: i32, iz: i32) -> Option<(f64, f64, f32, f32)> {
        let id = hash2(self.seed ^ 0x1A4E_5EED, ix, iz);
        if hash_f01(id) >= LAKE_P {
            return None;
        }
        // centres in the middle third of the cell: ≥ 924 blocks apart > 2 × max reach (≈ 446)
        let jx = 0.333 + 0.334 * hash_f01(mix64(id ^ 1)) as f64;
        let jz = 0.333 + 0.334 * hash_f01(mix64(id ^ 2)) as f64;
        let r = 90.0 + (LAKE_R_MAX - 90.0) * hash_f01(mix64(id ^ 3)).powf(1.5);
        let depth = lake_depth(r);
        Some(((ix as f64 + jx) * LAKE_CELL, (iz as f64 + jz) * LAKE_CELL, r, depth))
    }

    /// Probe a candidate: the flat water level is set by the *pre-lake* terrain on the rim,
    /// `L = ⌊min_rim H⌋ − 1`, and the lake is rejected where it would not be a natural basin
    /// (sea or river nearby, steep slope across the rim, deep cut inside, desert, volcano).
    /// A pure function of (seed, cell) evaluated at footprint 0, so every LOD level agrees.
    fn probe_lake(&self, cs: &ClimateSampler, cx: f64, cz: f64, r: f32) -> Option<f32> {
        self.probe_lake_why(cs, cx, cz, r).ok()
    }

    /// [`probe_lake`] with the rejection reason (debug / tests).
    pub fn probe_lake_why(&self, cs: &ClimateSampler, cx: f64, cz: f64, r: f32) -> Result<f32, &'static str> {
        let sample = |x: f64, z: f64| -> Result<f32, &'static str> {
            let cd = cs.sample_d(x, z, 0.0);
            if cd.c.v < -0.04 {
                return Err("sea");
            }
            // a river *channel* on a probe (valley flanks are fine: the level test below
            // rejects basins whose rim is cut down to the sea-level river)
            if cd.river_gate > 0.2 && cd.river_dist < cd.river_width + 25.0 {
                return Err("river");
            }
            let sh = self.shape_impl(&cd, x, z, 0.0, None);
            if sh.regions.badlands > 0.2 || (sh.regions.desert > 0.3 && r > 140.0) {
                return Err("dry");
            }
            if sh.water_level != i32::MIN {
                return Err("wet");
            }
            Ok(sh.height.v)
        };
        let centre = sample(cx, cz)?;
        if self.volcano_cone(cx, cz).map_or(false, |(cone, _)| cone.v > centre - 60.0) {
            return Err("volcano");
        }
        let mut rim = [0.0f32; LAKE_RIM_SAMPLES];
        for (k, rh) in rim.iter_mut().enumerate() {
            let a = (k as f64 + 0.5) * std::f64::consts::TAU / LAKE_RIM_SAMPLES as f64;
            *rh = sample(cx + a.cos() * r as f64, cz + a.sin() * r as f64)?;
        }
        rim.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (rim_lo, rim_hi) = (rim[0], rim[LAKE_RIM_SAMPLES - 1]);
        // spill level: 3rd-lowest rim probe (two notches may be dammed by the sill)
        let level = (rim[2] - 1.0).floor();
        if level - rim_lo > 10.0 {
            return Err("notch");
        }
        if level < SEA + 2.0 || level > 440.0 {
            return Err("level");
        }
        if rim_hi - rim_lo > 0.35 * r + 12.0 {
            return Err("slope");
        }
        // a lake must actually hold water: the basin floor at the centre is below the level
        if centre - lake_depth(r) > level - 2.0 {
            return Err("dry basin");
        }
        let mut inner_lo = centre;
        for k in 0..6 {
            let a = k as f64 * std::f64::consts::TAU / 6.0;
            inner_lo = inner_lo.min(sample(cx + a.cos() * 0.5 * r as f64, cz + a.sin() * 0.5 * r as f64)?);
        }
        if inner_lo < level - 12.0 {
            return Err("cut");
        }
        Ok(level)
    }

    /// Debug: (centre x, z, radius, probe result) of lake cell (ix, iz), if it has a candidate.
    pub fn lake_debug(&self, cs: &ClimateSampler, ix: i32, iz: i32) -> Option<(f64, f64, f32, Result<f32, &'static str>)> {
        let (cx, cz, r, _) = self.lake_candidate(ix, iz)?;
        Some((cx, cz, r, self.probe_lake_why(cs, cx, cz, r)))
    }

    fn lake_level_cached(&self, cs: &ClimateSampler, ix: i32, iz: i32, cx: f64, cz: f64, r: f32) -> Option<f32> {
        let slot = (((ix as u32).wrapping_mul(0x9E37_79B1) ^ (iz as u32).wrapping_mul(0x85EB_CA77)) >> 22) as usize % LAKE_CACHE_SIZE;
        let hit = LAKE_CACHE.with(|c| {
            let e = c.borrow()[slot];
            (e.valid && e.seed == self.seed && e.ix == ix && e.iz == iz).then_some(e.level)
        });
        if let Some(l) = hit {
            return if l.is_nan() { None } else { Some(l) };
        }
        let l = self.probe_lake(cs, cx, cz, r);
        LAKE_CACHE.with(|c| {
            c.borrow_mut()[slot] = LakeEntry { seed: self.seed, ix, iz, valid: true, level: l.unwrap_or(f32::NAN) }
        });
        l
    }

    /// Inland lake basins (Worley-type cell process, ≤ 1 lake per 1.7 km cell).
    ///
    /// With `ρ = |p − c|/R + 0.12·n(p)` (noise-perturbed normalised radius → irregular shores):
    /// * basin: `H₁ = H − D·smoothstep(1, 0.45, ρ)` (flat floor; islands survive where the
    ///   terrain is high),
    /// * sill: `H₂ = smax(H₁, L + 1.2 − 0.35u − 0.02u², 2)`, `u = R·|ρ − 1|` — a tent peaking
    ///   1.2 blocks above the level exactly on the rim ρ = 1 and below any terrain for u > 100
    ///   (so the influence radius 1.12R + 110 cuts nothing off),
    /// * water where `ρ < 1 ∧ H₂ < L`.
    ///
    /// Containment theorem: on ρ = 1 we have `H₂ ≥ L + 1.2`, so the water set `{ρ < 1, H₂ < L}`
    /// is enclosed by dry ground at or above its level — no vertical water faces, whatever the
    /// terrain does between the rim probes (where it dips, the sill becomes a small natural dam).
    fn apply_lake(&self, cs: &ClimateSampler, h: &mut D2, reg: &mut Regions, x: f64, z: f64) -> Option<i32> {
        let ix = (x / LAKE_CELL).floor() as i32;
        let iz = (z / LAKE_CELL).floor() as i32;
        // candidate centres lie in the middle third of their cell and reach ≤ 1.12·R_MAX + 110
        // blocks: cull neighbour cells by box distance before hashing
        const REACH_MAX: f64 = LAKE_R_MAX as f64 * 1.12 + 110.0;
        let (fx, fz) = (x / LAKE_CELL - ix as f64, z / LAKE_CELL - iz as f64);
        let box_d = |f: f64, d: i32| -> f64 {
            let (lo, hi) = (d as f64 + 0.333, d as f64 + 0.667);
            (lo - f).max(f - hi).max(0.0) * LAKE_CELL
        };
        for dz in -1..=1 {
            let bz = box_d(fz, dz);
            if bz >= REACH_MAX {
                continue;
            }
            for dx in -1..=1 {
                let bx = box_d(fx, dx);
                if bx * bx + bz * bz >= REACH_MAX * REACH_MAX {
                    continue;
                }
                let Some((cx, cz, r, depth)) = self.lake_candidate(ix + dx, iz + dz) else { continue };
                let (ddx, ddz) = (x - cx, z - cz);
                let d2 = ddx * ddx + ddz * ddz;
                // beyond u = 100 blocks outside the rim the sill is < L − 230 < any terrain
                let reach = r as f64 * 1.12 + 110.0;
                if d2 >= reach * reach {
                    continue;
                }
                let Some(level) = self.lake_level_cached(cs, ix + dx, iz + dz, cx, cz, r) else { continue };
                let dist = d2.sqrt().max(1e-3);
                let n = self.dune_phase.sample(x * 1.7 + 911.0, z * 1.7 - 377.0, 0.0);
                let rho = D2::new((dist / r as f64) as f32, (ddx / dist) as f32 / r, (ddz / dist) as f32 / r) + n * 0.12;
                if rho.v < 1.0 {
                    // flat-bottomed basin: full depth for ρ < 0.45, C¹ rise to the rim
                    *h = *h - smoothstep_d(1.0, 0.45, rho) * depth;
                }
                let u = sabs_tent(rho - 1.0) * r; // blocks from the rim
                let tent = (1.0 - u * 0.35 - u.sq() * 0.02) + level + 0.2;
                if tent.v > h.v - 2.0 {
                    *h = h.smax(tent, 2.0);
                }
                reg.lake = 1.0 - smoothstep(0.92, 1.0, rho.v);
                reg.lake_level = level;
                if rho.v < 1.0 && h.v < level {
                    return Some(level as i32);
                }
                return None;
            }
        }
        None
    }

    /// Volcano cone (dual) + optional lava lake level, if a volcano cell is near.
    fn volcano_cone(&self, x: f64, z: f64) -> Option<(D2, Option<i32>)> {
        let cx = (x / VOLCANO_CELL).floor() as i32;
        let cz = (z / VOLCANO_CELL).floor() as i32;
        let mut best: Option<(D2, Option<i32>)> = None;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let id = self.volcano.cell_id(cx + dx, cz + dz);
                if hash_f01(id) >= VOLCANO_P {
                    continue;
                }
                let (px, pz) = self.volcano.point_of(cx + dx, cz + dz);
                let (ddx, ddz) = (x - px, z - pz);
                let r_cone = 620.0 + 380.0 * hash_f01(mix64(id ^ 1)) as f64;
                let d2 = ddx * ddx + ddz * ddz;
                if d2 > (2.0 * r_cone) * (2.0 * r_cone) {
                    continue;
                }
                let dist = d2.sqrt().max(1e-3);
                let d = D2::new(dist as f32, (ddx / dist) as f32, (ddz / dist) as f32);
                let r = r_cone as f32;
                let hv = 160.0 + 150.0 * hash_f01(mix64(id ^ 2));
                let apex = SEA + hv;
                let kk = (hv + 40.0) / r.powf(0.62);
                let rc = 0.12 * r + 20.0;
                let depth = 0.16 * hv + 10.0;
                let rim = apex - kk * rc.powf(0.62);
                let (cone, lava) = if d.v >= rc {
                    (D2::c(apex) - d.powf(0.62) * kk, None)
                } else {
                    let t = d * (1.0 / rc);
                    let t4 = t.sq().sq();
                    ((1.0 - t4) * (-depth) + rim, Some((rim - 0.65 * depth).floor() as i32))
                };
                if best.map_or(true, |b| cone.v > b.0.v) {
                    best = Some((cone, lava));
                }
            }
        }
        best
    }

    /// Max over neighbouring hoodoo spires (superellipse domes), LOD-faded to their mean.
    fn hoodoo_spires(&self, x: f64, z: f64, fp: f64) -> D2 {
        // Expected spire volume per area: p·∫dome / cell² (dome ≈ 0.9·πr²·H).
        const MEAN: f32 = HOODOO_P * 0.9 * std::f32::consts::PI * 16.0 * 22.0 / (14.0 * 14.0);
        let fade = 1.0 - smoothstep(2.0, 6.0, fp as f32);
        if fade <= 0.0 {
            return D2::c(MEAN);
        }
        let cx = (x / HOODOO_CELL).floor() as i32;
        let cz = (z / HOODOO_CELL).floor() as i32;
        let mut best = D2::ZERO;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let id = hash2(self.seed ^ 0x400D, cx + dx, cz + dz);
                if hash_f01(id) >= HOODOO_P {
                    continue;
                }
                let (px, pz) = self.hoodoo.point_of(cx + dx, cz + dz);
                let r = 2.5 + 3.0 * hash_f01(mix64(id ^ 3));
                let (ddx, ddz) = ((x - px) as f32, (z - pz) as f32);
                let d2 = ddx * ddx + ddz * ddz;
                if d2 >= r * r {
                    continue;
                }
                let hs = 10.0 + 26.0 * hash_f01(mix64(id ^ 4));
                let dist = d2.sqrt().max(1e-3);
                let u = dist / r;
                let u4 = u * u * u * u;
                let core = (1.0 - u4).max(1e-4);
                let v = hs * core.powf(0.25);
                // dv/dd = −hs·u³ (1 − u⁴)^(−3/4) / r  (clamped: near-vertical walls)
                let dv = (-hs * u * u * u * core.powf(-0.75) / r).max(-40.0);
                if v > best.v {
                    best = D2::new(v, dv * ddx / dist, dv * ddz / dist);
                }
            }
        }
        best * fade + D2::c(MEAN * (1.0 - fade))
    }

    /// 3D density (> 0 solid):
    /// `ρ = (H − y) + [A_n·clamp(2.8·N₃(x, 0.6y, z) − bias) − (A_s/1.6)·r(ℓ)]·(1 − u⁴)`, u = |H − y|/J.
    /// * crags: N₃ is a 3-octave fBm (λ 72) sampled with y *stretched* (×0.6) → vertically
    ///   jointed ribs, buttresses and towers instead of round blobs; above the surface it is biased
    ///   downward (quadratically in the height above H) so overhangs stay attached;
    /// * rock shelters: `r(ℓ)` is the recess profile of the bedding (`Strata::recess`, soft beds)
    ///   and `A_s = notch_reach` = 1.6·|∇H|·D: subtracting |∇H|·D·r undercuts a steep face by
    ///   ≈ D·r blocks horizontally (a face of slope g is g·s blocks above the face at horizontal
    ///   depth s) → alcoves under hard ledges; thin fins holed from both sides become small arches.
    /// `J = A_n + A_s` (capped); A_s is recomputed here from (x, z, slope), A_n = J − A_s.
    /// Exactly `H − y` outside [H − J, H + J] (so the sign is exact there).
    pub fn density(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> f32 {
        let base = col.height - y as f32;
        let j = col.jaggedness;
        if j <= 0.0 || base.abs() >= j {
            return base;
        }
        let u = base.abs() / j;
        let u2 = u * u;
        let taper = 1.0 - u2 * u2;
        let a_s = if col.water_level == i32::MIN { self.notch_reach(x, z, col.slope).min(j) } else { 0.0 };
        let a_n = j - a_s;
        let mut r = 0.0;
        if a_n > 0.05 {
            let mut n = self.n3.sample3(x, y * 0.6, z) * 2.8;
            if base < 0.0 {
                n -= 0.35 * u + 0.45 * u * u;
            }
            r += a_n * n.clamp(-1.0, 1.0);
        }
        if a_s > 0.0 {
            let rec = self.strata.recess(y as f32 + self.strata.offset(x, z), self.strata.facies(x, z));
            r -= a_s * (1.0 / NOTCH_REACH) * rec;
        }
        // Taper 1 − u⁴ → the detail vanishes at the band edges, so ρ is continuous there and
        // equals H − y exactly outside the band.
        base + r * taper
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{Biome, WorldGen, BIOME_COUNT, SEA_LEVEL};
    use crate::noise::hash_f01;

    fn rnd(i: u64) -> f64 {
        hash_f01(mix64(i)) as f64
    }

    #[test]
    fn spline_d_matches_spline_eval() {
        let s = offset_spline();
        let mut outliers = 0;
        for i in 0..20000u64 {
            let c = (rnd(i * 3) * 2.2 - 1.1) as f32;
            let e = (rnd(i * 3 + 1) * 2.2 - 1.1) as f32;
            let p = (rnd(i * 3 + 2) * 2.2 - 1.1) as f32;
            let a = s.eval(&[c, e, p]);
            let b = spline_d(&s, &[D2::new(c, 1.0, 0.0), D2::new(e, 0.0, 1.0), D2::c(p)]);
            assert!((a - b.v).abs() < 1e-3, "{a} vs {}", b.v);
            // derivative w.r.t. C and E vs finite differences
            let hh = 1e-3;
            let fc = (s.eval(&[c + hh, e, p]) - s.eval(&[c - hh, e, p])) / (2.0 * hh);
            let fe = (s.eval(&[c, e + hh, p]) - s.eval(&[c, e - hh, p])) / (2.0 * hh);
            // The frozen spline is only C⁰ in its derivative at knots where the Fritsch–Carlson
            // limiter is active (left/right tangents differ), so a finite difference straddling
            // a knot disagrees there; require agreement everywhere else.
            if (fc - b.dx).abs() > 0.02 * fc.abs().max(10.0) || (fe - b.dz).abs() > 0.02 * fe.abs().max(10.0) {
                outliers += 1;
            }
        }
        assert!(outliers < 200, "{outliers} derivative mismatches of 20000");
    }

    #[test]
    fn terrace_filter_converges() {
        // Box-filtered terrace → exact terrace as delta → 0, → h − s·mean(g) for large delta.
        for i in 0..1000 {
            let h = 140.0 + i as f32 * 0.137;
            let a = terrace_d(D2::c(h), 10.0, 0.0).v;
            let b = terrace_d(D2::c(h), 10.0, 0.6).v;
            assert!((a - b).abs() < 1.6, "{a} {b}");
            let big = terrace_d(D2::c(h), 10.0, 200.0).v;
            assert!((big - (h - 10.0 * 0.4)).abs() < 0.3, "{big} vs {}", h - 4.0);
        }
    }

    #[test]
    fn fast_sin_cos_accuracy() {
        for i in 0..100_000 {
            let x = -40.0 + 80.0 * i as f32 / 100_000.0;
            let (s, c) = fast_sin_cos(x);
            assert!((s - x.sin()).abs() < 3e-4 && (c - x.cos()).abs() < 3e-4, "{x}: {s} {c}");
        }
    }

    #[test]
    fn dune_mean_constant() {
        let n = 100000;
        let m: f64 = (0..n)
            .map(|i| {
                let t = (i as f64 + 0.5) / n as f64;
                (1.0 - (std::f64::consts::PI * t).sin()).powf(1.5)
            })
            .sum::<f64>()
            / n as f64;
        assert!((m as f32 - DUNE_MEAN).abs() < 1e-3, "{m}");
    }

    #[test]
    fn determinism() {
        let a = WorldGen::new(99);
        let b = WorldGen::new(99);
        for i in 0..300u64 {
            let x = (rnd(i * 2) - 0.5) * 30_000.0;
            let z = (rnd(i * 2 + 1) - 0.5) * 30_000.0;
            let ca = a.column(x, z);
            let cb = b.column(x, z);
            assert_eq!(ca.height, cb.height);
            assert_eq!(ca.biome, cb.biome);
            assert_eq!(ca.water_level, cb.water_level);
            assert_eq!(a.density(x, ca.height as f64 + 1.3, z, &ca), b.density(x, cb.height as f64 + 1.3, z, &cb));
        }
    }

    #[test]
    fn density_sign_outside_band() {
        let g = WorldGen::new(42);
        for i in 0..3000u64 {
            let x = (rnd(i * 2) - 0.5) * 30_000.0;
            let z = (rnd(i * 2 + 1) - 0.5) * 30_000.0;
            let c = g.column(x, z);
            let j = c.jaggedness;
            for k in 0..40 {
                let y = (k as f64 / 39.0) * 511.0;
                let d = g.density(x, y, z, &c);
                let base = c.height - y as f32;
                if base > j + 2.0 {
                    assert!(d > 0.0);
                }
                if base < -(j + 2.0) {
                    assert!(d < 0.0);
                }
            }
            // Inside the band ρ is Lipschitz in y: |∂ρ/∂y| ≤ 1 + 4 (taper) + 1.25 (bias) + J·|∂N/∂y|.
            let step = 2.0 * j as f64 / 40.0;
            let mut prev = g.density(x, c.height as f64 - j as f64, z, &c);
            for k in 1..=40 {
                let y = c.height as f64 - j as f64 + step * k as f64;
                let d = g.density(x, y, z, &c);
                assert!((d - prev).abs() <= step as f32 * (6.5 + 0.25 * j) + 1e-3, "density jump {prev} → {d} (j {j}, y {y}, h {})", c.height);
                prev = d;
            }
        }
    }

    /// Neighbouring columns differ by < 6 blocks except at intended terrace / spire / caldera cliffs.
    #[test]
    fn height_continuity() {
        for &seed in &[1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            let mut worst = 0.0f32;
            let mut n_big = 0;
            let n = 6000u64;
            for i in 0..n {
                let x = ((rnd(i * 2 + seed) - 0.5) * 40_000.0).floor() + 0.5;
                let z = ((rnd(i * 2 + 1 + seed) - 0.5) * 40_000.0).floor() + 0.5;
                let c0 = g.column(x, z);
                for (ox, oz) in [(1.0, 0.0), (0.0, 1.0)] {
                    let c1 = g.column(x + ox, z + oz);
                    // intended cliffs: mesa terraces, hoodoo spires, calderas
                    let special = |c: &ColumnInfo| {
                        let cd = g.climate.sample_d(c.x, c.z, 0.0);
                        let r = g.terrain.shape(&cd, c.x, c.z, 0.0).regions;
                        r.badlands > 0.05 || r.hoodoo > 0.05 || r.volcanic > 0.05 || r.stack > 0.0
                    };
                    if special(&c0) || special(&c1) {
                        continue;
                    }
                    let dh = (c1.height - c0.height).abs();
                    worst = worst.max(dh);
                    if dh > 6.0 {
                        n_big += 1;
                        eprintln!("seed {seed} jump {dh} at ({x},{z}) biome {:?} slope {}", c0.biome, c0.slope);
                    }
                }
            }
            eprintln!("seed {seed}: worst neighbour jump {worst}");
            // ≤ 4 of 12 000: residual canyon-wall steps where |∇n_r| varies by > 3 %/block (the
            // river-distance estimate cannot see its Hessian term; see `slope_is_honest`).
            assert!(n_big <= 4, "{n_big} jumps > 6 blocks");
        }
    }

    /// Analytic slope (forward-mode AD) vs central finite differences.
    #[test]
    fn slope_matches_fd() {
        let g = WorldGen::new(2026);
        let mut errs = Vec::new();
        for i in 0..4000u64 {
            let x = (rnd(i * 2 + 5) - 0.5) * 30_000.0;
            let z = (rnd(i * 2 + 6) - 0.5) * 30_000.0;
            let c = g.column(x, z);
            if matches!(c.biome, Biome::Hoodoos | Biome::Badlands | Biome::Volcanic) {
                continue;
            }
            let e = 0.02;
            let hx = (g.column(x + e, z).height - g.column(x - e, z).height) / (2.0 * e as f32);
            let hz = (g.column(x, z + e).height - g.column(x, z - e).height) / (2.0 * e as f32);
            let fd = (hx * hx + hz * hz).sqrt();
            errs.push((c.slope - fd).abs() / (0.05 + fd));
        }
        errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = errs[errs.len() / 2];
        let p90 = errs[errs.len() * 9 / 10];
        eprintln!("slope rel. error: median {med:.4}, p90 {p90:.4}");
        // Exact AD except: gully steering direction treated as locally constant, eroded-fBm
        // damping factor treated as constant (both would need the Hessian).
        assert!(med < 0.06 && p90 < 0.35);
    }

    /// All 30 biomes must occur for at least one of the seeds 1 / 42 / 2026 within 40 km.
    #[test]
    fn all_biomes_present() {
        let mut found = [0usize; BIOME_COUNT];
        for &seed in &[1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            let step = 64.0;
            let n = (40_000.0 / step) as i32;
            for j in 0..n {
                for i in 0..n {
                    let x = -20_000.0 + i as f64 * step + 0.5;
                    let z = -20_000.0 + j as f64 * step + 0.5;
                    let c = g.column_lod(x, z, 8.0);
                    found[c.biome as usize] += 1;
                }
            }
        }
        let total: usize = found.iter().sum();
        for (i, &k) in found.iter().enumerate() {
            eprintln!("{:>18} {:6.3}%", Biome::from_u8(i as u8).name(), 100.0 * k as f64 / total as f64);
        }
        for (i, &k) in found.iter().enumerate() {
            let b = Biome::from_u8(i as u8);
            // DESIGN §2a: frozen water and snowy shores exist only on lakes above the snow line
            // ("or become very rare"), so they may legitimately be absent from this sample.
            if matches!(b, Biome::FrozenOcean | Biome::FrozenRiver | Biome::SnowyBeach) {
                continue;
            }
            assert!(k > 0, "biome {:?} never generated", b);
        }
    }

    /// DESIGN §2a: no snow, ice or frozen water below the local snow line; snowy biomes only
    /// by altitude. Brute force over random land/water columns of three seeds.
    #[test]
    fn no_snow_below_snow_line() {
        use crate::gen::biome::snow_line;
        use crate::materials::Material;
        for &seed in &[1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            let mut snowy = 0;
            for i in 0..12_000u64 {
                let x = ((rnd(i * 2 + seed * 77) - 0.5) * 40_000.0).floor() + 0.5;
                let z = ((rnd(i * 2 + 1 + seed * 77) - 0.5) * 40_000.0).floor() + 0.5;
                let c = g.column(x, z);
                let s = snow_line(seed, x, z, c.climate.temperature);
                assert!((300.0..=360.0).contains(&s), "snow line {s}");
                let y = c.height.floor() as i32;
                for d in 0..3 {
                    let m = g.surface_material(x.floor() as i32, y - d, z.floor() as i32, d, &c);
                    if matches!(m, Material::Snow | Material::PackedIce | Material::Ice) {
                        snowy += 1;
                        assert!((y - d) as f32 >= s - 4.0, "{m:?} at y {} below snow line {s} ({:?})", y - d, c.biome);
                    }
                }
                match c.biome {
                    Biome::SnowyPeaks | Biome::SnowySlopes | Biome::SnowyPlains | Biome::Glacier => {
                        assert!(c.height >= s - 0.01, "{:?} at {} below snow line {s}", c.biome, c.height)
                    }
                    Biome::FrozenOcean | Biome::FrozenRiver => {
                        assert!(c.water_level as f32 >= s, "{:?} water {} below snow line {s}", c.biome, c.water_level)
                    }
                    Biome::SnowyBeach => assert!(c.height >= s, "snowy beach at {}", c.height),
                    Biome::SnowyTaiga | Biome::AlpineGrove => {
                        assert!(c.height >= s - 90.5, "{:?} at {} (snow line {s})", c.biome, c.height)
                    }
                    _ => {}
                }
            }
            assert!(snowy > 50, "seed {seed}: no snow at all ({snowy})");
        }
    }

    /// DESIGN §2a: mountainous terrain (H > SEA+150) ≈ 10–15 % of land, concentrated in a few
    /// large ranges with main peaks 430–500.
    #[test]
    fn mountains_rare_but_big() {
        for &seed in &[1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            let (step, n) = (80.0, 500usize);
            let (mut land, mut mtn, mut high, mut core) = (0usize, 0usize, 0usize, 0usize);
            for j in 0..n {
                for i in 0..n {
                    let x = -20_000.0 + (i as f64 + 0.5) * step;
                    let z = -20_000.0 + (j as f64 + 0.5) * step;
                    let c = g.column_lod(x, z, step);
                    if c.height < SEA {
                        continue;
                    }
                    land += 1;
                    mtn += (c.height > SEA + 150.0) as usize;
                    high += (c.height > 430.0) as usize;
                    core += (g.climate.sample_d(x, z, step).orogeny.v > 0.5) as usize;
                }
            }
            let f = mtn as f64 / land as f64;
            let fc = core as f64 / land as f64;
            eprintln!("seed {seed}: mountainous {:.1}% of land, belt cores {:.1}%, H>430 cells {high}", 100.0 * f, 100.0 * fc);
            assert!((0.07..0.17).contains(&f), "mountainous fraction {f}");
            assert!((0.08..0.30).contains(&fc), "belt core fraction {fc}");
            assert!(high > 100, "no main peaks above 430 ({high})");
        }
    }

    /// DESIGN §2a: the lee side of the ranges (large upwind mountain mass) is drier.
    #[test]
    fn rain_shadow_dries_lee_side() {
        let (mut lee, mut nl, mut open, mut no) = (0.0f64, 0usize, 0.0f64, 0usize);
        for &seed in &[1u64, 42, 2026] {
            let g = WorldGen::new(seed);
            for i in 0..30_000u64 {
                let x = (rnd(i * 2 + seed * 3) - 0.5) * 40_000.0;
                let z = (rnd(i * 2 + 1 + seed * 3) - 0.5) * 40_000.0;
                let cd = g.climate.sample_d(x, z, 16.0);
                if cd.c.v < -0.1 || cd.orogeny.v > 0.05 {
                    continue;
                }
                if cd.shadow > 0.25 {
                    lee += cd.h.v as f64;
                    nl += 1;
                } else if cd.shadow < 0.02 {
                    open += cd.h.v as f64;
                    no += 1;
                }
                let (wx, wz) = g.climate.wind(x, z);
                assert!((wx * wx + wz * wz).sqrt() <= 1.0 + 1e-4);
            }
        }
        let (ml, mo) = (lee / nl as f64, open / no as f64);
        eprintln!("mean humidity: lee {ml:.3} ({nl}), open {mo:.3} ({no})");
        assert!(nl > 300 && no > 300);
        assert!(mo - ml > 0.35, "rain shadow too weak: {mo} vs {ml}");
    }

    /// Lakes: flat level; every water column's 4-neighbours are water at the same level or dry
    /// ground that holds it (no vertical water faces). Rows through several lakes at 1-block
    /// spacing (catches every shore crossing along the row) plus the cross-row neighbours.
    #[test]
    fn lakes_are_contained() {
        let mut lakes = 0;
        let mut water_cols = 0;
        for &seed in &[1u64, 42] {
            let g = WorldGen::new(seed);
            'cells: for iz in -10..10 {
                for ix in -10..10 {
                    let Some((cx, cz, r, Ok(level))) = g.terrain.lake_debug(&g.climate, ix, iz) else { continue };
                    lakes += 1;
                    let reach = (r * 1.25 + 4.0) as i32;
                    for row in (-reach..=reach).step_by(23) {
                        let z = (cz as i32 + row) as f64 + 0.5;
                        let mut prev: Option<ColumnInfo> = None;
                        for k in -reach..=reach {
                            let x = (cx as i32 + k) as f64 + 0.5;
                            let c = g.column(x, z);
                            let wet = |c: &ColumnInfo| c.water_level > SEA_LEVEL && c.water_level as f32 > c.height;
                            if wet(&c) {
                                water_cols += 1;
                                assert_eq!(c.water_level, level as i32, "lake level not flat");
                                let up = g.column(x, z + 1.0);
                                for nb in [prev.as_ref(), Some(&up)].into_iter().flatten() {
                                    assert!(
                                        wet(nb) || nb.height > level - 0.5,
                                        "water wall at ({x},{z}): level {level}, neighbour h {} wl {}",
                                        nb.height,
                                        nb.water_level
                                    );
                                }
                            } else if let Some(p) = &prev {
                                if wet(p) {
                                    assert!(c.height > level - 0.5, "water wall at ({x},{z}) h {}", c.height);
                                }
                            }
                            prev = Some(c);
                        }
                    }
                    if lakes >= 6 {
                        break 'cells;
                    }
                }
            }
        }
        eprintln!("{lakes} lakes, {water_cols} water columns checked");
        assert!(lakes >= 4 && water_cols > 500);
    }

    /// A dry column's 3D detail band never reaches below sea level (no unflooded air pockets
    /// under the waterline next to river/sea columns). Brute force over jagged columns.
    #[test]
    fn dry_band_stays_above_sea() {
        let g = WorldGen::new(1);
        let mut n = 0;
        for i in 0..60_000u64 {
            let x = ((rnd(i * 2 + 555) - 0.5) * 40_000.0).floor() + 0.5;
            let z = ((rnd(i * 2 + 556) - 0.5) * 40_000.0).floor() + 0.5;
            let c = g.column(x, z);
            if c.water_level != i32::MIN || c.jaggedness == 0.0 {
                continue;
            }
            n += 1;
            assert!(c.height - c.jaggedness >= SEA - 1e-3, "band reaches {} at ({x},{z})", c.height - c.jaggedness);
            for y in 100..SEA_LEVEL {
                assert!(g.density(x, y as f64 + 0.5, z, &c) > 0.0, "air below sea level at ({x},{y},{z})");
            }
        }
        assert!(n > 2000, "{n}");
    }

    /// Seeds must not share structure at the world origin (all octaves used to sit on a lattice
    /// vertex at (0,0), where gradient noise vanishes: C = −0.094 and a river for every seed).
    #[test]
    fn origin_differs_between_seeds() {
        let cs: Vec<f32> = (0..16u64).map(|s| ClimateSampler::new(s).sample(0.0, 0.0, 0.0).continentalness).collect();
        let mean = cs.iter().sum::<f32>() / cs.len() as f32;
        let var = cs.iter().map(|c| (c - mean) * (c - mean)).sum::<f32>() / cs.len() as f32;
        assert!(var.sqrt() > 0.2, "origin continentalness nearly seed-independent: {cs:?}");
        let rivers = (0..16u64).filter(|&s| ClimateSampler::new(s).sample(0.0, 0.0, 0.0).river > 0.9).count();
        assert!(rivers < 8, "{rivers}/16 seeds have a river through the origin");
    }

    /// Fuzz: every field finite and in range for random positions (incl. far and negative
    /// coordinates, cell/lattice boundaries) and footprints.
    #[test]
    fn fuzz_columns_finite() {
        let g = WorldGen::new(7);
        let fps = [0.0, 1.0, 1.5, 2.0, 8.0, 32.0, 64.0, 1000.0];
        for i in 0..6000u64 {
            let scale = [1.0, 1e3, 3e4, 4e5, 2e6][(i % 5) as usize];
            let mut x = (rnd(i * 3) - 0.5) * scale;
            let mut z = (rnd(i * 3 + 1) - 0.5) * scale;
            if i % 7 == 0 {
                // exactly on lattice / cell boundaries
                x = (x / 256.0).round() * 256.0;
                z = (z / 1400.0).round() * 1400.0;
            }
            let fp = fps[(i % fps.len() as u64) as usize];
            let c = g.column_lod(x, z, fp);
            let cl = c.climate;
            for v in [c.height, c.slope, c.surface_temperature, c.jaggedness, cl.continentalness, cl.erosion, cl.weirdness, cl.peaks_valleys, cl.temperature, cl.humidity, cl.river] {
                assert!(v.is_finite(), "non-finite at ({x},{z}) fp {fp}: {c:?}");
            }
            assert!((6.0..WORLD_HEIGHT as f32).contains(&c.height), "height {} at ({x},{z})", c.height);
            assert!(c.jaggedness >= 0.0 && c.height + c.jaggedness < WORLD_HEIGHT as f32);
            assert!(c.water_level == i32::MIN || (SEA_LEVEL..WORLD_HEIGHT).contains(&c.water_level));
            for v in [cl.continentalness, cl.erosion, cl.temperature, cl.humidity, cl.river] {
                assert!((-1.3..=1.3).contains(&v), "climate out of range {cl:?}");
            }
            for k in 0..8 {
                let y = c.height as f64 - c.jaggedness as f64 - 3.0 + k as f64 * (c.jaggedness as f64 + 3.0) / 4.0;
                assert!(g.density(x, y, z, &c).is_finite());
            }
        }
    }

    /// Far-LOD columns agree with full-resolution ones (no clipmap seams / band-limit drift):
    /// heights of footprint-8 samples stay close to footprint-0, lake levels are identical.
    #[test]
    fn lod_matches_full_resolution() {
        let g = WorldGen::new(42);
        let mut d8 = Vec::new();
        let mut d32 = Vec::new();
        for i in 0..3000u64 {
            let x = (rnd(i * 2 + 9) - 0.5) * 30_000.0;
            let z = (rnd(i * 2 + 10) - 0.5) * 30_000.0;
            let c0 = g.column(x, z);
            let c8 = g.column_lod(x, z, 8.0);
            let c32 = g.column_lod(x, z, 32.0);
            d8.push((c8.height - c0.height).abs());
            d32.push((c32.height - c0.height).abs());
            if c0.water_level > SEA_LEVEL && c8.water_level > SEA_LEVEL && c0.biome != Biome::Volcanic {
                assert_eq!(c0.water_level, c8.water_level, "lake level differs between LODs");
            }
        }
        d8.sort_by(|a, b| a.partial_cmp(b).unwrap());
        d32.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |v: &Vec<f32>, p: f64| v[((v.len() - 1) as f64 * p) as usize];
        eprintln!("|h_lod − h|: fp8 median {:.2} p95 {:.2}; fp32 median {:.2} p95 {:.2}", q(&d8, 0.5), q(&d8, 0.95), q(&d32, 0.5), q(&d32, 0.95));
        assert!(q(&d8, 0.5) < 2.0 && q(&d8, 0.95) < 12.0);
        assert!(q(&d32, 0.5) < 4.0 && q(&d32, 0.95) < 30.0);
    }

    /// The analytic slope must not hide steps: neighbour height differences larger than the
    /// reported slopes (plus margin) are rare (the river-distance estimate drops a Hessian term).
    #[test]
    fn slope_is_honest() {
        let g = WorldGen::new(1);
        let (mut n, mut bad) = (0usize, 0usize);
        for i in 0..20_000u64 {
            let x = ((rnd(i * 2 + 101) - 0.5) * 40_000.0).floor() + 0.5;
            let z = ((rnd(i * 2 + 102) - 0.5) * 40_000.0).floor() + 0.5;
            let c0 = g.column(x, z);
            if c0.height < SEA || matches!(c0.biome, Biome::Hoodoos | Biome::Badlands | Biome::Volcanic) {
                continue;
            }
            let c1 = g.column(x + 1.0, z);
            n += 1;
            if (c1.height - c0.height).abs() > 1.5 * c0.slope.max(c1.slope) + 1.5 {
                bad += 1;
            }
        }
        eprintln!("slope under-reported at {bad}/{n} land pairs");
        assert!((bad as f64) < 0.003 * n as f64, "{bad}/{n}");
    }

    /// Timing of `column()` at footprint 0 and 8 (single core). Prints µs/column.
    #[test]
    fn column_performance() {
        let g = WorldGen::new(1);
        for &fp in &[0.0, 8.0, 32.0] {
            let n = 200_000u64;
            let t0 = std::time::Instant::now();
            let mut acc = 0.0f32;
            for i in 0..n {
                let x = (i % 1000) as f64 * 7.3 - 3000.0;
                let z = (i / 1000) as f64 * 11.1 - 1000.0;
                acc += g.terrain.column(&g.climate, x, z, fp).height;
            }
            let us = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
            eprintln!("column(footprint {fp}): {us:.3} µs  (checksum {acc})");
        }
    }
}
