//! A planned screenshot. FROZEN shared type.
use crate::render::{Camera, Sun, ViewMode};
use crate::world::BuildParams;

#[derive(Clone, Debug)]
pub struct Shot {
    pub index: usize,
    /// Short file-name-safe id, e.g. "panorama".
    pub name: String,
    /// Human-readable caption (Russian is fine), e.g. "Панорама с вершины 412 м".
    pub title: String,
    pub camera: Camera,
    pub sun: Sun,
    pub mode: ViewMode,
    pub build: BuildParams,
    pub exposure: f32,
    pub haze: f32,
}
