//! Application state and UI binding. Runs on the UI thread only.

use crate::panel::{group_of, short, NodeDef, Panel, STATUS};
use crate::{Essential, KnobVal, MainWindow, MiniKnob, NavItem, Opt, ParamRow, Pedal, Slot, ToneRow};
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
use std::time::Duration;

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

enum Popup {
    Param(ParamRef),
    /// TYPE of the block open in the inspector
    Type,
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
    live: Option<String>,

    connected: bool,
    slot_names: Vec<String>,
    cur_slot: u8,

    /// signal flow currently shown on the pedalboard
    layout: Vec<Node>,
    pedals: Rc<VecModel<Pedal>>,
    /// params behind each pedal's mini knobs
    pedal_refs: Vec<Vec<ParamRef>>,
    insp: Option<Node>,
    insp_section: usize,
    insp_rows: Vec<ParamRef>,
    popup: Option<Popup>,
    /// slot write waiting for its pre-write backup read
    pending_write: Option<(u8, Patch, String)>,
    toast_timer: Timer,
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
            live: None,
            connected: false,
            slot_names: vec![String::new(); 9],
            cur_slot: 255,
            layout: vec![],
            pedals,
            pedal_refs: vec![],
            insp: None,
            insp_section: 0,
            insp_rows: vec![],
            popup: None,
            pending_write: None,
            toast_timer: Timer::default(),
        };
        let app = Rc::new(RefCell::new(app));
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        bind(ui);
        let mut a = app.borrow_mut();
        a.refresh_nav();
        a.refresh_list();
        a.refresh_slots();
        a.refresh_import_hint();
        if let Some(first) = a.visible.first().cloned() {
            a.select(&first);
        }
        if std::env::var_os("KATANA_NO_DEVICE").is_none() {
            a.dev.send(Cmd::Connect);
        }
        watch_library(a.lib.root());
    }

    /// Tones or setlists changed on disk (`haze derive`, another instance, a copied file):
    /// reread the library and refresh the sidebar. The open editor keeps its own copy of the
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
        eprintln!("biblioteka przeładowana: {} brzmień", self.lib.tones.len());
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
            NavItem { id: s("h-lib"), name: s("BIBLIOTEKA"), count: 0, kind: 4 },
            NavItem { id: s("all"), name: s("Wszystkie brzmienia"), count: self.lib.tones.len() as i32, kind: 0 },
            NavItem { id: s("fav"), name: s("Ulubione"), count: fav, kind: 1 },
        ];
        let sets: Vec<_> = self.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Setlist).collect();
        if !sets.is_empty() {
            v.push(NavItem { id: s("h-sets"), name: s("SETLISTY"), count: 0, kind: 4 });
            v.extend(sets.iter().map(|x| NavItem { id: s(&x.id), name: s(&x.name), count: x.tones.len() as i32, kind: 2 }));
        }
        let backs: Vec<_> = self.lib.setlists.iter().filter(|x| x.kind == SetlistKind::Backup).collect();
        if !backs.is_empty() {
            v.push(NavItem { id: s("h-back"), name: s("KOPIE WZMACNIACZA"), count: 0, kind: 4 });
            v.extend(backs.iter().rev().map(|x| NavItem { id: s(&x.id), name: s(&x.name), count: x.tones.len() as i32, kind: 3 }));
        }
        self.refresh_essentials();
        let ui = self.ui();
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
            name: s(&sm.name),
            amp: s(nice(&sm.amp)),
            fx: s(fx.join(" · ")),
            fav: t.favorite,
            live: self.live.as_deref() == Some(id),
            leds: ModelRc::new(VecModel::from(sm.leds.iter().map(|l| led_i(*l)).collect::<Vec<_>>())),
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
                if words.is_empty() {
                    return true;
                }
                let Some(t) = self.lib.tone(id) else { return false };
                let sm = t.patch.summary();
                let hay = format!("{} {} {} {}", sm.name, sm.amp, sm.fx_names.join(" "), t.origin).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        let rows: Vec<ToneRow> = self.visible.iter().filter_map(|id| self.row(id)).collect();
        self.rows.set_vec(rows);
        let title = match self.nav_sel.as_str() {
            "all" => "Wszystkie brzmienia".to_string(),
            "fav" => "Ulubione".to_string(),
            id => self.lib.setlists.iter().find(|x| x.id == id).map(|x| x.name.clone()).unwrap_or_default(),
        };
        self.ui().set_nav_title(s(title));
        self.sync_selected_index();
    }

    fn sync_selected_index(&self) {
        let idx = self.sel.as_ref().and_then(|id| self.visible.iter().position(|v| v == id)).map(|i| i as i32).unwrap_or(-1);
        self.ui().set_selected_index(idx);
    }

    /// Shortcut tiles in the sidebar rail: favourites, then recent tones, max 8.
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
        let knobs: Vec<KnobVal> = self
            .panel
            .knobs
            .iter()
            .map(|k| {
                let v = e.get(k.r);
                KnobVal {
                    label: s(k.label),
                    value: v as i32,
                    min: k.r.param.min as i32,
                    max: k.r.param.max as i32,
                    reset: k.r.param.init as i32,
                    display: s(v.to_string()),
                }
            })
            .collect();
        ui.set_knobs(ModelRc::new(VecModel::from(knobs)));
        self.refresh_rig();
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

    fn pedal_row(&self, n: Node) -> (Pedal, Vec<ParamRef>) {
        let d = self.panel.node(n);
        let refs = self.panel.pedal_knobs(d, &self.edit);
        let knobs: Vec<MiniKnob> = refs
            .iter()
            .map(|r| {
                let v = self.edit.get(*r);
                MiniKnob {
                    label: s(short(r.param.name)),
                    value: v as i32,
                    min: r.param.min as i32,
                    max: r.param.max as i32,
                    display: s(r.param.label(v).map(String::from).unwrap_or_else(|| v.to_string())),
                }
            })
            .collect();
        let p = Pedal {
            title: s(d.title),
            type_name: s(self.type_name(d)),
            on: self.node_on(d),
            has_sw: d.sw.is_some(),
            led: self.variant_sel(d).map_or(1, |c| c as i32 + 1),
            hue: d.hue,
            // silkscreen colour: dark on bright enclosures, cream on dark ones
            ink: ink_for(d.hue),
            knobs: ModelRc::new(VecModel::from(knobs)),
            movable: chain::movable(n),
            is_amp: n == Node::Amp,
        };
        (p, refs)
    }

    fn refresh_rig(&mut self) {
        let rt = self.routing();
        self.layout = rt.layout();
        let (rows, refs): (Vec<Pedal>, Vec<Vec<ParamRef>>) = self.layout.iter().map(|n| self.pedal_row(*n)).unzip();
        self.pedals.set_vec(rows);
        self.pedal_refs = refs;
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
    fn refresh_pedal(&mut self, n: Node) {
        if let Some(i) = self.layout.iter().position(|x| *x == n) {
            let (row, refs) = self.pedal_row(n);
            self.pedals.set_row_data(i, row);
            self.pedal_refs[i] = refs;
        }
        self.refresh_fx_count();
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
        ui.set_pedal_selected(self.layout.iter().position(|x| *x == n).map_or(-1, |i| i as i32));
        ui.set_insp_title(s(nice(d.title)));
        ui.set_insp_subtitle(s(nice(&self.type_name(d))));
        let hue = if n == Node::Amp { slint::Color::from_rgb_u8(0x8a, 0x6c, 0x34) } else { d.hue };
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
        const COLS: i32 = 6;
        const CELL_H: f32 = 122.0;
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
                name: s(nice(r.param.name)),
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
                self.toast(format!("Łańcuch: {}", PATTERN_NAMES[r2.pattern as usize]), false);
            }
            Some(_) => {}
            None => {
                if gap != from && gap != from + 1 {
                    self.toast("Tej kolejności Katana nie obsługuje", true);
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
        self.connected && self.sel.is_some() && self.live == self.sel
    }

    fn after_edit(&mut self) {
        let ui = self.ui();
        ui.set_ed_dirty(self.edit != self.saved);
        if let Some(id) = self.sel.clone() {
            self.refresh_row(&id);
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
            "Nie widzę zamontowanej partycji Windows z BOSS Tone Studio. Zamontuj ją (np. udisksctl mount -b /dev/nvme0n1p3) albo wskaż plik .tsl / folder poniżej.".to_string()
        } else {
            format!(
                "Znaleziono bibliotekę BOSS Tone Studio:\n{}\nImport jest idempotentny — duplikaty są scalane, ponowny import niczego nie podwaja.",
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
                ui.set_conn_text(s("połączony przez USB"));
                self.toast("Połączono z KATANA MkII", false);
            }
            Event::Disconnected(why) => {
                self.connected = false;
                let ui = self.ui();
                ui.set_connected(false);
                ui.set_conn_text(s("niepodłączony"));
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
                                self.toast(format!("Kopia {label} nieudana: {err} — zapis przerwany"), true);
                                return;
                            }
                        }
                        self.toast(format!("Kopia {label} zrobiona. Zapisuję „{wname}”…"), false);
                        self.dev.send(Cmd::WriteSlot { slot, patch: Box::new(wp) });
                        return;
                    }
                    self.pending_write = Some((wslot, wp, wname));
                }
                match self.lib.add_patch(*p, &format!("AMP · {label}"), "") {
                    Ok((id, new)) => {
                        self.toast(if new { format!("Kanał {label} dodany do biblioteki") } else { format!("Kanał {label} już jest w bibliotece") }, false);
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
                match self.lib.add_setlist(&format!("Wzmacniacz · {stamp}"), SetlistKind::Backup, ids) {
                    Ok(id) => {
                        self.nav_sel = id;
                        self.refresh_nav();
                        self.refresh_list();
                        self.toast(format!("Kopia 9 kanałów zapisana ({new} nowych brzmień)"), false);
                    }
                    Err(err) => self.toast(err.to_string(), true),
                }
            }
            Event::Written { slot, verified } => {
                self.busy("", -1.0);
                let label = SLOT_LABELS[slot as usize];
                if verified {
                    self.toast(format!("Zapisano w {label} i zweryfikowano odczytem ✓"), false);
                } else {
                    self.toast(format!("Zapis w {label}: odczyt kontrolny się różni!"), true);
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
                self.toast(format!("Import: {} setlist, {} patchy, {} nowych brzmień, {} duplikatów scalonych", r.setlists, r.patches, r.new_tones, r.duplicates), false);
                self.refresh_nav();
                self.refresh_list();
            }
            Err(e) => self.toast(format!("Import nieudany: {e}"), true),
        }
    }

    fn export(&mut self, name: &str, ids: Vec<String>) {
        let dir = export_dir();
        let path = dir.join(format!("{}.tsl", safe_file_name(name)));
        let r = std::fs::create_dir_all(&dir).and_then(|_| self.lib.export_tsl(name, &ids, &path));
        match r {
            Ok(()) => self.toast(format!("Zapisano {}", path.display()), false),
            Err(e) => self.toast(format!("Eksport nieudany: {e}"), true),
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
fn ink_for(hue: slint::Color) -> slint::Color {
    if 0.299 * hue.red() as f32 + 0.587 * hue.green() as f32 + 0.114 * hue.blue() as f32 > 140.0 {
        slint::Color::from_argb_u8(235, 18, 14, 10)
    } else {
        slint::Color::from_argb_u8(240, 244, 236, 220)
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
    // gains in dB get a sign
    if d.prm.contains("GAIN") || d.prm.contains("_GEQ_") || d.prm == "PRM_EQ_LEVEL" {
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
    ui.on_knob_changed(|i, v| with_app(|a| {
        let Some(k) = a.panel.knobs.get(i as usize) else { return };
        let r = k.r;
        if a.edit.get(r) == v as i16 {
            return;
        }
        a.set_param(r, v as i16);
        // update only the touched knob model row — no full editor rebuild per drag step
        let ui = a.ui();
        let m = ui.get_knobs();
        if let Some(mut row) = m.row_data(i as usize) {
            row.value = v;
            row.display = s(v.to_string());
            m.set_row_data(i as usize, row);
        }
        a.after_edit();
    }));
    ui.on_amp_family(|f| with_app(|a| {
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
        if a.insp == Some(n) {
            a.insp = None;
        } else {
            a.insp = Some(n);
            a.insp_section = 0;
        }
        a.refresh_inspector();
    }));
    ui.on_pedal_toggled(|i| with_app(|a| {
        let Some(n) = a.layout.get(i as usize).copied() else { return };
        let on = a.node_on(a.panel.node(n));
        a.set_node_on(n, !on);
    }));
    ui.on_pedal_knob(|i, k, v| with_app(|a| {
        let (i, k) = (i as usize, k as usize);
        let Some(r) = a.pedal_refs.get(i).and_then(|x| x.get(k)).copied() else { return };
        if a.edit.get(r) == v as i16 {
            return;
        }
        a.set_param(r, v as i16);
        let n = a.layout[i];
        a.refresh_pedal(n);
        if a.insp == Some(n) {
            a.refresh_inspector();
        }
        a.after_edit();
    }));
    ui.on_pedal_drag_start(|i| with_app(|a| {
        let ok = a.routing().valid_gaps(i as usize);
        a.ui().set_drop_ok(ModelRc::new(VecModel::from(ok)));
    }));
    ui.on_pedal_dropped(|from, gap| {
        // defer: the pedal component that fired this is about to be rebuilt
        Timer::single_shot(Duration::ZERO, move || with_app(|a| a.drop_pedal(from as usize, gap as usize)));
    });
    ui.on_insp_close(|| with_app(|a| {
        a.insp = None;
        a.refresh_inspector();
    }));
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
        ui.set_popup_title(s(format!("{} · {}", group_of(r.param), r.param.name)));
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
            None => {}
        }
    }));
    ui.on_insp_type_request(|| with_app(|a| {
        let Some(n) = a.insp else { return };
        a.popup = Some(Popup::Type);
        let ui = a.ui();
        ui.set_popup_title(s(format!("{} — typ", a.panel.node(n).title)));
        ui.set_popup_options(ui.get_insp_types());
        ui.invoke_open_popup();
    }));
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
                a.toast("Zapisano w bibliotece", false);
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
                a.toast(format!("Utworzono „{name}”"), false);
            }
            Err(e) => a.toast(e.to_string(), true),
        }
    }));
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
                a.toast(format!("„{name}” przeniesione do kosza ({}/trash)", a.lib.root().display()), false);
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
        a.ui().set_conn_text(s("łączenie…"));
        a.dev.send(Cmd::Connect);
    }));
    ui.on_select_slot(|i| with_app(|a| a.dev.send(Cmd::SelectSlot(i as u8))));
    ui.on_pull_slot(|i| with_app(|a| a.dev.send(Cmd::ReadSlot(i as u8))));
    ui.on_backup_amp(|| with_app(|a| a.dev.send(Cmd::ReadAllSlots)));
    ui.on_import_path(|p| with_app(|a| a.import_path(&p)));
    ui.on_import_windows(|| with_app(|a| {
        let dirs = find_bts_dirs();
        if dirs.is_empty() {
            a.toast("Brak zamontowanej biblioteki BOSS Tone Studio", true);
            return;
        }
        for d in dirs {
            a.import_path(&d.to_string_lossy());
        }
    }));
}

fn audition(a: &mut App) {
    if !a.connected {
        a.toast("Wzmacniacz niepodłączony", true);
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
