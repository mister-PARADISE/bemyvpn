//! Biome classification + surface rules. OWNER: gen-core agent. (stub — replace)
use super::ColumnInfo;
use crate::materials::Material;

pub struct SurfaceRules {
    pub seed: u64,
}

impl SurfaceRules {
    pub fn new(seed: u64) -> Self {
        SurfaceRules { seed }
    }
    pub fn material(&self, _x: i32, _y: i32, _z: i32, depth: i32, _col: &ColumnInfo) -> Material {
        if depth == 0 { Material::Grass } else if depth < 4 { Material::Dirt } else { Material::Stone }
    }
}
