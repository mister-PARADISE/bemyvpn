//! Terrain voxelisation shared by the chunk builder, feature roots, camera height and spawn.
//!
//! Semantics follow `world/build.rs` (level 0) exactly, minus caves:
//! * the 3-D band uses the world-aligned residual lattice `4ℤ³ + ½`:
//!   `ρ̂(x,y,z) = H(x,z) − y + I₄[R]`, `R_n = ρ(p_n) − (H_n − y_n)` (0 outside a node's band),
//!   evaluated only inside `[H − Jmax − 1.5, H + Jmax + 1.5]` (Jmax over the 4 corner nodes);
//!   below solid, above air; pure heightfield `y < ⌈H − ½⌉` when all 4 corners have J = 0.
//! * terrain air below `water_level` is liquid: lava for Volcanic lakes above sea level, else
//!   water; the top block freezes to ice for frozen biomes / lakes above the snow line.
//!
//! Every value is a pure function of world coordinates, so chunks agree on shared borders and
//! trees rooted in one chunk land at the same y in every chunk that sees them.
use std::collections::HashMap;
use voxel_world::gen::{Biome, ColumnInfo, WorldGen, SEA_LEVEL, WORLD_HEIGHT};
use voxel_world::materials::Material;
use voxel_world::noise::{derive_seed, salt, Simplex};

pub const LAT: i32 = 4;
pub const WH: i32 = WORLD_HEIGHT;

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
}

impl Default for Nodes {
    fn default() -> Self {
        Self::new()
    }
}

impl Nodes {
    pub fn new() -> Self {
        Nodes { map: HashMap::new(), nodes: Vec::new(), pool: Vec::new() }
    }
    pub fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.pool.clear();
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
        self.map.insert((nx, nz), i);
        i
    }
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
        let fx = x.rem_euclid(LAT) as f32 / LAT as f32;
        let fz = z.rem_euclid(LAT) as f32 / LAT as f32;
        let w = [(1.0 - fx) * (1.0 - fz), fx * (1.0 - fz), (1.0 - fx) * fz, fx * fz];
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

    /// Highest solid y of the column.
    pub fn top_solid(&self, nodes: &Nodes) -> i32 {
        if !self.banded {
            return self.floor - 1;
        }
        let mut buf = [false; 512];
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
