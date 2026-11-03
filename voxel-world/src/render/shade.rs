//! Surface shading: materials, sun + sky lighting, ambient occlusion, water optics.
//!
//! Lighting model for opaque surfaces (radiance toward the viewer):
//!   L = ρ/π · (E_sun(y) · max(0, n·l) · V_sun + E_sky(n) · A) + E_emit + specular,
//! * `E_sun(y)` = TOA sun illuminance × atmospheric transmittance at the point's altitude,
//!   `V_sun` = ray-traced shadow over every clipmap level (water is transparent to shadow rays);
//! * `E_sky(n)` = order-2 SH irradiance of the sky (Ramamoorthi–Hanrahan), ground bounce included;
//! * `A` (ambient occlusion): on level 0 the product of classic voxel corner AO (the 3 front-layer
//!   neighbours of each face vertex) and a *ray-traced per-vertex sky visibility*: 12
//!   cosine-distributed rays from each face vertex (cached per thread, bilinearly interpolated
//!   over the face — smooth and noise-free). Those rays also gather emitted radiance from
//!   lava/crystal (one-bounce emissive lighting). Far heightfields use analytic *horizon AO*:
//!   A = ⟨cos² θ_h(φ)⟩ over 6 azimuths, θ_h the max elevation of the terrain profile.
//! Water: Schlick Fresnel on a ripple normal (analytic gradient of a sum of simplex octaves,
//! band-limited by the pixel footprint with the removed slope variance added to the GGX
//! roughness), ray-traced reflection and refraction, Beer–Lambert absorption + single
//! scattering toward the viewer, refraction caustics (1/|det J| of the refraction map), sun glint.
use crate::gen::SEA_LEVEL;
use crate::materials::{self, Material};
use crate::math::{lerp, saturate, smoothstep, Vec3};
use crate::noise::{hash3, hash_f01, mix64, Simplex};
use crate::render::atmosphere::{sh_irradiance, sh_project, Atmosphere, E_SUN};
use crate::render::texture::{Textures, FACE_BOTTOM, FACE_SIDE, FACE_TOP};
use crate::render::trace::{Hit, HitKind, Ray, Scene, INF};
use crate::render::types::{Camera, RenderSettings, Sun, ViewMode};
use crate::world::World;
use std::f32::consts::PI;

const SEA: f64 = SEA_LEVEL as f64;
/// Water attenuation coefficients (1/m) and single-scattering colour.
const WATER_SIGMA: [f32; 3] = [0.42, 0.105, 0.075];
const AO_RAYS: usize = 12;
const AO_LEN: f64 = 22.0;
/// Distance (blocks) up to which per-vertex ray-traced sky visibility is used.
const VIS_DIST: f64 = 260.0;

/// Per-thread scratch: direct-mapped cache of per-vertex sky visibility.
pub struct Ctx {
    keys: Vec<u64>,
    vals: Vec<[f32; 4]>,
}

impl Ctx {
    pub fn new() -> Ctx {
        Ctx { keys: vec![u64::MAX; 1 << 13], vals: vec![[0.0; 4]; 1 << 13] }
    }
}

impl Default for Ctx {
    fn default() -> Self {
        Self::new()
    }
}

/// One radiance sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub color: Vec3,
    pub depth: f32,
}

pub struct FarGround {
    pub land: Vec3,
    pub water_frac: f32,
}

pub struct Frame<'a> {
    pub world: &'a World,
    pub scene: Scene<'a>,
    pub atm: Option<Atmosphere>,
    pub studio_sh: [Vec3; 9],
    pub tex: Textures,
    pub cam: [f64; 3],
    pub right: Vec3,
    pub up: Vec3,
    pub fwd: Vec3,
    pub tan_y: f32,
    pub aspect: f32,
    pub width: u32,
    pub height: u32,
    pub pix_angle: f32,
    pub sun: Vec3,
    pub studio: bool,
    pub max_dist: f64,
    pub ripple: Simplex,
    pub far: FarGround,
    ao_dirs: [[f32; 3]; AO_RAYS],
    studio_sun: Vec3,
}

#[inline]
fn v3(a: [f64; 3]) -> Vec3 {
    Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32)
}

#[inline]
fn d3(v: Vec3) -> [f64; 3] {
    [v.x as f64, v.y as f64, v.z as f64]
}

/// Orthonormal tangent frame around n (Frisvad / Duff et al. branchless).
#[inline]
fn frame(n: Vec3) -> (Vec3, Vec3) {
    let s = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    (Vec3::new(1.0 + s * n.x * n.x * a, s * b, -s * n.x), Vec3::new(b, s + n.y * n.y * a, -n.y))
}

/// Studio backdrop radiance.
pub fn studio_backdrop(d: Vec3) -> Vec3 {
    let top = Vec3::new(0.62, 0.66, 0.72);
    let mid = Vec3::new(0.46, 0.48, 0.52);
    let bot = Vec3::new(0.17, 0.17, 0.19);
    let y = d.y;
    if y > 0.0 {
        mid.lerp(top, smoothstep(0.0, 0.9, y))
    } else {
        mid.lerp(bot, smoothstep(0.0, 0.7, -y))
    }
}

impl<'a> Frame<'a> {
    pub fn new(world: &'a World, cam: &Camera, sun: &Sun, s: &RenderSettings) -> Frame<'a> {
        let scene = Scene::new(world, s.mode);
        let studio = matches!(s.mode, ViewMode::Diorama { .. });
        let sun_dir = sun.dir().normalize();
        let atm = if studio {
            None
        } else {
            Some(Atmosphere::new(cam.pos[1] - SEA, sun_dir, s.haze, s.max_distance as f64))
        };
        let studio_sun = Vec3::new(1.0, 0.95, 0.88) * 8.5;
        let studio_sh = sh_project(|d| studio_backdrop(d) * 0.9);
        let (right, up, fwd) = cam.basis();
        let tan_y = (cam.fov_y_deg.to_radians() * 0.5).tan() as f32;
        let aspect = s.width as f32 / s.height as f32;
        // Cosine-weighted hemisphere directions (Fibonacci spiral, z = up).
        let mut ao_dirs = [[0.0f32; 3]; AO_RAYS];
        for (i, d) in ao_dirs.iter_mut().enumerate() {
            let u = (i as f32 + 0.5) / AO_RAYS as f32;
            let r = u.sqrt();
            let a = i as f32 * 2.399_963;
            *d = [r * a.cos(), r * a.sin(), (1.0 - u).sqrt()];
        }
        // Far ground stats from the coarsest level's outer ring.
        let far = if let Some(lv) = world.levels.last() {
            let n = lv.size as usize;
            let mut sum = Vec3::ZERO;
            let mut cnt = 0.0f32;
            let mut wat = 0.0f32;
            let step = (n / 64).max(1);
            for j in (0..n).step_by(step) {
                for i in (0..n).step_by(step) {
                    let ring = i < n / 8 || j < n / 8 || i >= n - n / 8 || j >= n - n / 8;
                    if !ring {
                        continue;
                    }
                    let k = j * n + i;
                    cnt += 1.0;
                    if lv.water[k] > lv.h[k] {
                        wat += 1.0;
                    } else {
                        sum += Vec3::from_array(lv.albedo[k]);
                    }
                }
            }
            let land_n = (cnt - wat).max(1.0);
            FarGround { land: sum / land_n, water_frac: if cnt > 0.0 { wat / cnt } else { 0.5 } }
        } else {
            FarGround { land: Vec3::new(0.12, 0.14, 0.08), water_frac: 0.5 }
        };
        Frame {
            world,
            scene,
            atm,
            studio_sh,
            tex: Textures::new(),
            cam: cam.pos,
            right,
            up,
            fwd,
            tan_y,
            aspect,
            width: s.width,
            height: s.height,
            pix_angle: 2.0 * tan_y / s.height as f32,
            sun: sun_dir,
            studio,
            max_dist: s.max_distance as f64,
            ripple: Simplex::new(0xA11CE),
            far,
            ao_dirs,
            studio_sun,
        }
    }

    /// Primary ray direction through continuous pixel coordinates (px, py) (top-left origin).
    #[inline]
    pub fn primary_dir(&self, px: f32, py: f32) -> Vec3 {
        let sx = (2.0 * px / self.width as f32 - 1.0) * self.tan_y * self.aspect;
        let sy = (1.0 - 2.0 * py / self.height as f32) * self.tan_y;
        (self.fwd + self.right * sx + self.up * sy).normalize()
    }

    /// Sun direction jittered over the solar disk for sample index `j` (0 = centre).
    #[inline]
    fn sun_dir(&self, j: u32) -> Vec3 {
        if j == 0 {
            return self.sun;
        }
        let (t1, t2) = frame(self.sun);
        let a = j as f32 * 2.399_963;
        let r = ((j as f32 + 0.5) / 9.0).sqrt().min(1.0) * crate::render::atmosphere::SUN_RADIUS * 1.6;
        (self.sun + t1 * (r * a.cos()) + t2 * (r * a.sin())).normalize()
    }

    #[inline]
    fn sun_irradiance(&self, y: f64) -> Vec3 {
        match &self.atm {
            Some(a) => a.sun_illuminance(y - SEA),
            None => self.studio_sun,
        }
    }

    #[inline]
    fn sky_irradiance(&self, n: Vec3) -> Vec3 {
        match &self.atm {
            Some(a) => a.irradiance(n),
            None => sh_irradiance(&self.studio_sh, n),
        }
    }

    #[inline]
    fn sky_radiance(&self, d: Vec3) -> Vec3 {
        match &self.atm {
            Some(a) => a.sky(d),
            None => studio_backdrop(d),
        }
    }

    /// Radiance arriving at `o` from direction `d` (aerial perspective included). `fp0` = pixel
    /// footprint (blocks) at the origin, growing by `pix_angle` per block.
    pub fn radiance(&self, o: [f64; 3], d: Vec3, fp0: f32, depth: u32, jit: u32, ctx: &mut Ctx) -> Sample {
        let ray = Ray::new(o, d3(d));
        let hit = self.scene.trace(&ray, 0.0, self.max_dist, true);
        match hit {
            Some(h) => {
                let fp = fp0 + h.t as f32 * self.pix_angle;
                let l = self.shade(&h, &ray, d, fp, depth, jit, ctx);
                let l = match &self.atm {
                    Some(a) => {
                        let ap = a.aerial(d, h.t);
                        l * ap.t + ap.inscatter
                    }
                    None => l,
                };
                Sample { color: l, depth: h.t as f32 }
            }
            None => Sample { color: self.miss(d, depth == 0), depth: f32::INFINITY },
        }
    }

    /// Radiance for rays that leave the world: sky + sun disk, or the far planet ground.
    pub fn miss(&self, d: Vec3, sun_disk: bool) -> Vec3 {
        let Some(a) = &self.atm else { return studio_backdrop(d) };
        let inf = a.infinite(d);
        if a.hits_ground(d) {
            // Distant planet surface (beyond the clipmap): land/ocean mix.
            let e = a.sun_illuminance(0.0) * self.sun.y.max(0.0) + a.e_sky_up;
            let land = self.far.land * e * (1.0 / PI);
            let r = Vec3::new(d.x, -d.y, d.z);
            let cos = (-d.y).clamp(0.0, 1.0);
            let f = 0.02 + 0.98 * (1.0 - cos).powi(5);
            let sea = a.sky(r) * f + Vec3::new(0.004, 0.012, 0.02) * e.luminance() * (1.0 - f);
            let g = land.lerp(sea, self.far.water_frac);
            g * inf.t + inf.inscatter
        } else {
            let mut l = inf.inscatter;
            if sun_disk {
                l += a.sun_disk(d, inf.t);
            }
            l
        }
    }

    fn shade(&self, h: &Hit, ray: &Ray, d: Vec3, fp: f32, depth: u32, jit: u32, ctx: &mut Ctx) -> Vec3 {
        match h.kind {
            HitKind::Voxel { bx, by, bz, mat, face } => {
                if mat == Material::Water {
                    let wl = (by + 1) as f64;
                    return self.shade_water(h.p, d, h.t, fp, None, wl, depth, jit, ctx);
                }
                self.shade_voxel(h, bx, by, bz, mat, face, d, fp, None, jit, ctx)
            }
            HitKind::Height { level, water } => {
                if water {
                    let v = &self.scene.hf[level as usize];
                    let bed = v.terrain(h.p[0], h.p[2]);
                    return self.shade_water(h.p, d, h.t, fp, Some((level, bed)), h.p[1], depth, jit, ctx);
                }
                let _ = ray;
                self.shade_height(h, level, d, fp, None, jit)
            }
        }
    }

    // ------------------------------------------------------------------ voxels

    #[inline]
    fn biome_tint(&self, x: f64, z: f64, foliage: bool) -> Vec3 {
        // Bilinear blend of the climate of the 4 nearest columns → smooth tint gradients.
        let l0 = &self.world.l0;
        let gx = x - 0.5;
        let gz = z - 0.5;
        let (ix, iz) = (gx.floor() as i32, gz.floor() as i32);
        let (fx, fz) = ((gx - ix as f64) as f32, (gz - iz as f64) as f32);
        let mut t = Vec3::ZERO;
        for (dx, dz, w) in [(0, 0, (1.0 - fx) * (1.0 - fz)), (1, 0, fx * (1.0 - fz)), (0, 1, (1.0 - fx) * fz), (1, 1, fx * fz)] {
            let (te, hu) = l0.climate_at(ix + dx, iz + dz);
            let c = if foliage { materials::foliage_tint(te, hu) } else { materials::grass_tint(te, hu) };
            t += c * w;
        }
        t
    }

    /// Ambient irradiance at a voxel face point: bilinear blend of ray-traced per-vertex
    /// irradiance (near) or SH sky irradiance with a cover heuristic (far), times corner AO.
    fn voxel_ambient(&self, bx: i32, by: i32, bz: i32, face: u8, p: [f64; 3], n: Vec3, t: f64, ctx: &mut Ctx) -> Vec3 {
        let axis = (face / 2) as usize;
        let pos = face % 2 == 0;
        let (a1, a2) = match axis {
            0 => (2usize, 1usize),
            1 => (0, 2),
            _ => (0, 1),
        };
        let b = [bx, by, bz];
        let mut front = b;
        front[axis] += if pos { 1 } else { -1 };
        let l0 = &self.scene.l0;
        let occ = |o1: i32, o2: i32| {
            let mut q = front;
            q[a1] += o1;
            q[a2] += o2;
            l0.opaque(q[0], q[1], q[2]) as u8
        };
        let s1m = occ(-1, 0);
        let s1p = occ(1, 0);
        let s2m = occ(0, -1);
        let s2p = occ(0, 1);
        let near = t < VIS_DIST;
        let table: [f32; 4] = if near { [1.0, 0.86, 0.74, 0.62] } else { [1.0, 0.76, 0.58, 0.42] };
        let corner = |o1: i32, o2: i32, s1: u8, s2: u8| -> f32 {
            let k = if s1 == 1 && s2 == 1 { 3 } else { s1 + s2 + occ(o1, o2) };
            table[k as usize]
        };
        let c00 = corner(-1, -1, s1m, s2m);
        let c10 = corner(1, -1, s1p, s2m);
        let c01 = corner(-1, 1, s1m, s2p);
        let c11 = corner(1, 1, s1p, s2p);
        let u = saturate((p[a1] - b[a1] as f64) as f32);
        let v = saturate((p[a2] - b[a2] as f64) as f32);
        let bil = |a: f32, b: f32, c: f32, d: f32| lerp(lerp(a, b, u), lerp(c, d, u), v);
        let ao = bil(c00, c10, c01, c11);
        // Far model: SH sky irradiance with a cheap cover test (overhangs / canopy / cave mouths).
        let far = || {
            let mut k = 1.0f32;
            let (fx, fz) = (front[0], front[2]);
            if (self.world.l0.top(fx, fz) as i32) > front[1] + 2 {
                k = if face == 2 { 0.35 } else { 0.6 };
            }
            self.sky_irradiance(n) * k
        };
        if !near {
            return far() * ao;
        }
        let fade = smoothstep(VIS_DIST as f32 * 0.75, VIS_DIST as f32, t as f32);
        let mut ee = [Vec3::ZERO; 4];
        for (k, (o1, o2)) in [(0, 0), (1, 0), (0, 1), (1, 1)].iter().enumerate() {
            let mut vtx = b;
            if pos {
                vtx[axis] += 1;
            }
            vtx[a1] += o1;
            vtx[a2] += o2;
            let r = self.vertex_irradiance(vtx, face, ctx);
            ee[k] = Vec3::new(r[0], r[1], r[2]);
        }
        let e = ee[0].lerp(ee[1], u).lerp(ee[2].lerp(ee[3], u), v);
        let e = if fade > 0.0 { e.lerp(far(), fade) } else { e };
        e * ao
    }

    /// Ray-traced irradiance at a face vertex (12 cosine-distributed rays, Monte-Carlo estimate
    /// E = π/K Σ L_i): escaped rays see the sky radiance, blocked rays see a one-bounce estimate
    /// of the hit surface (albedo × sky/sun irradiance, judged by whether the hit block is exposed
    /// at the top of its column), emitters (lava, crystal) contribute their radiance. Memoised per
    /// thread (pure function of the vertex and face).
    fn vertex_irradiance(&self, v: [i32; 3], face: u8, ctx: &mut Ctx) -> [f32; 4] {
        let key = (v[0] as u32 as u64) | ((v[1] as u32 as u64 & 0x3FF) << 32) | ((v[2] as u32 as u64 & 0x3_FFFF) << 42) | ((face as u64) << 60);
        let slot = (mix64(key) as usize) & (ctx.keys.len() - 1);
        if ctx.keys[slot] == key {
            return ctx.vals[slot];
        }
        let n = v3(crate::render::trace::face_normal(face));
        let (t1, t2) = frame(n);
        let rot = hash_f01(hash3(0xA0, v[0], v[1], v[2] ^ (face as i32) << 20)) * 2.0 * PI;
        let (sr, cr) = rot.sin_cos();
        let o = [v[0] as f64 + n.x as f64 * 0.03, v[1] as f64 + n.y as f64 * 0.03, v[2] as f64 + n.z as f64 * 0.03];
        let mut sum = Vec3::ZERO;
        let e_sun = self.sun_irradiance(v[1] as f64);
        let e_up = self.sky_irradiance(Vec3::Y);
        for dl in &self.ao_dirs {
            let (x, y) = (dl[0] * cr - dl[1] * sr, dl[0] * sr + dl[1] * cr);
            let d = t1 * x + t2 * y + n * dl[2];
            let ray = Ray::new(o, d3(d));
            match self.scene.l0.trace(&ray, 0.0, AO_LEN, false) {
                None => {
                    sum += if d.y < -0.02 && !self.studio {
                        // Escaped downward (over a cliff edge): distant sunlit ground.
                        self.far.land * (e_sun * self.sun.y.max(0.0) + e_up) * (1.0 / PI)
                    } else {
                        self.sky_radiance(d)
                    };
                }
                Some(h) => {
                    if let HitKind::Voxel { mat, face: hf, bx, by, bz } = h.kind {
                        let e = mat.emission();
                        if e != Vec3::ZERO {
                            let fk = if hf == 2 { FACE_TOP } else if hf == 3 { FACE_BOTTOM } else { FACE_SIDE };
                            sum += e * self.tex.average(mat, fk).e;
                        } else {
                            let alb = materials::far_albedo(mat, 0.3, 0.2, by as f32);
                            let top = self.world.l0.top(bx, bz) as i32;
                            let exposed = top - by <= 2 && hf != 3;
                            let hn = v3(h.n);
                            let ndl = hn.dot(self.sun).max(0.0);
                            let e_hit = if exposed { e_up * 0.8 + e_sun * (ndl * 0.75) } else { e_up * 0.1 };
                            sum += alb * e_hit * (1.0 / PI);
                        }
                    }
                }
            }
        }
        let k = PI / AO_RAYS as f32;
        let r = [sum.x * k, sum.y * k, sum.z * k, 0.0];
        ctx.keys[slot] = key;
        ctx.vals[slot] = r;
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn shade_voxel(&self, h: &Hit, bx: i32, by: i32, bz: i32, mat: Material, face: u8, d: Vec3, fp: f32, underwater: Option<f64>, jit: u32, ctx: &mut Ctx) -> Vec3 {
        let p = h.p;
        let n = v3(h.n);
        let fk = if face == 2 {
            FACE_TOP
        } else if face == 3 {
            FACE_BOTTOM
        } else {
            FACE_SIDE
        };
        let fx = (p[0] - bx as f64) as f32;
        let fy = (p[1] - by as f64) as f32;
        let fz = (p[2] - bz as f64) as f32;
        let bh = hash3(0xB10C, bx, by, bz);
        let (mut u, mut v) = match face {
            0 => (1.0 - fz, 1.0 - fy),
            1 => (fz, 1.0 - fy),
            4 => (fx, 1.0 - fy),
            5 => (1.0 - fx, 1.0 - fy),
            _ => (fx, fz),
        };
        let natural = !mat.is_log() && !matches!(mat, Material::Cactus | Material::Sandstone | Material::RedSandstone | Material::Basalt);
        if fk != FACE_SIDE && natural {
            // Random 90° rotation / mirror of top textures (breaks tiling).
            let r = (bh >> 3) & 7;
            if r & 1 == 1 {
                std::mem::swap(&mut u, &mut v);
            }
            if r & 2 == 2 {
                u = 1.0 - u;
            }
            if r & 4 == 4 {
                v = 1.0 - v;
            }
        } else if fk == FACE_SIDE && natural && (bh >> 7) & 1 == 1 {
            u = 1.0 - u;
        }
        let u = u.clamp(0.0, 0.9999);
        let v = v.clamp(0.0, 0.9999);
        let cosv = (-d.dot(n)).abs().max(0.05);
        let lod = (fp * 16.0 / cosv.sqrt()).max(1e-6).log2();
        let tx = self.tex.sample(mat, fk, u, v, lod);
        let tint = if mat.grass_tinted() {
            self.biome_tint(p[0], p[2], false)
        } else if mat.foliage_tinted() {
            self.biome_tint(p[0], p[2], true)
        } else {
            Vec3::ONE
        };
        let mut albedo = tx.color(tint);
        if mat == Material::Terracotta {
            // Strata colour at the texel row's world height (bands of the frozen colormap).
            let row = if fk == FACE_SIDE { by as f32 + 1.0 - ((v * 16.0).floor() + 0.5) / 16.0 } else { by as f32 + 0.5 };
            albedo = albedo * materials::terracotta_band(row, 0.0);
        }
        // Per-block brightness jitter, faded out with distance.
        let jitter = 1.0 + 0.06 * (hash_f01(bh) * 2.0 - 1.0) * (1.0 - smoothstep(0.5, 4.0, fp * 4.0));
        albedo = albedo * jitter;
        let emit = mat.emission() * tx.e;
        let amb = self.voxel_ambient(bx, by, bz, face, p, n, h.t, ctx);
        let f0 = mat.specular_f0();
        let rough = match mat {
            Material::Ice | Material::PackedIce => 0.12,
            Material::Obsidian | Material::Crystal => 0.18,
            _ => 0.45,
        };
        let translucent = mat.is_leaves();
        let mut l = self.light(p, n, h.n, albedo, amb, d, f0, rough, underwater, translucent, jit, 1e-3);
        l += emit;
        l
    }

    /// Direct sun + sky ambient (+ glossy spec) for a surface point.
    #[allow(clippy::too_many_arguments)]
    fn light(&self, p: [f64; 3], n: Vec3, ng: [f64; 3], albedo: Vec3, amb: Vec3, d: Vec3, f0: f32, rough: f32, underwater: Option<f64>, translucent: bool, jit: u32, eps: f64) -> Vec3 {
        let sd = self.sun_dir(jit);
        let ndl = n.dot(sd);
        let ngl = ng[0] * sd.x as f64 + ng[1] * sd.y as f64 + ng[2] * sd.z as f64;
        let mut e_sun = Vec3::ZERO;
        let mut sun_vis = 0.0f32;
        if (ndl > 0.0 && ngl > -0.05) || (translucent && ndl < 0.0) {
            let e = self.sun_irradiance(p[1]);
            if e.max_elem() > 1e-5 {
                let off = if ngl > 0.0 { eps } else { eps * 2.0 };
                let sgn = if translucent && ngl < 0.0 { -1.0 } else { 1.0 };
                let mut o = [p[0] + ng[0] * off * sgn, p[1] + ng[1] * off * sgn, p[2] + ng[2] * off * sgn];
                if translucent && ngl < 0.0 {
                    // Light through one leaf block: start the shadow ray on the sun side of the block.
                    o = [o[0] + sd.x as f64 * 1.0, o[1] + sd.y as f64 * 1.0, o[2] + sd.z as f64 * 1.0];
                }
                let ray = Ray::new(o, d3(sd));
                if !self.scene.occluded(&ray, 0.0, INF) {
                    sun_vis = 1.0;
                    e_sun = e;
                    if let Some(w) = underwater {
                        let depth = (w - p[1]).max(0.0) as f32;
                        let path = depth / sd.y.max(0.15);
                        e_sun = e_sun * Vec3::new((-WATER_SIGMA[0] * path).exp(), (-WATER_SIGMA[1] * path).exp(), (-WATER_SIGMA[2] * path).exp());
                        e_sun = e_sun * self.caustics(p, depth);
                    }
                }
            }
        }
        let mut e_sky = amb;
        let ao = (amb.luminance() / self.sky_irradiance(n).luminance().max(1e-4)).min(1.0);
        if let Some(w) = underwater {
            let depth = (w - p[1]).max(0.0) as f32;
            e_sky = e_sky * Vec3::new((-WATER_SIGMA[0] * depth).exp(), (-WATER_SIGMA[1] * depth).exp(), (-WATER_SIGMA[2] * depth).exp());
        }
        let direct = if ndl > 0.0 {
            ndl
        } else if translucent {
            -ndl * 0.35
        } else {
            0.0
        };
        let mut l = albedo * (e_sun * direct + e_sky) * (1.0 / PI);
        if f0 > 0.0 {
            let v = -d;
            let nv = n.dot(v).max(1e-3);
            let r = d.reflect(n);
            let fr = f0 + (1.0 - f0) * (1.0 - nv).powi(5);
            l += self.sky_radiance(if r.y < 0.0 { Vec3::new(r.x, 0.02, r.z).normalize() } else { r }) * (fr * ao);
            if sun_vis > 0.0 && ndl > 0.0 {
                l += e_sun * ggx_spec(n, v, sd, rough, f0) * ndl;
            }
        }
        l
    }

    // ------------------------------------------------------------------ heightfields

    fn shade_height(&self, h: &Hit, level: u8, d: Vec3, fp: f32, underwater: Option<f64>, jit: u32) -> Vec3 {
        let v = &self.scene.hf[level as usize];
        let lv = v.lv;
        let c = lv.cell as f64;
        let (i, j, u, w) = v.locate(h.p[0], h.p[2]);
        let n1 = lv.size as i32 - 1;
        let idx = |a: i32, b: i32| lv.index(a.clamp(0, n1), b.clamp(0, n1));
        let hh = |a: i32, b: i32| lv.h[idx(a, b)];
        // Smooth normal: bilinear blend of central-difference vertex normals.
        let mut nn = Vec3::ZERO;
        let mut alb = Vec3::ZERO;
        let (uf, wf) = (u as f32, w as f32);
        for (di, dj, wt) in [(0, 0, (1.0 - uf) * (1.0 - wf)), (1, 0, uf * (1.0 - wf)), (0, 1, (1.0 - uf) * wf), (1, 1, uf * wf)] {
            let (a, b) = (i + di, j + dj);
            let gx = (hh(a + 1, b) - hh(a - 1, b)) / (2.0 * c as f32);
            let gz = (hh(a, b + 1) - hh(a, b - 1)) / (2.0 * c as f32);
            nn += Vec3::new(-gx, 1.0, -gz).normalize() * wt;
            alb += Vec3::from_array(lv.albedo[idx(a, b)]) * wt;
        }
        let n = nn.normalize();
        let (ci, cj) = (if uf < 0.5 { i } else { i + 1 }, if wf < 0.5 { j } else { j + 1 });
        let mat = lv.mat[idx(ci, cj)];
        // Sub-cell detail: hashed per-block brightness, faded with the footprint.
        let det = 1.0 - smoothstep(0.6, 2.0, fp / c as f32 * 0.5);
        if det > 0.0 {
            let (bx, bz) = (h.p[0].floor() as i32, h.p[2].floor() as i32);
            alb = alb * (1.0 + 0.10 * det * (hash_f01(hash3(0xFA2, bx >> 1, 0, bz >> 1)) * 2.0 - 1.0));
        }
        // Slope rock: steep far terrain shows bare stone regardless of the cell albedo.
        let slope = (1.0 - n.y.min(1.0)) * 1.0;
        if !matches!(mat, Material::Snow | Material::Ice | Material::PackedIce | Material::Lava) {
            let rock = Material::Stone.albedo() * 0.85;
            alb = alb.lerp(rock, smoothstep(0.45, 0.7, slope + 0.25 * (1.0 - n.y)));
        }
        let ao = self.horizon_ao(v, h.p, n);
        let f0 = mat.specular_f0();
        let off = 0.02 * c + 0.002 * h.t;
        let amb = self.sky_irradiance(n) * ao;
        let mut l = self.light(h.p, n, h.n, alb, amb, d, f0, 0.35, underwater, false, jit, off);
        l += mat.emission() * 0.8;
        l
    }

    /// Horizon-based ambient occlusion on a heightfield: ⟨cos² θ_h⟩ over 6 azimuths.
    fn horizon_ao(&self, v: &crate::render::trace::HfView, p: [f64; 3], n: Vec3) -> f32 {
        let c = v.lv.cell as f64;
        let rot = hash_f01(hash3(0x0A0, (p[0] / c) as i32, 0, (p[2] / c) as i32)) * 1.047;
        let y0 = p[1] + 0.3 * c;
        let mut acc = 0.0f32;
        for k in 0..6 {
            let a = rot + k as f32 * (PI / 3.0);
            let (s, co) = a.sin_cos();
            let mut tmax = 0.0f64;
            let mut dist = c;
            for _ in 0..6 {
                let hx = v.terrain(p[0] + co as f64 * dist, p[2] + s as f64 * dist);
                tmax = tmax.max((hx - y0) / dist);
                dist *= 2.1;
            }
            let t2 = (tmax * tmax) as f32;
            acc += 1.0 / (1.0 + t2); // cos²θ = 1/(1+tan²θ)
        }
        let ao = acc / 6.0;
        // Tilted surfaces see less sky: scale by (1 + n.y)/2 handled by the SH already.
        let _ = n;
        ao
    }

    // ------------------------------------------------------------------ water

    /// Ripple height-field gradient (∂h/∂x, ∂h/∂z), band-limited to footprint `fp`, plus the
    /// slope variance of the removed octaves. Also returns the Laplacian estimate for caustics.
    #[inline]
    fn ripple_grad(&self, x: f64, z: f64, fp: f32) -> (f32, f32, f32) {
        const L: [f32; 5] = [11.0, 4.7, 2.1, 0.95, 0.43];
        let mut gx = 0.0;
        let mut gz = 0.0;
        let mut var = 0.0;
        for (k, &lam) in L.iter().enumerate() {
            let amp = 0.065 * lam;
            let w = saturate((lam / fp.max(1e-4) - 2.0) * 0.5);
            let slope = amp / lam;
            if w <= 0.0 {
                var += slope * slope * 0.6;
                continue;
            }
            // Wind-stretched octaves: rotate + anisotropic scale.
            let ang = 0.6 + k as f64 * 1.3;
            let (sa, ca) = ang.sin_cos();
            let (rx, rz) = (x * ca - z * sa, x * sa + z * ca);
            let (sx, sz) = (lam as f64 * 1.6, lam as f64 * 0.8);
            let (_, dx, dz) = self.ripple.n2d(rx / sx + k as f64 * 17.3, rz / sz);
            let (dxs, dzs) = (dx as f64 / sx, dz as f64 / sz);
            // Back-rotate gradient.
            let (bx, bz) = (dxs * ca + dzs * sa, -dxs * sa + dzs * ca);
            gx += w * amp * bx as f32;
            gz += w * amp * bz as f32;
            var += (1.0 - w) * slope * slope * 0.6;
        }
        (gx, gz, var)
    }

    /// Refraction caustics: light focusing by the ripple surface, I = 1/|1 − D(1 − 1/η) ∇²h|.
    fn caustics(&self, p: [f64; 3], depth: f32) -> f32 {
        if depth < 0.05 {
            return 1.0;
        }
        let e = 0.15;
        let (gx1, _, _) = self.ripple_grad(p[0] + e, p[2], 0.0);
        let (gx0, _, _) = self.ripple_grad(p[0] - e, p[2], 0.0);
        let (_, gz1, _) = self.ripple_grad(p[0], p[2] + e, 0.0);
        let (_, gz0, _) = self.ripple_grad(p[0], p[2] - e, 0.0);
        let lap = ((gx1 - gx0) + (gz1 - gz0)) / (2.0 * e as f32);
        let k = depth.min(6.0) * (1.0 - 1.0 / 1.333) * 1.6;
        let j = (1.0 - k * lap).abs().max(0.25);
        let i = (1.0 / j).min(3.0);
        lerp(1.0, i, smoothstep(0.05, 1.0, depth) * (1.0 - smoothstep(4.0, 10.0, depth)))
    }

    #[allow(clippy::too_many_arguments)]
    fn shade_water(&self, p: [f64; 3], d: Vec3, t: f64, fp: f32, far: Option<(u8, f64)>, wl: f64, depth: u32, jit: u32, ctx: &mut Ctx) -> Vec3 {
        let (gx, gz, var) = self.ripple_grad(p[0], p[2], fp);
        let calm = 1.0 - 0.6 * smoothstep(800.0, 6000.0, t as f32);
        let n = Vec3::new(-gx * calm, 1.0, -gz * calm).normalize();
        let v = -d;
        let cos_i = v.dot(n).max(0.02);
        let f = 0.02 + 0.98 * (1.0 - cos_i).powi(5);
        // Reflection.
        let mut r = d.reflect(n);
        if r.y < 0.01 {
            r = Vec3::new(r.x, 0.01, r.z).normalize();
        }
        let refl = if depth == 0 {
            let o = [p[0], wl + 0.01, p[2]];
            let ray = Ray::new(o, d3(r));
            match self.scene.trace(&ray, 0.0, self.max_dist, false) {
                Some(h) => {
                    let fp2 = fp + h.t as f32 * self.pix_angle;
                    let l = self.shade(&h, &ray, r, fp2, depth + 1, jit, ctx);
                    match &self.atm {
                        Some(a) => {
                            let ap = a.aerial(r, h.t);
                            l * ap.t + ap.inscatter
                        }
                        None => l,
                    }
                }
                None => self.miss(r, false),
            }
        } else {
            self.sky_radiance(r)
        };
        // Refraction: path length through water to the bed.
        let eta = 1.0 / 1.333;
        let ci = cos_i;
        let k = 1.0 - eta * eta * (1.0 - ci * ci);
        let tr = if k > 0.0 { (d * eta + n * (eta * ci - k.sqrt())).normalize() } else { Vec3::new(d.x, -0.3, d.z).normalize() };
        let tr = if tr.y > -0.05 { Vec3::new(tr.x, -0.05, tr.z).normalize() } else { tr };
        let e_sun = self.sun_irradiance(wl);
        let sky_up = self.sky_irradiance(Vec3::Y);
        let (bed_l, path) = match far {
            None => {
                let o = [p[0], wl - 0.001, p[2]];
                let ray = Ray::new(o, d3(tr));
                let maxp = 40.0;
                match self.scene.l0.trace(&ray, 0.0, maxp, false) {
                    Some(h) if depth == 0 => {
                        let lb = match h.kind {
                            HitKind::Voxel { bx, by, bz, mat, face } => {
                                self.shade_voxel(&h, bx, by, bz, mat, face, tr, fp + h.t as f32 * self.pix_angle, Some(wl), jit, ctx)
                            }
                            _ => Vec3::ZERO,
                        };
                        (lb, h.t as f32)
                    }
                    Some(h) => (Vec3::ZERO, h.t as f32),
                    None => (Vec3::ZERO, maxp as f32),
                }
            }
            Some((level, bed)) => {
                let dep = (wl - bed).max(0.0) as f32;
                let path = dep / (-tr.y).max(0.05);
                let lb = if path < 30.0 {
                    let hb = [p[0] + tr.x as f64 * path as f64, bed, p[2] + tr.z as f64 * path as f64];
                    let vv = &self.scene.hf[level as usize];
                    let (i, j, _, _) = vv.locate(hb[0], hb[2]);
                    let n1 = vv.lv.size as i32 - 1;
                    let a = Vec3::from_array(vv.lv.albedo[vv.lv.index(i.clamp(0, n1), j.clamp(0, n1))]);
                    let att = Vec3::new((-WATER_SIGMA[0] * dep).exp(), (-WATER_SIGMA[1] * dep).exp(), (-WATER_SIGMA[2] * dep).exp());
                    a * (e_sun * self.sun.y.max(0.0) + sky_up) * att * (1.0 / PI)
                } else {
                    Vec3::ZERO
                };
                (lb, path)
            }
        };
        let att = Vec3::new((-WATER_SIGMA[0] * path).exp(), (-WATER_SIGMA[1] * path).exp(), (-WATER_SIGMA[2] * path).exp());
        // In-water single scattering toward the viewer (deep-water colour).
        let (te, _) = self.world.l0.climate_at(p[0].floor() as i32, p[2].floor() as i32);
        let wt = materials::water_tint(0.0, te);
        let scat_col = Vec3::new(0.05, 0.22, 0.30).lerp(wt * 0.35, 0.35);
        let scat = scat_col * (e_sun * (0.35 * self.sun.y.max(0.0)) + sky_up * 0.5) * (1.0 / (4.0 * PI));
        let trans = bed_l * att + scat * (Vec3::ONE - att);
        let mut l = refl * f + trans * (1.0 - f);
        // Sun glint (GGX, roughness widened by the sub-pixel ripple variance).
        let sd = self.sun_dir(jit);
        let ndl = n.dot(sd);
        if ndl > 0.0 && e_sun.max_elem() > 1e-5 {
            let rough = (0.004 + var + 0.0004 * fp).sqrt().min(0.5);
            let spec = ggx_spec(n, v, sd, rough, 0.02);
            if spec * e_sun.max_elem() > 1e-4 {
                let o = [p[0], wl + 0.02, p[2]];
                let ray = Ray::new(o, d3(sd));
                if !self.scene.occluded(&ray, 0.0, INF) {
                    l += e_sun * spec * ndl;
                }
            }
        }
        l
    }
}

/// GGX/Trowbridge-Reitz specular BRDF (Smith G, Schlick F) — returns f_r (1/sr).
#[inline]
pub fn ggx_spec(n: Vec3, v: Vec3, l: Vec3, rough: f32, f0: f32) -> f32 {
    let hv = (v + l).normalize();
    let nh = n.dot(hv).max(0.0);
    let nv = n.dot(v).max(1e-4);
    let nl = n.dot(l).max(1e-4);
    let a2 = (rough * rough).max(1e-5);
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let dfn = a2 / (PI * dd * dd);
    let k = rough * rough * 0.5;
    let g = (nv / (nv * (1.0 - k) + k)) * (nl / (nl * (1.0 - k) + k));
    let vh = v.dot(hv).max(0.0);
    let f = f0 + (1.0 - f0) * (1.0 - vh).powi(5);
    dfn * g * f / (4.0 * nv * nl)
}

#[allow(dead_code)]
fn _unused() -> f32 {
    E_SUN
}
