//! Caves. OWNER: caves-features.
//!
//! # Mathematics
//!
//! The carver is one continuous scalar field `F(p)`, measured (approximately) in **blocks**
//! (positive inside a cavity, ≈ signed distance near the walls). It is the smooth maximum
//! (`smax_k(a,b) = max(a,b) + max(k − |a−b|, 0)²/(4k)`, k = 2.5 → rounded fillets where passages
//! meet) of four geological ingredients, minus smooth penalty terms:
//!
//! 1. **Speleogenetic tunnel network** — real limestone caves dissolve along the *intersection
//!    lines of bedding planes and joint sets*. We model three implicit surface families with
//!    single-octave gradient noise (custom 3D simplex with analytic gradient, [`SimplexD3`]):
//!    * bedding `A(p) = S(x/λa, κa·y/λa, z/λa)` with κa > 1 → zero sheets are sub-horizontal
//!      strata,
//!    * joints `B`, `C` with κ < 1 → zero sheets are sub-vertical fractures.
//!
//!    Passages are tubes around the curves `A = 0 ∧ B = 0` and `A = 0 ∧ C = 0`. Because both
//!    families of curves lie on the *same* surface `A = 0`, on that 2-manifold they are two
//!    families of plane-like curves that generically **cross** — the passages form a connected
//!    2D graph per bedding level (two random curves in ℝ³ would almost never meet). Levels are
//!    linked by rarer sub-vertical **shafts** `B = 0 ∧ C = 0`.
//!
//!    Distance to an intersection curve uses the Gauss–Newton (minimum-norm) step of the
//!    linearised system `J δ = −v`, `v = (a, b)ᵀ`, `J = [∇a; ∇b]`:
//!    `d² = vᵀ (J Jᵀ)⁻¹ v = (a²|∇b|² − 2ab ∇a·∇b + b²|∇a|²) / |∇a × ∇b|²`
//!    — the exact first-order distance, so every tube has the prescribed radius `r` regardless
//!    of how steep the noise is. The tube field `(r² − d²)/(2r)` equals `r − d` to first order at
//!    the wall but is a smooth quadratic (no cone kink), ideal for trilinear interpolation on the
//!    4-block lattice. `r(p) ∈ [2.3, 5]` swells and pinches with a low-frequency noise.
//! 2. **Cheese caverns** — 3-octave fBm on `(x, 1.6y, z)` (λ = 110) above a threshold that rises
//!    smoothly toward the surface (so caverns rarely breach) and drifts regionally (λ ≈ 1.9 km),
//!    scaled by its mean gradient into blocks.
//! 3. **Ravines** — 2D: the zero set of a 2-octave noise `n(x,z)` with distance `|n|/|∇n|`
//!    (analytic gradient), gated by a rare low-frequency mask; the half-width shrinks with depth
//!    as `w(D) = w₀ + (w_top − w₀)(1 − D/D_r)^1.3`, walls roughened by 3D noise.
//! 4. **Entrances** — tunnels are pushed below a rock cover `D_min` by the penalty
//!    `1.6·max(0, D_min − D′)`; a 2D gate lowers `D_min` from 8 to −6 so passages break through
//!    the surface in a few places (never near rivers, coasts or under water).
//!
//! **Aquifers.** A jittered-grid Voronoi diagram (cell 56) assigns each cell an optional flat
//! water table `L`. `fill` returns water below `L`. Where two adjacent cells have different tables
//! the field is clipped by a *septum* `F ≤ 1.5·(δ − 2.5)` with `δ = (|p−c₂|² − |p−c₁|²)/(2|c₂−c₁|)`
//! the exact distance to the Voronoi bisector — a 5-block rock wall that keeps each pool level.
//! Lava fills everything carved at `y ≤ LAVA_LEVEL`.
//!
//! **Guarantees.** `F < 0` for `y < MIN_CARVE_Y + 0.5` and within 6 blocks below the floor of a
//! water-covered column (penalty slopes ≥ 4/block make this hold for lattice-interpolated values
//! too). The field is C¹ except on measure-zero creases of `smax`/`max` and Voronoi bisectors,
//! with feature sizes ≥ ~5 blocks, so a 4×4×4 lattice + trilinear interpolation reproduces it.
use super::{CaveFill, ColumnInfo, SEA_LEVEL};
use crate::noise::{derive_seed, hash2, hash3, hash_f01, mix64, salt, Fbm, Simplex};

/// Carved voxels at or below this y are filled with lava.
pub const LAVA_LEVEL: i32 = 11;
/// Nothing is carved below this y.
pub const MIN_CARVE_Y: i32 = 4;
/// Aquifer Voronoi cell size (blocks).
const AQ_CELL: f64 = 56.0;
/// Highest possible aquifer water table.
const AQ_MAX_LEVEL: f32 = (SEA_LEVEL - 14) as f32;
/// Aquifer presence threshold on a 2D simplex (≈ 45 % of cells hold water).
const AQ_PRESENCE_T: f32 = 0.05;
/// Septa are evaluated up to this far above `AQ_MAX_LEVEL` (where their taper has ended).
const SEPTUM_RISE: f32 = 12.0;
/// Minimum jitter of the aquifer sites inside their grid cell (see `aq_point`).
const AQ_JITTER_MIN: f64 = 0.2;

// ------------------------------------------------------------------ 3D simplex with gradient

const F3: f64 = 1.0 / 3.0;
const G3: f64 = 1.0 / 6.0;
const SCALE3: f32 = 106.0;
const GRAD3: [[f32; 3]; 16] = {
    const S: f32 = std::f32::consts::FRAC_1_SQRT_2;
    [
        [S, S, 0.0],
        [-S, S, 0.0],
        [S, -S, 0.0],
        [-S, -S, 0.0],
        [S, 0.0, S],
        [-S, 0.0, S],
        [S, 0.0, -S],
        [-S, 0.0, -S],
        [0.0, S, S],
        [0.0, -S, S],
        [0.0, S, -S],
        [0.0, -S, -S],
        [S, S, 0.0],
        [-S, S, 0.0],
        [0.0, -S, S],
        [0.0, -S, -S],
    ]
};

/// 3D simplex noise (same kernel as `noise::Simplex::n3`: radius² 0.5, (0.5 − r²)⁴ falloff)
/// returning the value and its exact analytic gradient
/// `∇ Σ t⁴ (g·d) = Σ (−8 t³ (g·d) d + t⁴ g)`.
#[derive(Clone, Copy, Debug)]
pub struct SimplexD3 {
    seed: u64,
}

impl SimplexD3 {
    pub fn new(seed: u64) -> Self {
        SimplexD3 { seed: mix64(seed ^ 0x3D3D_5151) }
    }

    /// (value, ∂/∂x, ∂/∂y, ∂/∂z) with respect to the input coordinates. Value ≈ [-1, 1].
    #[inline]
    pub fn n3d(&self, x: f64, y: f64, z: f64) -> (f32, f32, f32, f32) {
        let s = (x + y + z) * F3;
        let i = (x + s).floor();
        let j = (y + s).floor();
        let k = (z + s).floor();
        let t = (i + j + k) * G3;
        let x0 = (x - (i - t)) as f32;
        let y0 = (y - (j - t)) as f32;
        let z0 = (z - (k - t)) as f32;
        let (i1, j1, k1, i2, j2, k2) = if x0 >= y0 {
            if y0 >= z0 {
                (1, 0, 0, 1, 1, 0)
            } else if x0 >= z0 {
                (1, 0, 0, 1, 0, 1)
            } else {
                (0, 0, 1, 1, 0, 1)
            }
        } else if y0 < z0 {
            (0, 0, 1, 0, 1, 1)
        } else if x0 < z0 {
            (0, 1, 0, 0, 1, 1)
        } else {
            (0, 1, 0, 1, 1, 0)
        };
        let g3 = G3 as f32;
        let ii = i as i64 as i32;
        let jj = j as i64 as i32;
        let kk = k as i64 as i32;
        let mut acc = [0.0f32; 4];
        #[inline(always)]
        fn corner(acc: &mut [f32; 4], x: f32, y: f32, z: f32, h: u64) {
            let t = 0.5 - x * x - y * y - z * z;
            if t <= 0.0 {
                return;
            }
            let g = GRAD3[(h & 15) as usize];
            let gd = g[0] * x + g[1] * y + g[2] * z;
            let t2 = t * t;
            let t4 = t2 * t2;
            let k = -8.0 * t2 * t * gd;
            acc[0] += t4 * gd;
            acc[1] += k * x + t4 * g[0];
            acc[2] += k * y + t4 * g[1];
            acc[3] += k * z + t4 * g[2];
        }
        corner(&mut acc, x0, y0, z0, hash3(self.seed, ii, jj, kk));
        corner(
            &mut acc,
            x0 - i1 as f32 + g3,
            y0 - j1 as f32 + g3,
            z0 - k1 as f32 + g3,
            hash3(self.seed, ii + i1, jj + j1, kk + k1),
        );
        corner(
            &mut acc,
            x0 - i2 as f32 + 2.0 * g3,
            y0 - j2 as f32 + 2.0 * g3,
            z0 - k2 as f32 + 2.0 * g3,
            hash3(self.seed, ii + i2, jj + j2, kk + k2),
        );
        corner(
            &mut acc,
            x0 - 1.0 + 3.0 * g3,
            y0 - 1.0 + 3.0 * g3,
            z0 - 1.0 + 3.0 * g3,
            hash3(self.seed, ii + 1, jj + 1, kk + 1),
        );
        (acc[0] * SCALE3, acc[1] * SCALE3, acc[2] * SCALE3, acc[3] * SCALE3)
    }
}

/// An anisotropic implicit surface family `S(R·p ⊙ f)`: horizontal wavelength `λ`, vertical
/// stretch `κ` (κ > 1 → sub-horizontal sheets, κ < 1 → sub-vertical sheets), rotated about y.
#[derive(Clone, Copy, Debug)]
struct Sheet {
    n: SimplexD3,
    f: f64,
    fy: f64,
    c: f64,
    s: f64,
    off: (f64, f64, f64),
}

impl Sheet {
    fn new(seed: u64, wavelength: f64, kappa: f64, angle: f64) -> Self {
        let h = mix64(seed ^ 0x0FF5);
        Sheet {
            n: SimplexD3::new(seed),
            f: 1.0 / wavelength,
            fy: kappa / wavelength,
            c: angle.cos(),
            s: angle.sin(),
            off: (hash_f01(h) as f64 * 512.0, hash_f01(mix64(h)) as f64 * 512.0, hash_f01(mix64(h ^ 7)) as f64 * 512.0),
        }
    }
    /// Value and world-space gradient (chain rule through the rotation and scaling).
    #[inline]
    fn eval(&self, x: f64, y: f64, z: f64) -> (f32, [f32; 3]) {
        let u = (x * self.c - z * self.s) * self.f + self.off.0;
        let w = (x * self.s + z * self.c) * self.f + self.off.2;
        let v = y * self.fy + self.off.1;
        let (n, du, dv, dw) = self.n.n3d(u, v, w);
        let f = self.f as f32;
        let (c, s) = (self.c as f32, self.s as f32);
        // u = (x c − z s) f, w = (x s + z c) f
        let gx = (du * c + dw * s) * f;
        let gz = (-du * s + dw * c) * f;
        let gy = dv * self.fy as f32;
        (n, [gx, gy, gz])
    }
}

#[inline]
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Squared Gauss–Newton distance to the curve {a = 0, b = 0}:
/// `d² = (a²|∇b|² − 2ab ∇a·∇b + b²|∇a|²) / |∇a × ∇b|²` (regularised when the sheets are tangent).
#[inline]
#[cfg_attr(not(test), allow(dead_code))]
fn curve_dist2(a: f32, ga: [f32; 3], b: f32, gb: [f32; 3]) -> f32 {
    curve_dist2_angle(a, ga, b, gb).0
}

/// [`curve_dist2`] plus `sin²θ = |∇a × ∇b|² / (|∇a|²|∇b|²)`, θ = crossing angle of the sheets.
/// In terms of the sheet distances `dₐ = a/|∇a|`, `d_b = b/|∇b|` the distance is
/// `d² = (dₐ² + d_b² − 2 dₐ d_b cos θ) / (sin²θ + ε)` — exact for planes, but where the sheets
/// cross at a grazing angle (sin²θ → ε) the first-order model breaks down: the field becomes
/// extremely steep (measured ≈ 70 blocks/block) and carves thin ribbons along tangencies.
#[inline]
fn curve_dist2_angle(a: f32, ga: [f32; 3], b: f32, gb: [f32; 3]) -> (f32, f32) {
    let aa = dot3(ga, ga);
    let bb = dot3(gb, gb);
    let ab = dot3(ga, gb);
    let cross = (aa * bb - ab * ab).max(0.0);
    let det = cross + 0.04 * aa * bb + 1e-12;
    let d2 = (a * a * bb - 2.0 * a * b * ab + b * b * aa).max(0.0) / det;
    (d2, cross / (aa * bb + 1e-12))
}

/// Penalty (blocks) removing tube segments where the two sheets cross at a grazing angle
/// (θ ≲ 20°), where the linearised distance is unreliable: `GRAZE·(1 − smoothstep(s₀, s₁, sin²θ))`.
#[inline]
fn graze_penalty(sin2: f32) -> f32 {
    8.0 * (1.0 - smoothstep(0.04, 0.16, sin2))
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Polynomial smooth maximum (C¹): `max(a,b) + max(k − |a−b|, 0)² / (4k)`.
#[inline]
pub fn smax(a: f32, b: f32, k: f32) -> f32 {
    let h = (k - (a - b).abs()).max(0.0);
    a.max(b) + h * h / (4.0 * k)
}

/// The individual cave components at a point (blocks; > 0 = carve). For debug views.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaveParts {
    pub cheese: f32,
    pub tunnels: f32,
    pub shafts: f32,
    pub ravine: f32,
    /// Total penalty subtracted (surface cover, floor, water).
    pub penalty: f32,
    /// Final field.
    pub total: f32,
}

pub struct CaveCarver {
    pub seed: u64,
    bed: Sheet,
    joint1: Sheet,
    joint2: Sheet,
    radius_n: Simplex,
    shaft_gate: Simplex,
    cheese: Fbm,
    cheese_region: Simplex,
    entrance: Simplex,
    ravine_a: Simplex,
    ravine_b: Simplex,
    ravine_gate: Simplex,
    ravine_depth: Simplex,
    rough: Simplex,
    aq_seed: u64,
    aq_presence: Simplex,
    aq_height: Simplex,
}

/// Cheese fBm wavelength and its mean gradient → blocks conversion (measured by the test
/// `cheese_gradient_scale`: E|∇n| ≈ 0.022/block for λ = 150, 3 octaves).
const CHEESE_LAMBDA: f64 = 150.0;
const CHEESE_SCALE: f32 = 1.0 / 0.022;
/// Above this threshold the cheese fBm is not evaluated (see the fade in `parts`).
const CHEESE_T_SKIP: f32 = 0.62;
/// Fade (blocks) applied as the threshold approaches `CHEESE_T_SKIP`.
const CHEESE_FADE: f32 = 12.0;
/// Fade (blocks) of the shaft field outside the shaft districts.
const SHAFT_FADE: f32 = 12.0;

impl CaveCarver {
    pub fn new(seed: u64) -> Self {
        let s = |n: &str| derive_seed(seed, salt(n));
        CaveCarver {
            seed,
            bed: Sheet::new(s("caves.bedding"), 150.0, 3.4, 0.0),
            joint1: Sheet::new(s("caves.joint1"), 115.0, 0.55, 0.45),
            joint2: Sheet::new(s("caves.joint2"), 130.0, 0.55, 1.35),
            radius_n: Simplex::new(s("caves.radius")),
            shaft_gate: Simplex::new(s("caves.shaftgate")),
            cheese: Fbm::new(s("caves.cheese"), CHEESE_LAMBDA, 3, 2.0, 0.5),
            cheese_region: Simplex::new(s("caves.cheese_region")),
            entrance: Simplex::new(s("caves.entrance")),
            ravine_a: Simplex::new(s("caves.ravine_a")),
            ravine_b: Simplex::new(s("caves.ravine_b")),
            ravine_gate: Simplex::new(s("caves.ravine_gate")),
            ravine_depth: Simplex::new(s("caves.ravine_depth")),
            rough: Simplex::new(s("caves.rough")),
            aq_seed: s("caves.aquifer"),
            aq_presence: Simplex::new(s("caves.aquifer_presence")),
            aq_height: Simplex::new(s("caves.aquifer_height")),
        }
    }

    /// Continuous cave field (> 0 ⇒ carve). See module docs.
    #[inline]
    pub fn field(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> f32 {
        self.parts(x, y, z, col).total
    }

    /// The field with all of its components (debug / analysis).
    pub fn parts(&self, x: f64, y: f64, z: f64, col: &ColumnInfo) -> CaveParts {
        let yf = y as f32;
        let h = col.height;
        let jag = col.jaggedness;
        let mut parts = CaveParts { cheese: -50.0, tunnels: -50.0, shafts: -50.0, ravine: -50.0, ..Default::default() };
        // Far above any solid block or far below the carve floor: nothing to do.
        if yf > h + jag + 8.0 || yf < MIN_CARVE_Y as f32 - 6.0 {
            parts.penalty = 50.0;
            parts.total = -(yf - (h + jag)).abs().max(8.0);
            return parts;
        }
        let wet = col.water_level != i32::MIN && col.water_level as f32 > h;
        // ---- penalties (blocks) -------------------------------------------------------------
        let depth = h - yf; // below the smooth 2D surface
        let cover = depth - 0.6 * jag; // conservative rock cover over the point
        let floor_pen = 4.0 * (MIN_CARVE_Y as f32 + 4.0 - yf).max(0.0);
        let water_pen = if wet { 4.0 * (yf - (h - 10.0)).max(0.0) } else { 0.0 };
        if wet && yf > h - 5.0 {
            parts.penalty = 50.0;
            parts.total = -(floor_pen + water_pen).max(8.0);
            return parts;
        }
        // Where may a passage break through the surface?
        let c = &col.climate;
        let e_raw = self.entrance.n2(x * (1.0 / 260.0), z * (1.0 / 260.0));
        let mut gate = smoothstep(0.38, 0.62, e_raw);
        gate *= 1.0 - smoothstep(0.05, 0.3, c.river); // not into rivers
        gate *= smoothstep(-0.12, -0.02, c.continentalness); // not into the sea
        gate *= 1.0 - smoothstep(0.0, 6.0, SEA_LEVEL as f32 + 3.0 - h).max(0.0); // not at sea level
        // Dry columns next to water must keep a roof as well.
        let near_water = smoothstep(0.02, 0.25, c.river).max(1.0 - smoothstep(-0.12, -0.04, c.continentalness));
        let d_min = 8.0 + 6.0 * near_water - 14.0 * gate;
        let cover_pen = 1.6 * (d_min - cover).max(0.0);
        let base_pen = floor_pen + water_pen;

        // ---- tunnel network ----------------------------------------------------------------
        let (a, ga) = self.bed.eval(x, y, z);
        let (b, gb) = self.joint1.eval(x, y, z);
        let (cc, gc) = self.joint2.eval(x, y, z);
        let rn = self.radius_n.n3(x * (1.0 / 70.0), y * (1.0 / 45.0), z * (1.0 / 70.0));
        let r = 2.6 + 2.8 * smoothstep(-0.55, 0.65, rn);
        let (d_ab, s_ab) = curve_dist2_angle(a, ga, b, gb);
        let (d_ac, s_ac) = curve_dist2_angle(a, ga, cc, gc);
        let t_ab = (r * r - d_ab) / (2.0 * r) - graze_penalty(s_ab);
        let t_ac = (r * r - d_ac) / (2.0 * r) - graze_penalty(s_ac);
        let tunnels = t_ab.max(t_ac);
        // shafts: sub-vertical curves B = 0 ∧ C = 0, only in gated districts
        // The gate fades the shaft field by −SHAFT_FADE·(1 − sg) (instead of shrinking the radius
        // to a positive minimum and then jumping to −50 at sg = 0, which cut thin shafts off with
        // a flat face along the gate contour). At sg → 0 the field is ≤ rs/2 − SHAFT_FADE < −8,
        // so skipping the evaluation there is exact for the carve decision.
        let sg = smoothstep(0.25, 0.6, self.shaft_gate.n2(x * (1.0 / 400.0), z * (1.0 / 400.0)));
        let shafts = if sg > 0.0 {
            let rs = 1.4 + 1.8 * smoothstep(-0.5, 0.6, rn);
            let (d_bc, s_bc) = curve_dist2_angle(b, gb, cc, gc);
            (rs * rs - d_bc) / (2.0 * rs) - SHAFT_FADE * (1.0 - sg) - graze_penalty(s_bc)
        } else {
            -50.0
        };
        parts.tunnels = tunnels - cover_pen;
        parts.shafts = shafts - cover_pen;

        // ---- cheese caverns ----------------------------------------------------------------
        let region = self.cheese_region.n2(x * (1.0 / 1900.0), z * (1.0 / 1900.0));
        // Threshold: high near the surface, lower deep down, regional drift.
        let mut t = 0.385 - 0.10 * region;
        t += 0.42 * (1.0 - smoothstep(10.0, 42.0, cover));
        t -= 0.035 * (1.0 - smoothstep(30.0, 110.0, yf)); // big halls deep down
        // Skipping the (expensive) fBm above the cut-off T_SKIP must not change the carve
        // decision: the fade term drives the field to ≤ (n_max − T_SKIP)·S − CHEESE_FADE < 0
        // as t → T_SKIP (n_max ≈ 0.75 measured by `cheese_cutoff_is_continuous`), so caverns
        // taper out instead of being sliced flat along the t = T_SKIP iso-surface.
        if t < CHEESE_T_SKIP {
            let n = self.cheese.sample3(x, y * 1.6, z);
            let fade = CHEESE_FADE * smoothstep(CHEESE_T_SKIP - 0.12, CHEESE_T_SKIP, t);
            parts.cheese = ((n - t) * CHEESE_SCALE).min(12.0) - fade;
        }

        // ---- ravines -----------------------------------------------------------------------
        let rg_raw = self.ravine_gate.n2(x * (1.0 / 1500.0), z * (1.0 / 1500.0));
        let rg = smoothstep(0.42, 0.62, rg_raw) * (1.0 - near_water);
        if rg > 0.0 {
            parts.ravine = self.ravine(x, yf, z, h, depth, rg);
        }

        let mut f = smax(parts.tunnels, parts.shafts, 2.0);
        f = smax(f, parts.cheese, 2.5);
        f = smax(f, parts.ravine, 2.0);
        f -= base_pen;
        parts.penalty = base_pen + cover_pen;

        // ---- aquifer septa -----------------------------------------------------------------
        if yf < AQ_MAX_LEVEL + SEPTUM_RISE && f > -6.0 {
            let dist = self.aquifer_septum_dist(x, yf, z);
            if dist < 12.0 {
                // septum along the bisector, 5–8 blocks thick, its faces wobbling
                let wob = 0.75 + 0.75 * self.rough.n3(x * (1.0 / 23.0), yf as f64 * (1.0 / 17.0), z * (1.0 / 23.0));
                let wall = 1.5 * (dist - 2.6 - wob);
                f = f.min(wall);
            }
        }
        parts.total = f;
        parts
    }

    /// Ravine field (blocks) — see module docs.
    #[inline]
    fn ravine(&self, x: f64, yf: f32, z: f64, h: f32, depth: f32, gate: f32) -> f32 {
        const L1: f64 = 1.0 / 520.0;
        const L2: f64 = 1.0 / 190.0;
        let (n1, d1x, d1z) = self.ravine_a.n2d(x * L1, z * L1);
        let (n2, d2x, d2z) = self.ravine_b.n2d(x * L2 + 31.7, z * L2 - 11.3);
        let n = n1 + 0.3 * n2;
        let gx = d1x * L1 as f32 + 0.3 * d2x * L2 as f32;
        let gz = d1z * L1 as f32 + 0.3 * d2z * L2 as f32;
        let dh = n.abs() / (gx * gx + gz * gz).sqrt().max(1e-5);
        if dh > 14.0 {
            return -(dh - 14.0) - 6.0;
        }
        let dvar = self.ravine_depth.n2(x * (1.0 / 300.0), z * (1.0 / 300.0));
        // ravine depth below the surface (≥ 0.5 keeps u finite; the floor term then closes it)
        let d_r = (gate * (34.0 + 22.0 * dvar)).max(0.5);
        let w_top = 2.2 + 4.3 * gate;
        let u = (depth / d_r).clamp(0.0, 1.0);
        let w = 1.6 + (w_top - 1.6) * (1.0 - u).powf(1.3);
        let rough = 1.3 * self.rough.n3(x * (1.0 / 14.0), yf as f64 * (1.0 / 9.0), z * (1.0 / 14.0));
        let floor = 2.5 * (depth - d_r).max(0.0);
        // Gate fade: the slot closes continuously toward the gate contour (≤ w_top + 1.3 − 10 < 0
        // for gate ≤ 0.05) instead of ending in a vertical wall where d_r crossed a threshold.
        let fade = 10.0 * (1.0 - smoothstep(0.05, 0.4, gate));
        let _ = h;
        w - dh + rough - floor - fade
    }

    /// Effective distance from (x, z) to the nearest Voronoi bisector between the owning aquifer
    /// cell and any neighbouring cell whose water table differs. Bisector distance:
    /// `δ = (|p−c_k|² − |p−c₁|²) / (2|c_k−c₁|)` (exact for a Voronoi diagram). Above the higher
    /// of the two tables the septum is not needed; instead of switching it off at a fixed height
    /// (a jump of up to ≈ 18 blocks in the field → flat-cut walls) the distance grows by
    /// `1.2·(y − L_max − 1)₊`, so the wall tapers off over ≈ 10 blocks. `f32::MAX` if none.
    fn aquifer_septum_dist(&self, x: f64, y: f32, z: f64) -> f32 {
        let cx = (x / AQ_CELL).floor() as i32;
        let cz = (z / AQ_CELL).floor() as i32;
        let mut cells = [(0.0f64, 0.0f64, 0.0f64, 0i32, 0i32); 16];
        let mut n = 0;
        let mut best = 0usize;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (px, pz) = self.aq_point(cx + dx, cz + dz);
                let d2 = (px - x) * (px - x) + (pz - z) * (pz - z);
                cells[n] = (d2, px, pz, cx + dx, cz + dz);
                if d2 < cells[best].0 {
                    best = n;
                }
                n += 1;
            }
        }
        let (d1, p1x, p1z, c1x, c1z) = cells[best];
        let l1 = self.aq_level(c1x, c1z);
        let mut out = f32::MAX;
        for (k, &(dk, pkx, pkz, ckx, ckz)) in cells[..n].iter().enumerate() {
            if k == best {
                continue;
            }
            let sep = ((p1x - pkx).powi(2) + (p1z - pkz).powi(2)).sqrt().max(1e-6);
            let dist = ((dk - d1) / (2.0 * sep)) as f32;
            if dist >= out || dist > 14.0 {
                continue;
            }
            let lk = self.aq_level(ckx, ckz);
            if (l1 - lk).abs() > 0.5 {
                out = out.min(dist + 1.2 * (y - l1.max(lk) - 1.0).max(0.0));
            }
        }
        out
    }

    #[inline]
    fn aq_point(&self, cx: i32, cz: i32) -> (f64, f64) {
        let h = hash2(self.aq_seed, cx, cz);
        // Jitter in [a, 1 − a] with a = AQ_JITTER_MIN ≥ (√2 − 1)/(√2 + 1) ≈ 0.172 makes the 3×3
        // search exact: the own site is within (1 − a)√2 ≤ 1 + a cells of any point of the cell,
        // while every site outside the 3×3 block is farther than 1 + a. (With a = 0.1 the
        // restricted "Voronoi" diagram had spurious borders on grid lines without septa.)
        let jx = AQ_JITTER_MIN + (1.0 - 2.0 * AQ_JITTER_MIN) * hash_f01(h) as f64;
        let jz = AQ_JITTER_MIN + (1.0 - 2.0 * AQ_JITTER_MIN) * hash_f01(mix64(h)) as f64;
        ((cx as f64 + jx) * AQ_CELL, (cz as f64 + jz) * AQ_CELL)
    }

    /// Water table of an aquifer cell (world y), or −1e4 when the cell is dry.
    #[inline]
    /// Presence and level come from low-frequency noise evaluated at the cell centre (plus a
    /// little hash dither), quantised to 10-block steps: neighbouring cells usually share one
    /// table, so septa only appear where the regional water table steps.
    fn aq_level(&self, cx: i32, cz: i32) -> f32 {
        let px = (cx as f64 + 0.5) * AQ_CELL;
        let pz = (cz as f64 + 0.5) * AQ_CELL;
        let h = hash2(self.aq_seed ^ 0xA0A0, cx, cz);
        let pres = self.aq_presence.n2(px * (1.0 / 520.0), pz * (1.0 / 520.0)) + 0.2 * (hash_f01(h) - 0.5);
        if pres < AQ_PRESENCE_T {
            return -1.0e4;
        }
        let v = 0.5 + 0.5 * self.aq_height.n2(px * (1.0 / 800.0), pz * (1.0 / 800.0));
        let lo = LAVA_LEVEL as f32 + 3.0;
        let l = lo + (AQ_MAX_LEVEL - lo) * v.clamp(0.0, 1.0).powf(1.3);
        // 10-block steps 13, 23, …; capped so that the table never exceeds AQ_MAX_LEVEL (the old
        // rounding produced 133 > AQ_MAX_LEVEL, a table `fill` silently clipped to 126).
        let q = (l / 10.0).round() * 10.0 + 3.0;
        if q > AQ_MAX_LEVEL {
            q - 10.0
        } else {
            q
        }
    }

    /// Local aquifer water table at (x, z) (`None` = dry cell).
    pub fn water_table(&self, x: f64, z: f64) -> Option<f32> {
        let cx = (x / AQ_CELL).floor() as i32;
        let cz = (z / AQ_CELL).floor() as i32;
        let mut best = (f64::MAX, 0, 0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (px, pz) = self.aq_point(cx + dx, cz + dz);
                let d2 = (px - x) * (px - x) + (pz - z) * (pz - z);
                if d2 < best.0 {
                    best = (d2, cx + dx, cz + dz);
                }
            }
        }
        let l = self.aq_level(best.1, best.2);
        (l > -1.0e3).then_some(l)
    }

    /// What fills a carved voxel: lava at the bottom, water below the local aquifer table, air
    /// elsewhere. The rule depends on (x, y, z) only through the aquifer cell, so every pool is
    /// flat and two horizontally adjacent carved voxels of one cell never disagree. (An earlier
    /// extra condition `y < H − 10` made sealed caves under sloping sea/lake beds alternate
    /// between air and water from column to column; dry land is always ≥ 10 blocks above the
    /// highest table, so the condition only ever mattered there.)
    pub fn fill(&self, x: i32, y: i32, z: i32, col: &ColumnInfo) -> CaveFill {
        let _ = col;
        if y <= LAVA_LEVEL {
            return CaveFill::Lava;
        }
        if (y as f32) < AQ_MAX_LEVEL {
            if let Some(l) = self.water_table(x as f64 + 0.5, z as f64 + 0.5) {
                if (y as f32) < l {
                    return CaveFill::Water;
                }
            }
        }
        CaveFill::Air
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::WorldGen;
    use crate::noise::hash_f01;

    fn rnd(i: u64) -> f64 {
        hash_f01(mix64(i ^ 0xCAFE)) as f64
    }

    #[test]
    fn simplex_d3_gradient_matches_fd() {
        let n = SimplexD3::new(5);
        let h = 1e-4;
        for i in 0..3000u64 {
            let (x, y, z) = (rnd(i * 3) * 40.0, rnd(i * 3 + 1) * 40.0, rnd(i * 3 + 2) * 40.0);
            let (_, dx, dy, dz) = n.n3d(x, y, z);
            let fx = (n.n3d(x + h, y, z).0 - n.n3d(x - h, y, z).0) / (2.0 * h as f32);
            let fy = (n.n3d(x, y + h, z).0 - n.n3d(x, y - h, z).0) / (2.0 * h as f32);
            let fz = (n.n3d(x, y, z + h).0 - n.n3d(x, y, z - h).0) / (2.0 * h as f32);
            for (a, b) in [(dx, fx), (dy, fy), (dz, fz)] {
                assert!((a - b).abs() < 0.05 * (1.0 + b.abs()), "{a} vs {b}");
            }
        }
    }

    #[test]
    fn cheese_gradient_scale() {
        // E|∇n| of the cheese fBm (in noise units per block, y squashed ×1.6 is ignored: the
        // horizontal gradient dominates the wall distance of the flattened halls).
        let c = CaveCarver::new(3);
        let h = 0.5;
        let mut sum = 0.0f64;
        let n = 20000u64;
        for i in 0..n {
            let (x, y, z) = (rnd(i * 3) * 5000.0, rnd(i * 3 + 1) * 200.0, rnd(i * 3 + 2) * 5000.0);
            let gx = c.cheese.sample3(x + h, y * 1.6, z) - c.cheese.sample3(x - h, y * 1.6, z);
            let gz = c.cheese.sample3(x, y * 1.6, z + h) - c.cheese.sample3(x, y * 1.6, z - h);
            let gy = c.cheese.sample3(x, (y + h) * 1.6, z) - c.cheese.sample3(x, (y - h) * 1.6, z);
            sum += (((gx * gx + gy * gy + gz * gz) as f64).sqrt()) / (2.0 * h);
        }
        let g = sum / n as f64;
        eprintln!("cheese E|grad| = {g:.5} per block (const 1/{:.1})", CHEESE_SCALE);
        assert!((g as f32 * CHEESE_SCALE - 1.0).abs() < 0.25, "{g}");
    }

    #[test]
    fn curve_distance_is_metric() {
        // For two exact planes a = x, b = z the curve is the y-axis and d² = x² + z².
        let d = curve_dist2(3.0, [1.0, 0.0, 0.0], 4.0, [0.0, 0.0, 1.0]);
        assert!((d - 25.0 / 1.04).abs() < 1e-3);
        // Scaling the implicit functions must not change the distance.
        let d2 = curve_dist2(30.0, [10.0, 0.0, 0.0], 0.4, [0.0, 0.0, 0.1]);
        assert!((d - d2).abs() < 1e-3);
    }

    #[test]
    fn determinism_and_floor() {
        let g1 = WorldGen::new(77);
        let g2 = WorldGen::new(77);
        for i in 0..2000u64 {
            let x = rnd(i * 2) * 4000.0 - 2000.0;
            let z = rnd(i * 2 + 1) * 4000.0 - 2000.0;
            let col = g1.column(x, z);
            let y = rnd(i * 7) * 300.0;
            assert_eq!(g1.cave_field(x, y, z, &col), g2.cave_field(x, y, z, &col));
            for yy in [0.5, 1.5, 2.5, 3.5, 4.0] {
                assert!(g1.cave_field(x, yy, z, &col) < 0.0, "carved at y={yy}");
            }
            if col.water_level != i32::MIN && col.water_level as f32 > col.height {
                for k in 0..6 {
                    let yy = col.height as f64 - k as f64;
                    assert!(g1.cave_field(x, yy, z, &col) < 0.0, "carved under water");
                }
            }
        }
    }

    /// Cave volume fraction among underground blocks (y ∈ [4, H − 1]) on land, measured with the
    /// 4×4×4 lattice + trilinear interpolation exactly as the world builder carves (the raw field
    /// fraction is reported too; the lattice loses a little of the thinnest tunnels).
    #[test]
    fn cave_fraction_calibration() {
        let (mut tot, mut carved, mut carved_direct) = (0usize, 0usize, 0usize);
        let mut by: [(usize, usize); 6] = [(0, 0); 6];
        let mut comp = [0usize; 4];
        let (mut land_cols, mut open_cols) = (0usize, 0usize);
        for seed in [1u64, 2, 3] {
            let g = WorldGen::new(seed);
            for i in 0..500u64 {
                let lx = ((rnd(i * 2 + seed * 1000) * 16000.0 - 8000.0) / 4.0).floor();
                let lz = ((rnd(i * 2 + 1 + seed * 1000) * 16000.0 - 8000.0) / 4.0).floor();
                let corners: Vec<ColumnInfo> =
                    (0..4).map(|k| g.column((lx + (k & 1) as f64) * 4.0, (lz + (k >> 1) as f64) * 4.0)).collect();
                // one block column inside the lattice cell
                let (ox, oz) = (1.5 + (i % 2) as f64, 1.5 + (i / 2 % 2) as f64);
                let (x, z) = (lx * 4.0 + ox, lz * 4.0 + oz);
                let (tx, tz) = ((ox / 4.0) as f32, (oz / 4.0) as f32);
                let col = g.column(x, z);
                if col.height < SEA_LEVEL as f32 + 2.0 || col.water_level != i32::MIN {
                    continue;
                }
                let top = (col.height - 0.5).floor() as i32;
                let lat = |ly: i32| -> f32 {
                    let v: Vec<f32> = (0..4)
                        .map(|k| g.cave_field((lx + (k & 1) as f64) * 4.0, ly as f64 * 4.0, (lz + (k >> 1) as f64) * 4.0, &corners[k]))
                        .collect();
                    let a = v[0] + (v[1] - v[0]) * tx;
                    let b = v[2] + (v[3] - v[2]) * tx;
                    a + (b - a) * tz
                };
                let mut lo = lat(0);
                let mut lo_y = 0;
                let mut hi = lat(1);
                land_cols += 1;
                let mut opened = false;
                for y in 4..=top {
                    let yc = y as f32 + 0.5;
                    while yc > (lo_y + 1) as f32 * 4.0 {
                        lo_y += 1;
                        lo = hi;
                        hi = lat(lo_y + 1);
                    }
                    let f = lo + (hi - lo) * (yc / 4.0 - lo_y as f32);
                    let p = g.caves.parts(x, yc as f64, z, &col);
                    tot += 1;
                    let b = ((y / 64) as usize).min(5);
                    by[b].0 += 1;
                    if p.total > 0.0 {
                        carved_direct += 1;
                    }
                    if f > 0.0 {
                        carved += 1;
                        by[b].1 += 1;
                        let v = [p.cheese, p.tunnels, p.shafts, p.ravine];
                        comp[(0..4).fold(0, |b, i| if v[i] > v[b] { i } else { b })] += 1;
                        if y == top {
                            opened = true;
                        }
                    }
                }
                open_cols += opened as usize;
            }
        }
        let frac = carved as f64 / tot as f64;
        let bands: Vec<String> =
            by.iter().enumerate().map(|(i, &(t, c))| format!("y{}..: {:.1}%", i * 64, 100.0 * c as f64 / t.max(1) as f64)).collect();
        eprintln!(
            "cave fraction (lattice) {:.2}%, raw field {:.2}% ({tot} blocks); {}",
            100.0 * frac,
            100.0 * carved_direct as f64 / tot as f64,
            bands.join(", ")
        );
        eprintln!(
            "dominant component share of underground: cheese {:.2}% tunnels {:.2}% shafts {:.2}% ravine {:.2}%",
            100.0 * comp[0] as f64 / tot as f64,
            100.0 * comp[1] as f64 / tot as f64,
            100.0 * comp[2] as f64 / tot as f64,
            100.0 * comp[3] as f64 / tot as f64
        );
        eprintln!("surface openings: {:.2}% of land columns", 100.0 * open_cols as f64 / land_cols as f64);
        assert!((0.06..=0.12).contains(&frac), "cave fraction {frac}");
    }

    /// The 3×3 neighbourhood search of the aquifer Voronoi diagram must agree with a 5×5
    /// brute-force search everywhere (jitter bound, see `aq_point`).
    #[test]
    fn aquifer_voronoi_3x3_is_exact() {
        let c = CaveCarver::new(11);
        for i in 0..200_000u64 {
            let x = rnd(i * 2) * 40_000.0 - 20_000.0;
            let z = rnd(i * 2 + 1) * 40_000.0 - 20_000.0;
            let (cx, cz) = ((x / AQ_CELL).floor() as i32, (z / AQ_CELL).floor() as i32);
            let near = |r: i32| {
                let mut best = (f64::MAX, 0, 0);
                for dz in -r..=r {
                    for dx in -r..=r {
                        let (px, pz) = c.aq_point(cx + dx, cz + dz);
                        let d2 = (px - x).powi(2) + (pz - z).powi(2);
                        if d2 < best.0 {
                            best = (d2, cx + dx, cz + dz);
                        }
                    }
                }
                (best.1, best.2)
            };
            assert_eq!(near(1), near(2), "3×3 search misses the nearest site at ({x}, {z})");
        }
    }

    /// Aquifer tables are flat 10-block steps that never exceed `AQ_MAX_LEVEL`.
    #[test]
    fn aquifer_levels_bounded() {
        let c = CaveCarver::new(4);
        let mut wet = 0;
        for i in 0..20_000i32 {
            let l = c.aq_level(i % 211 - 105, i / 211 - 47);
            if l > -1.0e3 {
                wet += 1;
                assert!(l <= AQ_MAX_LEVEL && l > LAVA_LEVEL as f32, "level {l}");
                assert_eq!((l - 3.0) % 10.0, 0.0);
            }
        }
        assert!(wet > 4000 && wet < 16_000, "{wet}");
    }

    /// No jumps of the field near its zero level. For random points and 0.5-block vertical and
    /// horizontal steps with a large difference, the step is bisected (toward the steeper half)
    /// down to 2⁻¹³ block: a continuous field shrinks the difference to ~0 (it is steep only
    /// near critical points of the sheet noises, ≲ 200 blocks/block), a cut-off does not.
    /// (The old cheese / ravine / shaft / septum cut-offs jumped by up to 18–50 blocks.)
    #[test]
    fn field_has_no_jumps_near_zero() {
        let mut steep = 0;
        let mut worst_fine = 0.0f32;
        for seed in [1u64, 2] {
            let g = WorldGen::new(seed);
            for i in 0..4000u64 {
                let x = (rnd(i * 5 + seed) * 12000.0 - 6000.0).floor() + 0.5;
                let z = (rnd(i * 5 + 1 + seed) * 12000.0 - 6000.0).floor() + 0.5;
                let col = g.column(x, z);
                for k in 0..40u64 {
                    let y = 4.0 + rnd(i * 977 + k) * (col.height as f64 - 2.0).max(5.0);
                    for (dx, dy, dz) in [(0.0, 0.5, 0.0), (0.5, 0.0, 0.0), (0.0, 0.0, 0.5)] {
                        let f = |t: f64| g.caves.field(x + dx * t, y + dy * t, z + dz * t, &col);
                        let (a, b) = (f(0.0), f(1.0));
                        if a.max(b) <= -3.0 || (a - b).abs() < 4.0 {
                            continue;
                        }
                        steep += 1;
                        // bisect toward the steepest sub-interval
                        let (mut lo, mut hi) = (0.0f64, 1.0f64);
                        for _ in 0..12 {
                            let mid = 0.5 * (lo + hi);
                            if (f(mid) - f(lo)).abs() >= (f(hi) - f(mid)).abs() {
                                hi = mid;
                            } else {
                                lo = mid;
                            }
                        }
                        let d = (f(hi) - f(lo)).abs();
                        if d > worst_fine {
                            worst_fine = d;
                            eprintln!("steep step {a:.2} → {b:.2} at ({x}, {y:.2}, {z}) dir ({dx},{dy},{dz}): 2^-13-block step {d:.4}");
                        }
                    }
                }
            }
        }
        eprintln!("{steep} steep 0.5-block steps; worst difference over 2^-13 block: {worst_fine:.4}");
        assert!(worst_fine < 0.1, "field discontinuity: jump {worst_fine} over 2^-13 block");
    }

    /// Emulates the world builder (4×4×4 lattice, trilinear interpolation, wet guard) over
    /// regions holding aquifers and counts liquid faces that touch air: a carved water voxel
    /// next to a carved air voxel at the same height (an unsupported water wall), or carved
    /// water directly above carved air.
    #[test]
    fn no_unsupported_cave_water() {
        let (mut carved, mut water, mut side, mut over) = (0usize, 0usize, 0usize, 0usize);
        for seed in [1u64, 7] {
            let g = WorldGen::new(seed);
            let mut regions = 0;
            for i in 0..400u64 {
                let x0 = ((rnd(i * 3 + seed * 77) * 16000.0 - 8000.0) / 4.0).floor() as i32 * 4;
                let z0 = ((rnd(i * 3 + 1 + seed * 77) * 16000.0 - 8000.0) / 4.0).floor() as i32 * 4;
                if g.caves.water_table(x0 as f64, z0 as f64).is_none() {
                    continue;
                }
                regions += 1;
                if regions > 12 {
                    break;
                }
                const N: i32 = 40;
                let nn = (N / 4 + 1) as usize;
                let nodes: Vec<ColumnInfo> = (0..nn * nn)
                    .map(|k| g.column((x0 + 4 * (k % nn) as i32) as f64 + 0.5, (z0 + 4 * (k / nn) as i32) as f64 + 0.5))
                    .collect();
                let nj = 34usize; // y up to 136
                let vals: Vec<f32> = (0..nn * nn * nj)
                    .map(|k| {
                        let (n, j) = (k / nj, k % nj);
                        let c = &nodes[n];
                        g.cave_field(c.x, (4 * j) as f64 + 0.5, c.z, c)
                    })
                    .collect();
                let cols: Vec<ColumnInfo> =
                    (0..(N * N) as usize).map(|k| g.column((x0 + k as i32 % N) as f64 + 0.5, (z0 + k as i32 / N) as f64 + 0.5)).collect();
                // kind: 0 rock, 1 air (terrain or cave), 2 water, 3 lava; cave flag separate
                let ny = 4 * (nj - 1);
                let mut kind = vec![0u8; (N * N) as usize * ny];
                let mut cave = vec![false; (N * N) as usize * ny];
                for lz in 0..N {
                    for lx in 0..N {
                        let col = &cols[(lz * N + lx) as usize];
                        let (i, k) = ((lx / 4) as usize, (lz / 4) as usize);
                        let (tx, tz) = ((lx % 4) as f32 / 4.0, (lz % 4) as f32 / 4.0);
                        let wts = [(1.0 - tx) * (1.0 - tz), tx * (1.0 - tz), (1.0 - tx) * tz, tx * tz];
                        let ids = [k * nn + i, k * nn + i + 1, (k + 1) * nn + i, (k + 1) * nn + i + 1];
                        let top = (col.height - 0.5).ceil() as i32; // solid below
                        let wet = col.water_level != i32::MIN && col.water_level as f32 > col.height;
                        let guard = if wet { top - 6 } else { i32::MAX };
                        for y in 0..ny as i32 {
                            let at = ((lz * N + lx) as usize) * ny + y as usize;
                            if y >= top {
                                kind[at] = if y < col.water_level { 2 } else { 1 };
                                continue;
                            }
                            let j = (y / 4) as usize;
                            let fy = (y % 4) as f32 / 4.0;
                            let f = |jj: usize| (0..4).map(|q| wts[q] * vals[ids[q] * nj + jj]).sum::<f32>();
                            let v = f(j) + (f(j + 1) - f(j)) * fy;
                            if v > 0.0 && y >= MIN_CARVE_Y && y < guard {
                                cave[at] = true;
                                carved += 1;
                                kind[at] = match g.cave_fill(x0 + lx, y, z0 + lz, col) {
                                    CaveFill::Air => 1,
                                    CaveFill::Water => {
                                        water += 1;
                                        2
                                    }
                                    CaveFill::Lava => 3,
                                };
                            }
                        }
                    }
                }
                let at = |lx: i32, lz: i32, y: usize| ((lz * N + lx) as usize) * ny + y;
                for lz in 0..N {
                    for lx in 0..N {
                        for y in 1..ny {
                            let a = at(lx, lz, y);
                            if !(cave[a] && kind[a] == 2) {
                                continue;
                            }
                            if kind[at(lx, lz, y - 1)] == 1 {
                                over += 1;
                            }
                            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                                let (bx, bz) = (lx + dx, lz + dz);
                                if bx < 0 || bz < 0 || bx >= N || bz >= N {
                                    continue;
                                }
                                if kind[at(bx, bz, y)] == 1 {
                                    side += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        eprintln!("carved {carved}, cave water {water}, water faces touching air: side {side}, over air {over}");
        assert!(water > 2000, "test regions hold too little cave water ({water})");
        assert!(side as f64 <= 1e-3 * water as f64, "{side} side faces");
        assert_eq!(over, 0);
    }

    #[test]
    fn field_performance() {
        let g = WorldGen::new(1);
        let cols: Vec<ColumnInfo> = (0..64).map(|i| g.column(i as f64 * 37.5 + 100.0, i as f64 * -23.5)).collect();
        let n = 400_000usize;
        let t0 = std::time::Instant::now();
        let mut acc = 0.0f32;
        for i in 0..n {
            let c = &cols[i % 64];
            let y = (i / 64 % 60) as f64 * 4.0;
            acc += g.cave_field(c.x + (i / 3840) as f64 * 4.0, y, c.z, c);
        }
        let ns = t0.elapsed().as_secs_f64() * 1e9 / n as f64;
        eprintln!("cave_field: {ns:.0} ns/eval (checksum {acc})");
        let t0 = std::time::Instant::now();
        let mut k = 0u32;
        for i in 0..n {
            let c = &cols[i % 64];
            k += g.cave_fill(i as i32, (i % 200) as i32, (i / 200) as i32, c) as u32;
        }
        let ns2 = t0.elapsed().as_secs_f64() * 1e9 / n as f64;
        eprintln!("cave_fill: {ns2:.0} ns/eval ({k})");
        assert!(ns < 3000.0);
    }
}
