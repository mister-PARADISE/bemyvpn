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

#[cfg(test)]
mod tests;

use chunk::ChunkBuilder;
use std::cell::RefCell;
use terrain::{Liquids, Nodes};
use voxel_world::gen::WorldGen;

/// Global state of one module instance (one per worker).
pub struct State {
    pub gen: Option<WorldGen>,
    pub liq: Option<Liquids>,
    pub chunk: ChunkBuilder,
    pub qnodes: Nodes,
    pub far: Vec<u8>,
    pub spawn: [f32; 6],
    pub mats: Vec<f32>,
}

impl State {
    fn new() -> Self {
        State {
            gen: None,
            liq: None,
            chunk: ChunkBuilder::new(),
            qnodes: Nodes::new(),
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
        Some(g) => {
            s.qnodes.trim(4096);
            far::height_at(g, &mut s.qnodes, x, z)
        }
        None => 0.0,
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
        Some(g) => {
            s.qnodes.trim(4096);
            s.spawn = far::find_spawn(g, &mut s.qnodes, x, z);
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
/// Default 0b111. 0 disables merging (one quad per block face).
#[no_mangle]
pub extern "C" fn vw_set_merge(mask: u32) {
    with(|s| s.chunk.merge_mask = mask & 7);
}

/// Extension: total quads of the last chunk (all kinds).
#[no_mangle]
pub extern "C" fn vw_chunk_quads() -> u32 {
    with(|s| (0..3).map(|k| s.chunk.quad_count(k) as u32).sum())
}
