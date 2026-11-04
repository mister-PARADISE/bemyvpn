//! World trees (ultra-rare colossal landmark trees, `voxel_world::gen::worldtree`) stamped into a
//! chunk's dense voxel grid — exact chunks (s = 1) and every LOD level (s = 2^L), so the giants
//! are visible from tens of kilometres away. Logs override everything (buttress roots dig into the
//! ground); leaves fill air only.
use std::rc::Rc;
use voxel_world::gen::worldtree::{world_trees_in, WorldTree, WT_LEAF, WT_LOG};
use voxel_world::gen::WorldGen;
use voxel_world::materials::Material;

/// World trees touching the column region [rx0, rx0 + w) × [rz0, rz0 + w) of LOD cells of size `s`.
pub fn trees_for(gen: &WorldGen, rx0: i32, rz0: i32, w: i32, s: i32) -> Vec<Rc<WorldTree>> {
    let (x0, z0) = ((rx0 * s) as f64, (rz0 * s) as f64);
    world_trees_in(gen, x0, z0, x0 + (w * s) as f64, z0 + (w * s) as f64)
}

/// Highest voxel index (exclusive, in units of `s`) any of the trees reaches.
pub fn top(trees: &[Rc<WorldTree>], s: i32) -> i32 {
    trees.iter().map(|t| (t.bbox[4] / s as f32).ceil() as i32 + 1).max().unwrap_or(0)
}

/// Stamp the trees into `vox` (y-major layers of `w × w` columns; layer `j − yb + 1` holds voxel
/// row j, i.e. world y ∈ [j·s, j·s + s); voxel rows [yb, yt) exist).
#[allow(clippy::too_many_arguments)]
pub fn stamp(trees: &[Rc<WorldTree>], vox: &mut [u8], yb: i32, yt: i32, rx0: i32, rz0: i32, w: i32, s: i32) {
    if trees.is_empty() {
        return;
    }
    let layer = (w * w) as usize;
    let sf = s as f32;
    let e = if s > 1 { 0.3 * sf } else { 0.0 };
    let fine = s == 1;
    let air = Material::Air as u8;
    for t in trees {
        let (bx0, bz0) = (rx0 as f32 * sf, rz0 as f32 * sf);
        let (bx1, bz1) = (bx0 + w as f32 * sf, bz0 + w as f32 * sf);
        let (_, ylo, yhi) = t.prims_in(bx0, bz0, bx1, bz1, e);
        if ylo > yhi {
            continue;
        }
        let (log, leaf) = (t.log as u8, t.leaf as u8);
        for lz in 0..w {
            for lx in 0..w {
                let x = (rx0 + lx) as f32 * sf + 0.5 * sf;
                let z = (rz0 + lz) as f32 * sf + 0.5 * sf;
                if x < t.bbox[0] - e || x > t.bbox[3] + e || z < t.bbox[2] - e || z > t.bbox[5] + e {
                    continue;
                }
                // primitives over this column only
                let (c0x, c0z) = (x - 0.5 * sf, z - 0.5 * sf);
                let (colp, clo, chi) = t.prims_in(c0x, c0z, c0x + sf, c0z + sf, e);
                if clo > chi {
                    continue;
                }
                let j0 = ((clo.max(ylo)) / sf).floor() as i32;
                let j1 = ((chi.min(yhi)) / sf).ceil() as i32 + 1;
                let c = (lz * w + lx) as usize;
                for j in j0.max(yb)..j1.min(yt) {
                    let y = j as f32 * sf + 0.5 * sf;
                    let r = t.sample(x, y, z, &colp, e, fine);
                    if r == 0 {
                        continue;
                    }
                    let i = c + (j - yb + 1) as usize * layer;
                    if r == WT_LOG {
                        vox[i] = log;
                    } else if r == WT_LEAF && vox[i] == air {
                        vox[i] = leaf;
                    }
                }
            }
        }
    }
}
