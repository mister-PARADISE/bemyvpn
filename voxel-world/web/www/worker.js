// Воксельный мир — chunk / far-tile builder worker.
// Owns one instance of gen.wasm (the real Rust generator). Protocol (main <-> worker):
//   main → {type:'init', module|bytes, seedLo, seedHi, gen}      → {type:'ready', gen, ms}
//   main → {type:'seed', seedLo, seedHi, gen}                    → {type:'ready', gen, ms}
//   main → {type:'chunk', id, cx, cz, gen}                       → {type:'chunk', id, ok, gen, cx, cz, m0, m1, m2, ymin, ymax, ms}
//   main → {type:'far', id, cell, x0, z0, n, gen}                → {type:'far', id, ok, gen, buf, hmin, hmax, wet, ms}
// Far tiles are converted to the GPU layout used by index.html (16 bytes / vertex):
//   f32 terrain top, f32 water level (dilated by one cell onto the shore, -1e4 = none),
//   u8 r,g,b (sRGB far albedo), u8 material, i8 nx, i8 nz (unit normal ×127), u8 0, u8 0.
// Vertices: (n+1)² grid (row-major, z outer), then 4·(n+1) skirt copies of the edge vertices.
'use strict';

let ex = null;      // wasm exports
let gen = -1;
const queue = [];   // messages that arrive while the module is still instantiating
let starting = false;

function makeImports(mod) {
  const imports = {};
  for (const imp of WebAssembly.Module.imports(mod)) {
    const ns = imports[imp.module] || (imports[imp.module] = {});
    if (imp.kind === 'function') ns[imp.name] = () => 0;
    else if (imp.kind === 'memory') ns[imp.name] = new WebAssembly.Memory({ initial: 64, maximum: 32768 });
    else if (imp.kind === 'table') ns[imp.name] = new WebAssembly.Table({ initial: 64, element: 'anyfunc' });
    else if (imp.kind === 'global') ns[imp.name] = new WebAssembly.Global({ value: 'i32', mutable: false }, 0);
  }
  return imports;
}

function mem() { return ex.memory.buffer; }

function copyOut(ptr, len) {
  const out = new Uint8Array(len);
  if (len > 0) out.set(new Uint8Array(mem(), ptr, len));
  return out.buffer;
}

function buildChunk(m) {
  const t0 = performance.now();
  const rc = ex.vw_build_chunk(m.cx | 0, m.cz | 0);
  if (rc !== 0) {
    postMessage({ type: 'chunk', id: m.id, gen, ok: false, cx: m.cx, cz: m.cz, err: 'rc=' + rc });
    return;
  }
  const bufs = [];
  for (let k = 0; k < 3; k++) bufs.push(copyOut(ex.vw_mesh_ptr(k), ex.vw_mesh_len(k)));
  const msg = {
    type: 'chunk', id: m.id, gen, ok: true, cx: m.cx, cz: m.cz,
    m0: bufs[0], m1: bufs[1], m2: bufs[2],
    ymin: ex.vw_chunk_ymin(), ymax: ex.vw_chunk_ymax(),
    ms: performance.now() - t0,
  };
  postMessage(msg, bufs);
}

function buildFar(m) {
  const t0 = performance.now();
  const n = m.n | 0, cell = m.cell | 0;
  // One extra cell of margin on every side → central-difference normals and water dilation
  // that agree across neighbouring tiles.
  const N = n + 2, S = N + 1;
  ex.vw_build_far(cell, (m.x0 | 0) - cell, (m.z0 | 0) - cell, N);
  const len = ex.vw_far_len();
  if (len < S * S * 16) {
    postMessage({ type: 'far', id: m.id, gen, ok: false, err: 'far len ' + len + ' < ' + S * S * 16 });
    return;
  }
  const src = copyOut(ex.vw_far_ptr(), S * S * 16);
  const sf = new Float32Array(src);
  const su = new Uint8Array(src);
  const H = new Float32Array(S * S), W = new Float32Array(S * S);
  for (let k = 0; k < S * S; k++) {
    H[k] = sf[k * 4];
    const w = sf[k * 4 + 1];
    W[k] = w > H[k] + 0.01 ? w : -1e9;
  }
  const n1 = n + 1;
  const V = n1 * n1 + 4 * n1;
  const out = new ArrayBuffer(V * 16);
  const of = new Float32Array(out);
  const ou = new Uint8Array(out);
  const oi = new Int8Array(out);
  let hmin = 1e9, hmax = -1e9, wet = 0;
  const inv = 1 / (2 * cell);
  const writeVert = (dst, i, j) => {
    // (i, j) in output grid → (i+1, j+1) in source grid
    const si = i + 1, sj = j + 1, k = sj * S + si;
    const h = H[k];
    let w = W[k];
    if (w < -1e8) {
      // dilate water one cell onto the shore so far water meets the land without gaps
      for (let dj = -1; dj <= 1; dj++) for (let di = -1; di <= 1; di++) {
        const ww = W[(sj + dj) * S + si + di];
        if (ww > w) w = ww;
      }
    } else wet = 1;
    of[dst * 4] = h;
    of[dst * 4 + 1] = w < -1e8 ? -1e4 : w;
    const sb = k * 16, db = dst * 16;
    ou[db + 8] = su[sb + 8]; ou[db + 9] = su[sb + 9]; ou[db + 10] = su[sb + 10]; ou[db + 11] = su[sb + 11];
    let nx = (H[k - 1] - H[k + 1]) * inv, nz = (H[k - S] - H[k + S]) * inv;
    const l = 1 / Math.sqrt(nx * nx + nz * nz + 1);
    oi[db + 12] = Math.round(nx * l * 127);
    oi[db + 13] = Math.round(nz * l * 127);
    ou[db + 14] = 0; ou[db + 15] = 0;
    if (h < hmin) hmin = h;
    if (h > hmax) hmax = h;
    if (w > -1e8 && w > hmax) hmax = w;
  };
  for (let j = 0; j < n1; j++) for (let i = 0; i < n1; i++) writeVert(j * n1 + i, i, j);
  let d = n1 * n1;
  for (let t = 0; t < n1; t++) writeVert(d++, t, 0);
  for (let t = 0; t < n1; t++) writeVert(d++, t, n);
  for (let t = 0; t < n1; t++) writeVert(d++, 0, t);
  for (let t = 0; t < n1; t++) writeVert(d++, n, t);
  postMessage({ type: 'far', id: m.id, gen, ok: true, buf: out, hmin, hmax, wet, ms: performance.now() - t0 }, [out]);
}

async function start(m) {
  starting = true;
  const t0 = performance.now();
  try {
    let mod = m.module;
    if (!mod) mod = await WebAssembly.compile(m.bytes);
    const inst = await WebAssembly.instantiate(mod, makeImports(mod));
    ex = inst.exports;
    ex.vw_init(m.seedLo >>> 0, m.seedHi >>> 0);
    gen = m.gen;
    postMessage({ type: 'ready', gen, ms: performance.now() - t0 });
  } catch (e) {
    postMessage({ type: 'fatal', msg: String((e && e.message) || e) });
  }
  starting = false;
  while (queue.length) handle(queue.shift());
}

function handle(m) {
  try {
    if (m.type === 'seed') {
      const t0 = performance.now();
      ex.vw_init(m.seedLo >>> 0, m.seedHi >>> 0);
      gen = m.gen;
      postMessage({ type: 'ready', gen, ms: performance.now() - t0 });
    } else if (m.gen !== gen) {
      postMessage({ type: m.type, id: m.id, gen: m.gen, ok: false, stale: true });
    } else if (m.type === 'chunk') buildChunk(m);
    else if (m.type === 'far') buildFar(m);
  } catch (e) {
    postMessage({ type: m.type, id: m.id, gen: m.gen, ok: false, err: String((e && e.stack) || e) });
  }
}

onmessage = (e) => {
  const m = e.data;
  if (m.type === 'init') { start(m); return; }
  if (starting || !ex) { queue.push(m); return; }
  handle(m);
};
