//! render-review: rebuild one planned shot and render it (optionally probing pixels).
//! usage: render-review_shot SEED SHOT_INDEX [W H] [aa 0/1] [tag] [probe x,y ...]
use voxel_world::gen::WorldGen;
use voxel_world::render::shade::Frame;
use voxel_world::render::trace::{HitKind, Ray};
use voxel_world::render::{render_with_stats, save_png, RenderSettings};
use voxel_world::world::build_world;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seed: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(2026);
    let idx: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let w: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(960);
    let h: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(540);
    let aa = args.get(4).map(|s| s == "1").unwrap_or(false);
    let tag = args.get(5).cloned().unwrap_or_default();
    let probes: Vec<(f32, f32)> = args
        .iter()
        .skip(6)
        .filter_map(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect();
    let gen = WorldGen::new(seed);
    let t = std::time::Instant::now();
    let shots = voxel_world::scout::plan_shots(&gen);
    let shot = shots.iter().find(|s| s.index == idx).expect("shot");
    eprintln!("plan {:.1}s: {} {:?} sun {:?}", t.elapsed().as_secs_f64(), shot.name, shot.camera, shot.sun);
    let t = std::time::Instant::now();
    let world = build_world(&gen, &shot.build);
    eprintln!("build {:.1}s", t.elapsed().as_secs_f64());
    let s = RenderSettings {
        width: w,
        height: h,
        aa,
        exposure: shot.exposure,
        mode: shot.mode,
        haze: shot.haze,
        max_distance: if shot.name == "orbital" { 260_000.0 } else { 100_000.0 },
    };
    if !probes.is_empty() {
        let f = Frame::new(&world, &shot.camera, &shot.sun, &s);
        for (px, py) in probes {
            let d = f.primary_dir(px, py);
            let ray = Ray::new(shot.camera.pos, [d.x as f64, d.y as f64, d.z as f64]);
            match f.scene.trace(&ray, 0.0, s.max_distance as f64, true) {
                Some(hh) => {
                    let extra = match hh.kind {
                        HitKind::Height { level, .. } => {
                            let v = &f.scene.hf[level as usize];
                            let (ci, cj) = v.lv.cell_of(hh.p[0], hh.p[2]);
                            if v.lv.in_range(ci, cj) {
                                let k = v.lv.index(ci, cj);
                                format!("cell h {} water {} mat {:?} biome {:?} alb {:?}", v.lv.h[k], v.lv.water[k], v.lv.mat[k], v.lv.biome[k], v.lv.albedo[k])
                            } else {
                                String::new()
                            }
                        }
                        HitKind::Voxel { bx, bz, .. } => format!("spans {:?}", world.l0.spans(bx, bz)),
                    };
                    eprintln!("probe ({px},{py}): t={:.1} p={:?} n={:?} {:?} {}", hh.t, hh.p, hh.n, hh.kind, extra)
                }
                None => eprintln!("probe ({px},{py}): miss dir {:?}", d),
            }
        }
    }
    let (img, st) = render_with_stats(&world, &shot.camera, &shot.sun, &s);
    eprintln!(
        "render {:.1}s (setup {:.2} pass1 {:.1} aa {:.1} on {} px, post {:.2}) exposure {:.3}",
        st.setup_s + st.pass1_s + st.aa_s + st.post_s,
        st.setup_s,
        st.pass1_s,
        st.aa_s,
        st.aa_pixels,
        st.post_s,
        st.exposure
    );
    let path = format!("target/agent-render-review/s{seed}_{idx}_{}{tag}.png", shot.name);
    save_png(&img, std::path::Path::new(&path)).unwrap();
    eprintln!("-> {path}");
}
