# Web flythrough viewer — contract

Goal: a single web page (published as a claude.ai Artifact, opened on a PHONE) where the user flies
through the generated world in real time. The world is generated ON THE DEVICE by the real Rust
generator compiled to WebAssembly (no precomputed data except bookmarks), rendered with WebGL2.

Artifact hosting constraints (hard): page = `index.html` (no <html>/<head>/<body> tags; it is wrapped);
supporting files served next to it and fetched with RELATIVE urls (`worker.js`, `gen.wasm`,
`bookmarks.json`). No external network except script CDNs (not needed — use raw WebGL2, no libs).
Web Workers from own files work. No alert/confirm/prompt. Must work at phone width with touch, and
also on desktop. If WebAssembly is blocked by CSP, show a clear in-page message (do not fail silently).

## Layout

```
web/
  API.md                 (this contract — lead owns)
  wasm/                  Rust cdylib crate `vw_web` (owner: wasm-core)
    Cargo.toml           [workspace] standalone; voxel-world = { path = "../.." }; crate-type cdylib
    src/lib.rs
  build.sh               builds wasm → www/gen.wasm (owner: wasm-core)
  www/                   (owner: viewer) index.html, worker.js, (generated) gen.wasm, bookmarks.json
  tools/                 native helpers, e.g. bookmarks export (owner: wasm-core)
  test/                  Playwright smoke tests + screenshots (owner: integration)
```

Build: `cd web/wasm && cargo build --release --target wasm32-unknown-unknown` (target installed).
No wasm-bindgen (not available) — plain `#[no_mangle] pub extern "C"` functions, numbers and
pointers only; JS reads results from `memory.buffer`. Keep gen.wasm small (opt-level "s" or 3 + lto,
panic = "abort", strip). rayon must never be called inside wasm (no threads): do NOT call
`world::build_world`, `scout::plan_shots`, `HeightLevel::from_fn`, `Level0::from_fn`.

## WASM exports (all extern "C")

| fn | meaning |
|---|---|
| `vw_init(seed_lo: u32, seed_hi: u32)` | create the global `WorldGen` (replace previous) |
| `vw_alloc(n: u32) -> u32`, `vw_free(ptr: u32, n: u32)` | scratch memory for JS |
| `vw_build_chunk(cx: i32, cz: i32) -> u32` | build + mesh voxel chunk (x ∈ [cx·32, cx·32+32), z likewise). Returns 0 ok. Results readable until the next build call. |
| `vw_mesh_ptr(kind: u32) -> u32`, `vw_mesh_len(kind: u32) -> u32` | kind 0 = opaque quads, 1 = water quads, 2 = foliage quads (alpha-tested leaves). Length in BYTES. |
| `vw_chunk_ymin() / vw_chunk_ymax() -> i32` | vertical bounds of the last chunk (for culling) |
| `vw_build_far(cell: u32, x0: i32, z0: i32, n: u32) -> u32` | heightfield tile: (n+1)² vertices at world (x0 + i·cell, z0 + j·cell), band-limited by `column_lod(footprint = cell)` + `canopy_lod`. |
| `vw_far_ptr() -> u32`, `vw_far_len() -> u32` | far tile bytes |
| `vw_height_at(x: f64, z: f64) -> f32` | top of terrain+water at a column (camera collision / spawn) |
| `vw_biome_at(x: f64, z: f64) -> u32` | `Biome as u8` (HUD shows the name) |
| `vw_material_table_ptr() -> u32`, `vw_material_count() -> u32` | per material 4×f32: linear albedo r,g,b + flags (bit0 grass-tinted, bit1 foliage-tinted, bit2 emissive, bit3 leaves) |
| `vw_find_spawn(x: f64, z: f64) -> u32` + `vw_spawn_ptr()` | 6×f32 (x,y,z,yaw,pitch,0): a nice dry viewpoint near (x,z) (for custom seeds) |

### Chunk vertex format (kinds 0–2): 12 bytes per vertex, 4 vertices per quad (CCW from outside)

| bytes | field |
|---|---|
| 0 | x local 0..32 (u8) |
| 1 | z local 0..32 (u8) |
| 2–3 | y 0..512 (u16 LE) |
| 4 | normal index 0..5: +X, −X, +Y, −Y, +Z, −Z |
| 5 | material id (`Material as u8`) |
| 6 | ambient occlusion 0..3 (3 = fully lit), voxel corner AO |
| 7 | flags: bit0 = top-of-column surface, bits1.. reserved |
| 8–10 | tint sRGB8 (grass/foliage biome tint or 255,255,255) |
| 11 | sky light 0..255 (cheap: open sky above = 255; under overhang/canopy lower) |

JS uses one shared index buffer (quad q → 4q+0,1,2, 4q+0,2,3). Faces between two opaque blocks
are culled; water faces only where water meets air (top surface) — no water side walls inside water.
Meshing rules: build the chunk with a 1-column margin so border faces are culled correctly;
3D density band (overhangs, arches, shelters) INCLUDED; caves NOT carved (performance), except cave
entrances are optional; trees from `features_in(chunk ± MAX_FEATURE_RADIUS)` included (logs opaque,
leaves → kind 2). Target: ≤ 40 ms per chunk on a phone (≈ ≤ 15 ms on this x86 machine, measure).

### Far tile vertex format: 16 bytes per vertex

f32 terrain_top (incl. expected canopy), f32 water_level (or −1e9 when dry), u8×3 sRGB far albedo
(`materials::far_albedo` of the top material blended with leaves by canopy coverage), u8 material id.

## Viewer behaviour (index.html + worker.js)

* WebGL2, custom shaders, no libraries. A worker pool (2–3 workers, each with its own wasm instance)
  builds chunks and far tiles; transfers ArrayBuffers to the main thread.
* Near field: voxel chunks streamed around the camera (default radius ≈ 6 chunks = 192 blocks on a
  phone, adjustable). Far field: clipmap rings of far tiles to ≥ 8 km (cell 4 … 128). Skirts / overlap
  hide cracks; far tiles under loaded voxel chunks are hidden (or pushed down) to avoid z-fighting.
* Shading: procedural 16×16 pixel textures per material in the fragment shader (hash of block coords +
  face; mip-like fade by distance to avoid shimmer), sun Lambert + hemisphere sky ambient + AO,
  analytic atmosphere/fog (Rayleigh-ish colour shift with distance and toward the sun), sky gradient
  + sun disk, water with Fresnel sky reflection, waves, depth tint. Time-of-day slider moves the sun.
* Controls: phone — left virtual joystick (move), drag on right half (look), ▲/▼ buttons (up/down),
  speed toggle; desktop — WASD/QE + mouse drag (pointer lock optional), Shift = fast.
* UI: seed selector (1, 42, 2026, custom), bookmarks list (from bookmarks.json: per seed the 8
  scouted shots with Russian titles), time of day, view distance, HUD: biome name, altitude, FPS.
  Russian UI text. Loading indicator while the first chunks build.
* Must open in a working state: seed 42 at its panorama bookmark, already flying-ready.
