//! Terrain voxelisation shared by the chunk builder, feature roots, camera height and spawn.
//!
//! Semantics follow `world/build.rs` (level 0):
//! * the 3-D band uses the world-aligned residual lattice `4ℤ³ + ½`:
//!   `ρ̂(x,y,z) = H(x,z) − y + I₄[R]`, `R_n = ρ(p_n) − (H_n − y_n)` (0 outside a node's band),
//!   evaluated only inside `[H − Jmax − 1.5, H + Jmax + 1.5]` (Jmax over the 4 corner nodes);
//!   below solid, above air; pure heightfield `y < ⌈H − ½⌉` when all 4 corners have J = 0.
//! * terrain air below `water_level` is liquid: lava for Volcanic lakes above sea level, else
//!   water; the top block freezes to ice for frozen biomes / lakes above the snow line.
//! * caves (exact chunks and point queries only): `F̂ = I₄[F − τ]` on the same lattice, F the
//!   carver field (`WorldGen::cave_field`), carving solid blocks with `F̂ > 0` at
//!   `y ≥ MIN_CARVE_Y`, never within `WET_GUARD` blocks below the top of a water-covered column;
//!   carved blocks take `cave_fill` (air / aquifer water / lava). Unlike the world builder's
//!   radial taper, τ is a DEPTH taper: `τ = 30·((D − CAVE_D0)/CAVE_W)²₊` with D the depth below
//!   the node's smooth surface, so passages pinch out smoothly ≈ 95–120 blocks down and the
//!   lattice below `CAVE_D0 + CAVE_W` is never evaluated (`CAVE_OUT`); above `H + J + 8` the
//!   carver itself returns a cheap negative value; odd levels squeezed between two clearly
//!   negative even levels are interpolated (`CAVE_SKIP_T`). A deep cave is therefore closed by a
//!   smooth floor; the lava level (y ≤ 11) is only reached below ground lower than y ≈ 120.
//!
//! Every value is a pure function of world coordinates, so chunks agree on shared borders and
//! trees rooted in one chunk land at the same y in every chunk that sees them.
use std::collections::HashMap;
use voxel_world::gen::caves::MIN_CARVE_Y;
use voxel_world::gen::{Biome, CaveFill, ColumnInfo, WorldGen, SEA_LEVEL, WORLD_HEIGHT};
use voxel_world::materials::Material;
use voxel_world::noise::{derive_seed, salt, Simplex};

pub const LAT: i32 = 4;
pub const WH: i32 = WORLD_HEIGHT;

/// Caves run at full strength down to this depth below a lattice node's smooth surface…
pub const CAVE_D0: f32 = 88.0;
/// …then pinch out over this many blocks (τ = 30·u², u = (D − D0)/W); deeper levels are not
/// evaluated at all.
pub const CAVE_W: f32 = 32.0;
/// Lattice value of the never-evaluated deep levels (always "rock").
pub const CAVE_OUT: f32 = -64.0;
/// No carving within this many blocks below the top of a water-covered column (as the world
/// builder: sea / lake / river beds never leak).
pub const WET_GUARD: i32 = 6;

/// Cave lattice value at node column `col` (node position `col.x`, `col.z`), lattice level j
/// (y = 4j + ½): `F − τ(depth)`, or `CAVE_OUT` below the taper / below the carve floor. A pure
/// function of (seed, node, j) — memoised and direct evaluations are bit-identical.
#[inline]
pub fn cave_level(gen: &WorldGen, col: &ColumnInfo, j: i32) -> f32 {
    if j * LAT + LAT <= MIN_CARVE_Y {
        return CAVE_OUT;
    }
    let y = (j * LAT) as f64 + 0.5;
    let u = (col.height - y as f32 - CAVE_D0) * (1.0 / CAVE_W);
    if u >= 1.0 {
        return CAVE_OUT;
    }
    let tap = if u > 0.0 { 30.0 * u * u } else { 0.0 };
    gen.cave_field(col.x, y, col.z, col) - tap
}

/// Odd lattice levels are not evaluated where both even neighbours (4 blocks above and below) are
/// below −CAVE_SKIP_T: the carver field is ≈ a signed distance in blocks with features ≥ 5 blocks
/// across (tubes r ≥ 2.6: 4 blocks from a tube centre F ≥ −1.9), so a level squeezed between two
/// points ≥ 10 blocks outside any cavity is rock too; it takes the mean of the two (< −T, so
/// no cell around it changes sign). Saves ≈ 1/3 of the evaluations (deep rock, cover zone).
pub const CAVE_SKIP_T: f32 = 10.0;

/// The value a node memo holds at level j (with the odd-level skip). Pure function of
/// (seed, node, j): `Nodes::ensure_cave` and direct evaluation agree bit for bit.
pub fn node_cave_level(gen: &WorldGen, col: &ColumnInfo, j: i32) -> f32 {
    let (j0, j1) = cave_range(col);
    if j & 1 == 1 && j > j0 && j + 1 < j1 {
        let a = cave_level(gen, col, j - 1);
        let b = cave_level(gen, col, j + 1);
        if a < -CAVE_SKIP_T && b < -CAVE_SKIP_T {
            return 0.5 * (a + b);
        }
    }
    cave_level(gen, col, j)
}

/// Lattice levels worth memoising for a node: [j0, j1). Below j0 the value is `CAVE_OUT`, above
/// j1 the carver's early exit (y > H + J + 8) makes a direct evaluation nearly free.
#[inline]
fn cave_range(col: &ColumnInfo) -> (i32, i32) {
    let lo = col.height - CAVE_D0 - CAVE_W - 0.5;
    let j0 = (((lo / LAT as f32).floor() as i32) + 1).clamp(MIN_CARVE_Y / LAT, WH / LAT + 1);
    let hi = col.height + col.jaggedness.max(0.0) + 8.5;
    let j1 = ((hi / LAT as f32).ceil() as i32 + 1).clamp(j0, WH / LAT + 1);
    (j0, j1)
}

/// Carve decision inside one lattice cell from its two (column-blended) level values; `F̂` is
/// linear in y between them. A cell whose ends are both ≤ 0 is skipped (as the world builder).
#[inline]
pub fn carves(f0: f32, f1: f32, fy: f32) -> bool {
    (f0 > 0.0 || f1 > 0.0) && f0 + (f1 - f0) * fy > 0.0
}

/// Words of a per-level bit mask (levels 0 ..= WH/LAT + 1).
pub const MW: usize = (WH / LAT) as usize / 64 + 1;

/// Memoised cave lattice column of a node (`n < 0` = not evaluated yet); `pos` has bit j set
/// where the value at level j is > 0 (outside [j0, j0 + n) it never is).
#[derive(Clone, Copy)]
pub struct CaveCol {
    pub j0: i32,
    pub n: i32,
    pub off: u32,
    pub pos: [u64; MW],
}

/// One lattice node column: its `ColumnInfo` and the band residuals `R(j)` for
/// `j ∈ [j0, j0 + nj)` (node y = 4j + ½); 0 elsewhere.
#[derive(Clone, Copy)]
pub struct Node {
    pub col: ColumnInfo,
    pub j0: i32,
    pub nj: i32,
    pub off: u32,
}

/// Memo of lattice nodes (persistent across builds, bounded).
pub struct Nodes {
    map: HashMap<(i32, i32), u32>,
    pub nodes: Vec<Node>,
    pub pool: Vec<f32>,
    /// Cave lattice columns, index-aligned with `nodes`, values in `cpool`.
    pub cave: Vec<CaveCol>,
    pub cpool: Vec<f32>,
}

impl Default for Nodes {
    fn default() -> Self {
        Self::new()
    }
}

impl Nodes {
    pub fn new() -> Self {
        Nodes { map: HashMap::new(), nodes: Vec::new(), pool: Vec::new(), cave: Vec::new(), cpool: Vec::new() }
    }
    pub fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.pool.clear();
        self.cave.clear();
        self.cpool.clear();
    }
    /// Drop everything when the memo grows past `cap` nodes (call only between builds).
    pub fn trim(&mut self, cap: usize) {
        if self.nodes.len() > cap {
            self.clear();
        }
    }
    #[inline]
    pub fn res(&self, n: u32, j: i32) -> f32 {
        let nd = &self.nodes[n as usize];
        let k = j - nd.j0;
        if k < 0 || k >= nd.nj {
            0.0
        } else {
            self.pool[nd.off as usize + k as usize]
        }
    }
    #[inline]
    pub fn jag(&self, n: u32) -> f32 {
        self.nodes[n as usize].col.jaggedness.max(0.0)
    }
    /// Node at lattice index (nx, nz) = block (4nx, 4nz). `known` may supply its column.
    pub fn get(&mut self, gen: &WorldGen, nx: i32, nz: i32, known: Option<&ColumnInfo>) -> u32 {
        if let Some(&i) = self.map.get(&(nx, nz)) {
            return i;
        }
        let (x, z) = ((nx * LAT) as f64 + 0.5, (nz * LAT) as f64 + 0.5);
        let col = match known {
            Some(c) => *c,
            None => gen.column(x, z),
        };
        let jj = col.jaggedness;
        let mut node = Node { col, j0: 0, nj: 0, off: 0 };
        if jj > 0.0 {
            let ja = (((col.height - jj - 0.5) / LAT as f32).floor() as i32).clamp(0, WH / LAT);
            let jb = (((col.height + jj - 0.5) / LAT as f32).ceil() as i32).clamp(ja, WH / LAT);
            node.j0 = ja;
            node.nj = jb - ja + 1;
            node.off = self.pool.len() as u32;
            for j in ja..=jb {
                let y = (j * LAT) as f64 + 0.5;
                let base = col.height - y as f32;
                let v = if base.abs() < jj { gen.density(x, y, z, &col) - base } else { 0.0 };
                self.pool.push(v);
            }
        }
        let i = self.nodes.len() as u32;
        self.nodes.push(node);
        self.cave.push(CaveCol { j0: 0, n: -1, off: 0, pos: [0; MW] });
        self.map.insert((nx, nz), i);
        i
    }

    /// Evaluate (once) the memoised cave lattice column of node `i`.
    pub fn ensure_cave(&mut self, gen: &WorldGen, i: u32) {
        if self.cave[i as usize].n >= 0 {
            return;
        }
        let col = self.nodes[i as usize].col;
        let (j0, j1) = cave_range(&col);
        let off = self.cpool.len();
        self.cpool.resize(off + (j1 - j0) as usize, 0.0);
        let v = &mut self.cpool[off..];
        // even levels and the range ends first, then the odd levels between them, exactly as
        // `node_cave_level`
        for j in j0..j1 {
            if j & 1 == 0 || j == j0 || j + 1 == j1 {
                v[(j - j0) as usize] = cave_level(gen, &col, j);
            }
        }
        for j in j0 + 1..j1 - 1 {
            if j & 1 == 1 {
                let k = (j - j0) as usize;
                let (a, b) = (v[k - 1], v[k + 1]);
                v[k] = if a < -CAVE_SKIP_T && b < -CAVE_SKIP_T { 0.5 * (a + b) } else { cave_level(gen, &col, j) };
            }
        }
        let mut pos = [0u64; MW];
        for (k, &f) in v.iter().enumerate() {
            if f > 0.0 {
                let j = j0 as usize + k;
                pos[j >> 6] |= 1 << (j & 63);
            }
        }
        self.cave[i as usize] = CaveCol { j0, n: j1 - j0, off: off as u32, pos };
    }

    /// Cave lattice value of node `i` at level j (memo when evaluated, else computed directly —
    /// identical values either way).
    #[inline]
    pub fn cave(&self, gen: &WorldGen, i: u32, j: i32) -> f32 {
        let cc = self.cave[i as usize];
        let k = j - cc.j0;
        if k >= 0 && k < cc.n {
            self.cpool[cc.off as usize + k as usize]
        } else {
            node_cave_level(gen, &self.nodes[i as usize].col, j)
        }
    }

    /// Bilinear cave field of the column with corner nodes `ids` / weights `w` at level j.
    #[inline]
    pub fn bcave(&self, gen: &WorldGen, ids: &[u32; 4], w: &[f32; 4], j: i32) -> f32 {
        // a corner with weight 0 adds exactly ±0: skipping it (and never evaluating it — the
        // lattice row/column just past a chunk's margin) leaves the sum bit-identical
        let mut s = 0.0;
        for k in 0..4 {
            if w[k] != 0.0 {
                s += w[k] * self.cave(gen, ids[k], j);
            }
        }
        s
    }

    /// Does `F̂ > 0` carve block y of the column (corner nodes `ids`, weights `w`)? The same
    /// arithmetic as the chunk builder (`carves` on the cell's two blended level values).
    #[inline]
    pub fn carved_at(&self, gen: &WorldGen, ids: &[u32; 4], w: &[f32; 4], y: i32) -> bool {
        if y < MIN_CARVE_Y {
            return false;
        }
        let j = y.div_euclid(LAT);
        let f0 = self.bcave(gen, ids, w, j);
        let f1 = self.bcave(gen, ids, w, j + 1);
        carves(f0, f1, (y - j * LAT) as f32 / LAT as f32)
    }

    /// Corner node ids + bilinear weights of block column (x, z) (no `ColumnInfo` needed).
    pub fn corners(&mut self, gen: &WorldGen, x: i32, z: i32) -> ([u32; 4], [f32; 4]) {
        let nx = x.div_euclid(LAT);
        let nz = z.div_euclid(LAT);
        let mut ids = [0u32; 4];
        for (k, (di, dk)) in [(0, 0), (1, 0), (0, 1), (1, 1)].iter().enumerate() {
            ids[k] = self.get(gen, nx + di, nz + dk, None);
        }
        (ids, corner_weights(x, z))
    }
}

/// Bilinear weights of block column (x, z) w.r.t. its 4 lattice corner nodes.
#[inline]
pub fn corner_weights(x: i32, z: i32) -> [f32; 4] {
    let fx = x.rem_euclid(LAT) as f32 / LAT as f32;
    let fz = z.rem_euclid(LAT) as f32 / LAT as f32;
    [(1.0 - fx) * (1.0 - fz), fx * (1.0 - fz), (1.0 - fx) * fz, fx * fz]
}

/// Vertical structure of one column (before liquids / features).
#[derive(Clone, Copy, Default)]
pub struct Shape {
    pub h: f32,
    /// Every y < floor is solid; y = floor may or may not be.
    pub floor: i32,
    /// Every y ≥ hi is air (before liquids / features).
    pub hi: i32,
    pub banded: bool,
    pub nodes: [u32; 4],
    pub w: [f32; 4],
    /// Clamped water level (0 when dry).
    pub wl: i32,
}

impl Shape {
    pub fn new(nodes: &mut Nodes, gen: &WorldGen, x: i32, z: i32, col: &ColumnInfo, lookup: &dyn Fn(i32, i32) -> Option<ColumnInfo>) -> Shape {
        let nx = x.div_euclid(LAT);
        let nz = z.div_euclid(LAT);
        let mut ids = [0u32; 4];
        for (k, (di, dk)) in [(0, 0), (1, 0), (0, 1), (1, 1)].iter().enumerate() {
            let (a, b) = (nx + di, nz + dk);
            ids[k] = match nodes.map.get(&(a, b)) {
                Some(&i) => i,
                None => {
                    let known = lookup(a * LAT, b * LAT);
                    nodes.get(gen, a, b, known.as_ref())
                }
            };
        }
        let w = corner_weights(x, z);
        let jmax = ids.iter().map(|&i| nodes.jag(i)).fold(0.0f32, f32::max);
        let h = col.height;
        let wl = if col.water_level == i32::MIN { 0 } else { col.water_level.clamp(0, WH) };
        if jmax > 0.0 {
            let lo = ((h - jmax - 1.5).floor() as i32).clamp(1, WH);
            let hi = ((h + jmax + 1.5).ceil() as i32).clamp(1, WH);
            Shape { h, floor: lo, hi, banded: true, nodes: ids, w, wl }
        } else {
            let t = ((h - 0.5).ceil() as i32).clamp(1, WH);
            Shape { h, floor: t, hi: t, banded: false, nodes: ids, w, wl }
        }
    }

    /// Bilinear residual at lattice level j.
    #[inline]
    fn bres(&self, nodes: &Nodes, j: i32) -> f32 {
        let mut s = 0.0;
        for k in 0..4 {
            s += self.w[k] * nodes.res(self.nodes[k], j);
        }
        s
    }

    /// Solid flags for y ∈ [floor, hi) written to `out[y - floor]`.
    pub fn band(&self, nodes: &Nodes, out: &mut [bool]) {
        if !self.banded {
            return;
        }
        let (lo, hi) = (self.floor, self.hi);
        let mut j = lo.div_euclid(LAT);
        let mut r0 = self.bres(nodes, j);
        let mut r1 = self.bres(nodes, j + 1);
        for y in lo..hi {
            let jy = y.div_euclid(LAT);
            if jy != j {
                j = jy;
                r0 = r1;
                r1 = self.bres(nodes, j + 1);
            }
            let fy = (y - j * LAT) as f32 / LAT as f32;
            let d = self.h - (y as f32 + 0.5) + r0 + (r1 - r0) * fy;
            out[(y - lo) as usize] = d > 0.0;
        }
    }

    /// Terrain solidity of block y (same arithmetic as `band`).
    pub fn solid_at(&self, nodes: &Nodes, y: i32) -> bool {
        if y < self.floor {
            return true;
        }
        if y >= self.hi || !self.banded {
            return false;
        }
        let j = y.div_euclid(LAT);
        let r0 = self.bres(nodes, j);
        let r1 = self.bres(nodes, j + 1);
        let fy = (y - j * LAT) as f32 / LAT as f32;
        let d = self.h - (y as f32 + 0.5) + r0 + (r1 - r0) * fy;
        d > 0.0
    }

    /// Highest solid y of the column.
    pub fn top_solid(&self, nodes: &Nodes) -> i32 {
        if !self.banded {
            return self.floor - 1;
        }
        let mut buf = [false; WH as usize];
        let n = (self.hi - self.floor) as usize;
        self.band(nodes, &mut buf[..n]);
        for k in (0..n).rev() {
            if buf[k] {
                return self.floor + k as i32;
            }
        }
        self.floor - 1
    }
}

/// Liquid rules (water / lava / ice), identical to the world builder.
pub struct Liquids {
    ice: Simplex,
}

impl Liquids {
    pub fn new(seed: u64) -> Self {
        Liquids { ice: Simplex::new(derive_seed(seed, salt("worldbuild.ice"))) }
    }
    #[inline]
    pub fn liquid(&self, col: &ColumnInfo) -> Material {
        if col.biome == Biome::Volcanic && col.water_level > SEA_LEVEL {
            Material::Lava
        } else {
            Material::Water
        }
    }
    pub fn freezes(&self, gen: &WorldGen, x: f64, z: f64, col: &ColumnInfo) -> bool {
        if matches!(col.biome, Biome::FrozenOcean | Biome::FrozenRiver) {
            return true;
        }
        if col.water_level <= SEA_LEVEL || self.liquid(col) == Material::Lava {
            return false;
        }
        let rel = col.water_level as f32 - gen.surface.snow_line(col);
        if rel < -16.0 {
            return false;
        }
        rel > -8.0 + 8.0 * self.ice.n2(x / 23.0, z / 23.0)
    }
}

/// Material of invisible deep rock.
#[inline]
pub fn deep_fill(y: i32) -> Material {
    if y <= 0 {
        Material::Bedrock
    } else if y < 64 {
        Material::Deepslate
    } else {
        Material::Stone
    }
}

/// Is block (x, y, z) non-air — terrain rock, surface liquid (water / lava / ice) or cave water /
/// lava? The exact chunk builder's decision without trees: terrain `ρ̂`, then caves (`F̂ > 0` at
/// `y ≥ MIN_CARVE_Y`, not within `WET_GUARD` of a water-covered column's top), then `cave_fill`.
/// `col` / `s` are the column at (x + ½, z + ½) and its `Shape` built with `nodes`.
pub fn block_solid(gen: &WorldGen, nodes: &mut Nodes, x: i32, y: i32, z: i32, col: &ColumnInfo, s: &Shape) -> bool {
    if y < 0 {
        return true;
    }
    if y >= WH {
        return false;
    }
    if !s.solid_at(nodes, y) {
        return y < s.wl;
    }
    if y < MIN_CARVE_Y {
        return true;
    }
    let wet = col.water_level != i32::MIN && col.water_level as f32 > col.height;
    if wet && y >= s.top_solid(nodes) + 1 - WET_GUARD {
        return true;
    }
    for k in 0..4 {
        if s.w[k] != 0.0 {
            nodes.ensure_cave(gen, s.nodes[k]);
        }
    }
    if !nodes.carved_at(gen, &s.nodes, &s.w, y) {
        return true;
    }
    gen.cave_fill(x, y, z, col) != CaveFill::Air
}

/// Exact (lattice-consistent) column summary used outside the chunk region.
pub struct ColumnTop {
    pub col: ColumnInfo,
    /// Highest solid terrain block.
    pub top: i32,
    /// Clamped water level (0 when dry).
    pub wl: i32,
}

impl ColumnTop {
    /// First air above the top solid block, or −1 when liquid tops the column (feature root rule).
    pub fn root_y(&self) -> i32 {
        if self.wl > self.top + 1 {
            -1
        } else {
            self.top + 1
        }
    }
    /// Top of terrain + liquid (y of the first air block above everything).
    pub fn surface(&self) -> i32 {
        (self.top + 1).max(self.wl)
    }
}

pub fn column_top(nodes: &mut Nodes, gen: &WorldGen, x: i32, z: i32) -> ColumnTop {
    let col = gen.column(x as f64 + 0.5, z as f64 + 0.5);
    column_top_with(nodes, gen, x, z, col)
}

pub fn column_top_with(nodes: &mut Nodes, gen: &WorldGen, x: i32, z: i32, col: ColumnInfo) -> ColumnTop {
    let s = Shape::new(nodes, gen, x, z, &col, &|_, _| None);
    let top = s.top_solid(nodes);
    ColumnTop { col, top, wl: s.wl }
}
