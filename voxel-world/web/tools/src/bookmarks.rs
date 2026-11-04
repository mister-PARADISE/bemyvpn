//! Export the scouted shots of seeds 1, 42, 2026 as viewer bookmarks.
//!
//! `cargo run --release --bin bookmarks -- [OUT.json] [SEED ...]`
//! (default out: ../www/bookmarks.json relative to this crate, seeds 1 42 2026).
//!
//! JSON: {"convention": "yaw0=+Z,yaw90=+X", "angles": "...", "<seed>": [{name, title, x, y, z,
//! yaw, pitch, sun_az, sun_el, fov, index}, ...]}. yaw/pitch in radians
//! (`math::dir_from_yaw_pitch`: yaw 0 → +Z, yaw π/2 → +X, pitch > 0 up); sun_az/sun_el in
//! degrees (same azimuth convention); fov = vertical field of view in degrees. The cut-away
//! diorama shot (index 7) is skipped: it views a cut-out block from outside and makes no sense
//! in a continuous world.
use std::fmt::Write as _;
use std::time::Instant;
use voxel_world::gen::WorldGen;
use voxel_world::render::ViewMode;
use voxel_world::scout::plan_shots_detailed;

fn json_str(s: &str) -> String {
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

fn num(v: f64, digits: usize) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.digits$}");
    if s.contains('.') {
        let t = s.trim_end_matches('0').trim_end_matches('.');
        if t.is_empty() || t == "-" {
            "0".into()
        } else {
            t.into()
        }
    } else {
        s
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().cloned().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../www/bookmarks.json").to_string());
    let mut seeds: Vec<u64> = args.iter().skip(1).filter_map(|s| s.parse().ok()).collect();
    if seeds.is_empty() {
        seeds = vec![1, 42, 2026];
    }
    let mut j = String::new();
    j.push_str("{\n  \"convention\": \"yaw0=+Z,yaw90=+X\",\n");
    j.push_str("  \"angles\": \"yaw/pitch radians (pitch>0 up); sun_az/sun_el/fov degrees\",\n");
    for (si, &seed) in seeds.iter().enumerate() {
        let t = Instant::now();
        let gen = WorldGen::new(seed);
        let shots = plan_shots_detailed(&gen);
        eprintln!("seed {seed}: {} shots planned in {:.1} s", shots.len(), t.elapsed().as_secs_f64());
        let _ = write!(j, "  \"{seed}\": [\n");
        let kept: Vec<_> = shots.iter().filter(|p| !matches!(p.shot.mode, ViewMode::Diorama { .. }) && p.shot.name != "cutaway").collect();
        for (k, p) in kept.iter().enumerate() {
            let s = &p.shot;
            let c = &s.camera;
            eprintln!(
                "  #{} {:<13} ({:.0}, {:.0}, {:.0}) yaw {:.1}° pitch {:.1}° sun {:.0}/{:.0}  {}",
                s.index,
                s.name,
                c.pos[0],
                c.pos[1],
                c.pos[2],
                c.yaw.to_degrees(),
                c.pitch.to_degrees(),
                s.sun.azimuth_deg,
                s.sun.elevation_deg,
                s.title
            );
            let _ = write!(
                j,
                "    {{\"name\": {}, \"title\": {}, \"x\": {}, \"y\": {}, \"z\": {}, \"yaw\": {}, \"pitch\": {}, \"sun_az\": {}, \"sun_el\": {}, \"fov\": {}, \"index\": {}}}{}\n",
                json_str(&s.name),
                json_str(&s.title),
                num(c.pos[0], 1),
                num(c.pos[1], 1),
                num(c.pos[2], 1),
                num(c.yaw, 5),
                num(c.pitch, 5),
                num(s.sun.azimuth_deg, 2),
                num(s.sun.elevation_deg, 2),
                num(c.fov_y_deg, 1),
                s.index,
                ","
            );
            let _ = k;
        }
        // world tree: the nearest one to the origin, seen from 1.3 tree heights away
        let mut best: Option<(f64, std::rc::Rc<voxel_world::gen::worldtree::WorldTree>)> = None;
        for iz in -3..3 {
            for ix in -3..3 {
                if let Some(t) = voxel_world::gen::worldtree::tree_of_cell(&gen, ix, iz) {
                    let d = (t.x as f64).hypot(t.z as f64);
                    if best.as_ref().map_or(true, |b| d < b.0) {
                        best = Some((d, t));
                    }
                }
            }
        }
        if let Some((_, t)) = best {
            use voxel_world::gen::worldtree::WtKind;
            use voxel_world::materials::Material;
            let h = t.height as f64;
            let dist = 1.3 * h + 60.0;
            let cy = t.y0 as f64 + 0.42 * h;
            // pick the azimuth whose camera spot is clear of terrain with the most margin
            let mut pick = (f64::MIN, 0.0f64, 0.0f64, 0.0f64);
            for k in 0..16 {
                let a = k as f64 * std::f64::consts::TAU / 16.0;
                let (px, pz) = (t.x as f64 + dist * a.sin(), t.z as f64 + dist * a.cos());
                let mut clear = f64::MAX;
                for q in 1..=8 {
                    let f = q as f64 / 8.0;
                    let c = gen.column(t.x as f64 + (px - t.x as f64) * f, t.z as f64 + (pz - t.z as f64) * f);
                    let top = (c.height as f64).max(c.water_level as f64);
                    clear = clear.min(cy - top);
                }
                if clear > pick.0 {
                    pick = (clear, px, pz, a);
                }
            }
            let (_, px, pz, a) = pick;
            let py = cy.max(gen.column(px, pz).height as f64 + 25.0);
            // look back at the crown (yaw0 = +Z)
            let yaw = (a + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU);
            let pitch = ((t.y0 as f64 + 0.6 * h - py) / dist).atan();
            let what = match (t.kind, t.log) {
                (WtKind::Conifer, _) => "исполинская ель",
                (WtKind::Umbrella, Material::AcaciaLog) => "исполинская акация",
                (WtKind::Umbrella, _) => "древо-зонтик над джунглями",
                (_, Material::BirchLog) => "исполинская берёза",
                (_, Material::DarkOakLog) => "исполинский тёмный дуб",
                _ => "исполинский дуб",
            };
            let title = format!("Мировое древо: {what}, {:.0} м", h);
            eprintln!("  #9 world_tree   ({:.0}, {:.0}, {:.0}) yaw {:.1}°  {}", px, py, pz, yaw.to_degrees(), title);
            let _ = write!(
                j,
                "    {{\"name\": \"world_tree\", \"title\": {}, \"x\": {}, \"y\": {}, \"z\": {}, \"yaw\": {}, \"pitch\": {}, \"sun_az\": {}, \"sun_el\": {}, \"fov\": 60, \"index\": 9}},\n",
                json_str(&title),
                num(px, 1),
                num(py, 1),
                num(pz, 1),
                num(yaw, 5),
                num(pitch, 5),
                num((a.to_degrees() + 140.0).rem_euclid(360.0), 2),
                num(28.0, 2),
            );
        }
        // (drop the trailing comma of the last entry)
        if j.ends_with(",\n") {
            j.truncate(j.len() - 2);
            j.push('\n');
        }
        let _ = write!(j, "  ]{}\n", if si + 1 < seeds.len() { "," } else { "" });
    }
    j.push_str("}\n");
    std::fs::write(&out, j).expect("write bookmarks");
    eprintln!("wrote {out}");
}
