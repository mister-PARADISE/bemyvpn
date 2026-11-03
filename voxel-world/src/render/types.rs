//! Renderer public types. FROZEN shared file.
use crate::math::{dir_from_yaw_pitch, yaw_pitch_from_dir, Vec3};

/// Pinhole camera. Convention (see `math::dir_from_yaw_pitch`): yaw 0 → +Z, yaw π/2 → +X, pitch > 0 up.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub pos: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    /// Vertical field of view, degrees.
    pub fov_y_deg: f64,
}

impl Camera {
    pub fn look_at(pos: [f64; 3], target: [f64; 3], fov_y_deg: f64) -> Camera {
        let d = Vec3::new(
            (target[0] - pos[0]) as f32,
            (target[1] - pos[1]) as f32,
            (target[2] - pos[2]) as f32,
        );
        let (yaw, pitch) = yaw_pitch_from_dir(d);
        Camera { pos, yaw, pitch, fov_y_deg }
    }
    pub fn forward(&self) -> Vec3 {
        dir_from_yaw_pitch(self.yaw, self.pitch)
    }
    /// (right, up, forward) orthonormal basis (right-handed, y up). Screen x grows along `right`,
    /// screen y (upwards) along `up`: dir = f + right·sx·tan(fov/2)·aspect + up·sy·tan(fov/2).
    pub fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let f = self.forward();
        let mut right = f.cross(Vec3::Y);
        if right.length_sq() < 1e-10 {
            // Looking straight up/down: derive right from yaw.
            right = crate::math::dir_from_yaw_pitch(self.yaw, 0.0).cross(Vec3::Y);
        }
        let right = right.normalize();
        let up = right.cross(f).normalize();
        (right, up, f)
    }
    pub fn pos_vec(&self) -> Vec3 {
        Vec3::new(self.pos[0] as f32, self.pos[1] as f32, self.pos[2] as f32)
    }
}

/// Sun position. azimuth uses the same convention as camera yaw (0 → +Z, 90 → +X).
#[derive(Clone, Copy, Debug)]
pub struct Sun {
    pub azimuth_deg: f64,
    pub elevation_deg: f64,
}

impl Sun {
    /// Unit vector pointing TOWARD the sun.
    pub fn dir(&self) -> Vec3 {
        dir_from_yaw_pitch(self.azimuth_deg.to_radians(), self.elevation_deg.to_radians())
    }
}

/// Normal landscape view, or a cut-out "diorama" block of level-0 voxels floating in a studio
/// backdrop (cross-section reveals caves and strata). In Diorama mode only voxels with
/// min_x ≤ x < max_x, min_z ≤ z < max_z, y ≥ min_y exist; heightfield levels are ignored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewMode {
    Normal,
    Diorama { min_x: i32, max_x: i32, min_z: i32, max_z: i32, min_y: i32 },
}

#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    /// Adaptive anti-aliasing (extra samples on high-contrast pixels).
    pub aa: bool,
    /// Exposure multiplier applied before tone mapping.
    pub exposure: f32,
    pub mode: ViewMode,
    /// Atmosphere density multiplier (1 = default haze).
    pub haze: f32,
    /// Max ray distance in blocks.
    pub max_distance: f32,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            width: 1920,
            height: 1080,
            aa: true,
            exposure: 1.0,
            mode: ViewMode::Normal,
            haze: 1.0,
            max_distance: 40_000.0,
        }
    }
}

/// Linear HDR image (row-major, top row first).
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Vec3>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Image {
        Image { width, height, pixels: vec![Vec3::ZERO; (width * height) as usize] }
    }
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Vec3 {
        self.pixels[(y * self.width + x) as usize]
    }
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, c: Vec3) {
        let w = self.width;
        self.pixels[(y * w + x) as usize] = c;
    }
}
