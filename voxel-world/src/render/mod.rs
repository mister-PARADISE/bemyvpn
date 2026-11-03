//! CPU ray-traced renderer. OWNER: render agent (keep the public signatures of `render` and
//! `save_png`; add submodules freely: trace.rs, sky.rs, shade.rs, output.rs …).
pub mod types;

pub use types::*;

use crate::world::World;

/// Render the world. Returns a linear HDR image.
pub fn render(_world: &World, _cam: &Camera, _sun: &Sun, _settings: &RenderSettings) -> Image {
    todo!("render agent: render")
}

/// Tone-map (exposure already applied by `render` or here — document it) and write an 8-bit sRGB PNG.
pub fn save_png(_img: &Image, _path: &std::path::Path) -> std::io::Result<()> {
    todo!("render agent: save_png")
}
