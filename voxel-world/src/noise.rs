//! Deterministic hashing + gradient noise toolkit (pure math, no tables shared across seeds).
//! FROZEN shared file: agents use it, but do not change existing signatures/behaviour.
//!
//! Contents
//! * `mix64`, `hash2`, `hash3`, `derive_seed`, `salt` — integer hashing (SplitMix64 finalizer).
//! * `Simplex` — 2D simplex noise with analytic derivatives, 3D simplex noise. Output ≈ [-1, 1].
//! * `Fbm` — fractional Brownian motion (2D/3D), per-octave seeds + rotation, band-limited LOD sampling.
//! * `Ridged` — Musgrave ridged multifractal (sharp crests), output ≈ [0, 1].
//! * `ErodedFbm` — derivative-damped fBm (slopes suppress fine detail → erosion-like look).
//! * `Warp2` — 2D domain warping.
//! * `Worley2` — cellular (Voronoi) noise: F1/F2 distances, owning cell id + feature point.

use std::sync::OnceLock;

// ---------------------------------------------------------------- hashing

/// SplitMix64 finalizer: strong 64-bit avalanche mixer.
#[inline]
pub fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

const K1: u64 = 0x9E37_79B9_7F4A_7C15;
const K2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const K3: u64 = 0x1656_67B1_9E37_79F9;

/// Derive an independent sub-seed.
#[inline]
pub fn derive_seed(seed: u64, salt: u64) -> u64 {
    mix64(seed ^ mix64(salt.wrapping_add(K1)))
}

/// FNV-1a hash of a string — handy for `derive_seed(seed, salt("continentalness"))`.
pub fn salt(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[inline]
pub fn hash2(seed: u64, x: i32, z: i32) -> u64 {
    mix64(
        seed.wrapping_add((x as i64 as u64).wrapping_mul(K1))
            .wrapping_add((z as i64 as u64).wrapping_mul(K2)),
    )
}

#[inline]
pub fn hash3(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    mix64(
        seed.wrapping_add((x as i64 as u64).wrapping_mul(K1))
            .wrapping_add((y as i64 as u64).wrapping_mul(K3))
            .wrapping_add((z as i64 as u64).wrapping_mul(K2)),
    )
}

/// Uniform float in [0, 1) from a hash (uses the high 24 bits).
#[inline]
pub fn hash_f01(h: u64) -> f32 {
    (h >> 40) as f32 * (1.0 / 16_777_216.0)
}

/// Uniform float in [-1, 1) from a hash.
#[inline]
pub fn hash_f11(h: u64) -> f32 {
    hash_f01(h) * 2.0 - 1.0
}

// ---------------------------------------------------------------- gradients

fn grad2_table() -> &'static [[f32; 2]; 64] {
    static T: OnceLock<[[f32; 2]; 64]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0.0f32; 2]; 64];
        for (i, g) in t.iter_mut().enumerate() {
            // Offset by half a step so no gradient is axis aligned.
            let a = (i as f64 + 0.5) * std::f64::consts::TAU / 64.0;
            *g = [a.cos() as f32, a.sin() as f32];
        }
        t
    })
}

/// 3D gradients: the 12 cube-edge midpoints (normalized) + 4 repeats, classic simplex set.
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

// ---------------------------------------------------------------- simplex

const F2: f64 = 0.366_025_403_784_438_6; // (sqrt(3)-1)/2
const G2: f64 = 0.211_324_865_405_187_1; // (3-sqrt(3))/6
const F3: f64 = 1.0 / 3.0;
const G3: f64 = 1.0 / 6.0;

/// Empirically measured normalisation constants (see test `simplex_range`):
/// scale so that the observed maximum |value| over many samples ≈ 1.
const SCALE2: f32 = 99.83;
const SCALE3: f32 = 106.0;

#[derive(Clone, Copy, Debug)]
pub struct Simplex {
    seed: u64,
}

impl Simplex {
    pub fn new(seed: u64) -> Self {
        Simplex { seed: mix64(seed ^ 0x5151_5151) }
    }

    /// 2D simplex noise, ≈ [-1, 1].
    #[inline]
    pub fn n2(&self, x: f64, y: f64) -> f32 {
        self.n2d(x, y).0
    }

    /// 2D simplex noise with analytic gradient: (value, d/dx, d/dy).
    /// Derivatives are with respect to the *input* coordinates.
    #[inline]
    pub fn n2d(&self, x: f64, y: f64) -> (f32, f32, f32) {
        let s = (x + y) * F2;
        let i = (x + s).floor();
        let j = (y + s).floor();
        let t = (i + j) * G2;
        let x0 = (x - (i - t)) as f32;
        let y0 = (y - (j - t)) as f32;
        let (i1, j1) = if x0 > y0 { (1i32, 0i32) } else { (0i32, 1i32) };
        let g2 = G2 as f32;
        let x1 = x0 - i1 as f32 + g2;
        let y1 = y0 - j1 as f32 + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;
        let ii = i as i64 as i32;
        let jj = j as i64 as i32;
        let tab = grad2_table();

        let mut v = 0.0f32;
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        let mut corner = |cx: f32, cy: f32, h: u64| {
            let tt = 0.5 - cx * cx - cy * cy;
            if tt > 0.0 {
                let g = tab[(h & 63) as usize];
                let gd = g[0] * cx + g[1] * cy;
                let t2 = tt * tt;
                let t3 = t2 * tt;
                let t4 = t2 * t2;
                v += t4 * gd;
                dx += -8.0 * cx * t3 * gd + t4 * g[0];
                dy += -8.0 * cy * t3 * gd + t4 * g[1];
            }
        };
        corner(x0, y0, hash2(self.seed, ii, jj));
        corner(x1, y1, hash2(self.seed, ii + i1, jj + j1));
        corner(x2, y2, hash2(self.seed, ii + 1, jj + 1));
        (v * SCALE2, dx * SCALE2, dy * SCALE2)
    }

    /// 3D simplex noise, ≈ [-1, 1]. Kernel radius² = 0.5 (fully continuous).
    #[inline]
    pub fn n3(&self, x: f64, y: f64, z: f64) -> f32 {
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
        let x1 = x0 - i1 as f32 + g3;
        let y1 = y0 - j1 as f32 + g3;
        let z1 = z0 - k1 as f32 + g3;
        let x2 = x0 - i2 as f32 + 2.0 * g3;
        let y2 = y0 - j2 as f32 + 2.0 * g3;
        let z2 = z0 - k2 as f32 + 2.0 * g3;
        let x3 = x0 - 1.0 + 3.0 * g3;
        let y3 = y0 - 1.0 + 3.0 * g3;
        let z3 = z0 - 1.0 + 3.0 * g3;
        let ii = i as i64 as i32;
        let jj = j as i64 as i32;
        let kk = k as i64 as i32;

        #[inline(always)]
        fn c3(x: f32, y: f32, z: f32, h: u64) -> f32 {
            let t = 0.5 - x * x - y * y - z * z;
            if t <= 0.0 {
                return 0.0;
            }
            let g = GRAD3[(h & 15) as usize];
            let t2 = t * t;
            t2 * t2 * (g[0] * x + g[1] * y + g[2] * z)
        }
        let n = c3(x0, y0, z0, hash3(self.seed, ii, jj, kk))
            + c3(x1, y1, z1, hash3(self.seed, ii + i1, jj + j1, kk + k1))
            + c3(x2, y2, z2, hash3(self.seed, ii + i2, jj + j2, kk + k2))
            + c3(x3, y3, z3, hash3(self.seed, ii + 1, jj + 1, kk + 1));
        n * SCALE3
    }
}

// ---------------------------------------------------------------- fBm

/// Fixed per-octave rotation angle (radians) — irrational w.r.t. the lattice, breaks axis alignment.
const OCT_ROT: f64 = 0.618_033_988_749_894_8 * 1.7;

#[derive(Clone, Debug)]
pub struct Fbm {
    pub wavelength: f64,
    pub octaves: u32,
    pub lacunarity: f64,
    pub gain: f32,
    layers: Vec<Simplex>,
    rot: Vec<(f64, f64)>, // (cos, sin) per octave
    amp_sum: f32,
}

impl Fbm {
    /// `wavelength` in blocks of the first (largest) octave.
    pub fn new(seed: u64, wavelength: f64, octaves: u32, lacunarity: f64, gain: f32) -> Self {
        let layers = (0..octaves)
            .map(|i| Simplex::new(derive_seed(seed, i as u64 + 1)))
            .collect();
        let rot = (0..octaves)
            .map(|i| {
                let a = OCT_ROT * i as f64;
                (a.cos(), a.sin())
            })
            .collect();
        let mut amp_sum = 0.0;
        let mut a = 1.0f32;
        for _ in 0..octaves {
            amp_sum += a;
            a *= gain;
        }
        Fbm { wavelength, octaves, lacunarity, gain, layers, rot, amp_sum }
    }

    /// 2D fBm normalised by the amplitude sum: range ⊂ [-1, 1], typical std ≈ 0.2–0.3.
    #[inline]
    pub fn sample2(&self, x: f64, z: f64) -> f32 {
        let mut f = 1.0 / self.wavelength;
        let mut a = 1.0f32;
        let mut sum = 0.0f32;
        for (i, l) in self.layers.iter().enumerate() {
            let (c, s) = self.rot[i];
            let (px, pz) = (x * c - z * s, x * s + z * c);
            sum += a * l.n2(px * f, pz * f);
            f *= self.lacunarity;
            a *= self.gain;
        }
        sum / self.amp_sum
    }

    /// Band-limited 2D fBm: octaves whose wavelength is below ~2×`footprint` (blocks per sample)
    /// are smoothly faded out (anti-aliasing for far LOD). `footprint <= 1` ≈ `sample2`.
    #[inline]
    pub fn sample2_lod(&self, x: f64, z: f64, footprint: f64) -> f32 {
        let mut f = 1.0 / self.wavelength;
        let mut a = 1.0f32;
        let mut sum = 0.0f32;
        for (i, l) in self.layers.iter().enumerate() {
            let lambda = 1.0 / f;
            let w = (((lambda / footprint.max(1e-9)) - 2.0) * 0.5).clamp(0.0, 1.0) as f32;
            if w <= 0.0 {
                break;
            }
            let (c, s) = self.rot[i];
            let (px, pz) = (x * c - z * s, x * s + z * c);
            sum += w * a * l.n2(px * f, pz * f);
            f *= self.lacunarity;
            a *= self.gain;
        }
        sum / self.amp_sum
    }

    /// 3D fBm normalised by amplitude sum (rotation applied in the xz-plane).
    #[inline]
    pub fn sample3(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut f = 1.0 / self.wavelength;
        let mut a = 1.0f32;
        let mut sum = 0.0f32;
        for (i, l) in self.layers.iter().enumerate() {
            let (c, s) = self.rot[i];
            let (px, pz) = (x * c - z * s, x * s + z * c);
            sum += a * l.n3(px * f, y * f, pz * f);
            f *= self.lacunarity;
            a *= self.gain;
        }
        sum / self.amp_sum
    }
}

// ---------------------------------------------------------------- ridged multifractal

/// Musgrave's ridged multifractal. Output ≈ [0, 1], sharp crests where the base noise crosses 0.
#[derive(Clone, Debug)]
pub struct Ridged {
    pub wavelength: f64,
    pub octaves: u32,
    pub lacunarity: f64,
    pub gain: f32,
    pub offset: f32,
    layers: Vec<Simplex>,
    rot: Vec<(f64, f64)>,
    norm: f32,
}

impl Ridged {
    pub fn new(seed: u64, wavelength: f64, octaves: u32) -> Self {
        Self::with_params(seed, wavelength, octaves, 2.0, 2.0, 1.0)
    }

    /// `gain` controls how strongly crests feed detail into the next octave (Musgrave uses 2.0).
    pub fn with_params(seed: u64, wavelength: f64, octaves: u32, lacunarity: f64, gain: f32, offset: f32) -> Self {
        let layers = (0..octaves)
            .map(|i| Simplex::new(derive_seed(seed, 0x77 + i as u64)))
            .collect();
        let rot = (0..octaves)
            .map(|i| {
                let a = OCT_ROT * i as f64 + 0.3;
                (a.cos(), a.sin())
            })
            .collect();
        // Spectral weights w_i = lacunarity^(-H*i) with H = 1.
        let mut norm = 0.0;
        let mut w = 1.0f32;
        for _ in 0..octaves {
            norm += w * offset * offset;
            w /= lacunarity as f32;
        }
        Ridged { wavelength, octaves, lacunarity, gain, offset, layers, rot, norm }
    }

    #[inline]
    pub fn sample2(&self, x: f64, z: f64) -> f32 {
        self.sample2_lod(x, z, 0.0)
    }

    /// Band-limited variant (see `Fbm::sample2_lod`).
    #[inline]
    pub fn sample2_lod(&self, x: f64, z: f64, footprint: f64) -> f32 {
        let mut f = 1.0 / self.wavelength;
        let mut spectral = 1.0f32;
        let mut weight = 1.0f32;
        let mut sum = 0.0f32;
        for (i, l) in self.layers.iter().enumerate() {
            let lambda = 1.0 / f;
            let fade = if footprint > 0.0 {
                (((lambda / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32
            } else {
                1.0
            };
            if fade <= 0.0 {
                break;
            }
            let (c, s) = self.rot[i];
            let (px, pz) = (x * c - z * s, x * s + z * c);
            let mut sig = self.offset - l.n2(px * f, pz * f).abs();
            sig *= sig;
            sig *= weight;
            weight = (sig * self.gain).clamp(0.0, 1.0);
            sum += fade * sig * spectral;
            spectral /= self.lacunarity as f32;
            f *= self.lacunarity;
        }
        (sum / self.norm).clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------- eroded (derivative-damped) fBm

/// "Erosion" fBm: each octave's contribution is divided by (1 + k·|Σ∇|²), where Σ∇ is the running
/// sum of noise gradients. Steep areas lose fine detail, flat areas keep it → plausible
/// ridges/gullies without simulation. Output ≈ [-1, 1].
#[derive(Clone, Debug)]
pub struct ErodedFbm {
    pub wavelength: f64,
    pub octaves: u32,
    pub gain: f32,
    pub steepness: f32,
    layers: Vec<Simplex>,
    rot: Vec<(f64, f64)>,
    amp_sum: f32,
}

impl ErodedFbm {
    pub fn new(seed: u64, wavelength: f64, octaves: u32, gain: f32, steepness: f32) -> Self {
        let layers = (0..octaves)
            .map(|i| Simplex::new(derive_seed(seed, 0xE0 + i as u64)))
            .collect();
        let rot = (0..octaves)
            .map(|i| {
                let a = OCT_ROT * i as f64 + 1.1;
                (a.cos(), a.sin())
            })
            .collect();
        let mut amp_sum = 0.0;
        let mut a = 1.0f32;
        for _ in 0..octaves {
            amp_sum += a;
            a *= gain;
        }
        ErodedFbm { wavelength, octaves, gain, steepness, layers, rot, amp_sum }
    }

    #[inline]
    pub fn sample2(&self, x: f64, z: f64) -> f32 {
        self.sample2_lod(x, z, 0.0)
    }

    #[inline]
    pub fn sample2_lod(&self, x: f64, z: f64, footprint: f64) -> f32 {
        let mut f = 1.0 / self.wavelength;
        let mut a = 1.0f32;
        let mut sum = 0.0f32;
        let (mut gx, mut gz) = (0.0f32, 0.0f32);
        for (i, l) in self.layers.iter().enumerate() {
            let lambda = 1.0 / f;
            let fade = if footprint > 0.0 {
                (((lambda / footprint) - 2.0) * 0.5).clamp(0.0, 1.0) as f32
            } else {
                1.0
            };
            if fade <= 0.0 {
                break;
            }
            let (c, s) = self.rot[i];
            let (px, pz) = (x * c - z * s, x * s + z * c);
            let (n, dx, dz) = l.n2d(px * f, pz * f);
            // Rotate the derivative back into world orientation (keeps the sum coherent).
            let wdx = dx * c as f32 + dz * s as f32;
            let wdz = -dx * s as f32 + dz * c as f32;
            gx += wdx * a;
            gz += wdz * a;
            sum += fade * a * n / (1.0 + self.steepness * (gx * gx + gz * gz));
            a *= self.gain;
            f *= 2.0;
        }
        sum / self.amp_sum
    }
}

// ---------------------------------------------------------------- domain warp

#[derive(Clone, Debug)]
pub struct Warp2 {
    pub amplitude: f64,
    fx: Fbm,
    fz: Fbm,
}

impl Warp2 {
    pub fn new(seed: u64, wavelength: f64, octaves: u32, amplitude: f64) -> Self {
        Warp2 {
            amplitude,
            fx: Fbm::new(derive_seed(seed, 0xA1), wavelength, octaves, 2.0, 0.5),
            fz: Fbm::new(derive_seed(seed, 0xA2), wavelength, octaves, 2.0, 0.5),
        }
    }
    /// Returns warped coordinates (x + A·fx, z + A·fz). fBm values are rescaled ×2 so
    /// the displacement typically spans ±amplitude/2.
    #[inline]
    pub fn warp(&self, x: f64, z: f64) -> (f64, f64) {
        let dx = self.fx.sample2(x, z) as f64 * 2.0 * self.amplitude;
        let dz = self.fz.sample2(x, z) as f64 * 2.0 * self.amplitude;
        (x + dx, z + dz)
    }
    #[inline]
    pub fn warp_lod(&self, x: f64, z: f64, footprint: f64) -> (f64, f64) {
        let dx = self.fx.sample2_lod(x, z, footprint) as f64 * 2.0 * self.amplitude;
        let dz = self.fz.sample2_lod(x, z, footprint) as f64 * 2.0 * self.amplitude;
        (x + dx, z + dz)
    }
}

// ---------------------------------------------------------------- Worley / Voronoi

#[derive(Clone, Copy, Debug)]
pub struct WorleySample {
    /// Distance (blocks) to the nearest feature point.
    pub f1: f32,
    /// Distance (blocks) to the second-nearest feature point.
    pub f2: f32,
    /// Integer cell owning the nearest point.
    pub cell: (i32, i32),
    /// World position of the nearest feature point.
    pub point: (f64, f64),
    /// Stable hash id of the nearest cell.
    pub id: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Worley2 {
    seed: u64,
    pub cell_size: f64,
    pub jitter: f64,
}

impl Worley2 {
    pub fn new(seed: u64, cell_size: f64, jitter: f64) -> Self {
        Worley2 { seed: mix64(seed ^ 0x3033), cell_size, jitter: jitter.clamp(0.0, 1.0) }
    }

    /// Feature point of cell (cx, cz) in world coordinates.
    #[inline]
    pub fn point_of(&self, cx: i32, cz: i32) -> (f64, f64) {
        let h = hash2(self.seed, cx, cz);
        let jx = hash_f01(h) as f64;
        let jz = hash_f01(mix64(h)) as f64;
        let ox = 0.5 + (jx - 0.5) * self.jitter;
        let oz = 0.5 + (jz - 0.5) * self.jitter;
        ((cx as f64 + ox) * self.cell_size, (cz as f64 + oz) * self.cell_size)
    }

    #[inline]
    pub fn cell_id(&self, cx: i32, cz: i32) -> u64 {
        hash2(self.seed ^ 0xC311, cx, cz)
    }

    pub fn sample(&self, x: f64, z: f64) -> WorleySample {
        let cx = (x / self.cell_size).floor() as i32;
        let cz = (z / self.cell_size).floor() as i32;
        let mut f1 = f64::MAX;
        let mut f2 = f64::MAX;
        let mut best = (cx, cz);
        let mut bp = (0.0, 0.0);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (px, pz) = self.point_of(cx + dx, cz + dz);
                let d = ((px - x) * (px - x) + (pz - z) * (pz - z)).sqrt();
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                    best = (cx + dx, cz + dz);
                    bp = (px, pz);
                } else if d < f2 {
                    f2 = d;
                }
            }
        }
        WorleySample {
            f1: f1 as f32,
            f2: f2 as f32,
            cell: best,
            point: bp,
            id: self.cell_id(best.0, best.1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(n: u64) -> f64 {
        hash_f01(mix64(n)) as f64
    }

    #[test]
    fn simplex_range() {
        let s = Simplex::new(42);
        let mut mx2 = 0.0f32;
        let mut mx3 = 0.0f32;
        let mut sum2 = 0.0f64;
        let mut sq2 = 0.0f64;
        let n = 400_000u64;
        for i in 0..n {
            let x = rng(i * 3) * 1000.0 - 500.0;
            let y = rng(i * 3 + 1) * 1000.0 - 500.0;
            let z = rng(i * 3 + 2) * 1000.0 - 500.0;
            let a = s.n2(x, y);
            let b = s.n3(x, y, z);
            mx2 = mx2.max(a.abs());
            mx3 = mx3.max(b.abs());
            sum2 += a as f64;
            sq2 += (a * a) as f64;
        }
        let mean = sum2 / n as f64;
        let std = (sq2 / n as f64 - mean * mean).sqrt();
        eprintln!("max2={mx2} max3={mx3} mean2={mean} std2={std}");
        assert!(mx2 > 0.85 && mx2 <= 1.08, "2D max {mx2}");
        assert!(mx3 > 0.80 && mx3 <= 1.08, "3D max {mx3}");
        assert!(mean.abs() < 0.01);
    }

    #[test]
    fn simplex_derivative_matches_finite_difference() {
        let s = Simplex::new(7);
        let h = 1e-4;
        for i in 0..2000u64 {
            let x = rng(i * 2) * 100.0;
            let y = rng(i * 2 + 1) * 100.0;
            let (_, dx, dy) = s.n2d(x, y);
            let fx = (s.n2(x + h, y) - s.n2(x - h, y)) / (2.0 * h as f32);
            let fy = (s.n2(x, y + h) - s.n2(x, y - h)) / (2.0 * h as f32);
            assert!((dx - fx).abs() < 0.05 * (1.0 + fx.abs()), "dx {dx} vs {fx}");
            assert!((dy - fy).abs() < 0.05 * (1.0 + fy.abs()), "dy {dy} vs {fy}");
        }
    }

    #[test]
    fn continuity_3d() {
        let s = Simplex::new(9);
        let mut worst = 0.0f32;
        for i in 0..20000u64 {
            let x = rng(i * 3) * 50.0;
            let y = rng(i * 3 + 1) * 50.0;
            let z = rng(i * 3 + 2) * 50.0;
            let d = (s.n3(x, y, z) - s.n3(x + 1e-4, y, z)).abs();
            worst = worst.max(d);
        }
        assert!(worst < 0.01, "3D noise jump {worst}");
    }

    #[test]
    fn determinism_and_seed_independence() {
        let a = Fbm::new(1, 100.0, 5, 2.0, 0.5);
        let b = Fbm::new(1, 100.0, 5, 2.0, 0.5);
        let c = Fbm::new(2, 100.0, 5, 2.0, 0.5);
        let mut diff = 0.0;
        for i in 0..100 {
            let x = i as f64 * 13.7;
            assert_eq!(a.sample2(x, -x), b.sample2(x, -x));
            diff += (a.sample2(x, -x) - c.sample2(x, -x)).abs();
        }
        assert!(diff > 1.0);
    }

    #[test]
    fn ridged_and_eroded_ranges() {
        let r = Ridged::new(3, 500.0, 6);
        let e = ErodedFbm::new(3, 500.0, 7, 0.5, 1.0);
        for i in 0..20000u64 {
            let x = rng(i * 2) * 20000.0;
            let z = rng(i * 2 + 1) * 20000.0;
            let v = r.sample2(x, z);
            assert!((0.0..=1.0).contains(&v));
            let w = e.sample2(x, z);
            assert!(w.abs() <= 1.0);
        }
    }

    #[test]
    fn worley_basic() {
        let w = Worley2::new(5, 32.0, 1.0);
        for i in 0..1000u64 {
            let x = rng(i * 2) * 1000.0;
            let z = rng(i * 2 + 1) * 1000.0;
            let s = w.sample(x, z);
            assert!(s.f1 <= s.f2);
            assert!(s.f1 < 32.0 * 1.5);
        }
    }
}
