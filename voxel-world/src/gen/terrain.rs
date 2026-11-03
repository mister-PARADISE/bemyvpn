//! Terrain shaping: heights, 3D density. OWNER: gen-core agent. (stub — replace)
use super::{climate::ClimateSampler, ColumnInfo};

pub struct TerrainShaper {
    pub seed: u64,
}

impl TerrainShaper {
    pub fn new(seed: u64) -> Self {
        TerrainShaper { seed }
    }
    pub fn column(&self, _climate: &ClimateSampler, _x: f64, _z: f64, _footprint: f64) -> ColumnInfo {
        todo!("gen-core: TerrainShaper::column")
    }
    pub fn density(&self, _x: f64, y: f64, _z: f64, col: &ColumnInfo) -> f32 {
        col.height - y as f32
    }
}
