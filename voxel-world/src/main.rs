//! `voxel-world` command line (std only). OWNER: scout-main.
//!
//! ```text
//! voxel-world shots --seed S --out DIR [--width W --height H] [--only 1,3] [--fast] [--no-aa]
//!                   [--l0 N] [--hf-levels N]
//!     → DIR/seed{S}_{idx}_{name}.png + DIR/seed{S}_shots.json, progress & timings on stderr
//! voxel-world map --seed S --out FILE [--size 1024 --scale 16] [--with-shots] [--cx X --cz Z]
//!     → shaded-relief biome map (PNG), optionally with shot markers and view cones
//! voxel-world plan --seed S            → the 8 planned shots as JSON on stdout (no rendering)
//! ```
//! `--fast` = 960×540, level 0 of 1024², no anti-aliasing (explicit `--width/--height` win).
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use voxel_world::gen::WorldGen;
use voxel_world::render::{self, RenderSettings, ViewMode};
use voxel_world::scout::{self, map, PlannedShot};
use voxel_world::world::{self, BuildParams};

const USAGE: &str = "\
voxel-world — mathematical block-world generator + offline renderer

USAGE:
  voxel-world shots --seed S --out DIR [--width W --height H] [--only 1,3] [--fast] [--no-aa]
                    [--l0 N] [--hf-levels N]
  voxel-world map   --seed S --out FILE [--size 1024 --scale 16] [--with-shots] [--cx X --cz Z]
  voxel-world plan  --seed S

shots: plans 8 shots (panorama, river_valley, coast_sunset, drylands, alpine, forest, cutaway,
       orbital), builds each world clipmap and renders DIR/seed{S}_{idx}_{name}.png plus
       DIR/seed{S}_shots.json. --fast = 960x540, level-0 1024, no AA.
map:   shaded-relief biome map; --with-shots overlays shot markers and view cones (the scale is
       enlarged automatically to fit them unless --scale is given).
plan:  print the planned shots as JSON (no rendering).";

/// Minimal `--key value` / `--flag` parser.
struct Args {
    vals: HashMap<String, String>,
    flags: Vec<String>,
}

impl Args {
    fn parse(raw: &[String], flags: &[&str], keys: &[&str]) -> Result<Args, String> {
        let mut vals = HashMap::new();
        let mut fl = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let a = raw[i].as_str();
            let (name, inline) = match a.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
                _ => (a, None),
            };
            if flags.contains(&name) {
                fl.push(name.to_string());
            } else if keys.contains(&name) {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        raw.get(i).cloned().ok_or_else(|| format!("missing value for {name}"))?
                    }
                };
                vals.insert(name.to_string(), v);
            } else {
                return Err(format!("unknown argument '{a}'"));
            }
            i += 1;
        }
        Ok(Args { vals, flags: fl })
    }
    fn flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
    fn get<T: std::str::FromStr>(&self, k: &str) -> Result<Option<T>, String> {
        match self.vals.get(k) {
            None => Ok(None),
            Some(v) => v.parse::<T>().map(Some).map_err(|_| format!("invalid value for {k}: '{v}'")),
        }
    }
    fn str(&self, k: &str) -> Option<&str> {
        self.vals.get(k).map(|s| s.as_str())
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let res = match raw.first().map(|s| s.as_str()) {
        Some("shots") => cmd_shots(&raw[1..]),
        Some("map") => cmd_map(&raw[1..]),
        Some("plan") => cmd_plan(&raw[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err("expected a command: shots | map | plan".to_string()),
    };
    if let Err(e) = res {
        eprintln!("error: {e}\n\n{USAGE}");
        std::process::exit(2);
    }
}

// ------------------------------------------------------------------ plan

fn plan_timed(gen: &WorldGen) -> (Vec<PlannedShot>, f64) {
    let t = Instant::now();
    let plan = scout::plan_shots_detailed(gen);
    let dt = t.elapsed().as_secs_f64();
    eprintln!("[scout] seed {}: 8 shots planned in {:.2} s", gen.seed, dt);
    for p in &plan {
        let c = &p.shot.camera;
        eprintln!(
            "[scout]  {} {:<13} cam ({:>7.0},{:>4.0},{:>7.0}) yaw {:>4.0}° pitch {:>5.1}° fov {:>2.0}  sun {:>3.0}°/{:>4.1}°  {}{}",
            p.shot.index,
            p.shot.name,
            c.pos[0],
            c.pos[1],
            c.pos[2],
            c.yaw.to_degrees(),
            c.pitch.to_degrees(),
            c.fov_y_deg,
            p.shot.sun.azimuth_deg,
            p.shot.sun.elevation_deg,
            p.shot.title,
            if p.fallback { "  [fallback]" } else { "" }
        );
    }
    (plan, dt)
}

fn cmd_plan(raw: &[String]) -> Result<(), String> {
    let a = Args::parse(raw, &[], &["--seed"])?;
    let seed: u64 = a.get("--seed")?.unwrap_or(1);
    let gen = WorldGen::new(seed);
    let (plan, dt) = plan_timed(&gen);
    let recs: Vec<Option<ShotRecord>> = vec![None; plan.len()];
    println!("{}", json_document(seed, &plan, &recs, dt, None));
    Ok(())
}

// ------------------------------------------------------------------ shots

#[derive(Clone, Debug)]
struct ShotRecord {
    file: String,
    build: BuildParams,
    build_s: f64,
    render_s: f64,
    save_s: f64,
    width: u32,
    height: u32,
    aa: bool,
    spans: usize,
    memory_mb: f64,
    exposure_used: f32,
}

struct RunSettings {
    width: u32,
    height: u32,
    aa: bool,
    fast: bool,
}

fn cmd_shots(raw: &[String]) -> Result<(), String> {
    let a = Args::parse(
        raw,
        &["--fast", "--no-aa"],
        &["--seed", "--out", "--width", "--height", "--only", "--l0", "--hf-levels"],
    )?;
    let seed: u64 = a.get("--seed")?.unwrap_or(1);
    let out = PathBuf::from(a.str("--out").unwrap_or("shots"));
    let fast = a.flag("--fast");
    let width: u32 = a.get("--width")?.unwrap_or(if fast { 960 } else { 1920 });
    let height: u32 = a.get("--height")?.unwrap_or(if fast { 540 } else { 1080 });
    let aa = !(fast || a.flag("--no-aa"));
    let l0_override: Option<u32> = a.get("--l0")?;
    let hf_override: Option<u32> = a.get("--hf-levels")?;
    if let Some(l0) = l0_override {
        if !l0.is_power_of_two() || l0 < 64 {
            return Err("--l0 must be a power of two ≥ 64".into());
        }
    }
    if width == 0 || height == 0 {
        return Err("width/height must be positive".into());
    }
    let only: Option<Vec<usize>> = match a.str("--only") {
        None => None,
        Some(s) => {
            let mut v = Vec::new();
            for part in s.split(',').filter(|p| !p.trim().is_empty()) {
                let part = part.trim();
                let idx = part
                    .parse::<usize>()
                    .ok()
                    .or_else(|| scout::SHOT_NAMES.iter().position(|n| *n == part).map(|i| i + 1))
                    .ok_or_else(|| format!("--only: '{part}' is not a shot index (1–8) or name"))?;
                if !(1..=8).contains(&idx) {
                    return Err(format!("--only: index {idx} out of range 1–8"));
                }
                v.push(idx);
            }
            Some(v)
        }
    };
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let run = RunSettings { width, height, aa, fast };

    let t_all = Instant::now();
    let gen = WorldGen::new(seed);
    let (plan, scout_s) = plan_timed(&gen);
    let mut recs: Vec<Option<ShotRecord>> = vec![None; plan.len()];
    let json_path = out.join(format!("seed{seed}_shots.json"));
    let selected: Vec<usize> =
        (0..plan.len()).filter(|i| only.as_ref().map_or(true, |o| o.contains(&(i + 1)))).collect();
    for (n, &i) in selected.iter().enumerate() {
        let p = &plan[i];
        let shot = &p.shot;
        let mut build = shot.build;
        if let Some(l0) = l0_override {
            build = scout::rebuild_params(shot, l0);
        } else if fast {
            build = scout::rebuild_params(shot, shot.build.l0_size.min(1024));
        }
        if let Some(h) = hf_override {
            if build.hf_levels > 0 {
                build.hf_levels = h;
            }
        }
        eprintln!(
            "[shot {}/{}] #{} {} — {} | l0 {} hf {}×{} centre ({}, {})",
            n + 1,
            selected.len(),
            shot.index,
            shot.name,
            shot.title,
            build.l0_size,
            build.hf_size,
            build.hf_levels,
            build.center_x,
            build.center_z
        );
        let rec = render_shot(&gen, p, build, &run, &out)?;
        eprintln!(
            "[shot {}/{}] #{} {}: build {:.1} s, render {:.1} s, save {:.1} s → {}",
            n + 1,
            selected.len(),
            shot.index,
            shot.name,
            rec.build_s,
            rec.render_s,
            rec.save_s,
            out.join(&rec.file).display()
        );
        recs[i] = Some(rec);
        // metadata after every shot so partial runs stay documented
        let doc = json_document(seed, &plan, &recs, scout_s, Some(&run));
        std::fs::write(&json_path, doc).map_err(|e| format!("cannot write {}: {e}", json_path.display()))?;
    }
    if selected.is_empty() {
        let doc = json_document(seed, &plan, &recs, scout_s, Some(&run));
        std::fs::write(&json_path, doc).map_err(|e| format!("cannot write {}: {e}", json_path.display()))?;
    }
    eprintln!(
        "[done] seed {seed}: {} shot(s) in {:.1} s → {}",
        selected.len(),
        t_all.elapsed().as_secs_f64(),
        json_path.display()
    );
    Ok(())
}

fn render_shot(gen: &WorldGen, p: &PlannedShot, build: BuildParams, run: &RunSettings, out: &Path) -> Result<ShotRecord, String> {
    let shot = &p.shot;
    let t = Instant::now();
    let w = world::build_world(gen, &build);
    let build_s = t.elapsed().as_secs_f64();
    let spans = w.l0.span_count();
    let memory_mb = w.memory_bytes() as f64 / (1024.0 * 1024.0);
    eprintln!("    world: {:.1} s, {} spans, ~{:.0} MiB", build_s, spans, memory_mb);
    let settings = RenderSettings {
        width: run.width,
        height: run.height,
        aa: run.aa,
        exposure: shot.exposure,
        mode: shot.mode,
        haze: shot.haze,
        max_distance: if shot.name == "orbital" { 260_000.0 } else { 100_000.0 },
    };
    let t = Instant::now();
    let (img, st) = render::render_with_stats(&w, &shot.camera, &shot.sun, &settings);
    let render_s = t.elapsed().as_secs_f64();
    eprintln!(
        "    render: {:.1} s (setup {:.2}, pass1 {:.1}, aa {:.1} on {} px, post {:.2}), exposure {:.2}",
        render_s, st.setup_s, st.pass1_s, st.aa_s, st.aa_pixels, st.post_s, st.exposure
    );
    drop(w);
    let file = format!("seed{}_{}_{}.png", gen.seed, shot.index, shot.name);
    let t = Instant::now();
    render::save_png(&img, &out.join(&file)).map_err(|e| format!("cannot write {file}: {e}"))?;
    let save_s = t.elapsed().as_secs_f64();
    Ok(ShotRecord {
        file,
        build,
        build_s,
        render_s,
        save_s,
        width: run.width,
        height: run.height,
        aa: run.aa,
        spans,
        memory_mb,
        exposure_used: st.exposure,
    })
}

// ------------------------------------------------------------------ map

fn cmd_map(raw: &[String]) -> Result<(), String> {
    let a = Args::parse(raw, &["--with-shots"], &["--seed", "--out", "--size", "--scale", "--cx", "--cz"])?;
    let seed: u64 = a.get("--seed")?.unwrap_or(1);
    let out = PathBuf::from(a.str("--out").unwrap_or("map.png"));
    let size: u32 = a.get("--size")?.unwrap_or(1024);
    let scale_arg: Option<f64> = a.get("--scale")?;
    let cx: f64 = a.get("--cx")?.unwrap_or(0.0);
    let cz: f64 = a.get("--cz")?.unwrap_or(0.0);
    if !(16..=8192).contains(&size) {
        return Err("--size must be in 16..=8192".into());
    }
    let gen = WorldGen::new(seed);
    let plan = if a.flag("--with-shots") { plan_timed(&gen).0 } else { Vec::new() };
    let scale = match scale_arg {
        Some(s) if s > 0.0 => s,
        Some(_) => return Err("--scale must be positive".into()),
        None if !plan.is_empty() => map::fit_scale(&plan, cx, cz, size, 16.0, 1500.0),
        None => 16.0,
    };
    let mp = map::MapParams { cx, cz, size, scale };
    let t = Instant::now();
    let rgb = map::render_map(&gen, mp, &plan);
    eprintln!(
        "[map] seed {seed}: {size}×{size} px at {scale} blocks/px ({:.1} km across) in {:.2} s",
        size as f64 * scale / 1000.0,
        t.elapsed().as_secs_f64()
    );
    voxel_world::imageio::write_png_rgb8(&out, size, size, &rgb).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    eprintln!("[map] → {}", out.display());
    Ok(())
}

// ------------------------------------------------------------------ JSON (hand-written)

fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn jf(v: f64, prec: usize) -> String {
    if v.is_finite() {
        format!("{:.*}", prec, v)
    } else {
        "null".into()
    }
}

fn json_build(b: &BuildParams) -> String {
    format!(
        "{{\"center_x\": {}, \"center_z\": {}, \"l0_size\": {}, \"hf_size\": {}, \"hf_levels\": {}, \"cave_radius\": {}, \"feature_radius\": {}}}",
        b.center_x, b.center_z, b.l0_size, b.hf_size, b.hf_levels, b.cave_radius, b.feature_radius
    )
}

fn json_document(seed: u64, plan: &[PlannedShot], recs: &[Option<ShotRecord>], scout_s: f64, run: Option<&RunSettings>) -> String {
    let mut o = String::new();
    o.push_str("{\n");
    let _ = writeln!(o, "  \"seed\": {seed},");
    let _ = writeln!(o, "  \"generator\": {},", js(concat!("voxel-world ", env!("CARGO_PKG_VERSION"))));
    let _ = writeln!(
        o,
        "  \"scout\": {{\"radius\": {}, \"step\": {}, \"min_separation\": {}, \"seconds\": {}}},",
        scout::SCOUT_RADIUS,
        scout::SCOUT_STEP,
        scout::MIN_SEPARATION,
        jf(scout_s, 3)
    );
    if let Some(r) = run {
        let _ = writeln!(
            o,
            "  \"settings\": {{\"width\": {}, \"height\": {}, \"aa\": {}, \"fast\": {}}},",
            r.width, r.height, r.aa, r.fast
        );
    }
    o.push_str("  \"shots\": [\n");
    for (i, p) in plan.iter().enumerate() {
        let s = &p.shot;
        let c = &s.camera;
        let mode = match s.mode {
            ViewMode::Normal => "\"normal\"".to_string(),
            ViewMode::Diorama { min_x, max_x, min_z, max_z, min_y } => format!(
                "{{\"diorama\": {{\"min_x\": {min_x}, \"max_x\": {max_x}, \"min_z\": {min_z}, \"max_z\": {max_z}, \"min_y\": {min_y}}}}}"
            ),
        };
        o.push_str("    {\n");
        let _ = writeln!(o, "      \"index\": {},", s.index);
        let _ = writeln!(o, "      \"name\": {},", js(&s.name));
        let _ = writeln!(o, "      \"title\": {},", js(&s.title));
        let _ = writeln!(
            o,
            "      \"camera\": {{\"pos\": [{}, {}, {}], \"yaw_deg\": {}, \"pitch_deg\": {}, \"fov_y_deg\": {}}},",
            jf(c.pos[0], 2),
            jf(c.pos[1], 2),
            jf(c.pos[2], 2),
            jf(c.yaw.to_degrees(), 3),
            jf(c.pitch.to_degrees(), 3),
            jf(c.fov_y_deg, 2)
        );
        let _ = writeln!(
            o,
            "      \"sun\": {{\"azimuth_deg\": {}, \"elevation_deg\": {}}},",
            jf(s.sun.azimuth_deg, 3),
            jf(s.sun.elevation_deg, 3)
        );
        let _ = writeln!(o, "      \"mode\": {mode},");
        let _ = writeln!(o, "      \"build\": {},", json_build(&s.build));
        let _ = writeln!(o, "      \"exposure\": {},", jf(s.exposure as f64, 3));
        let _ = writeln!(o, "      \"haze\": {},", jf(s.haze as f64, 3));
        let _ = writeln!(
            o,
            "      \"scout\": {{\"score\": {}, \"target\": [{}, {}], \"ground_y\": {}, \"biome\": {}, \"fallback\": {}, \"note\": {}}},",
            jf(p.score as f64, 3),
            jf(p.target[0], 1),
            jf(p.target[1], 1),
            jf(p.ground_y as f64, 1),
            js(p.biome.name()),
            p.fallback,
            js(&p.note)
        );
        match &recs[i] {
            Some(r) => {
                let _ = writeln!(o, "      \"rendered\": true,");
                let _ = writeln!(o, "      \"file\": {},", js(&r.file));
                let _ = writeln!(o, "      \"image\": {{\"width\": {}, \"height\": {}, \"aa\": {}, \"exposure_used\": {}}},", r.width, r.height, r.aa, jf(r.exposure_used as f64, 4));
                let _ = writeln!(o, "      \"build_used\": {},", json_build(&r.build));
                let _ = writeln!(
                    o,
                    "      \"timings\": {{\"build_s\": {}, \"render_s\": {}, \"save_s\": {}}},",
                    jf(r.build_s, 3),
                    jf(r.render_s, 3),
                    jf(r.save_s, 3)
                );
                let _ = writeln!(o, "      \"world\": {{\"spans\": {}, \"memory_mib\": {}}}", r.spans, jf(r.memory_mb, 1));
            }
            None => {
                let _ = writeln!(o, "      \"rendered\": false");
            }
        }
        o.push_str(if i + 1 < plan.len() { "    },\n" } else { "    }\n" });
    }
    o.push_str("  ]\n}\n");
    o
}
