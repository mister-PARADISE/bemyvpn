//! Trees and other features. OWNER: caves+features agent. (stub — replace)
use super::{CanopyLod, ColumnInfo, Feature, WorldGen};

pub struct FeaturePlacer {
    pub seed: u64,
}

impl FeaturePlacer {
    pub fn new(seed: u64) -> Self {
        FeaturePlacer { seed }
    }
    pub fn features_in(&self, _gen: &WorldGen, _x0: i32, _z0: i32, _x1: i32, _z1: i32) -> Vec<Feature> {
        Vec::new()
    }
    pub fn canopy_lod(&self, _x: f64, _z: f64, _col: &ColumnInfo, _footprint: f64) -> CanopyLod {
        CanopyLod::default()
    }
}
