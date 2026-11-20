//! Application state and UI binding. Runs on the UI thread only.

use crate::panel::{group_of, NodeDef, Panel, STATUS};
use crate::history::History;
use crate::{BlockChip, Essential, PedalKnob, TemplateRow, MainWindow, NavItem, Opt, ParamRow, Pedal, Slot, ToneRow, T};
use katana_core::chain::{self, Node, Routing, PATTERN_NAMES};
use katana_core::map::{amp_family, amp_name, ParamRef, AMP_PANEL, PATCH_BLOCKS};
use katana_core::patch::{Led, Patch};
use katana_core::sysex::SLOT_LABELS;
use katana_core::{Library, SetlistKind};
use katana_midi::{Cmd, Device, Event};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

enum Popup {
    Param(ParamRef),
    /// TYPE of the block open in the inspector
    Type,
    /// audio device lists shown in the picker (value = index + 1, 0 = none / system)
    AudioIn(Vec<haze_dsp::DeviceInfo>),
    AudioOut(Vec<haze_dsp::DeviceInfo>),
}

/// Audio device choice, kept next to the library: `in=…`, `out=…`, `buffer=…`, `rate=…`.
fn load_audio_cfg(root: &std::path::Path) -> haze_dsp::Config {
    let mut c = haze_dsp::Config::default();
    for l in std::fs::read_to_string(root.join("audio.conf")).unwrap_or_default().lines() {
        match l.split_once('=') {
            Some(("in", v)) if !v.is_empty() => c.input = Some(v.to_string()),
            Some(("out", v)) if !v.is_empty() => c.output = Some(v.to_string()),
            Some(("buffer", v)) => c.buffer = v.parse().unwrap_or(c.buffer),
            Some(("rate", v)) => c.rate = v.parse().unwrap_or(c.rate),
            _ => {}
        }
    }
    c
}

/// Interface preferences (`ui.conf`): size in percent, library sort order.
fn load_ui_cfg(root: &std::path::Path) -> (i32, i32) {
    let (mut scale, mut sort) = (100, 0);
    for l in std::fs::read_to_string(root.join("ui.conf")).unwrap_or_default().lines() {
        match l.split_once('=') {
            Some(("scale", v)) => scale = v.parse().ok().filter(|x| [100, 125, 150].contains(x)).unwrap_or(100),
            Some(("sort", v)) => sort = v.parse().ok().filter(|x| (0..3).contains(x)).unwrap_or(0),
            _ => {}
        }
    }
    (scale, sort)
}

fn save_ui_cfg(root: &std::path::Path, scale: i32, sort: i32) {
    let _ = std::fs::write(root.join("ui.conf"), format!("scale={scale}\nsort={sort}\n"));
}

fn save_audio_cfg(root: &std::path::Path, c: &haze_dsp::Config) {
    let txt = format!(
        "in={}\nout={}\nbuffer={}\nrate={}\n",
        c.input.as_deref().unwrap_or(""),
        c.output.as_deref().unwrap_or(""),
        c.buffer,
        c.rate
    );
    let _ = std::fs::write(root.join("audio.conf"), txt);
}

/// Short header label of a cpal device name.
fn dev_label(name: Option<&str>, none: &str) -> String {
    match name {
        None => none.to_string(),
        Some("default") => "System default".to_string(),
        Some(n) => {
            let c = n.split_once("CARD=").map_or(n, |x| x.1);
            c.split(',').next().unwrap_or(c).to_string()
        }
    }
}

pub struct App {
    ui: slint::Weak<MainWindow>,
    lib: Library,
    dev: Device,
    panel: Panel,

    nav_sel: String,
    query: String,
    visible: Vec<String>,
    rows: Rc<VecModel<ToneRow>>,

    sel: Option<String>,
    /// Recently selected tone ids, newest first (session only).
    recent: Vec<String>,
    edit: Patch,
    saved: Patch,
    /// undo/redo of `edit`; reset when another tone (or the other A/B snapshot) comes up
    hist: History<Patch>,
    live: Option<String>,

    connected: bool,
    slot_names: Vec<String>,
    cur_slot: u8,

    /// signal flow currently shown on the pedalboard
    layout: Vec<Node>,
    pedals: Rc<VecModel<Pedal>>,
    insp: Option<Node>,
    insp_section: usize,
    insp_rows: Vec<ParamRef>,
    popup: Option<Popup>,
    /// slot write waiting for its pre-write backup read
    pending_write: Option<(u8, Patch, String)>,
    toast_timer: Timer,
    /// HARDWARE (Katana USB-MIDI) vs STANDALONE (PC DSP engine). Edits reach the amp only in HARDWARE.
    hw: bool,
    /// library tone filter: "", a style ("clean", "crunch", "hi-gain", "ambient", "lead")
    /// or "#name" for a user tag
    tag: String,
    /// library order (All / Favorites): 0 name, 1 recently edited, 2 amp
    sort: i32,
    /// interface size in percent, on top of the system scale `os_scale`
    ui_scale: i32,
    os_scale: f32,
    /// A/B compare: the snapshot not on screen, and which slot is on screen (0 = A, 1 = B)
    ab_other: Option<Patch>,
    ab_slot: i32,
    // ---- standalone PC DSP engine
    dsp: Arc<haze_dsp::Params>,
    engine: Option<haze_dsp::Engine>,
    audio_cfg: haze_dsp::Config,
    tuner: Option<haze_dsp::Tuner>,
    /// voicing chosen on the PC side that the Katana does not have (PUSHED / MODERN)
    dsp_voicing: Option<i32>,
    di_on: bool,
    meter_timer: Timer,
    meter_tick: u32,
    clip_hold: [u32; 2],
    // ---- practice suite
    /// looper memory handed to the current engine instance (frames), 0 = none yet
    loop_frames: usize,
    /// a user cabinet .wav sits in IR slot 3 and overrides the voicing's cab
    user_ir: bool,
    taps: Vec<std::time::Instant>,
}

fn s(x: impl AsRef<str>) -> SharedString {
    SharedString::from(x.as_ref())
}

fn led_i(l: Led) -> i32 {
    match l {
        Led::Off => 0,
        Led::Green => 1,
        Led::Red => 2,
        Led::Yellow => 3,
    }
}

fn local_stamp() -> String {
    // "2026-11-17 17:02" in local time, without pulling a date crate
    let out = std::process::Command::new("date").arg("+%Y-%m-%d %H:%M").output();
    out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

pub fn export_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    home.join("Katana")
}

fn safe_file_name(n: &str) -> String {
    let s: String = n.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect();
    let s = s.trim().to_string();
    if s.is_empty() { "katana".into() } else { s }
}

/// BOSS Tone Studio library dirs on mounted Windows partitions.
pub fn find_bts_dirs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![];
    for base in ["/mnt", "/media"] {
        if let Ok(rd) = std::fs::read_dir(base) {
            roots.extend(rd.flatten().map(|e| e.path()));
        }
        roots.push(PathBuf::from(base));
    }
    if let Some(user) = std::env::var_os("USER") {
        let p = Path::new("/run/media").join(user);
        if let Ok(rd) = std::fs::read_dir(&p) {
            roots.extend(rd.flatten().map(|e| e.path()));
        }
    }
    let mut found = vec![];
    for r in roots {
        let Ok(users) = std::fs::read_dir(r.join("Users")) else { continue };
        for u in users.flatten() {
            let pref = u.path().join("AppData/Local/Roland/BOSS TONE STUDIO for KATANA MkII/pref");
            if pref.join("ID_LIST").exists() {
                found.push(pref);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

impl App {
    pub fn install(ui: &MainWindow, lib: Library) {
        let dev = Device::spawn(|e| {
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_device(e)));
        });
        let rows = Rc::new(VecModel::default());
        ui.set_tones(ModelRc::from(rows.clone()));
        let pedals = Rc::new(VecModel::default());
        ui.set_pedals(ModelRc::from(pedals.clone()));
        let audio_cfg = load_audio_cfg(lib.root());
        ui.set_audio_buffer(audio_cfg.buffer as i32);
        ui.set_audio_rate(audio_cfg.rate as i32);
        ui.set_audio_input(s(dev_label(audio_cfg.input.as_deref(), "None (DI)")));
        ui.set_audio_device(s(dev_label(audio_cfg.output.as_deref(), "System default")));
        let app = App {
            ui: ui.as_weak(),
            lib,
            dev,
            panel: Panel::new(),
            nav_sel: "all".into(),
            query: String::new(),
            visible: vec![],
            rows,
            sel: None,
            recent: Vec::new(),
            edit: Patch::default(),
            saved: Patch::default(),
            hist: History::new(Patch::default()),
            live: None,
            connected: false,
            slot_names: vec![String::new(); 9],
            cur_slot: 255,
            layout: vec![],
            pedals,
            // lower deck starts on the amp, so the inspector is never an empty hole
            insp: Some(Node::Amp),
            insp_section: 0,
            insp_rows: vec![],
            popup: None,
            pending_write: None,
            toast_timer: Timer::default(),
            hw: std::env::var_os("HAZE_PC_MODE").is_none(),
            tag: String::new(),
            sort: 0,
            ui_scale: 100,
            os_scale: 1.0,
            ab_other: None,
            ab_slot: 0,
            dsp: Arc::new(haze_dsp::Params::default()),
            engine: None,
            audio_cfg,
            tuner: None,
            dsp_voicing: None,
            // no guitar plugged in is the common first run: start on the DI loop
            di_on: true,
            meter_timer: Timer::default(),
            meter_tick: 0,
            clip_hold: [0; 2],
            loop_frames: 0,
            user_ir: false,
            taps: Vec::new(),
        };
        let app = Rc::new(RefCell::new(app));
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        bind(ui);
        let mut a = app.borrow_mut();
        (a.ui_scale, a.sort) = load_ui_cfg(a.lib.root());
        ui.set_ui_scale(a.ui_scale);
        ui.set_sort(a.sort);
        // the native window exists once the loop runs: read the system scale, then apply ours
        Timer::single_shot(Duration::ZERO, || with_app(|a| {
            a.os_scale = a.ui().window().scale_factor();
            if a.ui_scale != 100 {
                a.apply_scale();
            }
        }));
        let hw = a.hw;
        a.set_mode(hw);
        a.refresh_nav();
        a.refresh_list();
        a.refresh_slots();
        a.refresh_import_hint();
        a.refresh_templates();
        if let Some(first) = a.visible.first().cloned() {
            a.select(&first);
        }
        if std::env::var_os("KATANA_NO_DEVICE").is_none() {
            a.dev.send(Cmd::Connect);
        }
        watch_library(a.lib.root());
    }

    /// Tones or setlists changed on disk (`haze derive`, another instance, a copied file):
    /// reread the library and refresh the preset lists. The open editor keeps its own copy of the
    /// patch, so unsaved edits stay.
    fn reload_library(&mut self) {
        let Ok(lib) = Library::open(self.lib.root()) else { return };
        let same = lib.tones.len() == self.lib.tones.len()
            && lib.setlists.len() == self.lib.setlists.len()
            && lib.tones.iter().zip(&self.lib.tones).all(|(a, b)| a.id == b.id && a.modified == b.modified && a.name == b.name && a.favorite == b.favorite)
            && lib.setlists.iter().zip(&self.lib.setlists).all(|(a, b)| a.id == b.id && a.tones == b.tones && a.name == b.name);
        if same {
            return; // our own write echoing back
        }
        self.lib = lib;
        eprintln!("library reloaded: {} presets", self.lib.tones.len());
        self.refresh_nav();
        self.refresh_list();
        self.refresh_essentials();
    }

    fn ui(&self) -> MainWindow {
        self.ui.upgrade().expect("ui alive")
    }

    // ------------------------------------------------------------------ toasts

    fn toast(&mut self, msg: impl Into<String>, error: bool) {
        let ui = self.ui();
        ui.set_toast(s(msg.into()));
        ui.set_toast_error(error);
        let w = self.ui.clone();
        self.toast_timer.start(TimerMode::SingleShot, Duration::from_millis(if error { 6000 } else { 3200 }), move || {
            if let Some(u) = w.upgrade() {
                u.set_toast(SharedString::new());
            }
        });
    }

    fn busy(&self, text: &str, progress: f32) {
        let ui = self.ui();
        ui.set_busy_text(s(text));
        ui.set_busy_progress(progress);
    }

    // ------------------------------------------------------------------ nav + list

    fn refresh_nav(&self) {
        let fav = self.lib.tones.iter().filter(|t| t.favorite).count() as i32;
        let mut v = vec![
            NavItem { id: s("h-lib"), name: s("LIBRARY"), count: 0, kind: 4 },
            NavItem { id: s("all"), name: s("All presets"), count: self.lib.tones.len() as i32, kind: 0 },
            NavItem { id: s("fav"), name: s("Favorites"), count: fav, kind: 1 },
        ];
        let sets: Vec<_> = self.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Setlist).collect();
        if !sets.is_empty() {
            v.push(NavItem { id: s("h-sets"), name: s("SETLISTS"), count: 0, kind: 4 });
            v.extend(sets.iter().map(|x| NavItem { id: s(&x.id), name: s(&x.name), count: x.tones.len() as i32, kind: 2 }));
        }
        let backs: Vec<_> = self.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Backup).collect();
        if !backs.is_empty() {
            v.push(NavItem { id: s("h-back"), name: s("AMP BACKUPS"), count: 0, kind: 4 });
            v.extend(backs.iter().rev().map(|x| NavItem { id: s(&x.id), name: s(&x.name), count: x.tones.len() as i32, kind: 3 }));
        }
        self.refresh_essentials();
        let ui = self.ui();
        ui.set_set_count(v.iter().filter(|n| n.kind == 2).count() as i32);
        ui.set_nav(ModelRc::new(VecModel::from(v)));
        ui.set_nav_selected(s(&self.nav_sel));
    }

    fn row(&self, id: &str) -> Option<ToneRow> {
        let t = self.lib.tone(id)?;
        let patch = if self.sel.as_deref() == Some(id) { &self.edit } else { &t.patch };
        let sm = patch.summary();
        let fx: Vec<String> = sm.fx_names.iter().zip(sm.leds.iter()).filter(|(_, l)| **l != Led::Off).map(|(n, _)| nice(n)).collect();
        Some(ToneRow {
            id: s(id),
            index: self.lib.tones.iter().position(|x| x.id == id).map_or(0, |i| i as i32 + 1),
            name: s(&sm.name),
            amp: s(nice(&sm.amp)),
            fx: s(fx.join(" · ")),
            fav: t.favorite,
            live: self.live.as_deref() == Some(id),
            leds: ModelRc::new(VecModel::from(sm.leds.iter().map(|l| led_i(*l)).collect::<Vec<_>>())),
            drive: drive_level(&sm.amp),
        })
    }

    fn refresh_list(&mut self) {
        let words: Vec<String> = self.query.to_lowercase().split_whitespace().map(String::from).collect();
        let base: Vec<String> = match self.nav_sel.as_str() {
            "all" => self.lib.tones.iter().map(|t| t.id.clone()).collect(),
            "fav" => self.lib.tones.iter().filter(|t| t.favorite).map(|t| t.id.clone()).collect(),
            id => self.lib.setlists.iter().find(|x| x.id == id).map(|x| x.tones.clone()).unwrap_or_default(),
        };
        self.visible = base
            .into_iter()
            .filter(|id| {
                let Some(t) = self.lib.tone(id) else { return false };
                match self.tag.strip_prefix('#') {
                    _ if self.tag.is_empty() => true,
                    Some(u) => t.tags.iter().any(|x| x == u),
                    None => tone_tags(&t.patch, self.panel.amp_type).contains(&self.tag.as_str()),
                }
            })
            .filter(|id| {
                if words.is_empty() {
                    return true;
                }
                let Some(t) = self.lib.tone(id) else { return false };
                let sm = t.patch.summary();
                let hay = format!("{} {} {} {} {}", sm.name, sm.amp, sm.fx_names.join(" "), t.origin, t.tags.join(" ")).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        // setlists keep their running order; the library follows the chosen sort
        if matches!(self.nav_sel.as_str(), "all" | "fav") {
            let key = |id: &String| self.lib.tone(id).map(|t| (t.name.to_lowercase(), t.modified, t.patch.get(self.panel.amp_type)));
            let mut keyed: Vec<_> = self.visible.drain(..).map(|id| (key(&id), id)).collect();
            match self.sort {
                1 => keyed.sort_by(|a, b| b.0.as_ref().map(|k| k.1).cmp(&a.0.as_ref().map(|k| k.1))),
                2 => keyed.sort_by(|a, b| {
                    let f = |k: &Option<(String, u64, i16)>| k.as_ref().map(|k| (amp_family(k.2).map_or(9, |x| x.0), k.2, k.0.clone()));
                    f(&a.0).cmp(&f(&b.0))
                }),
                _ => keyed.sort_by(|a, b| a.0.as_ref().map(|k| &k.0).cmp(&b.0.as_ref().map(|k| &k.0))),
            }
            self.visible = keyed.into_iter().map(|(_, id)| id).collect();
        }
        let rows: Vec<ToneRow> = self.visible.iter().filter_map(|id| self.row(id)).collect();
        self.rows.set_vec(rows);
        let title = match self.nav_sel.as_str() {
            "all" => "All presets".to_string(),
            "fav" => "Favorites".to_string(),
            id => self.lib.setlists.iter().find(|x| x.id == id).map(|x| x.name.clone()).unwrap_or_default(),
        };
        self.ui().set_nav_title(s(title));
        self.refresh_user_tags();
        self.sync_selected_index();
    }

    /// Chips of the user's own tags (every tag used in the library, sorted).
    fn refresh_user_tags(&self) {
        let mut v: Vec<&str> = self.lib.tones.iter().flat_map(|t| t.tags.iter().map(String::as_str)).collect();
        v.sort_unstable_by_key(|x| x.to_lowercase());
        v.dedup();
        self.ui().set_user_tags(ModelRc::new(VecModel::from(v.into_iter().map(s).collect::<Vec<_>>())));
    }

    fn apply_scale(&self) {
        let f = self.os_scale * self.ui_scale as f32 / 100.0;
        self.ui().window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: f });
    }

    fn sync_selected_index(&self) {
        let idx = self.sel.as_ref().and_then(|id| self.visible.iter().position(|v| v == id)).map(|i| i as i32).unwrap_or(-1);
        self.ui().set_selected_index(idx);
    }

    /// Quick-access chips of the preset picker: favourites, then recent tones, max 8.
    fn refresh_essentials(&self) {
        // favourites first, then recently used, max 8 tiles
        let favs = self.lib.tones.iter().filter(|t| t.favorite);
        let recent = self.recent.iter().filter_map(|id| self.lib.tone(id)).filter(|t| !t.favorite);
        let v: Vec<Essential> = favs
            .chain(recent)
            .take(8)
            .map(|t| {
                let fam = amp_family(t.patch.get(self.panel.amp_type)).map(|x| x.0);
                let c = match fam {
                    Some(0) => 0xa8843f,
                    Some(1) => 0x3a78c9,
                    Some(2) => 0xd07a2c,
                    Some(3) => 0xc2412f,
                    Some(4) => 0x7d4f2a,
                    _ => 0x55504a,
                };
                Essential {
                    id: s(&t.id),
                    name: s(&t.name),
                    abbr: s(abbr(&t.name)),
                    color: slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8),
                    live: self.live.as_deref() == Some(t.id.as_str()),
                    selected: self.sel.as_deref() == Some(t.id.as_str()),
                }
            })
            .collect();
        self.ui().set_essentials(ModelRc::new(VecModel::from(v)));
    }

    /// Starter rig cards: name, style, chain artwork (block colours in signal order, lit when on).
    fn refresh_templates(&self) {
        let rb = self.panel.routing_block;
        let rows: Vec<TemplateRow> = crate::templates::TEMPLATES
            .iter()
            .map(|t| {
                let p = t.build(&self.panel);
                let rt = Routing::from_bytes(p.block(rb));
                let blocks: Vec<BlockChip> = rt
                    .layout()
                    .iter()
                    .map(|n| {
                        let d = self.panel.node(*n);
                        BlockChip { hue: d.hue, on: d.sw.map_or(true, |r| p.get(r) != 0), is_amp: *n == Node::Amp }
                    })
                    .collect();
                TemplateRow {
                    name: s(t.name),
                    style: s(t.style),
                    blurb: s(t.blurb),
                    hue: slint::Color::from_rgb_u8((t.hue >> 16) as u8, (t.hue >> 8) as u8, t.hue as u8),
                    amp: s(nice(&amp_name(p.get(self.panel.amp_type)))),
                    chain: s(nice(PATTERN_NAMES[rt.pattern as usize])),
                    fx: s(p.summary().fx_names.iter().zip(p.summary().leds.iter()).filter(|(_, l)| **l != Led::Off).map(|(n, _)| nice(n)).collect::<Vec<_>>().join(" · ")),
                    blocks: ModelRc::new(VecModel::from(blocks)),
                }
            })
            .collect();
        self.ui().set_templates(ModelRc::new(VecModel::from(rows)));
    }

    fn refresh_row(&self, id: &str) {
        self.refresh_essentials();
        if let (Some(i), Some(r)) = (self.visible.iter().position(|v| v == id), self.row(id)) {
            self.rows.set_row_data(i, r);
        }
    }

    // ------------------------------------------------------------------ editor

    fn select(&mut self, id: &str) {
        let Some(t) = self.lib.tone(id) else { return };
        self.sel = Some(id.to_string());
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(8);
        self.edit = t.patch.clone();
        self.saved = t.patch.clone();
        self.hist.reset(self.edit.clone());
        self.refresh_history();
        self.ab_other = None;
        self.ab_slot = 0;
        self.dsp_voicing = None;
        self.refresh_ab();
        self.ui().set_has_selection(true);
        self.sync_selected_index();
        self.refresh_essentials();
        self.refresh_editor();
    }

    fn refresh_editor(&mut self) {
        let ui = self.ui();
        let Some(id) = self.sel.clone() else {
            ui.set_has_selection(false);
            return;
        };
        let Some(t) = self.lib.tone(&id) else { return };
        let e = &self.edit;
        let sm = e.summary();
        ui.set_ed_name(s(&sm.name));
        ui.set_ed_origin(s(format!("{}{}", t.origin, if t.memo.is_empty() { String::new() } else { format!(" · {}", t.memo) })));
        ui.set_ed_fav(t.favorite);
        ui.set_ed_tags(s(t.tags.join(", ")));
        ui.set_ed_dirty(self.edit != self.saved);
        ui.set_ed_live(self.live.as_deref() == Some(id.as_str()) && self.connected);
        let amp = e.get(self.panel.amp_type);
        ui.set_ed_amp_name(s(nice(&sm.amp)));
        match amp_family(amp) {
            Some((f, var)) => {
                ui.set_ed_amp_family(f as i32);
                ui.set_ed_amp_var(var);
            }
            None => {
                ui.set_ed_amp_family(-1);
                ui.set_ed_amp_var(false);
            }
        }
        self.refresh_rig();
        self.sync_dsp();
    }

    // ------------------------------------------------------------------ pedalboard

    fn routing(&self) -> Routing {
        Routing::from_bytes(self.edit.block(self.panel.routing_block))
    }

    /// Patch_2 byte (variant assignments / selected colour).
    fn p2(&self, off: usize) -> u8 {
        self.edit.block(self.panel.routing_block)[off]
    }

    fn variant_sel(&self, d: &NodeDef) -> Option<usize> {
        d.variant.map(|v| (self.p2(v.sel) as usize).min(2))
    }

    fn node_on(&self, d: &NodeDef) -> bool {
        d.sw.map_or(true, |r| self.edit.get(r) != 0)
    }

    fn type_name(&self, d: &NodeDef) -> String {
        if d.node == Node::Amp {
            return amp_name(self.edit.get(self.panel.amp_type));
        }
        match d.ty {
            Some(r) => {
                let v = self.edit.get(r);
                r.param.label(v).map(String::from).unwrap_or_else(|| format!("TYPE {v}"))
            }
            None => d.title.to_string(),
        }
    }

    fn pedal_row(&self, n: Node) -> Pedal {
        let d = self.panel.node(n);
        let knobs: Vec<PedalKnob> = self
            .panel
            .pedal_knobs(d, &self.edit)
            .iter()
            .map(|r| {
                let v = self.edit.get(*r);
                let span = (r.param.max - r.param.min).max(1) as f32;
                PedalKnob {
                    label: s(if r.param.prm == "PRM_PREAMP_A_LEVEL" { "Vol".to_string() } else { crate::panel::short(r.param.name) }),
                    norm: ((v - r.param.min) as f32 / span).clamp(0.0, 1.0),
                    text: s(param_text(r.param, v)),
                }
            })
            .collect();
        let p = Pedal {
            title: s(nice(d.title)),
            // blocks without a type (gate) show no subtype instead of repeating their name
            type_name: s(if d.ty.is_none() && n != Node::Amp { String::new() } else { nice(&self.type_name(d)) }),
            on: self.node_on(d),
            has_sw: d.sw.is_some(),
            led: self.variant_sel(d).map_or(1, |c| c as i32 + 1),
            hue: d.hue,
            // silkscreen colour: dark on bright enclosures, cream on dark ones
            ink: ink_for(d.hue),
            movable: chain::movable(n),
            is_amp: n == Node::Amp,
            knobs: ModelRc::new(VecModel::from(knobs)),
            kind: self.panel.nodes.iter().position(|x| x.node == n).unwrap_or(0) as i32,
        };
        p
    }

    fn refresh_rig(&mut self) {
        let rt = self.routing();
        self.layout = rt.layout();
        let rows: Vec<Pedal> = self.layout.iter().map(|n| self.pedal_row(*n)).collect();
        self.pedals.set_vec(rows);
        let ui = self.ui();
        ui.set_ed_chain_name(s(nice(PATTERN_NAMES[rt.pattern as usize])));
        self.refresh_fx_count();
        self.refresh_inspector();
    }

    /// "4 / 9" active-effects chip in the tone header.
    fn refresh_fx_count(&self) {
        let fx = self.pedals.iter().filter(|p| !p.is_amp);
        let (on, all) = fx.fold((0, 0), |(o, a), p| (o + p.on as i32, a + 1));
        let ui = self.ui();
        ui.set_ed_fx_on(on);
        ui.set_ed_fx_all(all);
    }

    /// Re-render one pedal in place (no model reset, no repeater rebuild).
    /// Knob on a pedal face: absolute position from a drag (`norm`), or `step` detents from the wheel.
    fn pedal_knob(&mut self, pedal: usize, k: usize, norm: Option<f32>, step: i32) {
        let Some(n) = self.layout.get(pedal).copied() else { return };
        let d = self.panel.node(n);
        let Some(r) = self.panel.pedal_knobs(d, &self.edit).get(k).copied() else { return };
        let (min, max) = (r.param.min, r.param.max);
        let cur = self.edit.get(r);
        let v = match norm {
            Some(x) => min + ((max - min) as f32 * x.clamp(0.0, 1.0)).round() as i16,
            None => cur + step as i16 * ((max - min) / 100).max(1),
        }
        .clamp(min, max);
        if v == cur {
            return;
        }
        self.set_param(r, v);
        self.refresh_pedal(n);
        if self.insp == Some(n) {
            let same = |x: &ParamRef| x.block == r.block && std::ptr::eq(x.param, r.param);
            if let Some(i) = self.insp_rows.iter().position(same) {
                let m = self.ui().get_insp_params();
                if let Some(mut row) = m.row_data(i) {
                    row.value = v as i32;
                    row.display = s(param_text(r.param, v));
                    m.set_row_data(i, row);
                }
            }
        }
        self.after_edit();
    }

    fn refresh_pedal(&mut self, n: Node) {
        if let Some(i) = self.layout.iter().position(|x| *x == n) {
            let row = self.pedal_row(n);
            self.pedals.set_row_data(i, row);
        }
        self.refresh_fx_count();
    }

    /// A param of node `n` by its PRM name, among those its current type uses.
    fn node_param(&self, n: Node, prm: &str) -> Option<ParamRef> {
        let d = self.panel.node(n);
        d.sections.iter().flat_map(|x| self.panel.section_params(x, &self.edit)).find(|r| r.param.prm == prm)
    }

    /// Inspector band above the knobs: EQ response curve, delay tempo sync.
    fn refresh_extras(&self) {
        let ui = self.ui();
        let extra = match self.insp {
            Some(Node::Eq) if self.insp_section == 0 => {
                use crate::eqcurve::*;
                let g = |prm: &str| self.node_param(Node::Eq, prm).map_or(0, |r| self.edit.get(r));
                let geq = self.panel.node(Node::Eq).ty.is_some_and(|r| self.edit.get(r) == 1);
                let (c, fill) = if geq {
                    let mut b = [0f32; 10];
                    for (i, x) in b.iter_mut().enumerate() {
                        *x = g(&format!("PRM_EQ_GEQ_BAND{}", i + 1)) as f32;
                    }
                    let lv = g("PRM_EQ_GEQ_LEVEL") as f32;
                    paths(|f| geq_db(&b, lv, f))
                } else {
                    let p = Para {
                        low_cut: low_cut_index(g("PRM_EQ_LOW_CUT")),
                        low_gain: g("PRM_EQ_LOW_GAIN") as f32,
                        low_mid: (freq_index(g("PRM_EQ_LOWMID_FREQ")), q_index(g("PRM_EQ_LOWMID_Q")), g("PRM_EQ_LOWMID_GAIN") as f32),
                        high_mid: (freq_index(g("PRM_EQ_HIGHMID_FREQ")), q_index(g("PRM_EQ_HIGHMID_Q")), g("PRM_EQ_HIGHMID_GAIN") as f32),
                        high_gain: g("PRM_EQ_HIGH_GAIN") as f32,
                        high_cut: high_cut_index(g("PRM_EQ_HIGH_CUT")),
                        level: g("PRM_EQ_LEVEL") as f32,
                    };
                    paths(|f| para_db(&p, f))
                };
                ui.set_insp_eq_curve(s(c));
                ui.set_insp_eq_fill(s(fill));
                1
            }
            Some(n @ (Node::Delay | Node::Delay2)) if self.insp_section == 0 => {
                ui.set_insp_delay_ms(self.node_param(n, "PRM_DLY_COMMON_DLY_TIME").map_or(0, |r| self.edit.get(r)) as i32);
                2
            }
            _ => 0,
        };
        ui.set_insp_extra(extra);
    }

    fn delay_sync(&mut self, ms: i32) {
        let Some(n @ (Node::Delay | Node::Delay2)) = self.insp else { return };
        let Some(r) = self.node_param(n, "PRM_DLY_COMMON_DLY_TIME") else { return };
        let v = (ms as i16).clamp(r.param.min, r.param.max);
        if v as i32 != ms {
            self.toast(format!("Delay max is {} ms: set to {v} ms", r.param.max), false);
        }
        self.set_param(r, v);
        self.refresh_pedal(n);
        self.refresh_inspector();
        self.after_edit();
    }

    fn refresh_inspector(&mut self) {
        let ui = self.ui();
        let Some(n) = self.insp else {
            ui.set_insp_open(false);
            ui.set_pedal_selected(-1);
            return;
        };
        let d = self.panel.node(n);
        ui.set_insp_open(true);
        ui.set_insp_is_amp(n == Node::Amp);
        ui.set_insp_kind(self.panel.nodes.iter().position(|x| x.node == n).unwrap_or(0) as i32);
        ui.set_pedal_selected(self.layout.iter().position(|x| *x == n).map_or(-1, |i| i as i32));
        ui.set_insp_title(s(nice(d.title)));
        ui.set_insp_subtitle(s(nice(&self.type_name(d))));
        // the amp carries the engine-mode accent (amber hardware, cyan standalone), dimmed:
        // the inspector brightens its hue for arcs and tabs
        let hue = match (n == Node::Amp, self.hw) {
            (true, true) => slint::Color::from_rgb_u8(0xb8, 0x70, 0x10),
            (true, false) => slint::Color::from_rgb_u8(0x00, 0xa0, 0xb4),
            _ => d.hue,
        };
        ui.set_insp_hue(hue);
        ui.set_insp_ink(ink_for(hue));
        ui.set_insp_has_sw(d.sw.is_some());
        ui.set_insp_on(self.node_on(d));
        ui.set_insp_variant(self.variant_sel(d).map_or(-1, |c| c as i32));
        let (types, cur): (Vec<Opt>, i32) = if n == Node::Amp {
            ((0..=32).map(|v| Opt { value: v, label: s(amp_name(v as i16)) }).collect(), self.edit.get(self.panel.amp_type) as i32)
        } else if let Some(r) = d.ty {
            (r.param.labels.iter().map(|(v, l)| Opt { value: *v as i32, label: s(l) }).collect(), self.edit.get(r) as i32)
        } else {
            (vec![], -1)
        };
        ui.set_insp_types(ModelRc::new(VecModel::from(types)));
        ui.set_insp_type(cur);
        self.insp_section = self.insp_section.min(d.sections.len() - 1);
        ui.set_insp_sections(ModelRc::new(VecModel::from(d.sections.iter().map(|x| s(nice(x.name))).collect::<Vec<_>>())));
        ui.set_insp_section(self.insp_section as i32);
        // switch and type live in the drawer header; amp TYPE in the grid
        let skip = |r: &ParamRef| {
            [d.sw, d.ty, Some(self.panel.amp_type)].iter().flatten().any(|x| x.block == r.block && std::ptr::eq(x.param, r.param))
        };
        self.insp_rows = self.panel.section_params(&d.sections[self.insp_section], &self.edit).into_iter().filter(|r| !skip(r)).collect();
        // Grid layout, computed here so the UI only places cells: a flow of COLS columns where
        // knobs and switches take one cell and lists take two; a group change closes the row
        // and adds a caption.
        // must match `cols` and the 92px cell of ui/inspector.slint
        const COLS: i32 = 8;
        const CELL_H: f32 = 108.0;
        const CAPTION_H: f32 = 40.0;
        let mut last_group = String::new();
        let (mut y, mut col) = (0f32, 0i32);
        let mut rows = Vec::with_capacity(self.insp_rows.len());
        for (i, r) in self.insp_rows.iter().enumerate() {
            let v = self.edit.get(*r);
            let g = group_of(r.param);
            let show = g != last_group && i > 0;
            last_group = g.clone();
            if show {
                if col > 0 {
                    y += CELL_H;
                    col = 0;
                }
                y += CAPTION_H;
            }
            let is_enum = !r.param.labels.is_empty();
            let switch = is_switch(r.param);
            let span = if is_enum && !switch { 2 } else { 1 };
            if col + span > COLS {
                y += CELL_H;
                col = 0;
            }
            let (c, py) = (col, y);
            col += span;
            if col == COLS {
                y += CELL_H;
                col = 0;
            }
            rows.push(ParamRow {
                group: s(nice(&g)),
                show_group: show,
                name: s(param_name(r.param)),
                value: v as i32,
                min: r.param.min as i32,
                max: r.param.max as i32,
                display: s(param_text(r.param, v)),
                is_enum,
                is_switch: switch,
                col: c,
                span,
                py,
            });
        }
        if col > 0 {
            y += CELL_H;
        }
        ui.set_insp_params(ModelRc::new(VecModel::from(rows)));
        ui.set_insp_params_h(y);
        self.refresh_extras();
    }

    /// Raw byte write inside a block (Patch_2 routing / colour memory), streamed live like a param.
    fn set_raw(&mut self, block: usize, off: usize, bytes: &[u8]) {
        let mut b = self.edit.block(block).to_vec();
        b[off..off + bytes.len()].copy_from_slice(bytes);
        self.edit.set_block(block, &b);
        if self.is_live() && block != STATUS {
            self.dev.send(Cmd::Param { rel: PATCH_BLOCKS[block].addr + off as u32, data: bytes.to_vec() });
        }
    }

    fn set_node_on(&mut self, n: Node, on: bool) {
        let d = self.panel.node(n);
        let (sw, var) = (d.sw, d.variant);
        let Some(sw) = sw else { return };
        self.set_param(sw, on as i16);
        if let Some(v) = var {
            let c = self.p2(v.sel).min(2);
            self.set_status_byte(v.led, if on { c + 1 } else { 0 });
        }
        self.refresh_pedal(n);
        if self.insp == Some(n) {
            self.ui().set_insp_on(on);
        }
        self.after_edit();
    }

    fn set_node_type(&mut self, n: Node, v: i16) {
        if n == Node::Amp {
            let at = self.panel.amp_type;
            self.set_param(at, v);
            if let Some((f, var)) = amp_family(v) {
                self.set_status_byte(0, f as u8);
                self.set_status_byte(12, var as u8);
            }
            self.refresh_editor();
            self.after_edit();
            return;
        }
        let d = self.panel.node(n);
        let (ty, var) = (d.ty, d.variant);
        let Some(ty) = ty else { return };
        self.set_param(ty, v);
        if let Some(va) = var {
            let c = self.p2(va.sel).min(2) as usize;
            let rb = self.panel.routing_block;
            self.set_raw(rb, va.asgn + c, &[v as u8]);
        }
        self.refresh_pedal(n);
        self.refresh_inspector();
        self.after_edit();
    }

    fn set_node_variant(&mut self, n: Node, c: usize) {
        let d = self.panel.node(n);
        let (ty, var, on) = (d.ty, d.variant, self.node_on(d));
        let (Some(ty), Some(va)) = (ty, var) else { return };
        let rb = self.panel.routing_block;
        self.set_raw(rb, va.sel, &[c as u8]);
        let t = self.p2(va.asgn + c) as i16;
        self.set_param(ty, t);
        if on {
            self.set_status_byte(va.led, c as u8 + 1);
        }
        // DELAY and DELAY 2 share one colour selector
        self.refresh_rig();
        self.after_edit();
    }

    fn drop_pedal(&mut self, from: usize, gap: usize) {
        let rt = self.routing();
        match rt.moved(from, gap) {
            Some(r2) if r2 != rt => {
                let rb = self.panel.routing_block;
                self.set_raw(rb, 0, &r2.to_bytes());
                self.refresh_rig();
                self.after_edit();
                self.toast(format!("Chain: {}", PATTERN_NAMES[r2.pattern as usize]), false);
            }
            Some(_) => {}
            None => {
                if gap != from && gap != from + 1 {
                    self.toast("The Katana can't run blocks in this order", true);
                }
            }
        }
    }

    /// Change one parameter of the edited patch; streams it to the amp if this tone is live.
    fn set_param(&mut self, r: ParamRef, v: i16) {
        let (rel, data) = self.edit.set(r, v);
        if self.is_live() {
            self.dev.send(Cmd::Param { rel, data });
        }
    }

    fn set_status_byte(&mut self, off: usize, v: u8) {
        let mut b = self.edit.block(STATUS).to_vec();
        b[off] = v;
        self.edit.set_block(STATUS, &b);
    }

    fn is_live(&self) -> bool {
        self.hw && self.connected && self.sel.is_some() && self.live == self.sel
    }

    // ------------------------------------------------------------------ engine mode, A/B

    fn set_mode(&mut self, hw: bool) {
        self.hw = hw;
        let ui = self.ui();
        ui.global::<T>().set_hw(hw);
        ui.set_hw_mode(hw);
        if hw {
            self.stop_engine();
        } else if std::env::var_os("KATANA_NO_DEVICE").is_none() {
            self.start_engine();
        }
        // back on the amp: it may have drifted while edits stayed local
        if self.is_live() {
            self.dev.send(Cmd::Audition(Box::new(self.edit.clone())));
        }
        ui.set_ed_live(self.is_live());
        if self.sel.is_some() {
            self.refresh_inspector();
        }
    }

    // ------------------------------------------------------------------ PC DSP engine

    fn start_engine(&mut self) {
        self.stop_engine();
        self.sync_dsp();
        self.dsp.set(haze_dsp::P::Source, self.di_on as i32 as f32);
        let mut res = haze_dsp::Engine::start(self.dsp.clone(), &self.audio_cfg);
        if let (Err(err), Some(want)) = (&res, self.audio_cfg.input.clone()) {
            // input gone (unplugged / busy): keep the app usable on the DI player
            self.toast(format!("Input {want}: {err} — playing DI"), true);
            let mut cfg = self.audio_cfg.clone();
            cfg.input = None;
            res = haze_dsp::Engine::start(self.dsp.clone(), &cfg);
        }
        match res {
            Ok(e) => {
                let ui = self.ui();
                ui.set_audio_host(s(&e.info.host));
                ui.set_audio_device(s(dev_label(self.audio_cfg.output.as_deref(), "System default")));
                ui.set_audio_input(s(dev_label(e.info.input.as_deref(), "None (DI)")));
                ui.set_audio_latency(e.info.latency_ms);
                ui.set_audio_running(true);
                if e.info.input.is_none() && !self.di_on {
                    self.set_di(true);
                }
                self.tuner = Some(haze_dsp::Tuner::new(e.fs()));
                self.engine = Some(e);
                // a fresh chain: looper memory and the user IR went with the old one
                self.loop_frames = 0;
                self.user_ir = false;
                self.sync_dsp();
                ui.set_ir_name(s("built-in (per amp)"));
                ui.set_di_name(s("built-in riff"));
                if ui.get_practice_open() {
                    self.ensure_loop_buffer();
                }
                self.meter_timer.start(TimerMode::Repeated, Duration::from_millis(33), || with_app(|a| a.meter_frame()));
            }
            Err(e) => self.toast(format!("Audio engine: {e}"), true),
        }
    }

    fn stop_engine(&mut self) {
        self.meter_timer.stop();
        self.engine = None;
        self.tuner = None;
        let ui = self.ui();
        ui.set_audio_running(false);
        ui.set_in_level(0.0);
        ui.set_out_level(0.0);
        ui.set_tuner_active(false);
        ui.set_tuner_note(s("—"));
    }

    /// 30 Hz while the engine runs: meters, clip LEDs, tuner. Nothing runs in HARDWARE mode.
    fn meter_frame(&mut self) {
        let Some(e) = self.engine.as_mut() else { return };
        e.collect_trash();
        let m = e.meters.clone();
        let mut fresh = vec![];
        while let Ok(x) = e.tuner.pop() {
            fresh.push(x);
        }
        let ui = self.ui();
        // minimised: keep the queues drained, skip the pitch maths and every property write
        if ui.window().is_minimized() {
            let _ = haze_dsp::Meters::take_peak(&m.in_peak);
            let _ = haze_dsp::Meters::take_peak(&m.out_peak);
            return;
        }
        // meter ballistics: instant rise, ~300 ms fall; snaps to 0 so a silent meter stops
        // changing (an endless geometric decay would repaint the bar every frame)
        let fall = |cur: f32, new: f32| if new > cur { new } else if cur < 0.002 { 0.0 } else { cur * 0.82 };
        ui.set_in_level(fall(ui.get_in_level(), haze_dsp::Meters::take_peak(&m.in_peak)));
        ui.set_out_level(fall(ui.get_out_level(), haze_dsp::Meters::take_peak(&m.out_peak)));
        for (i, flag) in [&m.in_clip, &m.out_clip].into_iter().enumerate() {
            if flag.swap(0, std::sync::atomic::Ordering::Relaxed) != 0 {
                self.clip_hold[i] = 45;
            }
            self.clip_hold[i] = self.clip_hold[i].saturating_sub(1);
        }
        ui.set_in_clip(self.clip_hold[0] > 0);
        ui.set_out_clip(self.clip_hold[1] > 0);
        self.meter_tick = self.meter_tick.wrapping_add(1);
        if ui.get_practice_open() {
            use std::sync::atomic::Ordering::Relaxed;
            let st = m.loop_state.load(Relaxed) as i32;
            let (pos, len) = (m.loop_pos.load(Relaxed) as f32, m.loop_len.load(Relaxed) as f32);
            let fs = self.engine.as_ref().map_or(48000.0, |e| e.fs());
            ui.set_loop_state(st);
            // recording fills the memory, playback sweeps the loop
            let (prog, secs) = if st == 1 { (len / self.loop_frames.max(1) as f32, len / fs) } else if len > 0.0 { (pos / len, len / fs) } else { (0.0, 0.0) };
            ui.set_loop_progress(prog);
            ui.set_loop_time(s(format!("{secs:.1} s")));
            ui.set_drum_step(m.drum_step.load(Relaxed) as i32);
        }
        if let Some(t) = self.tuner.as_mut() {
            for x in fresh {
                t.push(x);
            }
            // pitch every other frame (15 Hz) is plenty for a tuner
            if self.meter_tick % 2 == 0 {
                match t.estimate() {
                    Some(p) => {
                        ui.set_tuner_active(true);
                        ui.set_tuner_note(s(format!("{}{}", p.note, p.octave)));
                        ui.set_tuner_cents(p.cents);
                    }
                    None => ui.set_tuner_active(false),
                }
            }
        }
    }

    fn set_di(&mut self, on: bool) {
        self.di_on = on;
        self.dsp.set(haze_dsp::P::Source, on as i32 as f32);
        if on {
            if let Some(e) = self.engine.as_mut() {
                e.send(haze_dsp::Cmd::RewindDi);
            }
        }
        self.ui().set_di_on(on);
    }

    // ------------------------------------------------------------------ practice suite

    /// Looper memory: 60 s stereo + one overdub layer. Zeroed pages come from calloc, so RSS
    /// grows only with what actually gets recorded. Allocated on first use, never per frame.
    fn ensure_loop_buffer(&mut self) {
        let Some(e) = self.engine.as_mut() else { return };
        if self.loop_frames > 0 {
            return;
        }
        let frames = (e.fs() * 60.0) as usize;
        let buf = vec![0f32; frames * 4].into_boxed_slice();
        if e.send(haze_dsp::Cmd::SetLoopBuffer(buf)) {
            self.loop_frames = frames;
        }
    }

    fn loop_action(&mut self, a: i32) {
        use haze_dsp::LoopAction::*;
        self.ensure_loop_buffer();
        let act = match a {
            0 => Cycle,
            1 => Stop,
            2 => Play,
            3 => Undo,
            _ => Clear,
        };
        if let Some(e) = self.engine.as_mut() {
            e.send(haze_dsp::Cmd::Loop(act));
        }
    }

    fn set_bpm(&mut self, bpm: i32) {
        let bpm = bpm.clamp(40, 240);
        self.dsp.set(haze_dsp::P::Bpm, bpm as f32);
        self.ui().set_bpm(bpm);
    }

    /// Tap tempo: average of the last taps (reset after 2 s). Sets the drum BPM and the delay
    /// to a quarter note; the delay TIME is written into the patch, so the Katana follows when live.
    fn tap(&mut self) {
        let now = std::time::Instant::now();
        if self.taps.last().is_some_and(|t| now.duration_since(*t).as_secs_f32() > 2.0) {
            self.taps.clear();
        }
        self.taps.push(now);
        if self.taps.len() > 5 {
            self.taps.remove(0);
        }
        if self.taps.len() < 2 {
            return;
        }
        let span = now.duration_since(self.taps[0]).as_secs_f32() / (self.taps.len() - 1) as f32;
        let bpm = (60.0 / span).round() as i32;
        self.set_bpm(bpm);
        let ms = 60000.0 / bpm.clamp(40, 240) as f32;
        if let Some(e) = self.engine.as_mut() {
            e.send(haze_dsp::Cmd::DrumsRestart);
        }
        let node = self.panel.node(Node::Delay);
        let r = self.panel.section_params(&node.sections[0], &self.edit).into_iter().find(|r| r.param.name.contains("TIME"));
        match r {
            Some(r) if self.sel.is_some() => {
                let (lo, hi) = (r.param.min as f32, r.param.max as f32);
                let v = if r.param.max >= 1000 { ms } else { lo + (ms - 20.0) / 1980.0 * (hi - lo) };
                self.set_param(r, v.round().clamp(lo, hi) as i16);
                self.after_edit();
                if self.insp == Some(Node::Delay) {
                    self.refresh_inspector();
                }
            }
            _ => self.dsp.set(haze_dsp::P::DelayMs, ms),
        }
        self.toast(format!("TAP {bpm} BPM · delay {ms:.0} ms"), false);
    }

    fn load_wav(&mut self, path: &str, ir: bool) {
        let path = path.trim().trim_matches(|c| c == '\'' || c == '"');
        let path = std::path::Path::new(path);
        let Some(e) = self.engine.as_mut() else {
            self.toast("Switch to Standalone mode first (PC engine)", true);
            return;
        };
        let fs = e.fs();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let res = if ir {
            haze_dsp::conv::load_wav(path, fs, Some(8192)).map(|x| {
                let spec = haze_dsp::conv::IrSpectrum::new(&x, name.clone());
                e.send(haze_dsp::Cmd::SetIr(3, Box::new(spec)))
            })
        } else {
            haze_dsp::conv::load_wav(path, fs, Some(fs as usize * 300)).map(|x| e.send(haze_dsp::Cmd::SetDi(Box::new(x))))
        };
        match res {
            Ok(true) if ir => {
                self.user_ir = true;
                self.sync_dsp();
                self.ui().set_ir_name(s(&name));
                self.toast(format!("Cab IR: {name}"), false);
            }
            Ok(true) => {
                self.ui().set_di_name(s(&name));
                self.set_di(true);
                self.toast(format!("DI: {name}"), false);
            }
            Ok(false) => self.toast("Engine busy, try again", true),
            Err(err) => self.toast(format!("{name}: {err}"), true),
        }
    }

    /// Mirror the edited Katana patch into the PC engine's parameters (cheap: atomics only).
    fn sync_dsp(&mut self) {
        use haze_dsp::P;
        let d = self.dsp.clone();
        let e = &self.edit;
        let norm = |r: ParamRef| {
            let v = e.get(r) as f32;
            ((v - r.param.min as f32) / ((r.param.max - r.param.min) as f32).max(1.0)).clamp(0.0, 1.0)
        };
        // amp panel knobs
        for k in &self.panel.knobs {
            let v = norm(k.r);
            match k.label.to_uppercase().as_str() {
                "GAIN" => d.set(P::AmpGain, v),
                "BASS" => d.set(P::AmpBass, v),
                "MIDDLE" => d.set(P::AmpMid, v),
                "TREBLE" => d.set(P::AmpTreble, v),
                "PRESENCE" => d.set(P::AmpPresence, v),
                "VOLUME" => d.set(P::AmpMaster, v),
                _ => {}
            }
        }
        let voicing = self.dsp_voicing.unwrap_or_else(|| match amp_family(e.get(self.panel.amp_type)) {
            Some((0, _)) => 0,
            Some((1, false)) => 1,
            Some((1, true)) => 2,
            Some((2, _)) => 3,
            Some((3, _)) => 4,
            Some((4, false)) => 5,
            Some((4, true)) => 6,
            _ => 1,
        });
        d.set(P::AmpVoicing, voicing as f32);
        d.set(P::CabIr, if self.user_ir { 3.0 } else { match voicing { 0 => 0.0, 1 | 2 => 1.0, _ => 2.0 } });
        d.set(P::AmpSag, if voicing >= 3 { 0.45 } else { 0.25 });

        // pedals: on/off, type, and the knobs found by name in the first section
        let node = |n: Node| self.panel.node(n);
        let find = |n: Node, keys: &[&str]| -> Option<ParamRef> {
            let nd = node(n);
            self.panel.section_params(&nd.sections[0], e).into_iter().find(|r| keys.iter().any(|k| r.param.name.contains(k)))
        };
        let knob = |n: Node, keys: &[&str], def: f32| find(n, keys).map_or(def, norm);
        let ty = |n: Node| self.type_name(node(n)).to_uppercase();

        d.set(P::GateOn, self.node_on(node(Node::Ns)) as i32 as f32);
        d.set(P::GateThreshDb, -80.0 + 50.0 * knob(Node::Ns, &["THRESHOLD"], 0.3));

        let bt = ty(Node::Booster);
        d.set(P::DriveOn, self.node_on(node(Node::Booster)) as i32 as f32);
        let model = if bt.contains("CENTA") || bt.contains("KLON") {
            1
        } else if bt.contains("BLUES") {
            2
        } else if bt.contains("RAT") || bt.contains("DST") || bt.contains("DIST") {
            3
        } else if bt.contains("FUZZ") || bt.contains("MUFF") {
            4
        } else if bt.contains("METAL") || bt.contains("MT") || bt.contains("HM") {
            5
        } else if bt.contains("SCREAM") || bt.contains("OD") || bt.contains("TS") {
            0
        } else {
            2
        };
        d.set(P::DriveModel, model as f32);
        d.set(P::DriveAmt, knob(Node::Booster, &["DRIVE"], 0.5));
        d.set(P::DriveTone, knob(Node::Booster, &["TONE"], 0.5));
        d.set(P::DriveLevel, knob(Node::Booster, &["LEVEL"], 0.5));

        // MOD block, or FX when MOD is off
        let mod_kind = |t: &str| {
            if t.contains("CHORUS") {
                Some(0)
            } else if t.contains("FLANGER") {
                Some(1)
            } else if t.contains("PHASER") {
                Some(2)
            } else if t.contains("TREMOLO") {
                Some(3)
            } else if t.contains("ROTARY") || t.contains("UNI-V") || t.contains("VIBE") {
                Some(4)
            } else {
                None
            }
        };
        let pick = [Node::Mod, Node::Fx].into_iter().find_map(|n| if self.node_on(node(n)) { mod_kind(&ty(n)).map(|k| (n, k)) } else { None });
        d.set(P::ModOn, pick.is_some() as i32 as f32);
        if let Some((n, k)) = pick {
            d.set(P::ModType, k as f32);
            d.set(P::ModRate, 0.1 + 7.9 * knob(n, &["RATE"], 0.3).powi(2));
            d.set(P::ModDepth, knob(n, &["DEPTH"], 0.5));
            d.set(P::ModMix, if k == 3 { 1.0 } else { 0.5 });
        }

        d.set(P::DelayOn, self.node_on(node(Node::Delay)) as i32 as f32);
        if let Some(r) = find(Node::Delay, &["TIME"]) {
            let v = e.get(r) as f32;
            d.set(P::DelayMs, if r.param.max >= 1000 { v } else { 20.0 + norm(r) * 1980.0 });
        }
        d.set(P::DelayFeedback, 0.9 * knob(Node::Delay, &["FEEDBACK", "F.BACK"], 0.35));
        d.set(P::DelayMix, 0.8 * knob(Node::Delay, &["EFFECT LEVEL", "LEVEL"], 0.4));
        let dt = ty(Node::Delay);
        d.set(P::DelayPingPong, (dt.contains("PAN") || dt.contains("STEREO") || dt.contains("PING")) as i32 as f32);

        d.set(P::ReverbOn, self.node_on(node(Node::Reverb)) as i32 as f32);
        let rt = ty(Node::Reverb);
        d.set(
            P::ReverbType,
            if rt.contains("HALL") {
                1.0
            } else if rt.contains("SPRING") {
                2.0
            } else if rt.contains("MODULATE") || rt.contains("SHIMMER") {
                3.0
            } else {
                0.0
            },
        );
        d.set(P::ReverbDecay, knob(Node::Reverb, &["TIME"], 0.5));
        d.set(P::ReverbMix, 0.9 * knob(Node::Reverb, &["EFFECT LEVEL", "LEVEL"], 0.3));
        self.ui().set_dsp_voicing(self.dsp_voicing.unwrap_or(-1));
    }

    fn refresh_ab(&self) {
        let ui = self.ui();
        ui.set_ab_slot(self.ab_slot);
        ui.set_ab_has_b(self.ab_other.is_some() || self.ab_slot == 1);
    }

    /// Show snapshot `slot`. First visit to B makes it a copy of A; after that the two swap.
    fn ab_pick(&mut self, slot: i32) {
        if slot == self.ab_slot || self.sel.is_none() {
            return;
        }
        match self.ab_other.as_mut() {
            None => {
                self.ab_other = Some(self.edit.clone());
                self.toast("B is a copy of A. Edit B, compare with A/B", false);
            }
            Some(other) => {
                std::mem::swap(&mut self.edit, other);
                if self.is_live() {
                    self.dev.send(Cmd::Audition(Box::new(self.edit.clone())));
                }
            }
        }
        self.ab_slot = slot;
        self.hist.reset(self.edit.clone());
        self.refresh_ab();
        self.refresh_editor();
        self.after_edit();
    }

    fn after_edit(&mut self) {
        if matches!(self.insp, Some(Node::Eq | Node::Delay | Node::Delay2)) {
            self.refresh_extras();
        }
        self.hist.commit(&self.edit, std::time::Instant::now());
        self.refresh_history();
        self.sync_dsp();
        let ui = self.ui();
        ui.set_ed_dirty(self.edit != self.saved);
        if let Some(id) = self.sel.clone() {
            self.refresh_row(&id);
        }
    }

    /// Append a tone to a setlist; an empty `set` id makes a new setlist holding just this tone.
    fn add_to_setlist(&mut self, tid: &str, set: &str) {
        if set.is_empty() {
            let n = self.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Setlist).count() + 1;
            let name = format!("Setlist {n}");
            match self.lib.add_setlist(&name, SetlistKind::Setlist, vec![tid.to_string()]) {
                Ok(_) => {
                    self.refresh_nav();
                    self.toast(format!("Added to new setlist “{name}”"), false);
                }
                Err(e) => self.toast(e.to_string(), true),
            }
            return;
        }
        let Some(sl) = self.lib.setlists.iter().find(|x| x.id == set) else { return };
        if sl.tones.iter().any(|t| t == tid) {
            self.toast(format!("Already in “{}”", sl.name), false);
            return;
        }
        let name = sl.name.clone();
        if let Err(e) = self.lib.update_setlist(set, |x| x.tones.push(tid.to_string())) {
            self.toast(e.to_string(), true);
            return;
        }
        self.refresh_nav();
        if self.nav_sel == set {
            self.refresh_list();
        }
        self.toast(format!("Added to “{name}”"), false);
    }

    fn refresh_history(&self) {
        let ui = self.ui();
        ui.set_can_undo(self.hist.can_undo());
        ui.set_can_redo(self.hist.can_redo());
    }

    /// Undo (`redo == false`) or redo one edit step: the whole patch comes back, the amp gets it in one go.
    fn history_step(&mut self, redo: bool) {
        if self.sel.is_none() {
            return;
        }
        let got = if redo { self.hist.redo(&self.edit) } else { self.hist.undo(&self.edit) };
        let Some(p) = got else { return };
        self.edit = p;
        if self.is_live() {
            self.dev.send(Cmd::Audition(Box::new(self.edit.clone())));
        }
        self.refresh_editor();
        self.after_edit();
        if self.insp.is_some() {
            self.refresh_inspector();
        }
    }

    // ------------------------------------------------------------------ amp strip

    fn refresh_slots(&self) {
        let v: Vec<Slot> = (0..9)
            .map(|i| Slot { label: s(SLOT_LABELS[i]), name: s(&self.slot_names[i]), current: self.connected && self.cur_slot as usize == i })
            .collect();
        self.ui().set_slots(ModelRc::new(VecModel::from(v)));
    }

    fn refresh_import_hint(&self) {
        let dirs = find_bts_dirs();
        let hint = if dirs.is_empty() {
            "No mounted Windows partition with BOSS Tone Studio found. Mount it (e.g. udisksctl mount -b /dev/nvme0n1p3) or enter a .tsl file / folder below.".to_string()
        } else {
            format!(
                "Found a BOSS Tone Studio library:\n{}\nImport is idempotent: duplicates are merged, importing again never doubles anything.",
                dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join("\n")
            )
        };
        self.ui().set_import_hint(s(hint));
    }

    fn set_live_by_content(&mut self, p: &Patch) {
        let prev = self.live.take();
        self.live = self.lib.find_by_content(p).map(|t| t.id.clone());
        if self.live.is_none() && self.edit == *p {
            self.live = self.sel.clone();
        }
        for id in [prev, self.live.clone()].into_iter().flatten() {
            self.refresh_row(&id);
        }
    }

    fn on_device(&mut self, e: Event) {
        match e {
            Event::Connected { port: _ } => {
                self.connected = true;
                let ui = self.ui();
                ui.set_connected(true);
                ui.set_conn_text(s("Online"));
                self.toast("Connected to Katana MkII", false);
            }
            Event::Disconnected(why) => {
                self.connected = false;
                let ui = self.ui();
                ui.set_connected(false);
                ui.set_conn_text(s("Offline"));
                self.busy("", -1.0);
                self.refresh_slots();
                self.toast(why, true);
            }
            Event::SlotNames(n) => {
                self.slot_names = n;
                self.refresh_slots();
            }
            Event::CurrentSlot(c) => {
                self.cur_slot = c;
                self.refresh_slots();
            }
            Event::TempPatch(p) => self.set_live_by_content(&p),
            Event::TempEdited { rel, data } => {
                // knob turned on the amp: follow it in the editor when this tone is live
                if self.is_live() {
                    if self.edit.apply_rel(rel, &data) {
                        self.refresh_editor();
                        self.after_edit();
                    }
                } else if let Some(id) = self.live.take() {
                    self.refresh_row(&id);
                }
            }
            Event::Progress { label, done, total } => self.busy(&label, done as f32 / total.max(1) as f32),
            Event::SlotPatch(slot, p) => {
                self.busy("", -1.0);
                let label = SLOT_LABELS[slot as usize];
                if let Some((wslot, wp, wname)) = self.pending_write.take() {
                    if wslot == slot {
                        let stamp = local_stamp();
                        match self.lib.add_patch(*p, &format!("AMP · {label} (przed nadpisaniem {stamp})"), "") {
                            Ok((id, _)) => {
                                let _ = self.lib.add_setlist(&format!("{label} przed zapisem · {stamp}"), SetlistKind::Backup, vec![id]);
                                self.refresh_nav();
                            }
                            Err(err) => {
                                self.toast(format!("Backup of {label} failed: {err} — write aborted"), true);
                                return;
                            }
                        }
                        self.toast(format!("{label} backed up. Writing “{wname}”…"), false);
                        self.dev.send(Cmd::WriteSlot { slot, patch: Box::new(wp) });
                        return;
                    }
                    self.pending_write = Some((wslot, wp, wname));
                }
                match self.lib.add_patch(*p, &format!("AMP · {label}"), "") {
                    Ok((id, new)) => {
                        self.toast(if new { format!("Channel {label} added to the library") } else { format!("Channel {label} is already in the library") }, false);
                        self.nav_sel = "all".into();
                        self.refresh_nav();
                        self.refresh_list();
                        self.select(&id);
                    }
                    Err(err) => self.toast(err.to_string(), true),
                }
            }
            Event::AllSlots(all) => {
                self.busy("", -1.0);
                let stamp = local_stamp();
                let mut ids = vec![];
                let mut new = 0;
                for (i, p) in all.into_iter().enumerate() {
                    if let Ok((id, n)) = self.lib.add_patch(p, &format!("AMP · {} · {stamp}", SLOT_LABELS[i]), "") {
                        ids.push(id);
                        new += n as usize;
                    }
                }
                match self.lib.add_setlist(&format!("Amp backup · {stamp}"), SetlistKind::Backup, ids) {
                    Ok(id) => {
                        self.nav_sel = id;
                        self.ui().set_page(2);
                        self.refresh_nav();
                        self.refresh_list();
                        self.toast(format!("All 9 channels backed up ({new} new presets)"), false);
                    }
                    Err(err) => self.toast(err.to_string(), true),
                }
            }
            Event::Written { slot, verified } => {
                self.busy("", -1.0);
                let label = SLOT_LABELS[slot as usize];
                if verified {
                    self.toast(format!("Written to {label}, verified by read-back ✓"), false);
                } else {
                    self.toast(format!("Write to {label}: read-back differs!"), true);
                }
            }
            Event::Error(err) => {
                self.busy("", -1.0);
                self.pending_write = None;
                self.toast(err, true);
            }
        }
    }

    // ------------------------------------------------------------------ imports / exports

    fn import_path(&mut self, path: &str) {
        let path = path.trim();
        let p = PathBuf::from(shellexpand(path));
        let res = if p.is_dir() && p.join("ID_LIST").exists() {
            self.lib.import_bts_pref_dir(&p)
        } else if p.is_dir() {
            let mut total = katana_core::library::ImportReport::default();
            let mut files: Vec<_> = std::fs::read_dir(&p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            files.sort();
            for f in files.into_iter().filter(|f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("tsl"))) {
                if let Ok(r) = self.lib.import_tsl_file(&f) {
                    total.setlists += r.setlists;
                    total.patches += r.patches;
                    total.new_tones += r.new_tones;
                    total.duplicates += r.duplicates;
                }
            }
            Ok(total)
        } else {
            self.lib.import_tsl_file(&p)
        };
        match res {
            Ok(r) => {
                self.toast(format!("Import: {} setlists, {} patches, {} new presets, {} duplicates merged", r.setlists, r.patches, r.new_tones, r.duplicates), false);
                self.refresh_nav();
                self.refresh_list();
            }
            Err(e) => self.toast(format!("Import failed: {e}"), true),
        }
    }

    fn export(&mut self, name: &str, ids: Vec<String>) {
        let dir = export_dir();
        let path = dir.join(format!("{}.tsl", safe_file_name(name)));
        let r = std::fs::create_dir_all(&dir).and_then(|_| self.lib.export_tsl(name, &ids, &path));
        match r {
            Ok(()) => self.toast(format!("Saved {}", path.display()), false),
            Err(e) => self.toast(format!("Export failed: {e}"), true),
        }
    }
}

/// Two-letter tile label: initials of the first two words, else the first two characters.
fn abbr(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).collect();
    let first = |w: &str| w.chars().find(|c| c.is_alphanumeric()).unwrap_or('?');
    let s: String = match words.as_slice() {
        [a, b, ..] => [first(a), first(b)].iter().collect(),
        [a] => a.chars().filter(|c| c.is_alphanumeric()).take(2).collect(),
        [] => "?".into(),
    };
    s.to_uppercase()
}

/// "EFFECT LEVEL" -> "Effect Level"; short tokens (SW, EQ, 2x2) stay as they are.
/// Silkscreen colour: dark on bright enclosures, cream on dark ones.
/// Library tags derived from the patch: amp family plus an "ambient" flag for clean tones
/// with both delay and reverb on.
fn tone_tags(p: &Patch, amp_type: ParamRef) -> Vec<&'static str> {
    let mut v = vec![];
    let fam = amp_family(p.get(amp_type)).map(|(f, _)| f);
    match fam {
        Some(0 | 1) => v.push("clean"),
        Some(2) => v.push("crunch"),
        Some(3) => v.push("lead"),
        Some(4) => v.push("hi-gain"),
        _ => {}
    }
    let sm = p.summary();
    if matches!(fam, Some(0 | 1)) && sm.leds[3] != Led::Off && sm.leds[4] != Led::Off {
        v.push("ambient");
    }
    v
}

fn ink_for(hue: slint::Color) -> slint::Color {
    if 0.299 * hue.red() as f32 + 0.587 * hue.green() as f32 + 0.114 * hue.blue() as f32 > 140.0 {
        slint::Color::from_argb_u8(235, 18, 14, 10)
    } else {
        slint::Color::from_argb_u8(240, 244, 236, 220)
    }
}

/// How much gain an amp model implies, 0 acoustic .. 5 high gain, for the amp badge.
fn drive_level(amp: &str) -> i32 {
    let a = amp.to_uppercase();
    let has = |k: &[&str]| k.iter().any(|w| a.contains(w));
    if has(&["BROWN", "HIGAIN", "EXTREME", "METAL", "MODERN"]) {
        5
    } else if has(&["LEAD", "POWER DRIVE", "VINTAGE", "CUSTOM"]) {
        4
    } else if has(&["CRUNCH", "MS1959"]) {
        3
    } else if has(&["DRIVE", "PUSHED"]) {
        2
    } else if has(&["CLEAN"]) {
        1
    } else if has(&["ACOUSTIC"]) {
        0
    } else {
        3
    }
}

fn nice(name: &str) -> String {
    name.split(' ')
        .map(|w| {
            if w.chars().count() <= 2 || w.chars().any(|c| c.is_ascii_digit()) {
                w.to_string()
            } else {
                let mut c = w.chars();
                c.next().map(|f| f.to_string() + &c.as_str().to_lowercase()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 0/1 params without labels are on/off switches.
fn is_switch(d: &katana_core::map::ParamDef) -> bool {
    d.labels.is_empty() && d.min == 0 && d.max == 1
}

/// Inspector label of a parameter. One name per concept across the app: the preamp
/// output is "Volume" (as on the amp's panel), the solo switch is "Solo Boost".
fn param_name(d: &katana_core::map::ParamDef) -> String {
    match d.prm {
        "PRM_PREAMP_A_LEVEL" => "Volume".into(),
        _ if d.name == "SOLO SW" => "Solo Boost".into(),
        _ => nice(&d.name.replace('_', " ")),
    }
}

fn param_text(d: &katana_core::map::ParamDef, v: i16) -> String {
    if is_switch(d) {
        return if v != 0 { "ON".into() } else { "OFF".into() };
    }
    if let Some(l) = d.label(v) {
        return l.to_string();
    }
    // EQ index tables (BOSS frequency / Q steps), the map only stores the index
    const LOW_CUT: [&str; 18] = ["FLAT", "20 Hz", "25 Hz", "31.5 Hz", "40 Hz", "50 Hz", "63 Hz", "80 Hz", "100 Hz", "125 Hz", "160 Hz", "200 Hz", "250 Hz", "315 Hz", "400 Hz", "500 Hz", "630 Hz", "800 Hz"];
    const HIGH_CUT: [&str; 15] = ["630 Hz", "800 Hz", "1 kHz", "1.25 kHz", "1.6 kHz", "2 kHz", "2.5 kHz", "3.15 kHz", "4 kHz", "5 kHz", "6.3 kHz", "8 kHz", "10 kHz", "12.5 kHz", "FLAT"];
    const FREQ: [&str; 28] = [
        "20 Hz", "25 Hz", "31.5 Hz", "40 Hz", "50 Hz", "63 Hz", "80 Hz", "100 Hz", "125 Hz", "160 Hz", "200 Hz", "250 Hz", "315 Hz", "400 Hz",
        "500 Hz", "630 Hz", "800 Hz", "1 kHz", "1.25 kHz", "1.6 kHz", "2 kHz", "2.5 kHz", "3.15 kHz", "4 kHz", "5 kHz", "6.3 kHz", "8 kHz", "10 kHz",
    ];
    const Q: [&str; 6] = ["0.5", "1", "2", "4", "8", "16"];
    let table: Option<&[&str]> = match d.prm {
        "PRM_EQ_LOW_CUT" => Some(&LOW_CUT),
        "PRM_EQ_HIGH_CUT" => Some(&HIGH_CUT),
        "PRM_EQ_LOWMID_FREQ" | "PRM_EQ_HIGHMID_FREQ" => Some(&FREQ),
        "PRM_EQ_LOWMID_Q" | "PRM_EQ_HIGHMID_Q" => Some(&Q),
        _ => None,
    };
    if let Some(s) = table.and_then(|t| t.get(v as usize)) {
        return s.to_string();
    }
    // EQ gains in dB get a sign; the preamp GAIN is a plain 0–120 knob position
    if (d.prm.contains("GAIN") && !d.prm.starts_with("PRM_PREAMP_")) || d.prm.contains("_GEQ_") || d.prm == "PRM_EQ_LEVEL" {
        return format!("{v:+} dB").replace("+0 dB", "0 dB");
    }
    v.to_string()
}

fn shellexpand(p: &str) -> String {
    let p = p.trim_matches(|c| c == '\'' || c == '"');
    let p = p.strip_prefix("file://").unwrap_or(p);
    if let Some(rest) = p.strip_prefix("~/") {
        return format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest);
    }
    p.to_string()
}

fn with_app(f: impl FnOnce(&mut App)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        // never re-enter: UI callbacks are delivered from the event loop, not from setters
        if let Ok(mut a) = app.try_borrow_mut() {
            f(&mut a);
        }
    }
}

fn bind(ui: &MainWindow) {
    ui.on_search(|q| with_app(|a| {
        a.query = q.to_string();
        a.refresh_list();
    }));
    ui.on_nav_clicked(|id| with_app(|a| {
        a.nav_sel = id.to_string();
        a.ui().set_nav_selected(id);
        a.refresh_list();
    }));
    // One click = the amp plays it (when connected). Switching tones is the main job.
    ui.on_tone_clicked(|i| with_app(|a| {
        if let Some(id) = a.visible.get(i as usize).cloned() {
            a.select(&id);
            if a.connected {
                audition(a);
            }
        }
    }));
    ui.on_essential_clicked(|id| with_app(|a| {
        let id = id.to_string();
        a.select(&id);
        if a.connected {
            audition(a);
        }
    }));
    ui.on_tone_activated(|i| with_app(|a| {
        if let Some(id) = a.visible.get(i as usize).cloned() {
            a.select(&id);
            audition(a);
        }
    }));
    ui.on_tone_fav(|i| with_app(|a| {
        if let Some(id) = a.visible.get(i as usize).cloned() {
            let _ = a.lib.update_tone(&id, |t| t.favorite = !t.favorite);
            a.refresh_row(&id);
            a.refresh_nav();
            if a.sel.as_deref() == Some(&id) {
                let fav = a.lib.tone(&id).map(|t| t.favorite).unwrap_or(false);
                a.ui().set_ed_fav(fav);
            }
        }
    }));
    ui.on_toggle_fav(|| with_app(|a| {
        if let Some(id) = a.sel.clone() {
            let _ = a.lib.update_tone(&id, |t| t.favorite = !t.favorite);
            a.refresh_row(&id);
            a.refresh_nav();
            let fav = a.lib.tone(&id).map(|t| t.favorite).unwrap_or(false);
            a.ui().set_ed_fav(fav);
        }
    }));
    ui.on_amp_family(|f| with_app(|a| {
        a.dsp_voicing = None;
        let var = a.ui().get_ed_amp_var();
        let (_, n, vv) = AMP_PANEL[f as usize];
        let at = a.panel.amp_type;
        a.set_param(at, if var { vv } else { n });
        a.set_status_byte(0, f as u8);
        a.set_status_byte(12, var as u8);
        a.refresh_editor();
        a.after_edit();
    }));
    ui.on_amp_variation(|var| with_app(|a| {
        let at = a.panel.amp_type;
        let fam = amp_family(a.edit.get(at)).map(|x| x.0).unwrap_or(1);
        let (_, n, vv) = AMP_PANEL[fam];
        a.set_param(at, if var { vv } else { n });
        a.set_status_byte(0, fam as u8);
        a.set_status_byte(12, var as u8);
        a.refresh_editor();
        a.after_edit();
    }));
    ui.on_pedal_clicked(|i| with_app(|a| {
        let Some(n) = a.layout.get(i as usize).copied() else { return };
        // the inspector always shows a block: clicking the open one keeps it (and its tab)
        if a.insp != Some(n) {
            a.insp = Some(n);
            a.insp_section = 0;
            a.refresh_inspector();
        }
    }));
    ui.on_pedal_toggled(|i| with_app(|a| {
        let Some(n) = a.layout.get(i as usize).copied() else { return };
        let on = a.node_on(a.panel.node(n));
        a.set_node_on(n, !on);
    }));
    ui.on_pedal_drag_start(|i| with_app(|a| {
        let ok = a.routing().valid_gaps(i as usize);
        a.ui().set_drop_ok(ModelRc::new(VecModel::from(ok)));
    }));
    ui.on_pedal_dropped(|from, gap| {
        // defer: the pedal component that fired this is about to be rebuilt
        Timer::single_shot(Duration::ZERO, move || with_app(|a| a.drop_pedal(from as usize, gap as usize)));
    });
    ui.on_insp_toggle(|on| with_app(|a| {
        if let Some(n) = a.insp {
            a.set_node_on(n, on);
        }
    }));
    ui.on_insp_variant_picked(|c| with_app(|a| {
        if let Some(n) = a.insp {
            a.set_node_variant(n, c as usize);
        }
    }));
    ui.on_insp_type_picked(|v| with_app(|a| {
        if let Some(n) = a.insp {
            a.set_node_type(n, v as i16);
        }
    }));
    ui.on_insp_section_picked(|i| with_app(|a| {
        a.insp_section = i as usize;
        a.refresh_inspector();
    }));
    ui.on_insp_enum_request(|i| with_app(|a| {
        let Some(r) = a.insp_rows.get(i as usize).copied() else { return };
        let opts: Vec<Opt> = if is_switch(r.param) {
            vec![Opt { value: 0, label: s("OFF") }, Opt { value: 1, label: s("ON") }]
        } else {
            r.param.labels.iter().map(|(v, l)| Opt { value: *v as i32, label: s(l) }).collect()
        };
        a.popup = Some(Popup::Param(r));
        let ui = a.ui();
        ui.set_popup_title(s(format!("{} · {}", group_of(r.param), param_name(r.param))));
        ui.set_popup_options(ModelRc::new(VecModel::from(opts)));
        ui.invoke_open_popup();
    }));
    ui.on_popup_picked(|v| with_app(|a| {
        match a.popup.take() {
            Some(Popup::Param(r)) => {
                a.set_param(r, v as i16);
                a.refresh_editor();
                a.after_edit();
            }
            Some(Popup::Type) => {
                if let Some(n) = a.insp {
                    a.set_node_type(n, v as i16);
                }
            }
            Some(Popup::AudioIn(list)) => {
                let d = if v == 0 { None } else { list.get(v as usize - 1).cloned() };
                a.audio_cfg.input = d.as_ref().map(|d| d.id.clone());
                save_audio_cfg(a.lib.root(), &a.audio_cfg);
                a.ui().set_audio_input(s(dev_label(a.audio_cfg.input.as_deref(), "None (DI)")));
                match &d {
                    Some(d) if d.mic => a.toast(format!("{}: looks like a microphone — expect feedback on speakers. Use headphones!", d.label), true),
                    Some(d) => a.toast(format!("Guitar input: {}", d.label), false),
                    None => a.toast("No input: DI player only", false),
                }
                if d.is_some() && a.di_on {
                    a.set_di(false);
                }
                if a.engine.is_some() {
                    a.start_engine();
                }
            }
            Some(Popup::AudioOut(list)) => {
                a.audio_cfg.output = if v == 0 { None } else { list.get(v as usize - 1).map(|d| d.id.clone()) };
                save_audio_cfg(a.lib.root(), &a.audio_cfg);
                a.ui().set_audio_device(s(dev_label(a.audio_cfg.output.as_deref(), "System default")));
                if a.engine.is_some() {
                    a.start_engine();
                }
            }
            None => {}
        }
    }));
    ui.on_insp_type_request(|| with_app(|a| {
        let Some(n) = a.insp else { return };
        a.popup = Some(Popup::Type);
        let ui = a.ui();
        ui.set_popup_title(s(format!("{} — type", a.panel.node(n).title)));
        ui.set_popup_options(ui.get_insp_types());
        ui.invoke_open_popup();
    }));
    ui.on_pedal_knob_set(|i, k, x| with_app(|a| a.pedal_knob(i as usize, k as usize, Some(x), 0)));
    ui.on_pedal_knob_step(|i, k, d| with_app(|a| a.pedal_knob(i as usize, k as usize, None, d)));
    ui.on_insp_param(|i, v| with_app(|a| {
        let Some(r) = a.insp_rows.get(i as usize).copied() else { return };
        if a.edit.get(r) == v as i16 {
            return;
        }
        a.set_param(r, v as i16);
        let ui = a.ui();
        let m = ui.get_insp_params();
        if let Some(mut row) = m.row_data(i as usize) {
            row.value = v;
            row.display = s(param_text(r.param, v as i16));
            m.set_row_data(i as usize, row);
        }
        if let Some(n) = a.insp {
            a.refresh_pedal(n);
        }
        a.after_edit();
    }));
    ui.on_rename(|n| with_app(|a| {
        let n = n.trim().to_string();
        if n.is_empty() {
            return;
        }
        a.edit.set_name(&n);
        if a.is_live() {
            let data = a.edit.block(0).to_vec();
            a.dev.send(Cmd::Param { rel: 0, data });
        }
        a.refresh_editor();
        a.after_edit();
    }));
    ui.on_audition(|| with_app(audition));
    ui.on_mode_picked(|hw| with_app(|a| {
        a.set_mode(hw);
        a.toast(if hw { "Hardware mode: edits sync to the Katana" } else { "Standalone mode: PC DSP engine (beta), the Katana gets no edits" }, false);
    }));
    ui.on_audio_device_request(|input| with_app(|a| {
        let list: Vec<haze_dsp::DeviceInfo> = haze_dsp::list_devices(input).into_iter().filter(|d| input || d.id != "default").collect();
        let mut opts = vec![Opt { value: 0, label: s(if input { "— None (DI player only)" } else { "System default (PipeWire)" }) }];
        for (i, d) in list.iter().enumerate() {
            let warn = if d.mic { "   ⚠ microphone" } else { "" };
            opts.push(Opt { value: i as i32 + 1, label: s(format!("{}{warn}", d.label)) });
        }
        a.popup = Some(if input { Popup::AudioIn(list) } else { Popup::AudioOut(list) });
        let ui = a.ui();
        ui.set_popup_title(s(if input { "Guitar input" } else { "Audio output" }));
        ui.set_popup_options(ModelRc::new(VecModel::from(opts)));
        ui.invoke_open_popup();
    }));
    ui.on_audio_buffer_picked(|b| with_app(|a| {
        a.audio_cfg.buffer = b as u32;
        save_audio_cfg(a.lib.root(), &a.audio_cfg);
        a.ui().set_audio_buffer(b);
        if a.engine.is_some() {
            a.start_engine();
        }
    }));
    ui.on_audio_rate_picked(|r| with_app(|a| {
        a.audio_cfg.rate = r as u32;
        save_audio_cfg(a.lib.root(), &a.audio_cfg);
        a.ui().set_audio_rate(r);
        if a.engine.is_some() {
            a.start_engine();
        }
    }));
    ui.on_limiter_toggled(|on| with_app(|a| {
        a.dsp.set(haze_dsp::P::Limiter, on as i32 as f32);
        a.ui().set_limiter(on);
    }));
    ui.on_di_toggled(|on| with_app(|a| a.set_di(on)));
    ui.on_transport(|| with_app(|a| {
        if a.hw {
            return;
        }
        if a.ui().get_practice_open() {
            a.loop_action(0);
        } else {
            let on = !a.di_on;
            a.set_di(on);
        }
    }));
    ui.on_practice_toggled(|open| with_app(|a| {
        if open {
            a.ensure_loop_buffer();
        }
    }));
    ui.on_loop_action(|x| with_app(|a| a.loop_action(x)));
    ui.on_drum_toggled(|on| with_app(|a| {
        a.dsp.set(haze_dsp::P::DrumOn, on as i32 as f32);
        a.ui().set_drum_on(on);
    }));
    ui.on_drum_pattern_picked(|i| with_app(|a| {
        a.dsp.set(haze_dsp::P::DrumPattern, i as f32);
        a.ui().set_drum_pattern(i);
    }));
    ui.on_bpm_changed(|b| with_app(|a| a.set_bpm(b)));
    ui.on_tap(|| with_app(|a| a.tap()));
    ui.on_load_ir(|p| with_app(|a| a.load_wav(&p, true)));
    ui.on_load_di(|p| with_app(|a| a.load_wav(&p, false)));
    // screenshots / debugging: start with the practice drawer open
    if std::env::var_os("HAZE_PRACTICE").is_some() {
        ui.set_practice_open(true);
    }
    ui.on_dsp_voicing_picked(|v| with_app(|a| {
        a.dsp_voicing = Some(v);
        a.sync_dsp();
    }));
    ui.on_ab_picked(|i| with_app(|a| a.ab_pick(i)));
    // Pages: Library shows All / Favorites, Setlists shows one collection (the first by default,
    // "" when there is none). The Rig page keeps whatever collection the picker browses.
    ui.on_page_picked(|page| with_app(|a| {
        let in_set = a.nav_sel != "all" && a.nav_sel != "fav";
        match page {
            1 if in_set => a.nav_sel = "all".into(),
            2 if !in_set || a.nav_sel.is_empty() => {
                let sets = a.lib.setlists.iter();
                let first = sets.clone().find(|x| x.kind != SetlistKind::Backup).or_else(|| sets.clone().next());
                a.nav_sel = first.map(|x| x.id.clone()).unwrap_or_default();
            }
            _ => {}
        }
        a.ui().set_nav_selected(s(&a.nav_sel));
        a.ui().set_page(page);
        a.refresh_list();
    }));
    // ◀ ▶ in the preset header: step through the presets of the current collection
    ui.on_tone_step(|d| with_app(|a| {
        if a.visible.is_empty() {
            return;
        }
        let n = a.visible.len() as i32;
        let cur = a.sel.as_ref().and_then(|id| a.visible.iter().position(|v| v == id)).map_or(if d > 0 { -1 } else { n }, |i| i as i32);
        let next = (cur + d).rem_euclid(n) as usize;
        let id = a.visible[next].clone();
        a.select(&id);
        if a.connected {
            audition(a);
        }
    }));
    // ---------------------------------------------------------------- setlist editing
    // Rows are addressed by their position in the visible list; the setlist itself is edited
    // by tone id, so a search or tag filter never scrambles the stored order.
    ui.on_set_new(|| with_app(|a| {
        let n = a.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Setlist).count() + 1;
        match a.lib.add_setlist(&format!("Setlist {n}"), SetlistKind::Setlist, vec![]) {
            Ok(id) => {
                a.nav_sel = id;
                a.ui().set_nav_selected(s(&a.nav_sel));
                a.refresh_nav();
                a.refresh_list();
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
    ui.on_set_rename(|name| with_app(|a| {
        let name = name.trim().to_string();
        let id = a.nav_sel.clone();
        if name.is_empty() || !a.lib.setlists.iter().any(|x| x.id == id) {
            return;
        }
        if let Err(e) = a.lib.update_setlist(&id, |x| x.name = name) {
            a.toast(e.to_string(), true);
        }
        a.refresh_nav();
        a.refresh_list();
    }));
    ui.on_set_add_current(|| with_app(|a| {
        let (Some(tid), id) = (a.sel.clone(), a.nav_sel.clone()) else { return };
        a.add_to_setlist(&tid, &id);
    }));
    ui.on_tone_add_to(|i, set| with_app(|a| {
        let Some(tid) = a.visible.get(i as usize).cloned() else { return };
        a.add_to_setlist(&tid, &set);
    }));
    ui.on_song_move(|i, d| with_app(|a| {
        let id = a.nav_sel.clone();
        let Some(tid) = a.visible.get(i as usize).cloned() else { return };
        let r = a.lib.update_setlist(&id, |x| {
            if let Some(k) = x.tones.iter().position(|t| *t == tid) {
                let j = k as i32 + d;
                if j >= 0 && (j as usize) < x.tones.len() {
                    x.tones.swap(k, j as usize);
                }
            }
        });
        if let Err(e) = r {
            a.toast(e.to_string(), true);
        }
        a.refresh_list();
    }));
    ui.on_song_remove(|i| with_app(|a| {
        let id = a.nav_sel.clone();
        let Some(tid) = a.visible.get(i as usize).cloned() else { return };
        if let Err(e) = a.lib.update_setlist(&id, |x| {
            if let Some(k) = x.tones.iter().position(|t| *t == tid) {
                x.tones.remove(k);
            }
        }) {
            a.toast(e.to_string(), true);
        }
        a.refresh_nav();
        a.refresh_list();
    }));
    // Starter rigs: copy the template into the library and open it on the rig
    ui.on_template_use(|i| with_app(|a| {
        let Some(t) = crate::templates::TEMPLATES.get(i as usize) else { return };
        let p = t.build(&a.panel);
        match a.lib.add_patch(p, "Haze starter rig", t.blurb) {
            Ok((id, new)) => {
                a.nav_sel = "all".into();
                a.ui().set_nav_selected(s("all"));
                a.refresh_nav();
                a.refresh_list();
                a.select(&id);
                a.ui().set_page(0);
                if a.connected {
                    audition(a);
                }
                a.toast(if new { format!("“{}” added to your library", t.name) } else { format!("“{}” is already in your library", t.name) }, false);
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
    ui.on_sort_picked(|v| with_app(|a| {
        a.sort = v;
        a.ui().set_sort(v);
        save_ui_cfg(a.lib.root(), a.ui_scale, a.sort);
        a.refresh_list();
    }));
    ui.on_ui_scale_picked(|v| with_app(|a| {
        a.ui_scale = v;
        a.ui().set_ui_scale(v);
        save_ui_cfg(a.lib.root(), a.ui_scale, a.sort);
        a.apply_scale();
    }));
    ui.on_set_tags(|t| with_app(|a| {
        let Some(id) = a.sel.clone() else { return };
        let mut tags: Vec<String> = t.split(',').map(|x| x.trim().trim_start_matches('#').to_string()).filter(|x| !x.is_empty()).collect();
        tags.dedup();
        if let Err(e) = a.lib.update_tone(&id, |x| x.tags = tags) {
            a.toast(e.to_string(), true);
            return;
        }
        // a filter on a tag nobody carries any more would show an empty library
        if let Some(u) = a.tag.strip_prefix('#') {
            if !a.lib.tones.iter().any(|x| x.tags.iter().any(|g| g == u)) {
                a.tag.clear();
                a.ui().set_tag(s(""));
            }
        }
        a.refresh_editor();
        a.refresh_list();
    }));
    ui.on_tag_picked(|t| with_app(|a| {
        a.tag = t.to_string();
        a.ui().set_tag(t);
        a.refresh_list();
    }));
    ui.on_save(|| with_app(|a| {
        let Some(id) = a.sel.clone() else { return };
        let p = a.edit.clone();
        match a.lib.update_tone(&id, |t| {
            t.name = p.name();
            t.patch = p.clone();
        }) {
            Ok(()) => {
                a.saved = a.edit.clone();
                a.refresh_editor();
                a.refresh_row(&id);
                a.toast("Saved to library", false);
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
    ui.on_save_copy(|| with_app(|a| {
        let Some(id) = a.sel.clone() else { return };
        let origin = a.lib.tone(&id).map(|t| t.origin.clone()).unwrap_or_default();
        let mut p = a.edit.clone();
        let base = p.name();
        let mut name: String = format!("{} 2", base.chars().take(14).collect::<String>().trim_end());
        let mut n = 2;
        while a.lib.tones.iter().any(|t| t.name == name) && n < 99 {
            n += 1;
            name = format!("{} {n}", base.chars().take(16 - 1 - n.to_string().len()).collect::<String>().trim_end());
        }
        p.set_name(&name);
        match a.lib.add_patch(p, &origin, "") {
            Ok((nid, _)) => {
                a.edit = a.saved.clone();
                a.refresh_nav();
                a.refresh_list();
                a.select(&nid);
                a.toast(format!("Created “{name}”"), false);
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
    ui.on_insp_delay_sync(|ms| with_app(|a| a.delay_sync(ms)));
    ui.on_undo(|| with_app(|a| a.history_step(false)));
    ui.on_redo(|| with_app(|a| a.history_step(true)));
    ui.on_revert(|| with_app(|a| {
        a.edit = a.saved.clone();
        if a.is_live() {
            a.dev.send(Cmd::Audition(Box::new(a.edit.clone())));
        }
        a.refresh_editor();
        a.after_edit();
    }));
    ui.on_delete_tone(|| with_app(|a| {
        let Some(id) = a.sel.take() else { return };
        let name = a.lib.tone(&id).map(|t| t.name.clone()).unwrap_or_default();
        match a.lib.trash_tone(&id) {
            Ok(()) => {
                a.refresh_nav();
                a.refresh_list();
                if let Some(first) = a.visible.first().cloned() {
                    a.select(&first);
                } else {
                    a.refresh_editor();
                }
                a.toast(format!("“{name}” moved to trash ({}/trash)", a.lib.root().display()), false);
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
    ui.on_export_tone(|| with_app(|a| {
        if let Some(id) = a.sel.clone() {
            let name = a.edit.name();
            a.export(&name, vec![id]);
        }
    }));
    ui.on_export_list(|| with_app(|a| {
        let title = a.ui().get_nav_title().to_string();
        let ids = a.visible.clone();
        a.export(&title, ids);
    }));
    ui.on_write_slot(|slot| with_app(|a| {
        if a.sel.is_none() {
            return;
        }
        let name = a.edit.name();
        // read the slot first so its current content lands in the library, then write
        a.pending_write = Some((slot as u8, a.edit.clone(), name));
        a.dev.send(Cmd::ReadSlot(slot as u8));
    }));
    ui.on_connect(|| with_app(|a| {
        a.ui().set_conn_text(s("Connecting…"));
        a.dev.send(Cmd::Connect);
    }));
    ui.on_select_slot(|i| with_app(|a| a.dev.send(Cmd::SelectSlot(i as u8))));
    ui.on_pull_slot(|i| with_app(|a| a.dev.send(Cmd::ReadSlot(i as u8))));
    ui.on_backup_amp(|| with_app(|a| a.dev.send(Cmd::ReadAllSlots)));
    ui.on_import_path(|p| with_app(|a| a.import_path(&p)));
    ui.on_import_windows(|| with_app(|a| {
        let dirs = find_bts_dirs();
        if dirs.is_empty() {
            a.toast("No mounted BOSS Tone Studio library", true);
            return;
        }
        for d in dirs {
            a.import_path(&d.to_string_lossy());
        }
    }));
}

fn audition(a: &mut App) {
    if !a.connected {
        a.toast("Amp not connected", true);
        return;
    }
    let Some(id) = a.sel.clone() else { return };
    a.dev.send(Cmd::Audition(Box::new(a.edit.clone())));
    let prev = a.live.replace(id.clone());
    if let Some(p) = prev {
        a.refresh_row(&p);
    }
    a.refresh_row(&id);
    a.ui().set_ed_live(true);
}

impl Drop for App {
    fn drop(&mut self) {
        self.dev.send(Cmd::Shutdown);
    }
}

pub fn shutdown() {
    let app = APP.with(|a| a.borrow_mut().take());
    if let Some(app) = app {
        app.borrow().dev.send(Cmd::Shutdown);
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// inotify on <library>/tones and /setlists; blocks in poll (no CPU while idle), coalesces a
/// burst of writes into one reload 250 ms after the last event.
fn watch_library(root: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let dirs: Vec<std::ffi::CString> = ["tones", "setlists"]
        .iter()
        .filter_map(|d| std::ffi::CString::new(root.join(d).as_os_str().as_bytes()).ok())
        .collect();
    let _ = std::thread::Builder::new().name("haze-library-watch".into()).spawn(move || unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK);
        if fd < 0 {
            return;
        }
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_MOVED_FROM | libc::IN_DELETE;
        for d in &dirs {
            libc::inotify_add_watch(fd, d.as_ptr(), mask);
        }
        let mut buf = [0u8; 4096];
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        loop {
            if libc::poll(&mut pfd, 1, -1) < 0 {
                break;
            }
            // drain, then keep draining until the burst has been quiet for 250 ms
            loop {
                while libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) > 0 {}
                if libc::poll(&mut pfd, 1, 250) <= 0 {
                    break;
                }
            }
            if slint::invoke_from_event_loop(|| with_app(|a| a.reload_library())).is_err() {
                break;
            }
        }
        libc::close(fd);
    });
}

#[cfg(test)]
mod drive_tests {
    #[test]
    fn amp_models_land_on_the_drive_scale() {
        let d = super::drive_level;
        assert_eq!(d("ACOUSTIC"), 0);
        assert_eq!(d("CLEAN VARIATION"), 1);
        assert_eq!(d("VO DRIVE"), 2);
        assert_eq!(d("CRUNCH"), 3);
        assert_eq!(d("LEAD"), 4);
        assert_eq!(d("POWER DRIVE"), 4);
        assert_eq!(d("BROWN VARIATION"), 5);
        assert_eq!(d("R-FIER MODERN"), 5);
    }
}
