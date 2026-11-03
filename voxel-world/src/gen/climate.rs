//! Climate fields. OWNER: gen-core agent. (stub — replace)
use super::Climate;

pub struct ClimateSampler {
    pub seed: u64,
}

impl ClimateSampler {
    pub fn new(seed: u64) -> Self {
        ClimateSampler { seed }
    }
    /// `footprint` = blocks per sample (0 or 1 = full detail); see `Fbm::sample2_lod`.
    pub fn sample(&self, _x: f64, _z: f64, _footprint: f64) -> Climate {
        todo!("gen-core: ClimateSampler::sample")
    }
}
