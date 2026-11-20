//! `haze snapshot-check <dir> [--update]`: visual regression check.
//!
//! Renders a fixed set of views offscreen against a fixture library (the starter rigs plus one
//! setlist, so the result does not depend on the user's own presets), downsamples them 2× and
//! compares with the goldens in `<dir>`. A view fails when more than 0.2 % of its pixels differ by
//! more than 32 levels in any channel. `--update` rewrites the goldens.
//!
//! The UI fonts ship inside the binary (`ui/fonts`), so goldens only depend on the system for
//! fallback glyphs the bundled faces lack.

use crate::panel::Panel;
use crate::templates::TEMPLATES;
use katana_core::{Library, SetlistKind};
use std::path::{Path, PathBuf};
use std::process::Command;

/// (name, extra args, env)
const VIEWS: &[(&str, &[&str], &[(&str, &str)])] = &[
    ("rig", &[], &[("HAZE_FAKE_AMP", "1")]),
    ("rig-eq", &["--tab", "5"], &[]),
    ("rig-delay", &["--tab", "8"], &[]),
    ("library", &[], &[("HAZE_PAGE", "1")]),
    ("setlists", &[], &[("HAZE_PAGE", "2")]),
    ("starters", &[], &[("HAZE_PAGE", "3")]),
    ("picker", &[], &[("HAZE_PICKER", "1")]),
    ("live", &[], &[("HAZE_PAGE", "2"), ("HAZE_LIVE", "2")]),
    ("standalone", &[], &[("HAZE_PC_MODE", "1")]),
];

const MAX_BAD: f64 = 0.002;
const TOL: u8 = 32;

/// Fixture library: every starter rig, and a setlist of the first four.
pub fn fixture(root: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_dir_all(root);
    let mut lib = Library::open(root)?;
    let panel = Panel::new();
    let mut ids = vec![];
    for t in TEMPLATES {
        ids.push(lib.add_patch(t.build(&panel), "fixture", t.blurb)?.0);
    }
    lib.add_setlist("Friday gig", SetlistKind::Setlist, ids[..4].to_vec())?;
    Ok(())
}

fn decode(p: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(p).ok()?));
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let ch = info.color_type.samples();
    buf.truncate(info.buffer_size());
    // to RGB
    let rgb = buf.chunks(ch).flat_map(|p| [p[0], p[1.min(ch - 1)], p[2.min(ch - 1)]]).collect();
    Some((info.width, info.height, rgb))
}

/// 2× box downsample of an RGB image.
fn half(w: u32, h: u32, rgb: &[u8]) -> (u32, u32, Vec<u8>) {
    let (w2, h2) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((w2 * h2 * 3) as usize);
    for y in 0..h2 {
        for x in 0..w2 {
            for c in 0..3 {
                let at = |dx: u32, dy: u32| rgb[(((2 * y + dy) * w + 2 * x + dx) * 3 + c) as usize] as u32;
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    (w2, h2, out)
}

fn encode(p: &Path, w: u32, h: u32, rgb: &[u8]) -> std::io::Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(p)?);
    let mut e = png::Encoder::new(f, w, h);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.set_compression(png::Compression::High);
    e.write_header().map_err(std::io::Error::other)?.write_image_data(rgb).map_err(std::io::Error::other)
}

/// Share of pixels where some channel differs by more than `TOL`.
pub fn bad_share(a: &[u8], b: &[u8]) -> f64 {
    let bad = a.chunks(3).zip(b.chunks(3)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > TOL)).count();
    bad as f64 / (a.len() / 3).max(1) as f64
}

pub fn run(dir: &Path, update: bool) -> bool {
    let tmp: PathBuf = std::env::temp_dir().join(format!("haze-check-{}", std::process::id()));
    let lib = tmp.join("lib");
    if let Err(e) = fixture(&lib) {
        eprintln!("fixture: {e}");
        return false;
    }
    let _ = std::fs::create_dir_all(dir);
    let exe = std::env::current_exe().expect("exe");
    let mut ok = true;
    for (name, args, env) in VIEWS {
        let shot = tmp.join(format!("{name}.png"));
        let mut c = Command::new(&exe);
        c.arg("--png").arg(&shot).args(*args).env("KATANA_STUDIO_LIBRARY", &lib);
        for k in ["HAZE_FAKE_AMP", "HAZE_PAGE", "HAZE_PICKER", "HAZE_LIVE", "HAZE_PC_MODE", "HAZE_SCALE", "HAZE_ADD"] {
            c.env_remove(k);
        }
        c.envs(env.iter().copied());
        let run = c.output();
        let Some((w, h, rgb)) = run.ok().filter(|o| o.status.success()).and_then(|_| decode(&shot)) else {
            println!("FAIL {name}: render failed");
            ok = false;
            continue;
        };
        let (w, h, small) = half(w, h, &rgb);
        let golden = dir.join(format!("{name}.png"));
        if update {
            match encode(&golden, w, h, &small) {
                Ok(()) => println!("updated {}", golden.display()),
                Err(e) => {
                    println!("FAIL {name}: {e}");
                    ok = false;
                }
            }
            continue;
        }
        match decode(&golden) {
            None => {
                println!("FAIL {name}: no golden {} (run with --update)", golden.display());
                ok = false;
            }
            Some((gw, gh, _)) if (gw, gh) != (w, h) => {
                println!("FAIL {name}: size {w}x{h}, golden {gw}x{gh}");
                ok = false;
            }
            Some((_, _, g)) => {
                let share = bad_share(&small, &g);
                let pass = share <= MAX_BAD;
                println!("{} {name}: {:.3} % pixels differ", if pass { "ok  " } else { "FAIL" }, share * 100.0);
                if !pass {
                    let diff = tmp.join(format!("{name}-now.png"));
                    let _ = encode(&diff, w, h, &small);
                    println!("     current render: {}", diff.display());
                    ok = false;
                }
            }
        }
    }
    if ok {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_and_share() {
        let rgb = [10u8, 20, 30].repeat(16); // 4x4
        let (w, h, s) = half(4, 4, &rgb);
        assert_eq!((w, h), (2, 2));
        assert_eq!(&s[..3], &[10, 20, 30]);
        let mut other = s.clone();
        other[0] = 200;
        assert_eq!(bad_share(&s, &s), 0.0);
        assert_eq!(bad_share(&s, &other), 0.25);
    }
}
