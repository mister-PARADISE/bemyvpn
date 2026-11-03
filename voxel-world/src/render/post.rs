//! Image post-processing: bloom (multi-scale Gaussian of the bright pass), ACES tone mapping
//! (Stephen Hill's RRT+ODT fit with the sRGB↔ACES AP1 matrices), sRGB OETF and triangular-PDF
//! dithering (deterministic, hash of the pixel index).
use crate::materials::linear_to_srgb;
use crate::math::Vec3;
use crate::noise::{hash2, hash_f01};
use crate::render::types::Image;

/// Luminance after a cheap Reinhard curve (contrast metric for adaptive AA).
#[inline]
pub fn tm_luma(c: Vec3) -> f32 {
    let l = c.luminance().max(0.0);
    l / (1.0 + l)
}

#[inline]
fn aces_fitted(c: Vec3) -> Vec3 {
    let a = Vec3::new(
        0.59719 * c.x + 0.35458 * c.y + 0.04823 * c.z,
        0.07600 * c.x + 0.90834 * c.y + 0.01566 * c.z,
        0.02840 * c.x + 0.13383 * c.y + 0.83777 * c.z,
    );
    let rrt = |v: f32| {
        let v = v.max(0.0);
        (v * (v + 0.024_578_6) - 0.000_090_537) / (v * (0.983_729 * v + 0.432_951) + 0.238_081)
    };
    let b = Vec3::new(rrt(a.x), rrt(a.y), rrt(a.z));
    Vec3::new(
        1.60475 * b.x - 0.53108 * b.y - 0.07367 * b.z,
        -0.10208 * b.x + 1.10813 * b.y - 0.00605 * b.z,
        -0.00327 * b.x - 0.07276 * b.y + 1.07602 * b.z,
    )
}

/// Tone-map one linear (exposure-applied) pixel to display-linear [0,1].
#[inline]
pub fn tonemap(c: Vec3) -> Vec3 {
    let c = if c.is_finite() { c.max(Vec3::ZERO) } else { Vec3::ZERO };
    let t = aces_fitted(c * 1.6);
    Vec3::new(t.x.clamp(0.0, 1.0), t.y.clamp(0.0, 1.0), t.z.clamp(0.0, 1.0))
}

/// 8-bit sRGB with triangular dither of ±1 LSB.
pub fn to_srgb8(img: &Image) -> Vec<u8> {
    let mut out = vec![0u8; (img.width * img.height * 3) as usize];
    for (i, c) in img.pixels.iter().enumerate() {
        let t = tonemap(*c);
        let h = hash2(0xD17E, i as i32, 7);
        let n = hash_f01(h) - hash_f01(h.rotate_left(32)); // triangular in (-1, 1)
        for (k, v) in [t.x, t.y, t.z].iter().enumerate() {
            let s = linear_to_srgb(*v) * 255.0 + 0.5 + n * 0.5;
            out[i * 3 + k] = s.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn blur_1d(src: &[Vec3], w: usize, h: usize, sigma: f32, horizontal: bool) -> Vec<Vec3> {
    let r = (sigma * 3.0).ceil() as i32;
    let ker: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let ks: f32 = ker.iter().sum();
    let mut out = vec![Vec3::ZERO; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = Vec3::ZERO;
            for (k, &wt) in ker.iter().enumerate() {
                let o = k as i32 - r;
                let (sx, sy) = if horizontal {
                    ((x as i32 + o).clamp(0, w as i32 - 1) as usize, y)
                } else {
                    (x, (y as i32 + o).clamp(0, h as i32 - 1) as usize)
                };
                acc += src[sy * w + sx] * wt;
            }
            out[y * w + x] = acc / ks;
        }
    }
    out
}

/// Add a soft multi-scale bloom of pixels brighter than `threshold` (in exposed units).
pub fn bloom(img: &mut Image, threshold: f32, strength: f32) {
    let (w, h) = (img.width as usize, img.height as usize);
    let f = 4usize;
    let (dw, dh) = ((w / f).max(1), (h / f).max(1));
    let mut small = vec![Vec3::ZERO; dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = Vec3::ZERO;
            for yy in 0..f {
                for xx in 0..f {
                    let (sx, sy) = ((x * f + xx).min(w - 1), (y * f + yy).min(h - 1));
                    let c = img.pixels[sy * w + sx];
                    let l = c.luminance();
                    if l > threshold && l.is_finite() {
                        let k = ((l - threshold) / l).min(1.0);
                        acc += c * k / (1.0 + l * 0.02);
                    }
                }
            }
            small[y * dw + x] = acc / (f * f) as f32;
        }
    }
    let mut total = vec![Vec3::ZERO; dw * dh];
    for (sigma, wgt) in [(2.0f32, 0.5f32), (7.0, 0.3), (20.0, 0.2)] {
        let a = blur_1d(&small, dw, dh, sigma, true);
        let b = blur_1d(&a, dw, dh, sigma, false);
        for (t, v) in total.iter_mut().zip(b.iter()) {
            *t += *v * wgt;
        }
    }
    for y in 0..h {
        for x in 0..w {
            // Bilinear upsample.
            let fx = ((x as f32 + 0.5) / f as f32 - 0.5).max(0.0);
            let fy = ((y as f32 + 0.5) / f as f32 - 0.5).max(0.0);
            let (x0, y0) = ((fx as usize).min(dw - 1), (fy as usize).min(dh - 1));
            let (x1, y1) = ((x0 + 1).min(dw - 1), (y0 + 1).min(dh - 1));
            let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
            let s = |xx: usize, yy: usize| total[yy * dw + xx];
            let c = s(x0, y0).lerp(s(x1, y0), ax).lerp(s(x0, y1).lerp(s(x1, y1), ax), ay);
            img.pixels[y * w + x] += c * strength;
        }
    }
}
