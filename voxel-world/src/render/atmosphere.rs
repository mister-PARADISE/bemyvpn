//! Physically based atmosphere: single scattering (Rayleigh + Mie + ozone absorption) integrated
//! numerically on a spherical planet, with Hillaire's (2020) multiple-scattering approximation.
//!
//! Model (all lengths in metres, altitude h above sea level = world y − SEA_LEVEL):
//! * densities ρ_R = e^{−h/8000}, ρ_M = k_M e^{−h/1200} + k_F e^{−h/H_F} (aerosols + low "valley
//!   haze" layer, both scaled by `haze`), ozone tent ρ_O = max(0, 1 − |h − 25 km| / 15 km);
//! * extinction σ_t = β_R ρ_R + β_M^e ρ_M + β_O ρ_O, scattering σ_s = β_R ρ_R + β_M^s ρ_M;
//! * phases: Rayleigh 3/(16π)(1+c²), Mie Cornette–Shanks with g = 0.8.
//!
//! Precomputed per frame:
//! * **Transmittance LUT** T(r, μ) to the top of the atmosphere (Bruneton parameterisation);
//! * **Multiple scattering LUT** Ψ(h, μ_s) = L₂ / (1 − f_ms) (isotropic 2nd order + geometric series);
//! * **Sky-view / aerial-perspective LUT** over (relative sun azimuth φ, elevation e, distance t):
//!   the in-scatter integrals S_R = ∫ T σ_R T_sun dt, S_M = ∫ T σ_M T_sun dt, S_ms = ∫ T σ_s Ψ dt and
//!   the view transmittance T(t). Because the sun is at infinity the phase functions are constant
//!   along a ray, so L_in = E_sun (p_R(θ) S_R + p_M(θ) S_M + S_ms): phases are applied *per pixel*
//!   (sharp Mie halo) while the LUT only stores smooth quantities. Elevation rows are distributed
//!   ∝ (e − e_horizon)² around the planet's horizon; distance slices ∝ t² up to `max_distance`,
//!   plus one slice at infinity (top of atmosphere or ground).
//! * **Irradiance**: sky radiance projected onto order-2 spherical harmonics and convolved with the
//!   clamped cosine (Ramamoorthi–Hanrahan): E(n) = Σ Â_l L_lm Y_lm(n).
use crate::math::{smoothstep, Vec3};
use std::f64::consts::PI;

pub const RG: f64 = 6_360_000.0;
pub const RT: f64 = 6_460_000.0;
const BETA_R: [f64; 3] = [5.802e-6, 13.558e-6, 33.1e-6];
const HR: f64 = 8000.0;
const BETA_MS: f64 = 3.996e-6;
const BETA_ME: f64 = 4.40e-6;
const HM: f64 = 1200.0;
const BETA_O: [f64; 3] = [0.650e-6, 1.881e-6, 0.085e-6];
pub const MIE_G: f32 = 0.8;
/// Sun illuminance at the top of the atmosphere (scene units; noon ground ≈ 10).
pub const E_SUN: f32 = 11.5;
/// Angular radius of the drawn sun disk (radians).
pub const SUN_RADIUS: f32 = 0.0062;

#[derive(Clone, Copy, Debug)]
pub struct Medium {
    pub mie_k: f64,
    pub fog_k: f64,
    pub fog_h: f64,
}

impl Medium {
    pub fn new(haze: f32) -> Medium {
        let hz = haze.max(0.0) as f64;
        Medium { mie_k: 3.0 * hz, fog_k: 2.2 * hz, fog_h: 400.0 }
    }
    /// (σ_s Rayleigh RGB, σ_s Mie, σ_t RGB) at altitude h.
    #[inline]
    pub fn coeffs(&self, h: f64) -> ([f64; 3], f64, [f64; 3]) {
        let h = h.max(0.0);
        let rr = (-h / HR).exp();
        let rm = self.mie_k * (-h / HM).exp() + self.fog_k * (-h / self.fog_h).exp();
        let ro = (1.0 - (h - 25_000.0).abs() / 15_000.0).max(0.0);
        let sr = [BETA_R[0] * rr, BETA_R[1] * rr, BETA_R[2] * rr];
        let sm = BETA_MS * rm;
        let me = BETA_ME * rm;
        let st = [sr[0] + me + BETA_O[0] * ro, sr[1] + me + BETA_O[1] * ro, sr[2] + me + BETA_O[2] * ro];
        (sr, sm, st)
    }
}

#[inline]
pub fn phase_rayleigh(c: f32) -> f32 {
    3.0 / (16.0 * std::f32::consts::PI) * (1.0 + c * c)
}

#[inline]
pub fn phase_mie(c: f32, g: f32) -> f32 {
    let g2 = g * g;
    let k = 3.0 / (8.0 * std::f32::consts::PI) * (1.0 - g2) / (2.0 + g2);
    k * (1.0 + c * c) / (1.0 + g2 - 2.0 * g * c).max(1e-6).powf(1.5)
}

/// Distance from radius r along direction cosine μ to the sphere of radius R (outer intersection).
#[inline]
fn dist_to_sphere(r: f64, mu: f64, rr: f64) -> f64 {
    let disc = r * r * (mu * mu - 1.0) + rr * rr;
    (-r * mu + disc.max(0.0).sqrt()).max(0.0)
}

/// Distance to the ground along (r, μ) or None if the ray misses the planet.
#[inline]
fn dist_to_ground(r: f64, mu: f64) -> Option<f64> {
    if mu >= 0.0 {
        return None;
    }
    let disc = r * r * (mu * mu - 1.0) + RG * RG;
    if disc < 0.0 {
        return None;
    }
    let d = -r * mu - disc.sqrt();
    if d >= 0.0 {
        Some(d)
    } else {
        None
    }
}

// ------------------------------------------------------------------ transmittance LUT

const TW: usize = 256; // μ
const TH: usize = 128; // r

pub struct TransmittanceLut {
    data: Vec<[f32; 3]>,
}

impl TransmittanceLut {
    fn h() -> f64 {
        (RT * RT - RG * RG).sqrt()
    }
    pub fn new(med: &Medium) -> Self {
        let hh = Self::h();
        let mut data = vec![[0.0f32; 3]; TW * TH];
        for y in 0..TH {
            let xr = y as f64 / (TH - 1) as f64;
            let rho = hh * xr;
            let r = (rho * rho + RG * RG).sqrt();
            for x in 0..TW {
                let xm = x as f64 / (TW - 1) as f64;
                let dmin = RT - r;
                let dmax = rho + hh;
                let d = dmin + xm * (dmax - dmin);
                let mu = if d == 0.0 { 1.0 } else { ((hh * hh - rho * rho - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0) };
                let n = 64;
                let dt = d / n as f64;
                let mut od = [0.0f64; 3];
                for i in 0..n {
                    let t = (i as f64 + 0.5) * dt;
                    let ri = (t * t + 2.0 * r * mu * t + r * r).sqrt();
                    let (_, _, st) = med.coeffs(ri - RG);
                    for c in 0..3 {
                        od[c] += st[c] * dt;
                    }
                }
                data[y * TW + x] = [(-od[0]).exp() as f32, (-od[1]).exp() as f32, (-od[2]).exp() as f32];
            }
        }
        TransmittanceLut { data }
    }

    /// Transmittance from radius r toward the top of the atmosphere along μ (no ground test).
    pub fn lookup(&self, r: f64, mu: f64) -> [f32; 3] {
        let hh = Self::h();
        let r = r.clamp(RG, RT);
        let rho = (r * r - RG * RG).max(0.0).sqrt();
        let d = dist_to_sphere(r, mu, RT);
        let dmin = RT - r;
        let dmax = rho + hh;
        let xm = if dmax > dmin { (d - dmin) / (dmax - dmin) } else { 0.0 };
        let xr = rho / hh;
        let fx = (xm.clamp(0.0, 1.0) * (TW - 1) as f64) as f32;
        let fy = (xr.clamp(0.0, 1.0) * (TH - 1) as f64) as f32;
        let x0 = (fx as usize).min(TW - 2);
        let y0 = (fy as usize).min(TH - 2);
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x: usize, y: usize| self.data[y * TW + x];
        let (a, b, c, e) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
        let mut out = [0.0f32; 3];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * ax;
            let bot = c[k] + (e[k] - c[k]) * ax;
            out[k] = top + (bot - top) * ay;
        }
        out
    }

    /// Sun transmittance at radius r with sun zenith cosine μ_s, including the planet's shadow
    /// (smoothly over the sun's angular diameter).
    pub fn sun(&self, r: f64, mu_s: f64) -> [f32; 3] {
        let sin_h = RG / r.max(RG);
        let cos_h = -(1.0 - sin_h * sin_h).max(0.0).sqrt();
        let vis = smoothstep(-(SUN_RADIUS as f32), SUN_RADIUS as f32, (mu_s - cos_h) as f32);
        if vis <= 0.0 {
            return [0.0; 3];
        }
        let t = self.lookup(r, mu_s.max(cos_h));
        [t[0] * vis, t[1] * vis, t[2] * vis]
    }
}

// ------------------------------------------------------------------ multiple scattering LUT

const MS_N: usize = 32;

pub struct MultiScatLut {
    data: Vec<[f32; 3]>,
}

impl MultiScatLut {
    pub fn new(med: &Medium, tl: &TransmittanceLut, ground_albedo: [f32; 3]) -> Self {
        let mut data = vec![[0.0f32; 3]; MS_N * MS_N];
        // 64 directions on a Fibonacci sphere.
        let nd = 64usize;
        let dirs: Vec<[f64; 3]> = (0..nd)
            .map(|i| {
                let y = 1.0 - 2.0 * (i as f64 + 0.5) / nd as f64;
                let rr = (1.0 - y * y).sqrt();
                let a = i as f64 * PI * (3.0 - 5f64.sqrt());
                [rr * a.cos(), y, rr * a.sin()]
            })
            .collect();
        for iy in 0..MS_N {
            let h = (iy as f64 / (MS_N - 1) as f64).powi(2) * (RT - RG - 100.0) + 1.0;
            let r = RG + h;
            for ix in 0..MS_N {
                let mu_s = -1.0 + 2.0 * ix as f64 / (MS_N - 1) as f64;
                let sun = [(1.0 - mu_s * mu_s).max(0.0).sqrt(), mu_s, 0.0];
                let mut l2 = [0.0f64; 3];
                let mut fms = [0.0f64; 3];
                for d in &dirs {
                    let mu = d[1];
                    let ground = dist_to_ground(r, mu);
                    let tmax = ground.unwrap_or_else(|| dist_to_sphere(r, mu, RT));
                    let n = 20;
                    let dt = tmax / n as f64;
                    let mut tr = [1.0f64; 3];
                    for i in 0..n {
                        let t = (i as f64 + 0.5) * dt;
                        let p = [d[0] * t, r + d[1] * t, d[2] * t];
                        let rp = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                        let up = [p[0] / rp, p[1] / rp, p[2] / rp];
                        let ms = up[0] * sun[0] + up[1] * sun[1] + up[2] * sun[2];
                        let (sr, sm, st) = med.coeffs(rp - RG);
                        let ts = tl.sun(rp, ms);
                        for c in 0..3 {
                            let ss = sr[c] + sm;
                            let step_t = (-st[c] * dt).exp();
                            let integ = if st[c] > 0.0 { (1.0 - step_t) / st[c] } else { dt };
                            l2[c] += tr[c] * ss * ts[c] as f64 / (4.0 * PI) * integ;
                            fms[c] += tr[c] * ss * integ;
                            tr[c] *= step_t;
                        }
                    }
                    if let Some(tg) = ground {
                        let p = [d[0] * tg, r + d[1] * tg, d[2] * tg];
                        let rp = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                        let up = [p[0] / rp, p[1] / rp, p[2] / rp];
                        let ms = up[0] * sun[0] + up[1] * sun[1] + up[2] * sun[2];
                        let ts = tl.sun(rp, ms);
                        for c in 0..3 {
                            l2[c] += tr[c] * ts[c] as f64 * ms.max(0.0) * ground_albedo[c] as f64 / PI;
                        }
                    }
                }
                let mut out = [0.0f32; 3];
                for c in 0..3 {
                    let l = l2[c] / nd as f64;
                    // f_ms = ∫_{4π} ∫ T σ_s dt · p_u dω = mean over directions of ∫ T σ_s dt.
                    let f = fms[c] / nd as f64;
                    out[c] = (l / (1.0 - f).max(1e-3)) as f32;
                }
                data[iy * MS_N + ix] = out;
            }
        }
        MultiScatLut { data }
    }

    pub fn lookup(&self, h: f64, mu_s: f64) -> [f32; 3] {
        let fy = (((h - 1.0).max(0.0) / (RT - RG - 100.0)).sqrt().min(1.0) * (MS_N - 1) as f64) as f32;
        let fx = (((mu_s + 1.0) * 0.5).clamp(0.0, 1.0) * (MS_N - 1) as f64) as f32;
        let x0 = (fx as usize).min(MS_N - 2);
        let y0 = (fy as usize).min(MS_N - 2);
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x: usize, y: usize| self.data[y * MS_N + x];
        let (a, b, c, e) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
        let mut out = [0.0f32; 3];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * ax;
            let bot = c[k] + (e[k] - c[k]) * ax;
            out[k] = top + (bot - top) * ay;
        }
        out
    }
}

// ------------------------------------------------------------------ sky-view / aerial LUT

const NA: usize = 32;
const NE: usize = 96;
const ND: usize = 32;
const NS: usize = ND + 1; // + infinity

#[derive(Clone, Copy, Default)]
struct Entry {
    sr: [f32; 3],
    sm: [f32; 3],
    sms: [f32; 3],
    t: [f32; 3],
}

impl Entry {
    #[inline]
    fn madd(&mut self, o: &Entry, w: f32) {
        for k in 0..3 {
            self.sr[k] += o.sr[k] * w;
            self.sm[k] += o.sm[k] * w;
            self.sms[k] += o.sms[k] * w;
            self.t[k] += o.t[k] * w;
        }
    }
}

/// Result of an aerial-perspective query: L = L_surface · t + inscatter.
#[derive(Clone, Copy, Debug)]
pub struct Aerial {
    pub t: Vec3,
    pub inscatter: Vec3,
}

pub struct Atmosphere {
    pub medium: Medium,
    pub tl: TransmittanceLut,
    pub ms: MultiScatLut,
    lut: Vec<Entry>,
    /// Camera radius from planet centre.
    pub r_cam: f64,
    pub h_cam: f64,
    /// Elevation of the geometric horizon (negative).
    pub e_horizon: f64,
    pub max_dist: f64,
    pub sun: Vec3,
    sun_az: [f32; 2],
    /// Sun illuminance at the camera altitude (after transmittance).
    pub sun_e_cam: Vec3,
    sh: [Vec3; 9],
    /// Irradiance from the sky onto an upward facing surface (no sun).
    pub e_sky_up: Vec3,
    pub ground_albedo: Vec3,
    /// Uniform radiance of the (Lambertian) ground below the horizon, as baked into the SH
    /// irradiance: ρ_g (E_sun cos θ_s + E_sky↑) / π.
    pub l_ground: Vec3,
}

impl Atmosphere {
    /// `h_cam`: camera altitude above sea level (m); `sun`: unit vector toward the sun.
    pub fn new(h_cam: f64, sun: Vec3, haze: f32, max_dist: f64) -> Atmosphere {
        let medium = Medium::new(haze);
        let tl = TransmittanceLut::new(&medium);
        let ground_albedo = [0.11f32, 0.13, 0.09];
        let ms = MultiScatLut::new(&medium, &tl, ground_albedo);
        let h_cam = h_cam.max(0.5);
        let r_cam = RG + h_cam;
        let e_horizon = -(RG / r_cam).acos();
        let sxz = (sun.x * sun.x + sun.z * sun.z).sqrt();
        let sun_az = if sxz > 1e-6 { [sun.x / sxz, sun.z / sxz] } else { [0.0, 1.0] };
        let mut atm = Atmosphere {
            medium,
            tl,
            ms,
            lut: vec![Entry::default(); NA * NE * NS],
            r_cam,
            h_cam,
            e_horizon,
            max_dist: max_dist.max(100.0),
            sun,
            sun_az,
            sun_e_cam: Vec3::ZERO,
            sh: [Vec3::ZERO; 9],
            e_sky_up: Vec3::ZERO,
            ground_albedo: Vec3::from_array(ground_albedo),
            l_ground: Vec3::ZERO,
        };
        atm.sun_e_cam = atm.sun_illuminance(h_cam);
        atm.build_lut();
        atm.build_sh();
        atm
    }

    #[inline]
    fn elev_of_row(&self, v: f64) -> f64 {
        let eh = self.e_horizon;
        if v >= 0.5 {
            let s = (v - 0.5) * 2.0;
            eh + s * s * (PI / 2.0 - eh)
        } else {
            let s = (0.5 - v) * 2.0;
            eh - s * s * (PI / 2.0 + eh)
        }
    }

    #[inline]
    fn row_of_elev(&self, e: f64) -> f64 {
        let eh = self.e_horizon;
        if e >= eh {
            0.5 + 0.5 * ((e - eh) / (PI / 2.0 - eh)).max(0.0).sqrt()
        } else {
            0.5 - 0.5 * ((eh - e) / (PI / 2.0 + eh)).max(0.0).sqrt()
        }
    }

    fn build_lut(&mut self) {
        use rayon::prelude::*;
        let sun_e = self.sun.y.clamp(-1.0, 1.0) as f64;
        let sun_c = (1.0 - sun_e * sun_e).max(0.0).sqrt();
        // Frame where the sun has azimuth 0: sun = (0, sin e_s, cos e_s).
        let sun = [0.0, sun_e, sun_c];
        let r_cam = self.r_cam;
        let dmax = self.max_dist;
        let med = self.medium;
        let tl = &self.tl;
        let ms = &self.ms;
        let rows: Vec<Vec<Entry>> = (0..NA * NE)
            .into_par_iter()
            .map(|col| {
                let ia = col / NE;
                let ie = col % NE;
                let phi = PI * ia as f64 / (NA - 1) as f64;
                let e = self.elev_of_row(ie as f64 / (NE - 1) as f64);
                let d = [phi.sin() * e.cos(), e.sin(), phi.cos() * e.cos()];
                let mu = d[1];
                let ground = dist_to_ground(r_cam, mu);
                let tend = ground.unwrap_or_else(|| dist_to_sphere(r_cam, mu, RT));
                let mut out = vec![Entry::default(); NS];
                let mut acc = Entry { t: [1.0; 3], ..Default::default() };
                let mut tr = [1.0f64; 3];
                let mut sr = [0.0f64; 3];
                let mut sm = [0.0f64; 3];
                let mut sms = [0.0f64; 3];
                let march = |ta: f64, tb: f64, n: usize, tr: &mut [f64; 3], sr: &mut [f64; 3], sm: &mut [f64; 3], sms: &mut [f64; 3]| {
                    if tb <= ta {
                        return;
                    }
                    // Exponential spacing inside long segments.
                    for i in 0..n {
                        let fa = i as f64 / n as f64;
                        let fb = (i + 1) as f64 / n as f64;
                        let (t0, t1) = if tb - ta > 50_000.0 {
                            let k = (tb / ta.max(1.0)).ln();
                            (ta.max(1.0) * (k * fa).exp(), ta.max(1.0) * (k * fb).exp())
                        } else {
                            (ta + (tb - ta) * fa, ta + (tb - ta) * fb)
                        };
                        let dt = t1 - t0;
                        let t = 0.5 * (t0 + t1);
                        let p = [d[0] * t, r_cam + d[1] * t, d[2] * t];
                        let rp = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                        let up = [p[0] / rp, p[1] / rp, p[2] / rp];
                        let mus = up[0] * sun[0] + up[1] * sun[1] + up[2] * sun[2];
                        let (csr, csm, cst) = med.coeffs(rp - RG);
                        let ts = tl.sun(rp, mus);
                        let psi = ms.lookup(rp - RG, mus);
                        for c in 0..3 {
                            let step_t = (-cst[c] * dt).exp();
                            let integ = if cst[c] > 0.0 { (1.0 - step_t) / cst[c] } else { dt };
                            let w = tr[c] * integ;
                            sr[c] += w * csr[c] * ts[c] as f64;
                            sm[c] += w * csm * ts[c] as f64;
                            sms[c] += w * (csr[c] + csm) * psi[c] as f64;
                            tr[c] *= step_t;
                        }
                    }
                };
                let mut tprev = 0.0;
                for k in 0..ND {
                    let s = k as f64 / (ND - 1) as f64;
                    let tk = (dmax * s * s).min(tend);
                    march(tprev, tk, 3, &mut tr, &mut sr, &mut sm, &mut sms);
                    tprev = tprev.max(tk);
                    for c in 0..3 {
                        acc.sr[c] = sr[c] as f32;
                        acc.sm[c] = sm[c] as f32;
                        acc.sms[c] = sms[c] as f32;
                        acc.t[c] = tr[c] as f32;
                    }
                    out[k] = acc;
                }
                march(tprev, tend, 40, &mut tr, &mut sr, &mut sm, &mut sms);
                for c in 0..3 {
                    acc.sr[c] = sr[c] as f32;
                    acc.sm[c] = sm[c] as f32;
                    acc.sms[c] = sms[c] as f32;
                    acc.t[c] = tr[c] as f32;
                }
                out[ND] = acc;
                out
            })
            .collect();
        for (col, r) in rows.into_iter().enumerate() {
            let base = col * NS;
            self.lut[base..base + NS].copy_from_slice(&r);
        }
    }

    /// Relative azimuth coordinate and elevation coordinate of a direction (fractional indices).
    #[inline]
    fn coords(&self, d: Vec3) -> (f32, f32) {
        let hxz = (d.x * d.x + d.z * d.z).sqrt();
        let ca = if hxz > 1e-7 { (d.x * self.sun_az[0] + d.z * self.sun_az[1]) / hxz } else { 1.0 };
        let phi = ca.clamp(-1.0, 1.0).acos();
        let fa = phi / std::f32::consts::PI * (NA - 1) as f32;
        let e = (d.y.clamp(-1.0, 1.0) as f64).asin();
        let fe = (self.row_of_elev(e) * (NE - 1) as f64) as f32;
        (fa, fe)
    }

    #[inline]
    fn sample(&self, fa: f32, fe: f32, slice: usize, out: &mut Entry, w: f32) {
        let a0 = (fa.max(0.0) as usize).min(NA - 2);
        let e0 = (fe.max(0.0) as usize).min(NE - 2);
        let ax = (fa - a0 as f32).clamp(0.0, 1.0);
        let ex = (fe - e0 as f32).clamp(0.0, 1.0);
        let idx = |a: usize, e: usize| (a * NE + e) * NS + slice;
        out.madd(&self.lut[idx(a0, e0)], w * (1.0 - ax) * (1.0 - ex));
        out.madd(&self.lut[idx(a0 + 1, e0)], w * ax * (1.0 - ex));
        out.madd(&self.lut[idx(a0, e0 + 1)], w * (1.0 - ax) * ex);
        out.madd(&self.lut[idx(a0 + 1, e0 + 1)], w * ax * ex);
    }

    #[inline]
    fn resolve(&self, e: &Entry, d: Vec3) -> (Vec3, Vec3) {
        let c = d.dot(self.sun);
        let pr = phase_rayleigh(c);
        let pm = phase_mie(c, MIE_G);
        let l = Vec3::new(
            pr * e.sr[0] + pm * e.sm[0] + e.sms[0],
            pr * e.sr[1] + pm * e.sm[1] + e.sms[1],
            pr * e.sr[2] + pm * e.sm[2] + e.sms[2],
        ) * E_SUN;
        (Vec3::from_array(e.t), l)
    }

    /// Transmittance and in-scattered radiance along the camera ray `d` (unit) up to distance t.
    #[inline]
    pub fn aerial(&self, d: Vec3, t: f64) -> Aerial {
        let (fa, fe) = self.coords(d);
        let s = ((t / self.max_dist).max(0.0).sqrt() * (ND - 1) as f64).min((ND - 1) as f64) as f32;
        let s0 = (s as usize).min(ND - 2);
        let w = s - s0 as f32;
        let mut e = Entry::default();
        self.sample(fa, fe, s0, &mut e, 1.0 - w);
        self.sample(fa, fe, s0 + 1, &mut e, w);
        let (t, l) = self.resolve(&e, d);
        Aerial { t, inscatter: l }
    }

    /// Path to infinity (top of the atmosphere, or the ground for rays below the horizon).
    #[inline]
    pub fn infinite(&self, d: Vec3) -> Aerial {
        let (fa, fe) = self.coords(d);
        let mut e = Entry::default();
        self.sample(fa, fe, ND, &mut e, 1.0);
        let (t, l) = self.resolve(&e, d);
        Aerial { t, inscatter: l }
    }

    /// True if the ray from the camera along d hits the planet (sea-level sphere).
    #[inline]
    pub fn hits_ground(&self, d: Vec3) -> bool {
        dist_to_ground(self.r_cam, d.y as f64).is_some()
    }

    /// Sky radiance (no sun disk).
    #[inline]
    pub fn sky(&self, d: Vec3) -> Vec3 {
        self.infinite(d).inscatter
    }

    /// Radiance of the sun disk in direction d (0 outside the disk), with limb darkening.
    #[inline]
    pub fn sun_disk(&self, d: Vec3, t_inf: Vec3) -> Vec3 {
        let c = d.dot(self.sun);
        let cr = SUN_RADIUS.cos();
        if c < cr {
            return Vec3::ZERO;
        }
        let ang = c.min(1.0).acos();
        let x = (ang / SUN_RADIUS).min(1.0);
        let mu = (1.0 - x * x).sqrt();
        let limb = 1.0 - 0.6 * (1.0 - mu);
        let omega = std::f32::consts::PI * SUN_RADIUS * SUN_RADIUS * (1.0 - 0.6 / 3.0);
        t_inf * (E_SUN / omega * limb)
    }

    /// Sun illuminance (perpendicular) at world altitude h above sea level.
    #[inline]
    pub fn sun_illuminance(&self, h: f64) -> Vec3 {
        let r = RG + h.max(0.0);
        let t = self.tl.sun(r, self.sun.y as f64);
        Vec3::from_array(t) * E_SUN
    }

    /// Spherical-harmonic sky irradiance onto a surface with normal n (no direct sun).
    #[inline]
    pub fn irradiance(&self, n: Vec3) -> Vec3 {
        sh_irradiance(&self.sh, n)
    }

    fn build_sh(&mut self) {
        // Upper hemisphere: sky; lower: ground lit by sun + sky.
        let (nt, np) = (48usize, 96usize);
        let mut sh_up = [Vec3::ZERO; 9];
        let mut sh_lo = [0.0f32; 9];
        for it in 0..nt {
            let th = (it as f64 + 0.5) / nt as f64 * PI;
            let dw = (PI / nt as f64) * (2.0 * PI / np as f64) * th.sin();
            for ip in 0..np {
                let ph = (ip as f64 + 0.5) / np as f64 * 2.0 * PI;
                let d = Vec3::new((th.sin() * ph.cos()) as f32, th.cos() as f32, (th.sin() * ph.sin()) as f32);
                let y = sh_basis(d);
                if d.y >= 0.0 {
                    let l = self.sky(d);
                    for k in 0..9 {
                        sh_up[k] += l * (y[k] * dw as f32);
                    }
                } else {
                    for k in 0..9 {
                        sh_lo[k] += y[k] * dw as f32;
                    }
                }
            }
        }
        let e_up = sh_irradiance(&sh_up, Vec3::Y);
        let e_direct = self.sun_e_cam * self.sun.y.max(0.0);
        let l_ground = self.ground_albedo * (e_direct + e_up) * (1.0 / std::f32::consts::PI);
        let mut sh = sh_up;
        for k in 0..9 {
            sh[k] += l_ground * sh_lo[k];
        }
        self.sh = sh;
        self.e_sky_up = e_up;
        self.l_ground = l_ground;
    }
}

/// Real SH basis up to l = 2.
#[inline]
pub fn sh_basis(d: Vec3) -> [f32; 9] {
    let (x, y, z) = (d.x, d.y, d.z);
    [
        0.282_095,
        0.488_603 * y,
        0.488_603 * z,
        0.488_603 * x,
        1.092_548 * x * y,
        1.092_548 * y * z,
        0.315_392 * (3.0 * z * z - 1.0),
        1.092_548 * x * z,
        0.546_274 * (x * x - y * y),
    ]
}

/// Irradiance from SH radiance coefficients (cosine-lobe convolution, Â = π, 2π/3, π/4).
#[inline]
pub fn sh_irradiance(sh: &[Vec3; 9], n: Vec3) -> Vec3 {
    let y = sh_basis(n);
    let a = [
        std::f32::consts::PI,
        2.094_395,
        2.094_395,
        2.094_395,
        0.785_398,
        0.785_398,
        0.785_398,
        0.785_398,
        0.785_398,
    ];
    let mut e = Vec3::ZERO;
    for k in 0..9 {
        e += sh[k] * (a[k] * y[k]);
    }
    e.max(Vec3::ZERO)
}

/// Project an arbitrary radiance function onto SH (used for the studio environment).
pub fn sh_project<F: Fn(Vec3) -> Vec3>(f: F) -> [Vec3; 9] {
    let (nt, np) = (32usize, 64usize);
    let mut sh = [Vec3::ZERO; 9];
    for it in 0..nt {
        let th = (it as f64 + 0.5) / nt as f64 * PI;
        let dw = ((PI / nt as f64) * (2.0 * PI / np as f64) * th.sin()) as f32;
        for ip in 0..np {
            let ph = (ip as f64 + 0.5) / np as f64 * 2.0 * PI;
            let d = Vec3::new((th.sin() * ph.cos()) as f32, th.cos() as f32, (th.sin() * ph.sin()) as f32);
            let l = f(d);
            let y = sh_basis(d);
            for k in 0..9 {
                sh[k] += l * (y[k] * dw);
            }
        }
    }
    sh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_is_blue_and_sunset_is_red() {
        let noon = Atmosphere::new(200.0, Vec3::new(0.0, 0.9, 0.436).normalize(), 1.0, 40_000.0);
        let zen = noon.sky(Vec3::Y);
        assert!(zen.z > zen.x * 1.5, "zenith should be blue: {:?}", zen);
        let set = Atmosphere::new(200.0, Vec3::new(0.0, 0.05, 1.0).normalize(), 1.0, 40_000.0);
        let e = set.sun_illuminance(0.0);
        assert!(e.x > e.z * 2.0, "sunset sun should be red: {:?}", e);
        let noon_e = noon.sun_illuminance(0.0);
        assert!(noon_e.y > 8.0 && noon_e.y < 11.5, "noon sun {:?}", noon_e);
        // Aerial perspective: transmittance decreases monotonically with distance.
        let d = Vec3::new(0.3, -0.02, 1.0).normalize();
        let mut prev = 1.01;
        for k in 0..20 {
            let a = noon.aerial(d, k as f64 * 2000.0);
            assert!(a.t.y <= prev + 1e-4);
            prev = a.t.y;
        }
        assert!(prev < 0.8 && prev > 0.05, "T(38km)={}", prev);
        // SH: both hemispheres positive; sky light is bluer than ground bounce.
        let up = noon.irradiance(Vec3::Y);
        let dn = noon.irradiance(-Vec3::Y);
        assert!(up.y > 0.0 && dn.y > 0.0);
        assert!(up.z / up.x > dn.z / dn.x);
    }
}

#[cfg(test)]
mod sh_tests {
    use super::*;

    /// The order-2 SH irradiance must match a direct quadrature of ∫ L cos θ dω (sky above the
    /// horizon, l_ground below) for every normal, also for a strongly anisotropic sunset sky.
    #[test]
    fn sh_irradiance_matches_quadrature() {
        for (sun_el, tol) in [(0.9f32, 0.06f32), (0.08, 0.10)] {
            let s = Vec3::new(0.3, sun_el, 0.8).normalize();
            let atm = Atmosphere::new(150.0, s, 1.0, 40_000.0);
            let env = |d: Vec3| if d.y >= 0.0 { atm.sky(d) } else { atm.l_ground };
            for n in [Vec3::Y, Vec3::X, -Vec3::Z, Vec3::new(0.5, 0.6, -0.4).normalize(), s, Vec3::new(-s.x, 0.0, -s.z).normalize()] {
                let (nt, np) = (180usize, 360usize);
                let mut e = Vec3::ZERO;
                for it in 0..nt {
                    let th = (it as f64 + 0.5) / nt as f64 * PI;
                    let dw = ((PI / nt as f64) * (2.0 * PI / np as f64) * th.sin()) as f32;
                    for ip in 0..np {
                        let ph = (ip as f64 + 0.5) / np as f64 * 2.0 * PI;
                        let d = Vec3::new((th.sin() * ph.cos()) as f32, th.cos() as f32, (th.sin() * ph.sin()) as f32);
                        let c = d.dot(n);
                        if c > 0.0 {
                            e += env(d) * (c * dw);
                        }
                    }
                }
                let sh = atm.irradiance(n);
                let rel = (sh.luminance() - e.luminance()).abs() / e.luminance();
                eprintln!("sun {sun_el} n {:?}: quad {:?} sh {:?} rel {:.3}", n, e, sh, rel);
                assert!(rel < tol, "SH irradiance off by {rel}");
            }
        }
    }
}
