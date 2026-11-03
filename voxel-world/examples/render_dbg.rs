use voxel_world::math::{dir_from_yaw_pitch, Vec3};
use voxel_world::render::atmosphere::Atmosphere;
fn main() {
    for el in [58.0f64, 20.0, 4.0] {
        let sun = dir_from_yaw_pitch(150f64.to_radians(), el.to_radians());
        let a = Atmosphere::new(100.0, sun, 1.0, 40000.0);
        eprintln!("sun el {el}: E_sun(0) {:?}  E_sky_up {:?}", a.sun_illuminance(0.0), a.e_sky_up);
        for n in [Vec3::Y, Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z, -Vec3::Y] {
            eprintln!("   irr {:?} -> {:?}", n, a.irradiance(n));
        }
        eprintln!("   sky zenith {:?} horizon {:?} anti {:?}", a.sky(Vec3::Y), a.sky(Vec3::new(0.0, 0.02, 1.0).normalize()), a.sky(-sun));
        let ap = a.aerial(Vec3::new(1.0, -0.01, 0.0).normalize(), 8000.0);
        eprintln!("   aerial 8km T {:?} L {:?}", ap.t, ap.inscatter);
    }
}
