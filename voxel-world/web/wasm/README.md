# vw_web — the generator as WebAssembly

Implements `web/API.md`. Build: `web/build.sh` (→ `web/www/gen.wasm`), tests:
`cd web/wasm && cargo test --release -- --nocapture` (prints timings).
Debug view of the raw meshes: serve `web/` and open `tools/preview/preview.html?seed=42&bm=forest`
(`&mode=ao|sky|mat`, `&spawn=1`, `&merge=0..7`); `node tools/preview/shoot.js out.png '<query>'`.

The module has **no imports**. After every call, re-create typed-array views on `memory.buffer`
(memory may grow).

## Exports — details beyond API.md

| fn | returns |
|---|---|
| `vw_build_chunk` | 0 ok, 1 = `vw_init` not called |
| `vw_build_far` | 0 ok, 1 = not initialised, 2 = bad size (`n` 1..1024, `cell` ≥ 1) |
| `vw_find_spawn` | 0 ok (6×f32 at `vw_spawn_ptr`: x, y, z, yaw, pitch, 0 — y is the eye, ≈ 32 above ground), 1 = not initialised |
| `vw_height_at` | first free y above terrain + liquid (no trees, no caves), matching the chunk voxels except at cave mouths |
| `vw_solid_at(x, y, z)` | 1 = the block holds rock / water / lava / ice exactly as the exact chunk voxelises it (caves carved, trees ignored), 0 = air or not initialised. Caches the last 16 block columns |
| `vw_set_merge(mask)` | *extension*: bit k = merge coplanar +Y faces of kind k. Default `0b110` (water + foliage). `7` also merges opaque tops (−10…20 % quads on land, but T-junction sparkles possible) |
| `vw_chunk_quads()` | *extension*: total quads of the last chunk |

## Chunk vertices (12 bytes, API.md) — semantics

* Quads: 4 vertices CCW from outside; the start vertex is rotated so the 0–2 diagonal joins
  the darker AO pair (no AO anisotropy) — use the shared index buffer as specified.
* Merged quads (water / foliage tops by default) span several blocks: derive per-block texture
  coordinates from the world position (`floor(pos − normal·0.5)`), never from the vertex.
* Culling: opaque faces toward air / water / leaves; water faces toward air / leaves only (all
  directions, in practice tops); **leaf faces only toward air / water (leaf–leaf faces culled) →
  render foliage double-sided** with alpha test.
* AO 0..3: voxel corner AO; opaque blocks and leaves occlude.
* Sky 0..255 per vertex: mean openness of the 2×2 columns around the vertex at the air-side
  height (sky 1.0, under leaves only 0.55, under rock 0.15 fading to 0 from 4 to 16 blocks below
  the column's top opaque block) → open ground 255, cliff walls ≈ 147, forest floor ≈ 140,
  overhang undersides ≈ 38, cave interiors 0.
* Caves (exact chunks only, depth-limited ≈ 90–120 blocks below the surface): see `src/terrain.rs`
  and `src/chunk.rs`. `cargo test --release find_cave_entrances -- --ignored --nocapture`
  (`VW_SEED`, `VW_CX`, `VW_CZ`) lists surface entrances near a chunk; `VW_NO_CAVES=1` makes
  `timings_per_chunk` measure the same binary without caves.
* Flags bit0: the block's top neighbour is not opaque (exposed top) — set on all faces of that
  block (grass side fringe, snow caps …).
* Tint: grass/foliage biome tint (surface temperature + humidity); **Terracotta: per-y strata
  colour as a ratio** (table albedo = palest band, tint = band / palest, so `albedo × tint` is the
  exact `terracotta_band` colour). Multiply tint unconditionally; 255,255,255 otherwise.

## Far tile vertices (16 bytes)

`f32 top, f32 water (−1e9 dry), u8 r,g,b (sRGB), u8 material` = 12 bytes as in API.md, then
4 extra bytes: `u8 canopy coverage (0..255), u8 biome, u8 canopy height (blocks), u8 0`.
Order: j (z) outer, i (x) inner.

## Material table

40 × (r, g, b, flags) f32, flags stored as a **number** (e.g. 10.0 = foliage-tinted leaves):
bit0 grass-tinted, bit1 foliage-tinted, bit2 emissive (lava, crystal), bit3 leaves,
bit4 banded tint (terracotta, see above). Grass/Moss albedo is white (tint carries the colour).

## Measured (x86, 1 thread; load-dependent)

Native chunk: avg ≈ 7 ms cold / 4.5 ms warm (streaming, feature tiles memoised) over 10 terrain
types × 2 seeds; dense forests / cliffs ≤ 13 ms cold. Wasm in V8 ≈ 6–8 ms. Far tile 65² ≈ 17–25 ms.
≈ 2 600 quads (≈ 120 KB) per chunk on average; forests up to ≈ 6 000.
