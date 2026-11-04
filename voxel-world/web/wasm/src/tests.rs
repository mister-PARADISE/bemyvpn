//! Native tests: `cargo test --release -- --nocapture` (timings are printed).
use crate::chunk::{ChunkBuilder, CORNERS, CS, KIND_FOLIAGE, KIND_OPAQUE, KIND_WATER, NORMALS, QB, VB};
use crate::far::{build_far, FAR_VB};
use crate::terrain::{block_solid, cave_level, column_top, node_cave_level, Liquids, Nodes, Shape, WH};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use voxel_world::gen::{Biome, CaveFill, WorldGen};
use voxel_world::materials::{Material, MATERIAL_COUNT};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cls {
    Air,
    Water,
    Leaf,
    Full,
}

fn cls(m: Material) -> Cls {
    if m == Material::Air {
        Cls::Air
    } else if m == Material::Water {
        Cls::Water
    } else if m.is_leaves() {
        Cls::Leaf
    } else {
        Cls::Full
    }
}

fn face_visible(inside: Cls, outside: Cls) -> bool {
    match inside {
        Cls::Air => false,
        Cls::Full => outside != Cls::Full,
        Cls::Leaf => outside == Cls::Air || outside == Cls::Water,
        Cls::Water => outside == Cls::Air || outside == Cls::Leaf,
    }
}

fn kind_of(c: Cls) -> usize {
    match c {
        Cls::Water => KIND_WATER,
        Cls::Leaf => KIND_FOLIAGE,
        _ => KIND_OPAQUE,
    }
}

#[derive(Clone, Copy, Debug)]
struct V {
    x: i32,
    y: i32,
    z: i32,
    n: usize,
    mat: u8,
    ao: u8,
    flags: u8,
    tint: [u8; 3],
    sky: u8,
}

fn verts(buf: &[u8]) -> Vec<V> {
    buf.chunks_exact(VB)
        .map(|b| V {
            x: b[0] as i32,
            z: b[1] as i32,
            y: u16::from_le_bytes([b[2], b[3]]) as i32,
            n: b[4] as usize,
            mat: b[5],
            ao: b[6],
            flags: b[7],
            tint: [b[8], b[9], b[10]],
            sky: b[11],
        })
        .collect()
}

/// Unit faces (block x, y, z in world coords of the INSIDE block, normal) covered by a quad.
fn unit_faces(q: &[V], cx: i32, cz: i32) -> Vec<(i32, i32, i32, usize)> {
    let n = q[0].n;
    let nv = NORMALS[n];
    let axis = (0..3).find(|&a| nv[a] != 0).unwrap();
    let p = |v: &V| [v.x + cx * CS, v.y, v.z + cz * CS];
    let ps: Vec<[i32; 3]> = q.iter().map(p).collect();
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for v in &ps {
        for a in 0..3 {
            lo[a] = lo[a].min(v[a]);
            hi[a] = hi[a].max(v[a]);
        }
    }
    assert_eq!(lo[axis], hi[axis], "quad not planar along its normal");
    let plane = lo[axis];
    let mut out = Vec::new();
    let (t1, t2) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    for a in lo[t1]..hi[t1] {
        for b in lo[t2]..hi[t2] {
            let mut blk = [0i32; 3];
            blk[t1] = a;
            blk[t2] = b;
            blk[axis] = if nv[axis] > 0 { plane - 1 } else { plane };
            out.push((blk[0], blk[1], blk[2], n));
        }
    }
    out
}

fn build(b: &mut ChunkBuilder, g: &WorldGen, cx: i32, cz: i32) {
    b.build(g, cx, cz);
}

/// Brute-force expected faces from the builder's own voxel region.
fn expected_faces(b: &ChunkBuilder, cx: i32, cz: i32) -> HashMap<(i32, i32, i32, usize), usize> {
    let mut e = HashMap::new();
    let (x0, z0) = (cx * CS, cz * CS);
    let ylo = b.ymin.min(0).max(0);
    let _ = ylo;
    for x in x0..x0 + CS {
        for z in z0..z0 + CS {
            for y in 0..WH {
                let m = b.block_at(cx, cz, x, y, z).unwrap();
                let ci = cls(m);
                if ci == Cls::Air {
                    continue;
                }
                for (d, nv) in NORMALS.iter().enumerate() {
                    let o = b.block_at(cx, cz, x + nv[0], y + nv[1], z + nv[2]).unwrap_or(Material::Air);
                    let o = if y + nv[1] < 0 { Material::Stone } else { o };
                    if face_visible(ci, cls(o)) {
                        e.insert((x, y, z, d), kind_of(ci));
                    }
                }
            }
        }
    }
    e
}

fn emitted_faces(b: &ChunkBuilder, cx: i32, cz: i32) -> HashMap<(i32, i32, i32, usize), usize> {
    let mut got = HashMap::new();
    for k in 0..3 {
        let vs = verts(&b.out[k]);
        for q in vs.chunks_exact(4) {
            for f in unit_faces(q, cx, cz) {
                let prev = got.insert(f, k);
                assert!(prev.is_none(), "duplicate face {f:?}");
            }
        }
    }
    got
}

/// Chunk coordinates of a mix of terrain types near the origin of `g`.
fn terrain_mix(g: &WorldGen) -> Vec<(&'static str, i32, i32)> {
    let cats: [(&str, fn(&voxel_world::gen::ColumnInfo) -> bool); 10] = [
        ("mountain", |c| c.jaggedness > 10.0 && c.height > 260.0),
        ("cliffs", |c| c.jaggedness > 4.0 && c.slope > 1.0),
        ("dense_forest", |c| matches!(c.biome, Biome::DarkForest | Biome::Jungle | Biome::OldGrowthTaiga)),
        ("forest", |c| matches!(c.biome, Biome::Forest | Biome::Taiga | Biome::BirchForest)),
        ("plains", |c| matches!(c.biome, Biome::Plains | Biome::Meadow | Biome::Savanna)),
        ("ocean", |c| c.biome.is_ocean()),
        ("coast", |c| matches!(c.biome, Biome::Beach | Biome::StonyShore)),
        ("dry", |c| matches!(c.biome, Biome::Desert | Biome::Badlands | Biome::Hoodoos)),
        ("snow", |c| matches!(c.biome, Biome::SnowySlopes | Biome::SnowyPeaks | Biome::Glacier)),
        ("river_swamp", |c| matches!(c.biome, Biome::River | Biome::Swamp)),
    ];
    let mut out = Vec::new();
    for (name, f) in cats.iter() {
        'search: for r in 0..60i32 {
            for k in 0..(8 * r.max(1)) {
                let a = k as f64 / (8 * r.max(1)) as f64 * std::f64::consts::TAU;
                let (x, z) = (r as f64 * 160.0 * a.cos(), r as f64 * 160.0 * a.sin());
                let c = g.column(x, z);
                if f(&c) {
                    out.push((*name, (x / 32.0).floor() as i32, (z / 32.0).floor() as i32));
                    break 'search;
                }
            }
        }
    }
    out
}

#[test]
fn vertex_format_and_winding() {
    let g = WorldGen::new(42);
    let mut b = ChunkBuilder::new();
    let mix = terrain_mix(&g);
    assert!(mix.len() >= 6, "terrain mix too small: {mix:?}");
    for &(_, cx, cz) in mix.iter().take(6) {
        build(&mut b, &g, cx, cz);
        for k in 0..3 {
            let buf = &b.out[k];
            assert_eq!(buf.len() % QB, 0);
            for q in verts(buf).chunks_exact(4) {
                let n = q[0].n;
                assert!(n < 6);
                for v in q {
                    assert_eq!(v.n, n);
                    assert!(v.x <= 32 && v.z <= 32 && v.y <= WH);
                    assert!((v.mat as usize) < MATERIAL_COUNT);
                    assert!(v.ao <= 3);
                    assert!(v.flags <= 1);
                    assert_eq!(v.mat, q[0].mat);
                    assert!(v.y >= b.ymin && v.y <= b.ymax, "y {} outside [{}, {}]", v.y, b.ymin, b.ymax);
                }
                let m = Material::from_u8(q[0].mat);
                assert_eq!(kind_of(cls(m)), k, "material {m:?} in kind {k}");
                if !(m.grass_tinted() || m.foliage_tinted() || m == Material::Terracotta) {
                    assert_eq!(q[0].tint, [255, 255, 255]);
                }
                // CCW from outside: (v1 − v0) × (v2 − v0) along the normal; quad is a rectangle
                let p: Vec<[i64; 3]> = q.iter().map(|v| [v.x as i64, v.y as i64, v.z as i64]).collect();
                let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
                let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
                let e3 = [p[3][0] - p[0][0], p[3][1] - p[0][1], p[3][2] - p[0][2]];
                let cr = |a: [i64; 3], b: [i64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let nv = NORMALS[n];
                for c in [cr(e1, e2), cr(e2, e3)] {
                    let dot = c[0] * nv[0] as i64 + c[1] * nv[1] as i64 + c[2] * nv[2] as i64;
                    let len2 = c[0] * c[0] + c[1] * c[1] + c[2] * c[2];
                    assert!(dot > 0 && dot * dot == len2, "winding/planarity: {q:?}");
                }
                // opposite edges equal (parallelogram)
                assert_eq!([p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]], [p[2][0] - p[3][0], p[2][1] - p[3][1], p[2][2] - p[3][2]]);
            }
        }
    }
    // corner table: each face's corners lie on its plane
    for d in 0..6 {
        let nv = NORMALS[d];
        let axis = (0..3).find(|&a| nv[a] != 0).unwrap();
        let want = if nv[axis] > 0 { 1 } else { 0 };
        for c in CORNERS[d] {
            assert_eq!(c[axis], want);
        }
    }
}

#[test]
fn faces_exact_no_hidden_faces() {
    // every emitted face is visible (never between two opaque blocks) and every visible face
    // is emitted exactly once — both with and without top-face merging
    let g = WorldGen::new(42);
    let mix = terrain_mix(&g);
    for merge in [0u32, 7] {
        let mut b = ChunkBuilder::new();
        b.merge_mask = merge;
        for &(name, cx, cz) in &mix {
            build(&mut b, &g, cx, cz);
            let exp = expected_faces(&b, cx, cz);
            let got = emitted_faces(&b, cx, cz);
            for (f, k) in &got {
                assert_eq!(exp.get(f), Some(k), "{name}: unexpected face {f:?} kind {k}");
            }
            for (f, k) in &exp {
                assert_eq!(got.get(f), Some(k), "{name}: missing face {f:?} kind {k}");
            }
        }
    }
}

#[test]
fn chunk_border_seams() {
    let g = WorldGen::new(42);
    let mix = terrain_mix(&g);
    let mut a = ChunkBuilder::new();
    let mut b = ChunkBuilder::new();
    a.merge_mask = 0;
    b.merge_mask = 0;
    let mut checked_trees = 0;
    let mut total_shared = 0;
    for &(name, cx, cz) in &mix {
        for (dx, dz) in [(1, 0), (0, 1)] {
            build(&mut a, &g, cx, cz);
            build(&mut b, &g, cx + dx, cz + dz);
            // the two strips adjacent to the border agree block by block in both builds
            for t in 0..CS {
                for side in [CS - 1, CS] {
                    let (x, z) = if dx == 1 { (cx * CS + side, cz * CS + t) } else { (cx * CS + t, cz * CS + side) };
                    for y in 0..WH {
                        let ma = a.block_at(cx, cz, x, y, z).unwrap();
                        let mb = b.block_at(cx + dx, cz + dz, x, y, z).unwrap();
                        // margin columns carry deep-fill rock: compare visibility classes
                        assert_eq!(cls(ma), cls(mb), "{name}: block mismatch at ({x},{y},{z}): {ma:?} vs {mb:?}");
                        if ma.is_leaves() || ma.is_log() {
                            checked_trees += 1;
                        }
                    }
                }
            }
            // faces on the border plane: A's +X/+Z faces ↔ blocks of B, and vertex lighting of
            // coplanar top faces agrees at shared vertices
            let ea = emitted_faces(&a, cx, cz);
            let eb = emitted_faces(&b, cx + dx, cz + dz);
            let d_out = if dx == 1 { 0 } else { 4 };
            let d_in = if dx == 1 { 1 } else { 5 };
            for t in 0..CS {
                for y in 0..WH - 1 {
                    let (xa, za) = if dx == 1 { (cx * CS + CS - 1, cz * CS + t) } else { (cx * CS + t, cz * CS + CS - 1) };
                    let (xb, zb) = (xa + dx, za + dz);
                    let ia = cls(a.block_at(cx, cz, xa, y, za).unwrap());
                    let ib = cls(b.block_at(cx + dx, cz + dz, xb, y, zb).unwrap());
                    assert_eq!(ea.contains_key(&(xa, y, za, d_out)), face_visible(ia, ib), "{name}: seam face A");
                    assert_eq!(eb.contains_key(&(xb, y, zb, d_in)), face_visible(ib, ia), "{name}: seam face B");
                }
            }
            let mut va: HashMap<(i32, i32, i32), (u8, u8)> = HashMap::new();
            for v in verts(&a.out[0]).iter().filter(|v| v.n == 2) {
                let w = (v.x + cx * CS, v.y, v.z + cz * CS);
                let on = if dx == 1 { w.0 == (cx + 1) * CS } else { w.2 == (cz + 1) * CS };
                if on {
                    va.insert(w, (v.ao, v.sky));
                }
            }
            let mut shared = 0;
            for v in verts(&b.out[0]).iter().filter(|v| v.n == 2) {
                let w = (v.x + (cx + dx) * CS, v.y, v.z + (cz + dz) * CS);
                if let Some(&(ao, sky)) = va.get(&w) {
                    // AO is per face (depends on which side the face is), sky is per vertex
                    let _ = ao;
                    assert_eq!(sky, v.sky, "{name}: sky light differs across the seam at {w:?}");
                    shared += 1;
                }
            }
            total_shared += shared;
        }
    }
    eprintln!("seam test: {checked_trees} tree blocks on borders, {total_shared} shared top vertices");
    assert!(total_shared > 200);
}

#[test]
fn deterministic_and_cache_independent() {
    let g = WorldGen::new(2026);
    let mix = terrain_mix(&g);
    let mut warm = ChunkBuilder::new();
    let mut first: Vec<[Vec<u8>; 4]> = Vec::new();
    for &(_, cx, cz) in &mix {
        // warm the caches with neighbours in a different order
        for (dx, dz) in [(1, 1), (-1, 0), (0, -1)] {
            build(&mut warm, &g, cx + dx, cz + dz);
        }
        build(&mut warm, &g, cx, cz);
        first.push(warm.out.clone());
    }
    for (i, &(name, cx, cz)) in mix.iter().enumerate() {
        let mut cold = ChunkBuilder::new();
        build(&mut cold, &g, cx, cz);
        assert!(cold.out == first[i], "{name}: output depends on cache state");
    }
    // a second generator instance gives the same bytes
    let g2 = WorldGen::new(2026);
    let mut c2 = ChunkBuilder::new();
    let (_, cx, cz) = mix[0];
    build(&mut c2, &g2, cx, cz);
    assert!(c2.out == first[0]);
}

#[test]
fn trees_are_rooted_and_heights_consistent() {
    let g = WorldGen::new(42);
    let mix = terrain_mix(&g);
    let mut b = ChunkBuilder::new();
    let mut bare = ChunkBuilder::new();
    bare.features = false;
    bare.caves = false; // the camera-height query (column_top) ignores caves
    let mut nodes = Nodes::new();
    let mut logs = 0;
    let mut cols = 0;
    let mut floating = 0;
    for &(name, cx, cz) in &mix {
        build(&mut b, &g, cx, cz);
        build(&mut bare, &g, cx, cz);
        for x in cx * CS..cx * CS + CS {
            for z in cz * CS..cz * CS + CS {
                // terrain (+ liquid) top of the feature-less build equals the point query used
                // for camera collision / spawn
                let mut y = WH - 1;
                while bare.block_at(cx, cz, x, y, z).unwrap() == Material::Air {
                    y -= 1;
                }
                let ct = column_top(&mut nodes, &g, x, z);
                assert_eq!(ct.surface(), y + 1, "{name}: height_at mismatch at ({x},{z})");
                cols += 1;
                // trunks stand on the ground: the lowest log block of a column that holds the
                // feature root sits on solid terrain or on another feature block
                for yy in 1..WH - 1 {
                    let m = b.block_at(cx, cz, x, yy, z).unwrap();
                    if m.is_log() && !b.block_at(cx, cz, x, yy - 1, z).unwrap().is_log() {
                        logs += 1;
                        let under = b.block_at(cx, cz, x, yy - 1, z).unwrap();
                        let ground = bare.block_at(cx, cz, x, yy - 1, z).unwrap();
                        if yy == ct.top + 1 && !(under.is_opaque() && ground.is_opaque()) {
                            floating += 1;
                        }
                    }
                }
            }
        }
    }
    eprintln!("height consistency checked on {cols} columns, {logs} log runs, {floating} floating");
    assert_eq!(floating, 0);
    assert!(cols > 1000);
}

#[test]
fn far_tile_sanity() {
    let g = WorldGen::new(42);
    let liq = Liquids::new(42);
    let mut a = Vec::new();
    let mut b = Vec::new();
    for &(cell, x0, z0) in &[(4u32, 0i32, 0i32), (32, -4096, 2048), (128, 8192, -8192)] {
        let n = 32u32;
        let t = Instant::now();
        build_far(&g, &liq, cell, x0, z0, n, &mut a);
        let el = t.elapsed();
        assert_eq!(a.len(), (n as usize + 1).pow(2) * FAR_VB);
        let mut wet = 0;
        for v in a.chunks_exact(FAR_VB) {
            let top = f32::from_le_bytes([v[0], v[1], v[2], v[3]]);
            let water = f32::from_le_bytes([v[4], v[5], v[6], v[7]]);
            assert!(top.is_finite() && (0.0..WH as f32 + 64.0).contains(&top), "top {top}");
            assert!(water == -1.0e9 || (water > top && water < WH as f32), "water {water} top {top}");
            assert!((v[11] as usize) < MATERIAL_COUNT);
            assert!((v[13] as usize) < voxel_world::gen::BIOME_COUNT && v[15] == 0);
            if water > top {
                wet += 1;
                let m = Material::from_u8(v[11]);
                assert!(matches!(m, Material::Water | Material::Ice | Material::Lava), "{m:?}");
            }
        }
        // shared edge with the next tile to the east is bit-identical
        build_far(&g, &liq, cell, x0 + (n * cell) as i32, z0, n, &mut b);
        let m = n as usize + 1;
        for j in 0..m {
            let ea = &a[(j * m + n as usize) * FAR_VB..(j * m + n as usize + 1) * FAR_VB];
            let eb = &b[(j * m) * FAR_VB..(j * m + 1) * FAR_VB];
            assert_eq!(ea, eb);
        }
        eprintln!("far tile cell {cell}: 33² in {:.2} ms, {wet} wet vertices", el.as_secs_f64() * 1e3);
    }
}

#[test]
fn timings_per_chunk() {
    // cold: fresh caches per chunk; warm: streaming a 4×4 block around each spot (the viewer's
    // pattern, features/nodes memoised across neighbours)
    // VW_NO_CAVES=1 measures the same binary without caves (baseline for the cave cost)
    let caves = std::env::var("VW_NO_CAVES").map(|v| v != "1").unwrap_or(true);
    let mut lines = Vec::new();
    let mut tot_cold = 0.0;
    let mut tot_warm = 0.0;
    let mut n_cold = 0;
    let mut n_warm = 0;
    let mut quads = 0usize;
    let mut bytes = 0usize;
    for seed in [42u64, 1] {
        let g = WorldGen::new(seed);
        let mix = terrain_mix(&g);
        for &(name, cx, cz) in &mix {
            let mut cold = ChunkBuilder::new();
            cold.caves = caves;
            let t = Instant::now();
            cold.build(&g, cx, cz);
            let tc = t.elapsed().as_secs_f64() * 1e3;
            let mut warm = ChunkBuilder::new();
            warm.caves = caves;
            let t = Instant::now();
            let mut k = 0;
            for dz in -2..2 {
                for dx in -2..2 {
                    warm.build(&g, cx + dx, cz + dz);
                    quads += (0..3).map(|q| warm.quad_count(q)).sum::<usize>();
                    bytes += warm.out.iter().map(|o| o.len()).sum::<usize>();
                    k += 1;
                }
            }
            let tw = t.elapsed().as_secs_f64() * 1e3 / k as f64;
            let (nt, fb) = (warm.feats.len(), warm.feats.mem_bytes());
            if fb > 0 {
                eprintln!("  {name}: feature cache {nt} tiles, {:.0} KB ({:.1} KB/tile)", fb as f64 / 1024.0, fb as f64 / 1024.0 / nt as f64);
            }
            tot_cold += tc;
            tot_warm += tw * k as f64;
            n_cold += 1;
            n_warm += k;
            lines.push(format!(
                "seed {seed:>4} {name:<13} chunk ({cx:>4},{cz:>4}) cold {tc:6.2} ms  warm avg {tw:6.2} ms  quads o/w/f {}/{}/{} y {}..{}",
                cold.quad_count(0),
                cold.quad_count(1),
                cold.quad_count(2),
                cold.ymin,
                cold.ymax
            ));
        }
    }
    for l in &lines {
        eprintln!("{l}");
    }
    eprintln!(
        "AVERAGE (caves {}) cold {:.2} ms, warm {:.2} ms per chunk; {:.0} quads, {:.1} KB per chunk",
        if caves { "on" } else { "off" },
        tot_cold / n_cold as f64,
        tot_warm / n_warm as f64,
        quads as f64 / n_warm as f64,
        bytes as f64 / n_warm as f64 / 1024.0
    );
}

#[test]
fn merge_reduces_quads() {
    let g = WorldGen::new(42);
    let mix = terrain_mix(&g);
    let mut a = ChunkBuilder::new();
    let mut b = ChunkBuilder::new();
    a.merge_mask = 0;
    b.merge_mask = 7;
    let (mut qa, mut qb) = (0, 0);
    for &(name, cx, cz) in &mix {
        a.build(&g, cx, cz);
        b.build(&g, cx, cz);
        let s = |c: &ChunkBuilder| (0..3).map(|k| c.quad_count(k)).collect::<Vec<_>>();
        eprintln!("{name:<13} unmerged {:?} merged {:?}", s(&a), s(&b));
        qa += s(&a).iter().sum::<usize>();
        qb += s(&b).iter().sum::<usize>();
    }
    eprintln!("total quads unmerged {qa}, merged {qb}");
    assert!(qb <= qa);
    let _ = HashSet::<u8>::new();
}

fn is_fill(m: Material) -> bool {
    matches!(m, Material::Air | Material::Water | Material::Lava)
}

#[test]
fn cave_level_skip_keeps_signs() {
    // the odd-level skip (`node_cave_level`) may only replace values that are rock anyway
    let g = WorldGen::new(42);
    let (mut levels, mut skipped, mut flipped) = (0usize, 0usize, 0usize);
    for i in 0..1500i32 {
        let (nx, nz) = ((i * 37) % 997 - 500, (i * 61) % 991 - 495);
        let col = g.column((nx * 4) as f64 + 0.5, (nz * 4) as f64 + 0.5);
        for j in 1..WH / 4 {
            let exact = cave_level(&g, &col, j);
            let memo = node_cave_level(&g, &col, j);
            levels += 1;
            skipped += (exact != memo) as usize;
            flipped += ((exact > 0.0) != (memo > 0.0)) as usize;
        }
    }
    eprintln!("cave lattice skip: {skipped} of {levels} levels interpolated, {flipped} sign flips");
    assert!(skipped > levels / 20, "skip never used");
    assert!(flipped * 1000 <= skipped, "{flipped} sign flips");
}

/// Chunks with caves: the terrain mix plus a row near the origin and the seed-42 entrances
/// found by `find_cave_entrances`.
fn cave_chunks(g: &WorldGen) -> Vec<(i32, i32)> {
    let mut list: Vec<(i32, i32)> = terrain_mix(g).iter().map(|&(_, a, b)| (a, b)).collect();
    for i in -3..3 {
        list.push((i, 3));
    }
    list
}

#[test]
fn caves_carved_and_point_queries_agree() {
    // caves only ever turn terrain rock into air / water / lava, and the point query used for
    // camera collision (`vw_solid_at`) agrees with the chunk voxels block by block
    let g = WorldGen::new(42);
    let mut cav = ChunkBuilder::new();
    cav.features = false;
    let mut bare = ChunkBuilder::new();
    bare.features = false;
    bare.caves = false;
    let mut nodes = Nodes::new();
    let (mut carved, mut water, mut lava, mut breach, mut queries) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (cx, cz) in cave_chunks(&g) {
        cav.build(&g, cx, cz);
        bare.build(&g, cx, cz);
        for x in cx * CS..cx * CS + CS {
            for z in cz * CS..cz * CS + CS {
                let col = g.column(x as f64 + 0.5, z as f64 + 0.5);
                let s = Shape::new(&mut nodes, &g, x, z, &col, &|_, _| None);
                let (mut top_c, mut top_b) = (-1, -1);
                for y in 0..WH {
                    let mc = cav.block_at(cx, cz, x, y, z).unwrap();
                    let mb = bare.block_at(cx, cz, x, y, z).unwrap();
                    if mc != Material::Air {
                        top_c = y;
                    }
                    if mb != Material::Air {
                        top_b = y;
                    }
                    if is_fill(mb) {
                        assert_eq!(mc, mb, "caves changed a non-rock block at ({x},{y},{z})");
                    } else if is_fill(mc) {
                        let want = match g.cave_fill(x, y, z, &col) {
                            CaveFill::Air => Material::Air,
                            CaveFill::Water => Material::Water,
                            CaveFill::Lava => Material::Lava,
                        };
                        assert_eq!(mc, want, "cave fill at ({x},{y},{z})");
                        carved += 1;
                        water += (mc == Material::Water) as usize;
                        lava += (mc == Material::Lava) as usize;
                    }
                    let q = block_solid(&g, &mut nodes, x, y, z, &col, &s);
                    assert_eq!(q, mc != Material::Air, "vw_solid_at disagrees with the chunk at ({x},{y},{z}): {mc:?}");
                    queries += 1;
                }
                if top_c < top_b {
                    breach += 1;
                }
            }
        }
    }
    eprintln!("caves: {carved} carved blocks ({water} water, {lava} lava), {breach} columns with a carved top, {queries} point queries agree");
    assert!(carved > 2000, "no caves carved");
}

/// Helper for the viewer smoke test (not run by default):
/// `VW_SEED=42 VW_CX=0 VW_CZ=0 cargo test --release find_cave_entrances -- --ignored --nocapture`
/// prints surface cave entrances (dry columns whose top is carved ≥ 6 blocks deep) near a chunk.
#[test]
#[ignore]
fn find_cave_entrances() {
    let env = |k: &str, d: i64| std::env::var(k).ok().and_then(|s| s.parse::<i64>().ok()).unwrap_or(d);
    let seed = env("VW_SEED", 42) as u64;
    let (cx0, cz0) = (env("VW_CX", 0) as i32, env("VW_CZ", 0) as i32);
    let g = WorldGen::new(seed);
    let mut cav = ChunkBuilder::new();
    cav.features = false;
    let mut nodes = Nodes::new();
    let mut found = 0;
    'outer: for r in 0..48i32 {
        for dz in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dz.abs() != r {
                    continue;
                }
                let (cx, cz) = (cx0 + dx, cz0 + dz);
                cav.build(&g, cx, cz);
                nodes.trim(20000);
                let mut best = (0, 0, 0, 0);
                for x in cx * CS + 2..cx * CS + CS - 2 {
                    for z in cz * CS + 2..cz * CS + CS - 2 {
                        let tb = column_top(&mut nodes, &g, x, z);
                        if tb.wl > tb.top + 1 {
                            continue;
                        }
                        let mut y = tb.top;
                        while y > 0 && cav.block_at(cx, cz, x, y, z).unwrap() == Material::Air {
                            y -= 1;
                        }
                        if tb.top - y > best.0 {
                            best = (tb.top - y, x, z, tb.top);
                        }
                    }
                }
                let (depth, x, z, top) = best;
                if depth >= 6 {
                    let b = g.column(x as f64 + 0.5, z as f64 + 0.5).biome;
                    eprintln!("ENTRANCE seed {seed} chunk ({cx},{cz}) column ({x},{z}) top {top} open {depth} deep {b:?}");
                    found += 1;
                    if found >= 8 {
                        break 'outer;
                    }
                }
            }
        }
    }
}
