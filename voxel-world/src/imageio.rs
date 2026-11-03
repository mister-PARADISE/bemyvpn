//! Minimal PNG writer for debug maps (8-bit sRGB RGB). FROZEN shared file.
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Write an RGB8 buffer (row-major, top row first) as PNG. Creates parent directories.
pub fn write_png_rgb8(path: &Path, width: u32, height: u32, rgb: &[u8]) -> std::io::Result<()> {
    assert_eq!(rgb.len(), (width * height * 3) as usize);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = File::create(path)?;
    let w = BufWriter::new(f);
    let mut enc = png::Encoder::new(w, width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(rgb).map_err(std::io::Error::other)?;
    Ok(())
}
