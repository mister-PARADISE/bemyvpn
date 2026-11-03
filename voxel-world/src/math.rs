//! Small linear-algebra + scalar helpers shared by every module.
//! FROZEN shared file: agents may use it but must not change existing signatures.

use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const ONE: Vec3 = Vec3 { x: 1.0, y: 1.0, z: 1.0 };
    pub const X: Vec3 = Vec3 { x: 1.0, y: 0.0, z: 0.0 };
    pub const Y: Vec3 = Vec3 { x: 0.0, y: 1.0, z: 0.0 };
    pub const Z: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 1.0 };

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }
    #[inline]
    pub const fn splat(v: f32) -> Self {
        Vec3 { x: v, y: v, z: v }
    }
    #[inline]
    pub fn from_array(a: [f32; 3]) -> Self {
        Vec3::new(a[0], a[1], a[2])
    }
    #[inline]
    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
    #[inline]
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    #[inline]
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    #[inline]
    pub fn length_sq(self) -> f32 {
        self.dot(self)
    }
    #[inline]
    pub fn length(self) -> f32 {
        self.length_sq().sqrt()
    }
    #[inline]
    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l > 0.0 {
            self / l
        } else {
            Vec3::ZERO
        }
    }
    #[inline]
    pub fn mul_elem(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x * o.x, self.y * o.y, self.z * o.z)
    }
    #[inline]
    pub fn div_elem(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x / o.x, self.y / o.y, self.z / o.z)
    }
    #[inline]
    pub fn min(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }
    #[inline]
    pub fn max(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
    #[inline]
    pub fn abs(self) -> Vec3 {
        Vec3::new(self.x.abs(), self.y.abs(), self.z.abs())
    }
    #[inline]
    pub fn max_elem(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
    #[inline]
    pub fn min_elem(self) -> f32 {
        self.x.min(self.y).min(self.z)
    }
    #[inline]
    pub fn exp(self) -> Vec3 {
        Vec3::new(self.x.exp(), self.y.exp(), self.z.exp())
    }
    #[inline]
    pub fn powf(self, p: f32) -> Vec3 {
        Vec3::new(self.x.powf(p), self.y.powf(p), self.z.powf(p))
    }
    #[inline]
    pub fn lerp(self, o: Vec3, t: f32) -> Vec3 {
        self + (o - self) * t
    }
    #[inline]
    pub fn luminance(self) -> f32 {
        0.2126 * self.x + 0.7152 * self.y + 0.0722 * self.z
    }
    /// Reflect `self` (incident direction) about normal `n`.
    #[inline]
    pub fn reflect(self, n: Vec3) -> Vec3 {
        self - n * (2.0 * self.dot(n))
    }
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Mul<Vec3> for f32 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        v * self
    }
}
impl Mul<Vec3> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, o: Vec3) -> Vec3 {
        self.mul_elem(o)
    }
}
impl Div<f32> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn div(self, s: f32) -> Vec3 {
        let inv = 1.0 / s;
        Vec3::new(self.x * inv, self.y * inv, self.z * inv)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}
impl AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, o: Vec3) {
        *self = *self + o;
    }
}
impl SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, o: Vec3) {
        *self = *self - o;
    }
}
impl MulAssign<f32> for Vec3 {
    #[inline]
    fn mul_assign(&mut self, s: f32) {
        *self = *self * s;
    }
}
impl MulAssign<Vec3> for Vec3 {
    #[inline]
    fn mul_assign(&mut self, o: Vec3) {
        *self = *self * o;
    }
}
impl DivAssign<f32> for Vec3 {
    #[inline]
    fn div_assign(&mut self, s: f32) {
        *self = *self / s;
    }
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
#[inline]
pub fn lerp64(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
#[inline]
pub fn saturate(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}
/// Hermite smoothstep, returns 0 for x<=e0 and 1 for x>=e1 (works for e0>e1 too).
#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = saturate((x - e0) / (e1 - e0));
    t * t * (3.0 - 2.0 * t)
}
/// Quintic smootherstep (C2-continuous).
#[inline]
pub fn smootherstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = saturate((x - e0) / (e1 - e0));
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}
/// Linear remap of x from [a0,a1] to [b0,b1] (unclamped).
#[inline]
pub fn remap(x: f32, a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    b0 + (x - a0) * (b1 - b0) / (a1 - a0)
}
/// Clamped remap.
#[inline]
pub fn remap_clamped(x: f32, a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    lerp(b0, b1, saturate((x - a0) / (a1 - a0)))
}

/// Unit forward vector for yaw/pitch (radians).
/// Convention used EVERYWHERE (camera and sun):
/// yaw = 0 looks toward +Z, yaw = +PI/2 looks toward +X; pitch > 0 looks up.
#[inline]
pub fn dir_from_yaw_pitch(yaw: f64, pitch: f64) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3::new((sy * cp) as f32, sp as f32, (cy * cp) as f32)
}

/// Inverse of `dir_from_yaw_pitch`.
#[inline]
pub fn yaw_pitch_from_dir(d: Vec3) -> (f64, f64) {
    let d = d.normalize();
    let yaw = (d.x as f64).atan2(d.z as f64);
    let pitch = (d.y as f64).clamp(-1.0, 1.0).asin();
    (yaw, pitch)
}
