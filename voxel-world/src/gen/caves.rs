//! Caves. OWNER: caves+features agent. (stub — replace)
use super::{CaveFill, ColumnInfo};

pub struct CaveCarver {
    pub seed: u64,
}

impl CaveCarver {
    pub fn new(seed: u64) -> Self {
        CaveCarver { seed }
    }
    pub fn field(&self, _x: f64, _y: f64, _z: f64, _col: &ColumnInfo) -> f32 {
        -1.0
    }
    pub fn fill(&self, _x: i32, _y: i32, _z: i32, _col: &ColumnInfo) -> CaveFill {
        CaveFill::Air
    }
}
