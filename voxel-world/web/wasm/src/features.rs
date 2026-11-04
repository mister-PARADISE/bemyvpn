//! Trees / boulders for chunks: generated once per 32×32 root tile (memoised across chunk
//! builds), anchored on the root column's top solid block exactly like the world builder, and
//! turned into per-column runs with the builder's order-independent priorities
//! (class replace_solid 3 > log 2 > other 1 > leaves 0, then a hash of the root).
//!
//! With caves, a feature whose root block or trunk base block is carved (a cave entrance under
//! it) is dropped — decided from the cave lattice at those blocks, a pure function of the world,
//! so every chunk that sees the tree agrees (no half trees, no trees floating over a hole).
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
    /// Cave check (lazily evaluated): false when a cave entrance carved the ground under it.
    pub grounded: Option<bool>,
}

/// Is the ground under the feature's root column and trunk bases intact (not carved)?
fn grounded(gen: &WorldGen, nodes: &mut Nodes, p: &Placed) -> bool {
    let y = p.base_y - 1;
    let mut seen: [(i16, i16); 8] = [(i16::MIN, 0); 8];
    let mut ns = 0;
    for fc in &p.cols {
        let base = (fc.dx == 0 && fc.dz == 0) || (fc.mat.is_log() && fc.y0 <= 0);
        if !base || seen[..ns].contains(&(fc.dx, fc.dz)) {
            continue;
        }
        if ns < seen.len() {
            seen[ns] = (fc.dx, fc.dz);
            ns += 1;
        }
        let (ids, w) = nodes.corners(gen, p.rx + fc.dx as i32, p.rz + fc.dz as i32);
        if nodes.carved_at(gen, &ids, &w, y) {
            return false;
        }
    }
    true
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
    /// Approximate heap bytes held by the memo.
    pub fn mem_bytes(&self) -> usize {
        self.tiles
            .values()
            .map(|v| v.capacity() * std::mem::size_of::<Placed>() + v.iter().map(|p| p.cols.capacity() * std::mem::size_of::<FeatureColumn>()).sum::<usize>())
            .sum()
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
                grounded: None,
            });
        }
        self.tiles.insert((tx, tz), v);
    }

    /// Runs of every feature touching the region [rx0, rx0+w) × [rz0, rz0+w), sorted by
    /// (column, decreasing priority, y0, y1, material). Returns the highest run top (y1).
    /// `caves`: drop features standing over a carved block (see module docs).
    #[allow(clippy::too_many_arguments)]
    pub fn collect_runs(&mut self, gen: &WorldGen, nodes: &mut Nodes, rx0: i32, rz0: i32, w: i32, caves: bool, runs: &mut Vec<Run>) -> i32 {
        let r = MAX_FEATURE_RADIUS;
        let (ta, tb) = ((rx0 - r).div_euclid(TILE), (rx0 + w - 1 + r).div_euclid(TILE));
        let (sa, sb) = ((rz0 - r).div_euclid(TILE), (rz0 + w - 1 + r).div_euclid(TILE));
        let mut top = 0;
        for tz in sa..=sb {
            for tx in ta..=tb {
                self.tile(gen, nodes, tx, tz);
                let placed = self.tiles.get_mut(&(tx, tz)).unwrap();
                for p in placed.iter_mut() {
                    if p.rx + p.radius < rx0 || p.rz + p.radius < rz0 || p.rx - p.radius >= rx0 + w || p.rz - p.radius >= rz0 + w {
                        continue;
                    }
                    if caves {
                        let g = match p.grounded {
                            Some(g) => g,
                            None => {
                                let g = grounded(gen, nodes, p);
                                p.grounded = Some(g);
                                g
                            }
                        };
                        if !g {
                            continue;
                        }
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
