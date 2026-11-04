//! `vw_web` — the voxel-world generator compiled to WebAssembly for the web viewer.
//!
//! Plain `extern "C"` exports (no wasm-bindgen): numbers and pointers only, results are read by
//! JS from `memory.buffer` (re-create typed-array views after every call: memory may grow).
//! See `web/API.md` for the contract and `web/wasm/README.md` for details/deviations.
//!
//! No threads: nothing here calls rayon (world::build_world, scout::plan_shots, …).
pub mod chunk;
pub mod far;
pub mod features;
pub mod terrain;
pub mod worldtree;

#[cfg(test)]
mod tests;

use chunk::ChunkBuilder;
use std::cell::RefCell;
use terrain::{block_solid, Liquids, Nodes, Shape, WH};
use voxel_world::gen::{ColumnInfo, WorldGen};

/// Point-query column cache entry (block column, its `ColumnInfo` and `Shape` in `qnodes`).
pub struct QCol {
    pub x: i32,
    pub z: i32,
    pub col: ColumnInfo,
    pub shape: Shape,
}

/// Columns kept for `vw_solid_at` (a camera box touches ≤ 4 columns per frame).
const QCOLS: usize = 16;
/// `qnodes` is dropped (with the column cache that indexes it) past this many lattice nodes.
const QNODES_CAP: usize = 4096;

/// Global state of one module instance (one per worker).
pub struct State {
    pub gen: Option<WorldGen>,
    pub liq: Option<Liquids>,
    pub chunk: ChunkBuilder,
    pub qnodes: Nodes,
    pub qcols: Vec<QCol>,
    pub qnext: usize,
    pub far: Vec<u8>,
    pub spawn: [f32; 6],
    pub mats: Vec<f32>,
}

impl State {
    /// Bound the point-query memo; the column cache holds indices into it, so it goes too.
    fn trim_q(&mut self) {
        if self.qnodes.nodes.len() > QNODES_CAP {
            self.qnodes.clear();
            self.qcols.clear();
            self.qnext = 0;
        }
    }

    fn new() -> Self {
        State {
            gen: None,
            liq: None,
            chunk: ChunkBuilder::new(),
            qnodes: Nodes::new(),
            qcols: Vec::new(),
            qnext: 0,
            far: Vec::new(),
            spawn: [0.0; 6],
            mats: far::material_table(),
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut b = s.borrow_mut();
        if b.is_none() {
            *b = Some(State::new());
        }
        f(b.as_mut().unwrap())
    })
}

/// Create the global `WorldGen` for `seed = seed_hi << 32 | seed_lo` (replaces the previous one).
#[no_mangle]
pub extern "C" fn vw_init(seed_lo: u32, seed_hi: u32) {
    let seed = ((seed_hi as u64) << 32) | seed_lo as u64;
    with(|s| {
        s.gen = Some(WorldGen::new(seed));
        s.liq = Some(Liquids::new(seed));
        s.chunk.reset();
        s.qnodes.clear();
        s.qcols.clear();
        s.qnext = 0;
    });
}

/// Scratch memory for JS (8-byte aligned).
#[no_mangle]
pub extern "C" fn vw_alloc(n: u32) -> u32 {
    let layout = match std::alloc::Layout::from_size_align(n.max(1) as usize, 8) {
        Ok(l) => l,
        Err(_) => return 0,
    };
    unsafe { std::alloc::alloc(layout) as usize as u32 }
}

#[no_mangle]
pub extern "C" fn vw_free(ptr: u32, n: u32) {
    if ptr == 0 {
        return;
    }
    if let Ok(layout) = std::alloc::Layout::from_size_align(n.max(1) as usize, 8) {
        unsafe { std::alloc::dealloc(ptr as usize as *mut u8, layout) }
    }
}

/// Build + mesh voxel chunk (cx, cz). 0 = ok, 1 = `vw_init` not called.
#[no_mangle]
pub extern "C" fn vw_build_chunk(cx: i32, cz: i32) -> u32 {
    with(|s| match &s.gen {
        Some(g) => {
            s.chunk.build(g, cx, cz);
            0
        }
        None => {
            for o in s.chunk.out.iter_mut() {
                o.clear();
            }
            1
        }
    })
}

/// Build + mesh a Voxy-style LOD section (level ≥ 1; 2^level blocks per voxel). Same outputs as
/// `vw_build_chunk`; vertex positions and `vw_chunk_ymin/ymax` are in LOD-voxel units.
#[no_mangle]
pub extern "C" fn vw_build_lod(level: u32, sx: i32, sz: i32) -> u32 {
    with(|s| match &s.gen {
        Some(g) => {
            s.chunk.build_lod(g, level, sx, sz);
            0
        }
        None => {
            for o in s.chunk.out.iter_mut() {
                o.clear();
            }
            1
        }
    })
}

/// kind 0 opaque, 1 water, 2 foliage.
#[no_mangle]
pub extern "C" fn vw_mesh_ptr(kind: u32) -> u32 {
    with(|s| s.chunk.out.get(kind as usize).map(|v| v.as_ptr() as usize as u32).unwrap_or(0))
}

/// Length in BYTES (12 per vertex, 48 per quad).
#[no_mangle]
pub extern "C" fn vw_mesh_len(kind: u32) -> u32 {
    with(|s| s.chunk.out.get(kind as usize).map(|v| v.len() as u32).unwrap_or(0))
}

#[no_mangle]
pub extern "C" fn vw_chunk_ymin() -> i32 {
    with(|s| s.chunk.ymin)
}

#[no_mangle]
pub extern "C" fn vw_chunk_ymax() -> i32 {
    with(|s| s.chunk.ymax)
}

/// Far tile: (n+1)² vertices (16 bytes each) at (x0 + i·cell, z0 + j·cell), j-major.
/// 0 = ok, 1 = not initialised, 2 = bad size.
#[no_mangle]
pub extern "C" fn vw_build_far(cell: u32, x0: i32, z0: i32, n: u32) -> u32 {
    with(|s| {
        let (Some(g), Some(l)) = (&s.gen, &s.liq) else {
            s.far.clear();
            return 1;
        };
        if n == 0 || n > 1024 || cell == 0 {
            s.far.clear();
            return 2;
        }
        far::build_far(g, l, cell, x0, z0, n, &mut s.far);
        0
    })
}

#[no_mangle]
pub extern "C" fn vw_far_ptr() -> u32 {
    with(|s| s.far.as_ptr() as usize as u32)
}

#[no_mangle]
pub extern "C" fn vw_far_len() -> u32 {
    with(|s| s.far.len() as u32)
}

/// Top of terrain + water (y of the first free block) at a column; 0 if not initialised.
#[no_mangle]
pub extern "C" fn vw_height_at(x: f64, z: f64) -> f32 {
    with(|s| match &s.gen {
        Some(_) => {
            s.trim_q();
            far::height_at(s.gen.as_ref().unwrap(), &mut s.qnodes, x, z)
        }
        None => 0.0,
    })
}

/// 1 if the block containing (x, y, z) is not air — terrain rock, surface water / lava / ice, or
/// cave water / lava — exactly as the exact chunks voxelise it (caves carved), trees ignored;
/// 0 for air (open sky or a cave) and when not initialised. For camera collision: a handful of
/// calls per frame (recent columns are cached).
#[no_mangle]
pub extern "C" fn vw_solid_at(x: f64, y: f64, z: f64) -> u32 {
    with(|s| {
        if s.gen.is_none() || !(x.is_finite() && y.is_finite() && z.is_finite()) {
            return 0;
        }
        let (bx, by, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        if by >= WH {
            return 0;
        }
        if by < 0 {
            return 1;
        }
        s.trim_q();
        let State { gen, qnodes, qcols, qnext, .. } = s;
        let g = gen.as_ref().unwrap();
        let qi = match qcols.iter().position(|q| q.x == bx && q.z == bz) {
            Some(i) => i,
            None => {
                let col = g.column(bx as f64 + 0.5, bz as f64 + 0.5);
                let shape = Shape::new(qnodes, g, bx, bz, &col, &|_, _| None);
                let q = QCol { x: bx, z: bz, col, shape };
                if qcols.len() < QCOLS {
                    qcols.push(q);
                    qcols.len() - 1
                } else {
                    let i = *qnext % QCOLS;
                    *qnext = (i + 1) % QCOLS;
                    qcols[i] = q;
                    i
                }
            }
        };
        let q = &qcols[qi];
        block_solid(g, qnodes, bx, by, bz, &q.col, &q.shape) as u32
    })
}

/// `Biome as u8` at a column.
#[no_mangle]
pub extern "C" fn vw_biome_at(x: f64, z: f64) -> u32 {
    with(|s| match &s.gen {
        Some(g) => g.column(x, z).biome as u32,
        None => 0,
    })
}

/// Per material 4×f32: linear albedo r, g, b, flags (as a number: bit0 grass-tinted,
/// bit1 foliage-tinted, bit2 emissive, bit3 leaves, bit4 banded-tint (terracotta)).
#[no_mangle]
pub extern "C" fn vw_material_table_ptr() -> u32 {
    with(|s| s.mats.as_ptr() as usize as u32)
}

#[no_mangle]
pub extern "C" fn vw_material_count() -> u32 {
    voxel_world::materials::MATERIAL_COUNT as u32
}

/// Search a nice dry viewpoint near (x, z); result (x, y, z, yaw, pitch, 0) at `vw_spawn_ptr`.
/// 0 = ok, 1 = not initialised.
#[no_mangle]
pub extern "C" fn vw_find_spawn(x: f64, z: f64) -> u32 {
    with(|s| match &s.gen {
        Some(_) => {
            s.trim_q();
            s.spawn = far::find_spawn(s.gen.as_ref().unwrap(), &mut s.qnodes, x, z);
            0
        }
        None => 1,
    })
}

#[no_mangle]
pub extern "C" fn vw_spawn_ptr() -> u32 {
    with(|s| s.spawn.as_ptr() as usize as u32)
}

/// Extension (not in API.md): which kinds merge coplanar +Y faces (bit k = kind k).
/// Default 0b110 (water + foliage); 7 also merges opaque tops (fewer quads, but T-junction
/// sparkles possible); 0 = one quad per block face everywhere.
#[no_mangle]
pub extern "C" fn vw_set_merge(mask: u32) {
    with(|s| s.chunk.merge_mask = mask & 7);
}

/// Extension: total quads of the last chunk (all kinds).
#[no_mangle]
pub extern "C" fn vw_chunk_quads() -> u32 {
    with(|s| (0..3).map(|k| s.chunk.quad_count(k) as u32).sum())
}
