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
//! * **Gully filter** (dendritic drainage): 4 octaves of slope-aligned plane waves on a jittered
//!   lattice, wave vector ⟂ a smooth steering gradient, amplitude ∝ local slope × wavelength,
//!   each octave steered by the previous ones (see [`TerrainShaper::gullies`]).
//! * **Rivers**: with `d = |n|/|∇n|` (distance to the zero set of the river noise) the valley is the
//!   lower envelope `H = smin(H₀, V(d))`, `V(d) = bank − D(1 − d²/w²)` inside the channel and
//!   `bank + σ(E)·u + κ(E)·u²`, `u = d − w`, outside: σ ≈ 0.05 (flood plains) … 2.5 (canyons).
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
use super::biome::{classify, BiomeInput};
use super::climate::{lerp_d, smoothstep_d, ClimateD, ClimateSampler, DEroded, DFbm, DRidged, D2};
use super::{ColumnInfo, SEA_LEVEL, WORLD_HEIGHT};
use crate::math::{saturate, smoothstep};
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
const OROGENIC_UPLIFT: f32 = 130.0;

/// Mountain amplitude A(C, E): uplift that multiplies the [0,1] mountain shape.
fn amp_spline() -> Spline {
    Spline::new(
        0,
        vec![
            (-0.16, 0.0.into()),
            (-0.03, cs(1, &[(-1.0, 70.0), (-0.5, 25.0), (0.0, 5.0), (1.0, 1.0)])),
            (0.12, cs(1, &[(-1.0, 200.0), (-0.65, 140.0), (-0.3, 50.0), (0.0, 18.0), (0.5, 6.0), (1.0, 2.0)])),
            (0.32, cs(1, &[(-1.0, 240.0), (-0.7, 205.0), (-0.4, 120.0), (-0.1, 40.0), (0.4, 12.0), (1.0, 4.0)])),
            (0.65, cs(1, &[(-1.0, 250.0), (-0.7, 220.0), (-0.4, 140.0), (-0.1, 50.0), (0.4, 16.0), (1.0, 5.0)])),
        ],
    )
}

/// River valley wall slope σ(E) (rise per block) and convexity κ(E).
fn wall_spline() -> Spline {
    Spline::consts(1, &[(-1.0, 2.6), (-0.6, 1.5), (-0.25, 0.55), (0.1, 0.18), (0.5, 0.07), (1.0, 0.04)])
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
    swamp_n: DFbm,
    hoodoo: Worley2,
    volcano: Worley2,
}

/// Inland lakes: cell size, candidate probability, max radius, rim probes.
const LAKE_CELL: f64 = 1700.0;
const LAKE_P: f32 = 0.55;
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

/// |x| as a dual (kink at 0 is the sill crest).
#[inline]
fn sabs_tent(x: D2) -> D2 {
    x.abs()
}

const VOLCANO_CELL: f64 = 3600.0;
const VOLCANO_P: f32 = 0.14;
const HOODOO_CELL: f64 = 14.0;
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
            swamp_n: DFbm::new(s("terrain.swamp"), 36.0, 2, 2.0, 0.5),
            hoodoo: Worley2::new(s("terrain.hoodoo"), HOODOO_CELL, 0.75),
            volcano: Worley2::new(s("terrain.volcano"), VOLCANO_CELL, 0.7),
        }
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

    /// Region weights from climate alone.
    pub fn regions(&self, cd: &ClimateD, x: f64, z: f64, fp: f64) -> Regions {
        let (t, hu, e, c) = (cd.t.v, cd.h.v, cd.e.v, cd.c.v);
        let hot = smoothstep(0.30, 0.46, t);
        let dry = smoothstep(-0.26, -0.40, hu);
        let desert_all = hot * dry;
        let very_dry = smoothstep(-0.50, -0.64, hu);
        let badlands = desert_all * very_dry * smoothstep(0.50, 0.20, e);
        let hoodoo = if badlands > 0.0 {
            badlands * smoothstep(0.16, 0.30, self.select.value(x + 3100.0, z - 1700.0, fp))
        } else {
            0.0
        };
        let swamp = smoothstep(0.30, 0.48, hu)
            * smoothstep(-0.20, 0.02, t)
            * smoothstep(-0.10, 0.30, e)
            * smoothstep(0.40, 0.22, c)
            * smoothstep(-0.14, -0.06, c);
        // sea cliffs follow the *raw* erosion (the coast-shaping input of the offset spline)
        let cliff = smoothstep(-0.25, -0.60, cd.e_raw.v) * smoothstep(-0.20, -0.15, c) * smoothstep(0.02, -0.06, c);
        // dry-land landforms only on land (no mesas or spires on the sea floor)
        let land = smoothstep(-0.16, -0.08, c);
        Regions {
            desert: desert_all * (1.0 - badlands) * land,
            badlands: badlands * land,
            hoodoo: hoodoo * land,
            swamp,
            cliff,
            ..Default::default()
        }
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
        let mut reg = self.regions(cd, x, z, fp);
        // Offset (hypsometry + coast shape) from the raw erosion; uplift, valley walls etc. from
        // the effective, orogeny-gated erosion.
        let inp = [cd.c, cd.e, cd.pv];
        let base = spline_d(&self.offset, &[cd.c, cd.e_raw, cd.pv]);
        let amp = spline_d(&self.amp, &inp).maxf(0.0);

        // --- mountains / hills
        let q = &cd.q;
        let (ridge, steer) = self.ridged.sample_with_steer(q.x, q.z, fp);
        let (ridge, steer) = (q.chain(ridge), q.chain(steer));
        let ridge = ((ridge - 0.12) * 1.25).clamp(0.0, 1.0);
        let hills = self.hills.sample(x, z, fp);
        let hills01 = (hills * 1.3 + 0.45).clamp(0.0, 1.0);
        // Ridges dominate in high uplift and on pv-peaks; rounded eroded hills elsewhere.
        let s_mix = (smoothstep_d(-0.5, 0.6, cd.pv) * 0.5 + smoothstep_d(40.0, 200.0, amp) * 0.5).clamp(0.0, 1.0);
        let shape = lerp_d(hills01, ridge, s_mix);
        let vfac = smoothstep_d(-0.95, -0.35, cd.pv) * 0.65 + 0.35;
        // Orogenic massif: broad dome over the belt core (land only).
        let massif = smoothstep_d(0.05, 0.9, cd.orogeny) * smoothstep_d(-0.12, 0.05, cd.c) * OROGENIC_UPLIFT;
        let mut mountain = amp * shape * vfac + massif;
        let mut steer_dbg = (0.0, 0.0);
        // dendritic gullies on mountain flanks (strength ∝ local relief)
        if mountain.v > 12.0 {
            let ga = 0.65 * smoothstep(12.0, 60.0, mountain.v);
            // Smooth steering proxy (see `DRidged::sample_with_steer`): regional slope + rounded
            // main crests. Sharp crests / fast-turning gradients must not steer the gully phase.
            let k = amp.v * vfac.v * s_mix.v * 1.25;
            let gx = base.dx + k * steer.dx;
            let gz = base.dz + k * steer.dz;
            steer_dbg = (gx, gz);
            let g = self.gullies(x, z, (gx, gz), ga, fp);
            mountain = mountain + g;
        }
        let mut h = base + mountain;

        // --- seafloor relief
        let sea_w = 1.0 - smoothstep(-0.26, -0.16, cd.c.v);
        if sea_w > 0.0 {
            h = h + hills * (18.0 * sea_w);
        }

        // --- fine roughness ±2–4 blocks (less on dunes/swamps/beaches)
        let rug = smoothstep(10.0, 120.0, amp.v);
        // flat beaches / shallows: no speckle islands at the waterline
        let shore = 0.25 + 0.75 * smoothstep(1.5, 9.0, (h.v - SEA).abs());
        let rough_amp = (1.0 + 1.4 * rug) * (1.0 - 0.85 * reg.desert) * (1.0 - 0.7 * reg.swamp) * shore;
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
            let low = smoothstep_d(SEA + 18.0, SEA + 5.0, h);
            let target = self.swamp_n.sample(x, z, fp) * 2.6 + (SEA + 0.35);
            h = lerp_d(h, target, low * (0.9 * reg.swamp));
        }

        // --- rivers: lower envelope with a U-channel + valley walls
        let gate = cd.river_gate;
        if gate > 0.0 && cd.c.v > -0.35 {
            let n = cd.river_n;
            let gl = n.grad_len().max(1e-7);
            // d = |n|/|∇n| as a dual (gradient of the distance ≈ unit vector across the river).
            let sgn = if n.v < 0.0 { -1.0 } else { 1.0 };
            let d = D2::new(n.v.abs() / gl, sgn * n.dx / gl, sgn * n.dz / gl);
            let d = D2::new(d.v.min(1.0e5), d.dx, d.dz);
            let w = (cd.river_width * cd.river_width + 0.36 * (fp * fp) as f32).sqrt();
            let depth = 2.5 + 0.25 * w;
            let above = (h.v - SEA).max(0.0);
            let bank = SEA - 0.6 + (1.0 - gate) * above * 0.95;
            let sigma = spline_d(&self.wall, &inp);
            let kappa = 0.012 * (1.0 - saturate((cd.e.v + 1.0) * 0.5)) + 0.0015;
            let v = if d.v < w {
                let r = d * (1.0 / w);
                (1.0 - r.sq()) * (-depth) + bank
            } else {
                let u = d - w;
                u * sigma + u.sq() * kappa + bank
            };
            let k = 6.0 + 10.0 * saturate((cd.e.v + 1.0) * 0.5);
            if v.v < h.v + k {
                h = h.smin(v, k);
            }
            reg.channel = gate * (1.0 - smoothstep(w * 0.7, w * 1.15, d.v));
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

        // --- soft ceiling (world height 512)
        if h.v > 466.0 {
            let t = ((h.v - 466.0) / 30.0).tanh();
            h = h.map(466.0 + 30.0 * t, 1.0 - t * t);
        }
        h = h.maxf(6.0);

        // --- jaggedness (3D detail band)
        let slope = h.grad_len();
        let j_m = if mountain.v > 60.0 {
            let jg = smoothstep(-0.15, 0.30, self.jag.value(x, z, fp));
            26.0 * smoothstep(60.0, 230.0, mountain.v) * (0.3 + 0.7 * jg)
        } else {
            0.0
        };
        let j_c = 14.0 * reg.cliff * smoothstep(SEA + 3.0, SEA + 12.0, h.v);
        let j_s = 9.0 * smoothstep(1.2, 2.8, slope) * smoothstep(SEA + 2.0, SEA + 20.0, h.v);
        // no 3D detail on lake basins: an overhang hollow next to a water column would expose
        // a vertical water face
        let tame = (1.0 - reg.badlands) * (1.0 - reg.desert) * (1.0 - 0.8 * reg.volcanic) * (1.0 - reg.swamp)
            * if reg.lake > 0.0 || water_level > SEA_LEVEL { 0.0 } else { 1.0 };
        let mut jag = j_m.max(j_c).max(j_s) * tame;
        jag = jag.min(28.0).min((WORLD_HEIGHT as f32 - 3.0 - h.v).max(0.0));
        if jag < 0.5 {
            jag = 0.0;
        }

        if water_level == i32::MIN && h.v < SEA {
            water_level = SEA_LEVEL;
        } else if water_level > SEA_LEVEL && reg.volcanic <= 0.5 && h.v >= water_level as f32 {
            // hoodoo spire / soft ceiling raised the column above its lake level
            water_level = i32::MIN;
        }
        Shape { height: h, mountain: mountain.v, regions: reg, jaggedness: jag, water_level, steer: steer_dbg }
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
        // centres in the middle 40 % of the cell: ≥ 1020 blocks apart > 2 × max reach (≈ 446)
        let jx = 0.3 + 0.4 * hash_f01(mix64(id ^ 1)) as f64;
        let jz = 0.3 + 0.4 * hash_f01(mix64(id ^ 2)) as f64;
        let r = 90.0 + (LAKE_R_MAX - 90.0) * hash_f01(mix64(id ^ 3)).powf(1.5);
        let depth = 5.0 + 0.045 * r;
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
        let (mut rim_lo, mut rim_hi) = (f32::MAX, f32::MIN);
        for k in 0..LAKE_RIM_SAMPLES {
            let a = (k as f64 + 0.5) * std::f64::consts::TAU / LAKE_RIM_SAMPLES as f64;
            let h = sample(cx + a.cos() * r as f64, cz + a.sin() * r as f64)?;
            rim_lo = rim_lo.min(h);
            rim_hi = rim_hi.max(h);
        }
        let level = (rim_lo - 1.0).floor();
        if level < SEA + 2.0 || level > 440.0 {
            return Err("level");
        }
        if rim_hi - rim_lo > 0.35 * r + 12.0 {
            return Err("slope");
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
    /// * basin: `H₁ = H − D·max(0, 1 − ρ²)²` (islands survive where the terrain is high),
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
        for dz in -1..=1 {
            for dx in -1..=1 {
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
                    let q = 1.0 - rho.sq();
                    *h = *h - q.sq() * depth;
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
        const MEAN: f32 = 0.6 * 0.9 * std::f32::consts::PI * 16.0 * 22.0 / (14.0 * 14.0);
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
                if hash_f01(id) >= 0.6 {
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

    /// 3D density (> 0 solid): `ρ = (H − y) + J·clamp(2.8·N₃(x, 1.4y, z) − bias)·(1 − u⁴)`,
    /// u = |H − y|/J. Exactly `H − y` outside [H − J, H + J] (so the sign is exact there).
    pub fn density(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> f32 {
        let base = col.height - y as f32;
        let j = col.jaggedness;
        if j <= 0.0 || base.abs() >= j {
            return base;
        }
        let mut n = self.n3.sample3(x, y * 1.4, z) * 2.8;
        let u = base.abs() / j;
        if base < 0.0 {
            // Above the surface: bias downward (quadratically in the height above H) → shelves and
            // overhangs stay attached to the slope; isolated floating blobs become rare.
            n -= 0.35 * u + 0.45 * u * u;
        }
        // Taper 1 − u⁴ → the detail vanishes at the band edges, so ρ is continuous there and
        // equals H − y exactly outside the band.
        let u2 = u * u;
        base + j * n.clamp(-1.0, 1.0) * (1.0 - u2 * u2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{Biome, WorldGen, BIOME_COUNT};
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
                        r.badlands > 0.05 || r.hoodoo > 0.05 || r.volcanic > 0.05
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
            assert!(n_big <= 2, "{n_big} jumps > 6 blocks");
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
            assert!(k > 0, "biome {:?} never generated", Biome::from_u8(i as u8));
        }
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
