//! Dev tool: orthographic side views of world trees. Usage: wtree SEED CELLX CELLZ OUT
use std::path::Path;
use voxel_world::gen::worldtree::{tree_of_cell, WT_LEAF, WT_LOG, WT_NONE};
use voxel_world::gen::WorldGen;
use voxel_world::imageio::write_png_rgb8;
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = a[0].parse().unwrap();
    let g = WorldGen::new(seed);
    let mut tiles = Vec::new();
    for c in a[1..a.len() - 1].chunks(2) {
        let (ix, iz): (i32, i32) = (c[0].parse().unwrap(), c[1].parse().unwrap());
        let t = tree_of_cell(&g, ix, iz).expect("no tree");
        let b = t.bbox;
        let (prims, _, _) = t.prims_in(b[0], b[2], b[3], b[5], 0.0);
        let (w, h) = (320usize, 480usize);
        let span = ((b[3] - b[0]).max(b[5] - b[2]).max((b[4] - b[1]) * 320.0 / 480.0)) * 1.05;
        let s = span / w as f32;
        let cx = 0.5 * (b[0] + b[3]);
        let mut img = vec![[200u8, 220, 240]; w * h];
        for py in 0..h {
            for px in 0..w {
                let x = cx + (px as f32 - w as f32 / 2.0) * s;
                let y = b[1] + (h - 1 - py) as f32 * s;
                if y < t.y0 { img[py * w + px] = [110, 90, 60]; }
                let mut z = b[2];
                while z < b[5] {
                    let r = t.sample(x, y, z, &prims, 0.0, true);
                    if r != WT_NONE {
                        let f = 1.0 - 0.6 * (z - b[2]) / (b[5] - b[2]);
                        img[py * w + px] = if r == WT_LOG { [(120.0 * f) as u8, (85.0 * f) as u8, (50.0 * f) as u8] } else if r == WT_LEAF { [(60.0 * f) as u8, (150.0 * f) as u8, (50.0 * f) as u8] } else { [0, 0, 0] };
                        break;
                    }
                    z += 1.0;
                }
            }
        }
        eprintln!("{:?} h {:.0} at ({}, {})", t.kind, t.height, t.x, t.z);
        tiles.push(img);
    }
    let n = tiles.len();
    let mut out = vec![0u8; 320 * n * 480 * 3];
    for (i, t) in tiles.iter().enumerate() {
        for y in 0..480 { for x in 0..320 { let p = t[y * 320 + x]; let o = (y * 320 * n + i * 320 + x) * 3; out[o..o + 3].copy_from_slice(&p); } }
    }
    write_png_rgb8(Path::new(&a[a.len() - 1]), (320 * n) as u32, 480, &out).unwrap();
}
