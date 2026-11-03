//! Trees / boulders for chunks: generated once per 32×32 root tile (memoised across chunk
//! builds), anchored on the root column's top solid block exactly like the world builder, and
//! turned into per-column runs with the builder's order-independent priorities
//! (class replace_solid 3 > log 2 > other 1 > leaves 0, then a hash of the root).
use crate::terrain::{column_top, Nodes, WH};
use std::collections::HashMap;
use voxel_world::gen::{FeatureColumn, WorldGen, MAX_FEATURE_RADIUS};
use voxel_world::materials::Material;
use voxel_world::noise::{derive_seed, hash2, salt};

pub const TILE: i32 = 32;

pub struct Placed {
    pub rx: i32,
    pub rz: i32,
    pub base_y: i32,
    pub radius: i32,
    pub key: u32,
    pub cols: Vec<FeatureColumn>,
}

#[derive(Clone, Copy, Debug)]
pub struct Run {
    pub col: u32,
    pub y0: i32,
    pub y1: i32,
    pub mat: Material,
    pub replace: bool,
    pub prio: u64,
}

#[inline]
fn run_class(replace: bool, m: Material) -> u64 {
    if replace {
        3
    } else if m.is_log() {
        2
    } else if m.is_leaves() {
        0
    } else {
        1
    }
}

pub struct FeatureCache {
    tiles: HashMap<(i32, i32), Vec<Placed>>,
    seed: Option<u64>,
    key_seed: u64,
}

impl Default for FeatureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureCache {
    pub fn new() -> Self {
        FeatureCache { tiles: HashMap::new(), seed: None, key_seed: 0 }
    }
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.seed = None;
    }
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
    /// Bound the memo: drop tiles far from chunk (cx, cz) once it holds many.
    pub fn trim_around(&mut self, cx: i32, cz: i32) {
        if self.tiles.len() > 160 {
            let (tx, tz) = ((cx * 32).div_euclid(TILE), (cz * 32).div_euclid(TILE));
            self.tiles.retain(|&(a, b), _| (a - tx).abs() <= 5 && (b - tz).abs() <= 5);
            if self.tiles.len() > 160 {
                self.tiles.clear();
            }
        }
    }

    fn tile(&mut self, gen: &WorldGen, nodes: &mut Nodes, tx: i32, tz: i32) {
        if self.seed != Some(gen.seed) {
            self.tiles.clear();
            self.seed = Some(gen.seed);
            self.key_seed = derive_seed(gen.seed, salt("worldbuild.feature_key"));
        }
        if self.tiles.contains_key(&(tx, tz)) {
            return;
        }
        let (x0, z0) = (tx * TILE, tz * TILE);
        let mut v = Vec::new();
        for f in gen.features_in(x0, z0, x0 + TILE, z0 + TILE) {
            if f.columns.is_empty() {
                continue;
            }
            let ry = column_top(nodes, gen, f.root_x, f.root_z).root_y();
            if ry < 0 {
                continue;
            }
            let radius = f.radius();
            v.push(Placed {
                rx: f.root_x,
                rz: f.root_z,
                base_y: ry,
                radius,
                key: (hash2(self.key_seed, f.root_x, f.root_z) >> 32) as u32,
                cols: f.columns,
            });
        }
        self.tiles.insert((tx, tz), v);
    }

    /// Runs of every feature touching the region [rx0, rx0+w) × [rz0, rz0+w), sorted by
    /// (column, decreasing priority, y0, y1, material). Returns the highest run top (y1).
    pub fn collect_runs(&mut self, gen: &WorldGen, nodes: &mut Nodes, rx0: i32, rz0: i32, w: i32, runs: &mut Vec<Run>) -> i32 {
        let r = MAX_FEATURE_RADIUS;
        let (ta, tb) = ((rx0 - r).div_euclid(TILE), (rx0 + w - 1 + r).div_euclid(TILE));
        let (sa, sb) = ((rz0 - r).div_euclid(TILE), (rz0 + w - 1 + r).div_euclid(TILE));
        let mut top = 0;
        for tz in sa..=sb {
            for tx in ta..=tb {
                self.tile(gen, nodes, tx, tz);
                let placed = &self.tiles[&(tx, tz)];
                for p in placed {
                    if p.rx + p.radius < rx0 || p.rz + p.radius < rz0 || p.rx - p.radius >= rx0 + w || p.rz - p.radius >= rz0 + w {
                        continue;
                    }
                    for fc in &p.cols {
                        if fc.mat == Material::Air {
                            continue;
                        }
                        let lx = p.rx + fc.dx as i32 - rx0;
                        let lz = p.rz + fc.dz as i32 - rz0;
                        if lx < 0 || lz < 0 || lx >= w || lz >= w {
                            continue;
                        }
                        let y0 = (p.base_y + fc.y0 as i32).max(0);
                        let y1 = (p.base_y + fc.y1 as i32).min(WH);
                        if y1 <= y0 {
                            continue;
                        }
                        top = top.max(y1);
                        runs.push(Run {
                            col: (lz * w + lx) as u32,
                            y0,
                            y1,
                            mat: fc.mat,
                            replace: fc.replace_solid,
                            prio: (run_class(fc.replace_solid, fc.mat) << 32) | p.key as u64,
                        });
                    }
                }
            }
        }
        runs.sort_unstable_by(|a, b| {
            a.col
                .cmp(&b.col)
                .then(b.prio.cmp(&a.prio))
                .then(a.y0.cmp(&b.y0))
                .then(a.y1.cmp(&b.y1))
                .then(a.mat.cmp(&b.mat))
                .then(a.replace.cmp(&b.replace))
        });
        top
    }
}
