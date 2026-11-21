//! Katana Studio — tone librarian & editor for BOSS KATANA MkII.
//!
//! `haze`                             GUI
//! `haze import <path>`      import .tsl file, folder of .tsl, or BTS `pref` dir
//! `haze import-windows`     find BOSS Tone Studio on mounted Windows partitions and import
//! `haze list`               print the library
//! `haze dump <tone>`        every parameter as BLOCK.PRM=value
//! `haze derive <base> <name> BLOCK.PRM=value…`  new tone from a copy of <base>, saved to the library
//! `haze dsp-render out.wav [s]` render the DI riff through the PC DSP chain
//! `haze dsp-probe [input]`  open the audio device muted for 3 s, report callback load
//! `haze dsp-devices`        list selectable audio inputs / outputs
//! `haze --png out.png [--tab N] [--hover X,Y] [--size WxH]`  render one frame offscreen (CPU renderer) for visual checks
//! `haze snapshot-check <dir> [--update]`  render the fixed views against a fixture library, compare with goldens

// Windows: a GUI exe opens no console window; CLI subcommands attach to the caller's console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod check;
mod panel;
mod snapshot;
mod eqcurve;
mod pcfx;
mod practice;
mod history;
mod templates;
#[cfg(test)]
mod uitest;

use katana_core::Library;

slint::include_modules!();

fn main() {
/// GUI-subsystem exe started from cmd/PowerShell: print into that console. Skipped when the
/// parent already gave us handles (pipes from `snapshot-check`, redirects).
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::env::var_os("KATANA_STUDIO_LIBRARY").map(Into::into).unwrap_or_else(Library::default_root);
    #[cfg(windows)]
    if !args.is_empty() {
        attach_console();
    }
    let mut lib = match Library::open(&root) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Nie mogę otworzyć biblioteki {}: {e}", root.display());
            std::process::exit(1);
        }
    };
    match args.first().map(String::as_str) {
        Some("import") => {
            let Some(p) = args.get(1) else { return eprintln!("użycie: haze import <plik.tsl|folder>") };
            let p = std::path::Path::new(p);
            let r = if p.join("ID_LIST").exists() { lib.import_bts_pref_dir(p) } else { lib.import_tsl_file(p) };
            report(r);
        }
        Some("import-windows") => {
            let dirs = app::find_bts_dirs();
            if dirs.is_empty() {
                if cfg!(windows) {
                    eprintln!("Nie znaleziono biblioteki BOSS Tone Studio w %LOCALAPPDATA%\\Roland.");
                } else {
                    eprintln!("Nie znaleziono BOSS Tone Studio na zamontowanych partycjach (/mnt, /media, /run/media).");
                }
                std::process::exit(2);
            }
            for d in dirs {
                println!("{}", d.display());
                report(lib.import_bts_pref_dir(&d));
            }
        }
        Some("list") => {
            for t in &lib.tones {
                let s = t.patch.summary();
                println!("{:<17} {:<18} {}", s.name, s.amp, t.origin);
            }
            println!("— {} brzmień, {} setlist · {}", lib.tones.len(), lib.setlists.len(), lib.root().display());
        }
        Some("snapshot-check") => {
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map_or("crates/katana-studio/tests/golden", String::as_str);
            let ok = check::run(std::path::Path::new(dir), args.iter().any(|a| a == "--update"));
            std::process::exit(if ok { 0 } else { 1 });
        }
        Some("--png") => {
            let out = args.get(1).cloned().unwrap_or_else(|| "haze.png".into());
            let tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).and_then(|t| t.parse().ok()).unwrap_or(-1);
            std::env::set_var("KATANA_NO_DEVICE", "1");
            let hover = args.iter().position(|a| a == "--hover").and_then(|i| args.get(i + 1)).and_then(|v| {
                let (x, y) = v.split_once(',')?;
                Some((x.parse().ok()?, y.parse().ok()?))
            });
            let (w, h) = args
                .iter()
                .position(|a| a == "--size")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or((1600, 1000));
            snapshot::render(lib, &out, tab, hover, w, h);
        }
        Some("dsp-render") => {
            // offline render of the built-in DI riff through the PC chain (drive, cab, delay, reverb)
            let out = args.get(1).cloned().unwrap_or_else(|| "haze-dsp.wav".into());
            let secs: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(9.6);
            let p = std::sync::Arc::new(haze_dsp::Params::default());
            use haze_dsp::P;
            for (k, v) in [(P::DriveOn, 1.0), (P::AmpVoicing, 3.0), (P::AmpGain, 0.6), (P::DelayOn, 1.0), (P::ReverbOn, 1.0), (P::ModOn, 1.0)] {
                p.set(k, v);
            }
            let fs = 48000;
            let t = std::time::Instant::now();
            let buf = haze_dsp::render_offline(p, fs as f32, secs);
            let el = t.elapsed();
            match haze_dsp::write_wav(std::path::Path::new(&out), fs, &buf) {
                Ok(()) => println!("{out}: {secs} s, rendered in {:.0} ms", el.as_secs_f32() * 1000.0),
                Err(e) => eprintln!("{e}"),
            }
        }
        Some("dsp-devices") => {
            for (k, inp) in [("wejścia", true), ("wyjścia", false)] {
                println!("{k}:");
                for d in haze_dsp::list_devices(inp) {
                    println!("  {:<28} {}{}{}", d.id, d.label, if d.mic { "  [mikrofon?]" } else { "" }, if d.busy { "  [zajęte]" } else { "" });
                }
            }
        }
        Some("dsp-probe") => {
            // open the real audio device for 3 s with the output muted: proves cpal I/O + load
            let p = std::sync::Arc::new(haze_dsp::Params::default());
            p.set(haze_dsp::P::Source, 1.0);
            p.set(haze_dsp::P::OutGainDb, -120.0);
            let cfg = haze_dsp::Config { input: args.get(1).cloned(), ..Default::default() };
            match haze_dsp::Engine::start(p, &cfg) {
                Ok(e) => {
                    println!("{:?}", e.info);
                    let mut worst = 0f32;
                    for _ in 0..30 {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        worst = worst.max(e.load());
                    }
                    println!("callback load peak {:.1}%, xruns {}", worst * 100.0, e.meters.xruns.load(std::sync::atomic::Ordering::Relaxed));
                }
                Err(e) => eprintln!("engine: {e}"),
            }
        }
        Some("probe") => probe(&lib, args.get(1).map(String::as_str)),
        Some("dump") => {
            let Some(q) = args.get(1) else { return eprintln!("użycie: haze dump <brzmienie>") };
            dump(&find_tone(&lib, q).patch);
        }
        Some("derive") => {
            let (Some(base), Some(name)) = (args.get(1), args.get(2)) else {
                return eprintln!("użycie: haze derive <bazowe brzmienie> <nowa nazwa> BLOK.PRM=wartość…");
            };
            derive(&mut lib, base, name, &args[3..]);
        }
        Some(other) if other != "--gui" => {
            eprintln!("nieznana komenda: {other}");
            std::process::exit(2);
        }
        _ => run_gui(lib),
    }
}

/// `haze probe [tone]`: hardware check without the GUI. Connects, prints the channel names and
/// the temporary patch; with a tone name, auditions it (temporary area only, nothing stored)
/// and reads the temporary patch back to prove the amp took it.
fn probe(lib: &Library, tone: Option<&str>) {
    use katana_midi::{Cmd, Device, Event};
    use std::time::Duration;
    let (tx, rx) = std::sync::mpsc::channel();
    let dev = Device::spawn(move |e| {
        let _ = tx.send(e);
    });
    let wait = |what: &str, f: &mut dyn FnMut(&Event) -> bool| loop {
        match rx.recv_timeout(Duration::from_secs(8)) {
            Ok(e) => {
                if let Event::Error(m) | Event::Disconnected(m) = &e {
                    eprintln!("BŁĄD: {m}");
                    std::process::exit(1);
                }
                if f(&e) {
                    return e;
                }
            }
            Err(_) => {
                eprintln!("BŁĄD: brak odpowiedzi ({what})");
                std::process::exit(1);
            }
        }
    };
    dev.send(Cmd::Connect);
    if let Event::Connected { port } = wait("połączenie", &mut |e| matches!(e, Event::Connected { .. })) {
        println!("połączono: {port}");
    }
    if let Event::SlotNames(n) = wait("nazwy kanałów", &mut |e| matches!(e, Event::SlotNames(_))) {
        println!("kanały: {}", n.join(" | "));
    }
    if let Event::TempPatch(p) = wait("bufor", &mut |e| matches!(e, Event::TempPatch(_))) {
        println!("gra teraz: {}", p.name());
    }
    let Some(q) = tone else { return };
    let q = q.to_lowercase();
    let Some(t) = lib.tones.iter().find(|t| t.name.to_lowercase().contains(&q)) else {
        eprintln!("nie ma brzmienia „{q}”");
        std::process::exit(2);
    };
    let t0 = std::time::Instant::now();
    dev.send(Cmd::Audition(Box::new(t.patch.clone())));
    // experiment hooks (KATANA_PROBE=twice | poke:REL=VAL)
    match std::env::var("KATANA_PROBE").as_deref() {
        Ok("twice") => {
            std::thread::sleep(Duration::from_millis(80));
            dev.send(Cmd::Audition(Box::new(t.patch.clone())));
        }
        Ok(s) if s.starts_with("poke:") => {
            let (r, v) = s[5..].split_once('=').unwrap();
            std::thread::sleep(Duration::from_millis(300));
            dev.send(Cmd::Param { rel: r.parse().unwrap(), data: vec![v.parse().unwrap()] });
        }
        _ => {}
    }
    dev.send(Cmd::ReadTemp);
    if let Event::TempPatch(p) = wait("odczyt po wysłaniu", &mut |e| matches!(e, Event::TempPatch(_))) {
        let same = *p == t.patch;
        println!("wysłano: {} · wzmacniacz gra: {} · zgodne bajt w bajt: {} · {:?}", t.name, p.name(), same, t0.elapsed());
        if !same {
            for i in 0..katana_core::map::PATCH_BLOCKS.len() {
                if p.block(i) != t.patch.block(i) {
                    let b = &katana_core::map::PATCH_BLOCKS[i];
                    let diffs: Vec<String> = p
                        .block(i)
                        .iter()
                        .zip(t.patch.block(i))
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .map(|(o, (a, s))| {
                            let n = b.params.iter().find(|d| d.off as usize == o).map_or("?", |d| d.name);
                            format!("{o}:{n} wysł={s} jest={a}")
                        })
                        .collect();
                    println!("  {} ({} B): {}", b.key, diffs.len(), diffs.join(", "));
                }
            }
        }
    }
    dev.send(Cmd::Shutdown);
    std::thread::sleep(Duration::from_millis(100));
}

fn find_tone<'a>(lib: &'a Library, q: &str) -> &'a katana_core::library::Tone {
    let q = q.to_lowercase();
    let exact = lib.tones.iter().find(|t| t.name.to_lowercase() == q);
    exact.or_else(|| lib.tones.iter().find(|t| t.name.to_lowercase().contains(&q))).unwrap_or_else(|| {
        eprintln!("nie ma brzmienia „{q}”");
        std::process::exit(2);
    })
}

/// Every parameter of a patch as BLOCK.PRM=value (label for lists), the syntax `derive` takes.
fn dump(p: &katana_core::Patch) {
    use katana_core::map::{ParamRef, PATCH_BLOCKS};
    for (b, bd) in PATCH_BLOCKS.iter().enumerate() {
        for d in bd.params.iter().filter(|d| !d.prm.is_empty()) {
            let v = p.get(ParamRef { block: b, param: d });
            let l = d.label(v).map(|l| format!("  ({l})")).unwrap_or_default();
            println!("{}.{}={v}{l}   [{}..{}] {}", bd.key, d.prm, d.min, d.max, d.name);
        }
    }
}

/// New tone = copy of `base` with BLOCK.PRM=value changes, saved to the library.
fn derive(lib: &mut Library, base: &str, name: &str, sets: &[String]) {
    use katana_core::map::{ParamRef, PATCH_BLOCKS};
    let mut p = find_tone(lib, base).patch.clone();
    for kv in sets {
        let Some((k, v)) = kv.split_once('=') else { return eprintln!("zły zapis: {kv}") };
        let Some((bk, prm)) = k.split_once('.') else { return eprintln!("zły klucz: {k}") };
        let found = PATCH_BLOCKS.iter().enumerate().find(|(_, b)| b.key == bk).and_then(|(b, bd)| {
            bd.params.iter().find(|d| d.prm == prm).map(|d| ParamRef { block: b, param: d })
        });
        let Some(r) = found else { return eprintln!("nie ma parametru {k}") };
        let v: i16 = match v.parse() {
            Ok(n) => n,
            Err(_) => match r.param.labels.iter().find(|(_, l)| l.eq_ignore_ascii_case(v)) {
                Some((n, _)) => *n,
                None => return eprintln!("zła wartość {kv}"),
            },
        };
        if v < r.param.min || v > r.param.max {
            return eprintln!("{k}={v} poza zakresem {}..{}", r.param.min, r.param.max);
        }
        p.set(r, v);
    }
    p.set_name(name);
    match lib.add_patch(p, "Haze", "") {
        Ok((id, true)) => println!("zapisane: {name} ({id})"),
        Ok((id, false)) => println!("już jest w bibliotece: {id}"),
        Err(e) => eprintln!("błąd zapisu: {e}"),
    }
}

fn report(r: std::io::Result<katana_core::library::ImportReport>) {
    match r {
        Ok(r) => println!(
            "setlisty: {} (pominięte puste: {}) · patche: {} · nowe brzmienia: {} · duplikaty scalone: {}",
            r.setlists, r.skipped_setlists, r.patches, r.new_tones, r.duplicates
        ),
        Err(e) => {
            eprintln!("błąd importu: {e}");
            std::process::exit(1);
        }
    }
}

fn run_gui(lib: Library) {
    let ui = MainWindow::new().expect("create window");
    // Wayland app_id / X11 WM_CLASS: matches haze.desktop, so the compositor and launchers show
    // the icon. Only after the window exists: before that there is no Slint context to hold it,
    // and the call is silently dropped. The native window is created on show, which reads it.
    if let Err(e) = slint::set_xdg_app_id("haze") {
        eprintln!("app_id: {e}");
    }
    app::App::install(&ui, lib);
    ui.run().expect("event loop");
    app::shutdown();
}
