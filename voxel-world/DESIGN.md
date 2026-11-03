# Voxel World — mathematical terrain generator + offline renderer

Goal: a block world whose generation is *pure mathematics* (noise fields, fractal sums, splines,
implicit surfaces, point processes) — looking like a modern block-game world but richer — plus a
CPU ray tracer that produces 1920×1080 screenshots with a **render distance of 32 768 blocks**
(radius; 65 536 for the orbital shot) — a typical block game tops out at ~0.5 km. No GPU is available, no player, no mobs, no weather.

Hardware budget: **4 CPU cores, 15 GB RAM**. Always build with `--release`.
Budgets per screenshot: world build ≤ 30 s, render ≤ 90 s (1920×1080 with AA).
Default clipmap: level 0 = 2048² voxels, heightfield levels k=1..5 (cells 2…32 blocks, 2048² cells each) → 32 768-block radius. Orbital: 6 levels.
Earth curvature: the renderer drops geometry by d²/(2R), R = 6 371 000 m, relative to the camera (≈80 m at 32 km).

## 0. LATEST USER REQUIREMENTS — HIGHEST PRIORITY (critics: check these; engineers: implement)

**Playability: walkable terrain and natural building spots.** The user (who will later play in
this world) says: "a character will struggle to run around, there is no place to build a base or a
house, everything is crooked. I'm not asking for flat things, but improve it."
* **Walkability.** Remove high-frequency "stair-step" roughness from ordinary terrain: outside
  mountains/cliffs, neighbouring columns should differ by ≤ 1 block almost everywhere (target:
  ≥ 80 % of non-mountain land columns have max |Δh| to their 4 neighbours ≤ 1). Shape comes from
  smooth large-scale undulation, not per-block noise. Fine detail amplitude must fade where it would
  create constant 1–2 block steps.
* **Bimodal slopes (natural).** Concentrate steepness into distinct cliffs, escarpments and rock
  bands; between them, long gentle walkable slopes — not a uniform "cheese grater".
* **Natural flat spots** (geomorphologically motivated, smooth soft edges, never perfectly square):
  river terraces (flat shelves along rivers at a few levels), hillside benches (20–80 block shelves),
  plateau meadows, broad floodplains, besides the large plains. Target: every 300×300 block region of
  non-mountain land contains at least one ≥ 16×16 area with height range ≤ 2.
* Add a test/metric that measures both targets on seeds 1, 42, 2026 and report the numbers.

## Conventions

* 1 unit = 1 block = 1 m. **y is up.** World y ∈ [0, 512). `SEA_LEVEL = 140`: water fills y < 140.
* The block at integer (x,y,z) occupies [x,x+1)×[y,y+1)×[z,z+1). Column functions are sampled at
  block centres (x+0.5, z+0.5).
* Directions: yaw 0 → +Z, yaw +90° → +X, pitch > 0 looks up (`math::dir_from_yaw_pitch`).
  Sun azimuth uses the same convention. Camera basis: `render::Camera::basis()`.
* Determinism: every value is a function of (seed, coordinates). No time/thread-dependent
  randomness. Use `noise::derive_seed(seed, noise::salt("name"))` for sub-seeds.
* Colours are linear RGB; sRGB only at PNG output.

## File ownership

| File(s) | Owner | Notes |
|---|---|---|
| `math.rs`, `noise.rs`, `spline.rs`, `materials.rs`, `imageio.rs`, `gen/mod.rs`, `world/mod.rs`, `render/types.rs`, `shots.rs`, `lib.rs` | lead | **FROZEN** — use, never edit |
| `gen/climate.rs`, `gen/terrain.rs`, `gen/biome.rs` (+ `gen/*_util.rs` you add) | gen-core | |
| `gen/caves.rs`, `gen/features.rs` | caves-features | |
| `world/build.rs` | world-build | |
| `render/mod.rs` (keep `render`/`save_png` signatures) + any new `render/*.rs` | render | |
| `scout.rs`, `main.rs`, `map.rs` (new, add `pub mod map;` is NOT allowed in lib.rs — put map code in `scout.rs` or main.rs) | scout-main | |
| `examples/<owner>_*.rs`, `target/agent-<owner>/` | each owner | debug tools / images |

Other agents edit other files *concurrently*. If the build breaks because of a file you do not
own, wait a minute and retry — never edit it. If a frozen API is insufficient, work around it in
your own files and mention it in your report.

## Noise toolkit (`noise.rs`, frozen)

`Simplex` (2D with analytic gradient `n2d`, 3D `n3`; output ≈ [-1,1], std ≈ 0.54),
`Fbm` (per-octave seeds + rotation; `sample2`, band-limited `sample2_lod(x,z,footprint)`,
`sample3`; output normalised by amplitude sum so typical std is only ≈ 0.2–0.3 — **calibrate
thresholds empirically**), `Ridged` (Musgrave ridged multifractal, [0,1]), `ErodedFbm`
(derivative-damped fBm: Σ aᵢ·nᵢ / (1 + k·|Σ∇n|²) → erosion-like slopes), `Warp2` (domain warp),
`Worley2` (Voronoi F1/F2/cell id), hashing (`hash2`, `hash3`, `hash_f01`, `mix64`).
`spline.rs`: monotone cubic (Fritsch–Carlson) **nested** splines: control-point values may
themselves be splines over another input → smooth multi-parameter terrain shaping.

## 1. Climate (gen-core, `climate.rs`)

`ClimateSampler::sample(x, z, footprint) -> Climate` (`footprint` > 1 ⇒ use `*_lod` variants).
Suggested fields (tune freely, keep semantics documented in `gen/mod.rs::Climate`):

| field | construction | target distribution |
|---|---|---|
| continentalness C | `Warp2`(λ≈2400, A≈700) then `Fbm`(λ≈5200, 7 oct) | ≈45 % ocean (C < -0.18), ≈8 % coast band, rest inland; oceans must be big connected seas, continents several thousand blocks across |
| erosion E | warped `Fbm`(λ≈2600, 5 oct) | spread over [-1,1] |
| weirdness W | `Fbm`(λ≈1100, 5 oct); **pv = 1 − |3|W| − 2|** | |
| temperature T | `Fbm`(λ≈4200, 4 oct) | [-1,1], hot and cold regions both present within ~8 km |
| humidity H | `Fbm`(λ≈3200, 4 oct) | [-1,1] |
| river R | `|Fbm(λ≈1800, 4 oct)|` on warped coords; R = 1 − smoothstep(w₀, w₁, |n|) | thin meandering network; width grows with erosion (wide in flat land, narrow canyons in mountains); none in ocean |

Raw fBm has a bell-shaped distribution: **calibrate** each field with a monotone remap (measure
quantiles over ~200 k samples in a test, then map them to the target range — empirical CDF
equalisation). Commit the measured constants.

## 2. Terrain height & 3D density (gen-core, `terrain.rs`)

`TerrainShaper::column(climate, x, z, footprint) -> ColumnInfo` and `density(x,y,z,col)`.

Height H(x,z) is built from **nested splines** of (C, E, PV) plus fractal detail:

* **Base offset** O(C): deep ocean ≈ 40–70, ocean ≈ 100–120, coast ≈ 136–144, lowland ≈ 146–160,
  inland ≈ 160–200, far inland plateaus ≈ 200+. Coasts: high erosion → gentle beaches;
  low erosion → sea cliffs (step in the spline).
* **Mountain amplitude** A(C,E): 0 near the sea, up to ≈ 300 blocks for E ≈ −1 inland. Peaks reach
  y ≈ 450–490 (world height 512).
* **Shape**: mountains = A · mix(hills, ridged, smoothstep(−0.2, 0.6, pv)); valleys lowered where
  pv < −0.4. Hills from `ErodedFbm` (λ≈900, 8 oct, steepness≈1–2) → eroded gullies and sharp
  ridges; small-scale roughness ±2–4 blocks (λ≈40).
* **Rivers**: carve H toward a bed at SEA_LEVEL − 3…−6 with a smooth U/V cross-section using
  `climate.river`; in mountains this yields canyons with steep walls. Rivers meet the sea naturally.
* **Special landforms (mathematical, per biome climate region)**:
  * Badlands mesas: smooth terraces `h' = (⌊h/s⌋ + smoothstep(0.82, 1, frac(h/s)))·s` (s≈8–14) →
    flat-topped mesas with vertical cliffs.
  * Hoodoos: Voronoi (`Worley2`, cell≈14) spires where F1 < r: tall rounded pillars.
  * Desert dunes: `A·(1 − |sin(k·(p·w) + φ(p))|)^1.5`, w = fixed wind direction, φ = fBm warp,
    A ≈ 5–14, plus barchan modulation.
  * Volcanic: rare Worley-cell volcano cones `Hv·max(0, 1 − d/R)^1.6` with a caldera at the top
    (crater lake = lava: world builder fills liquid in Volcanic biome above sea level with Lava).
  * Optional: glacier tongues, alpine tarns (Worley basins with a fixed water level).
* **3D density**: d = (H − y) + J(x,z)·N₃(x, 1.4y, z) with N₃ a 3D fBm (λ≈60–80, 3 oct) and
  J = `jaggedness` (0 on plains, up to ≈ 20–30 on peaks/cliffs/coastal bluffs) → overhangs,
  arches, rock shelves. Outside the band [H−J−2, H+J+2] sign(d) = sign(H−y) exactly.
  Avoid lots of tiny floating blobs.
* `water_level`: SEA_LEVEL when H < SEA_LEVEL (oceans/rivers); swamp puddles slightly above;
  `i32::MIN` when dry.
* `slope` = |∇H| (finite differences or analytic), `surface_temperature` = T − lapse·max(0, H−SEA)
  (≈ −1 per 250 m), `biome` = `biome::classify(...)`.
* Band-limited LOD: when `footprint` > 1 use `sample2_lod` everywhere so far heightfields don't alias.

### 2a. USER REQUIREMENTS (override anything above/below that conflicts)

**Mountains: rarer but bigger.** Mountains must concentrate in a few LARGE ranges instead of
being everywhere. Gate the mountain amplitude A with a low-frequency "orogeny belt" mask
(e.g. `Fbm` λ ≈ 9000, smoothstep threshold) so that mountainous terrain (H > SEA+150) covers only
≈ 10–15 % of land, while those ranges are massive: main peaks y ≈ 430–500, broad foothills,
range widths of several km and lengths of 10–30 km. Between ranges: plains, rolling hills,
plateaus, river valleys, mesas.

**Snow is altitude-driven, never a lowland biome.** A single snow line at y ≈ 330
(≈ 190 m above sea), modulated ±12 blocks by low-frequency noise and at most ±15 by climate
temperature. Above the snow line snow covers exposed surfaces whose slope is below a threshold that
grows with altitude (gentle faces get snow first → snow in gullies/couloirs, bare rock on cliffs);
above y ≈ 420 permanent snow/packed ice (glaciers) on everything but near-vertical faces.
**Below the snow line there is no snow at all**: cold lowlands are taiga/tundra-like with grass,
podzol, gravel, moss, spruce — no white ground, no frozen oceans/rivers (ice may appear only on
lakes above the snow line). The snowy biomes (SnowySlopes, SnowyPeaks, Glacier, AlpineGrove,
SnowyPlains/SnowyTaiga/SnowyBeach/FrozenOcean/FrozenRiver) must therefore be assigned by ALTITUDE
zones relative to the snow line (or become very rare), not by lowland climate.

**Treeline (features + biomes).** Trees thin out and shrink with altitude: full forests up to
≈ snow line − 90, then sparse stunted conifers/bushes, none above ≈ snow line − 30. Slope
also reduces density. Result on every big mountain: forest → alpine meadow → bare rock → snow.

**Rain shadow (climate) — wind is a FIELD, not one direction.** Prevailing (climatic, static)
wind varies across the world like real circulation cells: wind = curl of a low-frequency scalar
"pressure" potential ψ (e.g. `Simplex::n2d` / fBm of 2–3 octaves, λ ≈ 20–40 km):
w(x,z) = (∂ψ/∂z, −∂ψ/∂x), normalised. This field is divergence-free and swirls around highs/lows,
so different regions get different wind directions with smooth turns between them. Effective
humidity = base humidity − k·(mountain mass upwind), where "upwind" means stepping BACKWARD along
the local wind field (streamline tracing, 6–10 steps of 400–800 blocks, using only the
low-frequency mountain-amplitude fields so it stays cheap). Windward slopes wet (forest/jungle),
leeward sides dry (steppe/savanna/desert); coasts facing the local wind are wetter.

**Anti-monotony (everyone).** The world must never feel uniform:
* Regional terrain "character" from a low-frequency selector: rolling hills, flat meander plains,
  high plateaus with escarpments (and canyons where rivers cut them), mesa country, dune seas,
  archipelagos/skerries along some coasts, volcanic fields — each covering 1–5 km, blended smoothly.
* Inland lakes: Worley-cell basins with a flat water level (lake shores, islands), some at altitude.
* Biome patches of very different sizes (some small groves/clearings inside big forests,
  meadows inside taiga, oases in desert), and sub-biome variation (forest density, tree mix,
  surface material patches) driven by extra mid-frequency noise.
* No two shots of the same world should look alike.

## 3. Biomes & surface rules (gen-core, `biome.rs`)

All 30 `Biome`s should appear somewhere (Volcanic and Hoodoos rare). Classification uses C, E,
pv, T (surface), H, height, slope, river. Key rules: snow line from surface temperature;
peaks (stony/snowy/glacier) from altitude + pv; beaches only on gentle coasts; stony shore on steep
ones; rivers; swamps in hot-humid low flats; jungle hot-wet; desert hot-dry; badlands hot-very-dry
with low-mid erosion; savanna hot-medium; taiga/snowy taiga cold; dark forest wet-temperate, etc.

`SurfaceRules::material(x, y, z, depth, col)` — must be **cheap** (hash + at most one or two noise
evaluations). Examples: grass/dirt(3–4)/stone; sand(4–6)+sandstone(8) in deserts/beaches; red sand
+ banded `Terracotta` (≈40 deep) in badlands; podzol patches in taiga; mud/clay in swamps; snow on
cold flat tops, bare stone on slopes > ≈1.3; packed ice sheets on glaciers; basalt + obsidian in
volcanic; ocean floors sand/gravel/clay by depth; **underground**: stone → deepslate below
y≈64 (dithered), bedrock y < 3 (dithered), granite/andesite/calcite strata blobs from low-freq 3D
noise (visible in the cut-away screenshot).

## 4. Caves (caves-features, `caves.rs`)

`field(x,y,z,col) > 0` ⇒ carve. Must be smooth on a ~4-block scale (it is sampled on a 4×4×4
lattice and trilinearly interpolated). Combine (max):

* **Cheese caverns**: 3D fBm (λ≈100 xz, y squashed ×1.6) minus a threshold rising toward the
  surface (rarely breaches).
* **Spaghetti tunnels**: two independent 3D noises a, b; tunnel = w − √(a²+b²): the tube around
  the *intersection curve of two implicit surfaces* a=0 ∧ b=0. Radius ≈ 2–5 blocks. May breach the
  surface (cave entrances), gated by a 2D noise.
* **Ravines**: rare 2D river-like slots, deep and narrow (width shrinks with depth).
* Never carve y < 4, never within 4 blocks below the floor of a water-covered column.
`fill(x,y,z,col)`: lava below y≈12; water in aquifers (below a low-frequency local water table where an
aquifer noise is high); else air.

## 5. Features (caves-features, `features.rs`)

`features_in(gen, x0,z0,x1,z1)` returns features whose **root** is in the rectangle —
deterministic and independent of the query rectangle (jittered grid / blue-noise with per-cell
hashing, e.g. cell 5–7 blocks, priority-based rejection against neighbours for minimum spacing).
Roots: `gen.column(...)`; skip underwater, slope > 0.9, jaggedness > 6. Shapes are analytic
(rasterised per column into `FeatureColumn` runs): ellipsoid/noisy-sphere canopies, layered cone
conifers with ring modulation, 2×2 mega spruce & mega jungle trees, acacia with bent trunk and flat
discs, dark oak 2×2 with wide flat canopy, birch, bushes, cacti, boulders, ice spires.
Densities per biome: dense forests (closed canopy in dark forest/jungle), sparse savanna, rare
trees on plains, none on peaks. Respect `MAX_FEATURE_RADIUS = 12`.
`canopy_lod(x,z,col,footprint)`: statistical expectation (coverage, mean canopy height, leaf
material) of the same point process — for far LOD.

### 5a. USER REQUIREMENTS — tree variety (reviewers: IMPLEMENT if missing, not optional)

The user wants MUCH more tree variety, produced by a fast parametric generator ("one generator,
endless variations"), not a handful of fixed templates:

* **Tree genome.** Every tree = species template + continuous parameters drawn from its hash:
  height (log-normal age classes: sapling / young / mature / old), trunk thickness (1, 2×2, 3×3, up
  to 5×5 for giants), trunk lean and curvature (quadratic Bézier spine), branch count / angles /
  lengths (3D line rasterisation), crown family (ellipsoid, stacked cones, umbrella, columnar,
  multi-blob clusters, weeping curtain), crown asymmetry, leaf density / holes (hash dithering),
  occasional dead branches, snags (dead standing trees), stumps and fallen logs (horizontal logs).
  No two trees identical; species stay recognisable.
* **Spruce biomes.** Taiga: mixed spruce sizes 8–25 with clusters and clearings. Old-growth taiga:
  mega spruce (2×2, 30–45 tall) dominate, with occasional **colossal spruce (3×3, 50–70 tall)**.
  Alpine grove: smaller, wind-swept (lean away from the local prevailing wind if available).
* **Rare landmark giants** ("ancient trees"): a separate very sparse point process (grid cell
  ≈ 200–300 blocks, low acceptance by biome) — roughly one per few hundred metres in forests,
  rarer in open land: ancient oak with huge spreading branches (radius ≈ 16–24), giant sequoia-like
  conifer (3×3–5×5 trunk, 60–90 tall, columnar crown), jungle emergent giant towering above the
  canopy with an umbrella crown, baobab-like fat trunk in savanna, ancient weeping willow-like tree
  by rivers/swamps, lone giant oak in a meadow. Buttress roots may use `replace_solid`. Normal
  trees are suppressed inside a landmark's crown (deterministically: query the landmark grid), so
  placement stays independent of the query rectangle.
* `MAX_FEATURE_RADIUS` is now **24**. Rasterise analytically per column (intervals of ellipsoids /
  cones, Bézier trunks, DDA branches), merge runs, no per-voxel allocations — tree generation must
  stay cheap (giants are rare). Only existing `Material`s may be used (materials.rs is frozen).
* `canopy_lod` must include the expected contribution of landmark giants and taller spruces.
* **Beauty guardrails (hard rule).** Variety must never make trees ugly: every species keeps its
  recognisable silhouette; parameters are correlated (crown size ∝ height, trunk thickness ∝ height,
  branch length ∝ crown radius) and clamped to natural ranges (lean ≤ ~15°, mild asymmetry, sparse
  holes); no floating leaves, no leaf blobs detached from branches, no trunk poking through the
  crown top, no stair-stepped "noise mess". Render a catalogue of ≥ 8 random instances per species
  + the landmark giants, LOOK at it, and tighten ranges until every instance looks good.

## 6. World build (world-build, `world/build.rs`)

`build_world(gen, params) -> World` (storage types in frozen `world/mod.rs`).

* **Level 0** (`l0_size`², exact voxels): per 16×16 chunk in parallel (rayon). For each column:
  `gen.column(x+0.5, z+0.5)`; below the 3D band solid, above air, inside the band evaluate
  `gen.density` on a 4×4×4 lattice (needs `ColumnInfo` at lattice xz) with trilinear interpolation.
  Caves within `cave_radius`: carve where interpolated `cave_field` > 0 (same lattice), fill via
  `cave_fill`. Liquids: water (Ice top in frozen biomes, Lava for Volcanic lakes above sea level)
  from the top solid up to `water_level`. Materials: `gen.surface_material` with `depth` counted from
  the top of each solid run — per block inside `cave_radius` (cut-away needs real strata), only the
  top ~24 of each run outside it (deeper = Stone/Deepslate by y). Merge equal neighbours into spans.
* **Features** within `feature_radius`: gather `features_in` per chunk (expand by
  `MAX_FEATURE_RADIUS`), place at the root column's top solid +0, fill air only unless
  `replace_solid`, logs override leaves. Seamless across chunk borders.
* **Heightfield levels** k = 1..=hf_levels via `HeightLevel::from_fn`: `column_lod(x,z,cell)`,
  `canopy_lod`, h = terrain + coverage·canopy height, water = `water_level` if above terrain,
  albedo = `materials::far_albedo` of the top material blended with leaves by coverage.
* Log timings per stage; ≤ 30 s for defaults (5 heightfield levels); memory ≤ 4 GB.

## 7. Renderer (render, `render/*.rs`)

`render(world, cam, sun, settings) -> Image` (linear HDR) and `save_png(img, path)`.

* **Traversal.** Level 0: 2D DDA over columns along the ray (Amanatides–Woo) with hierarchical
  empty-space skipping on the u16 max-mip; inside a column intersect the ray's y-interval with the
  spans → exact voxel hit + face normal. Heightfield levels: for the ray portion inside level k's
  square but outside the finer level's square, max-mipmap quadtree traversal; at the leaf hit the
  flat-top cell box or water plane; shade with a smooth normal from neighbouring heights.
  Diorama mode: level 0 only, columns outside the box (and y < min_y) are empty, studio backdrop.
  **Validate traversal against brute-force stepping in unit tests.**
* **Lighting.** Sun (with ray-traced shadows on all levels) attenuated by atmospheric
  transmittance; sky ambient from the sky model; ambient occlusion (voxel corner AO on level 0 +
  a few short hemisphere rays in caves/overhangs; heightfield horizon AO on far levels);
  emissive lava/crystal.
* **Textures.** Procedural 16×16 per-face pixel textures per material (hash-based texels +
  material structure: stone speckles, grass top + side fringe, sand grains, gravel, bark stripes /
  ring tops, leaves with dark gaps, snow, ice, terracotta bands by y, ore specks in stone).
  Texture LOD: fade texel detail when a texel is smaller than a pixel (avoid shimmer).
* **Water.** Ripple normals from 2D noise derivatives, Schlick Fresnel, ray-traced reflection,
  depth-based absorption/scattering for refraction, sun glint.
* **Sky & atmosphere.** Single-scattering Rayleigh + Mie (+ optional ozone) integrated
  numerically; sky-view LUT per frame; sun disk; **aerial perspective** along every view ray
  (in-scatter + transmittance) — this is what sells the 32 km distance. `haze` scales densities.
* **Image.** ACES-style tone map, exposure, sRGB, light dithering. Adaptive AA: 1 spp, then
  4–8 extra samples on pixels with high local contrast. rayon over tiles/rows.
* Budget ≤ 90 s for 1920×1080 on 4 cores.

## 8. Scouting & CLI (scout-main, `scout.rs`, `main.rs`)

`plan_shots(gen) -> Vec<Shot>` — exactly 8 shots per world, chosen automatically by scanning a
coarse grid (step ≈ 48–64, radius ≈ 12 km) with `column_lod`, scoring candidates, refining
locally, and choosing view directions by evaluating terrain along candidate rays:

1. `panorama` — from a prominent summit, long view (sun side-lit, ~25°).
2. `river_valley` — along a river valley / canyon.
3. `coast_sunset` — coastline (cliffs or beach), sun 3–6° above horizon near the view direction.
4. `drylands` — badlands/hoodoos mesas if present, else desert dunes, else savanna; low grazing sun.
5. `alpine` — snowy peaks / glacier range, looking up at peaks.
6. `forest` — jungle / dark forest / old-growth taiga canopy from just above.
7. `cutaway` — Diorama: a ~160–200 block square cut out of mountainous terrain with caves,
   viewed diagonally from outside; build with small l0 (256), `hf_levels` 0.
8. `orbital` — from ~3 km altitude looking over continents and ocean (hf_levels 6 → 65 km radius).
Normal shots use hf_levels 5 (32 768-block radius).

Shots ≥ ~1500 blocks apart where possible; camera never inside terrain.
CLI (std only, no clap):
`voxel-world shots --seed S --out DIR [--width W --height H] [--only 1,3] [--fast] [--no-aa]`
→ `DIR/seed{S}_{idx}_{name}.png` + `DIR/seed{S}_shots.json`, timings on stderr.
`voxel-world map --seed S --out FILE [--size 1024 --scale 16] [--with-shots]` → shaded-relief
biome map. `--fast` = 960×540, smaller level 0.
