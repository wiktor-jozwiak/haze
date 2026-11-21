//! Practice page: its settings (`practice.conf`), the mixer and drum-grid models, and the
//! mixer meters' ballistics.
//!
//! Fader positions are 0..100 with gain (pos/71)², so 71 is 0 dB and 100 is +6 dB: the
//! square law gives the bottom of the throw the fine control a level fader needs. The drum grid
//! shows the groove that plays; only the four user grooves can be edited, and the one selected
//! is written into the DSP's user rows.

use crate::{DrumRow, MainWindow, MixCh};
use haze_dsp::practice::{self as dp, KIT_GROUPS, PATTERNS, USER, USER_ROWS};
use haze_dsp::{Meters, Params, P};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::path::Path;
use std::rc::Rc;

/// Mixer channels: guitar, loop, drum bus, then the kit's groups.
pub const CHANNELS: usize = 3 + KIT_GROUPS.len();
const BUS: [&str; 3] = ["Guitar", "Loop", "Drums"];
const KIT_P: [P; 6] = [P::DrumKickLv, P::DrumSnareLv, P::DrumHatLv, P::DrumCymLv, P::DrumTomLv, P::DrumClickLv];
/// fader position of 0 dB
pub const UNITY: i32 = 71;
pub const USER_SLOTS: usize = 4;

fn ch_param(i: usize) -> P {
    match i {
        0 => P::GuitarLevel,
        1 => P::LoopLevel,
        2 => P::DrumLevel,
        k => KIT_P[(k - 3).min(KIT_P.len() - 1)],
    }
}

/// Linear gain of a fader position.
pub fn gain(pos: i32) -> f32 {
    let x = pos.clamp(0, 100) as f32 / UNITY as f32;
    x * x
}

fn db_text(pos: i32) -> String {
    if pos <= 0 {
        "−∞".into()
    } else {
        let db = 20.0 * gain(pos).log10();
        if db.abs() < 0.05 {
            "0.0".into()
        } else {
            format!("{db:+.1}").replace('-', "−")
        }
    }
}

fn s(x: impl AsRef<str>) -> SharedString {
    SharedString::from(x.as_ref())
}

/// Practice settings: tempo, groove, metronome, looper sync, speed trainer, mixer, kit, grooves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PracticeCfg {
    pub bpm: i32,
    /// preset 0..`USER`-1, or `USER` for the user groove in `slot`
    pub pattern: i32,
    pub sig: i32,
    pub count_in: bool,
    pub sync: bool,
    pub trainer: bool,
    pub step: i32,
    pub bars: i32,
    pub target: i32,
    /// fader positions 0..100 and mutes, `CHANNELS` order
    pub mix: [i32; CHANNELS],
    pub mute: [bool; CHANNELS],
    /// semitones −6..6, decay %, swing %, humanise %, fills 0 off / 1 every 8 / 2 every 4 / 3 every 2
    pub tune: i32,
    pub decay: i32,
    pub swing: i32,
    pub human: i32,
    pub fills: i32,
    pub slot: usize,
    pub users: [[u32; 8]; USER_SLOTS],
    pub ulen: [i32; USER_SLOTS],
}

impl Default for PracticeCfg {
    fn default() -> Self {
        // U1 starts as the rock beat so the grid has something to edit
        let (rock, _) = dp::groove_rows(1, 0);
        PracticeCfg {
            bpm: 100,
            pattern: 0,
            sig: 0,
            count_in: true,
            sync: true,
            trainer: false,
            step: 5,
            bars: 4,
            target: 160,
            mix: [UNITY; CHANNELS],
            mute: [false; CHANNELS],
            tune: 0,
            decay: 100,
            swing: 0,
            human: 30,
            fills: 2,
            slot: 0,
            users: [rock, [0; 8], [0; 8], [0; 8]],
            ulen: [16; USER_SLOTS],
        }
    }
}

fn list<T: std::str::FromStr>(v: &str) -> impl Iterator<Item = T> + '_ {
    v.split(',').filter_map(|x| x.trim().parse().ok())
}

impl PracticeCfg {
    pub fn load(root: &Path) -> Self {
        let mut c = PracticeCfg::default();
        for l in std::fs::read_to_string(root.join("practice.conf")).unwrap_or_default().lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            let n = v.parse::<i32>().ok();
            match (k, n) {
                ("bpm", Some(n)) => c.bpm = n.clamp(40, 240),
                ("pattern", Some(n)) => c.pattern = n.clamp(0, USER as i32),
                ("sig", Some(n)) => c.sig = n.clamp(0, 2),
                ("countin", Some(n)) => c.count_in = n != 0,
                ("sync", Some(n)) => c.sync = n != 0,
                ("trainer", Some(n)) => c.trainer = n != 0,
                ("step", Some(n)) => c.step = n.clamp(1, 10),
                ("bars", Some(n)) => c.bars = n.clamp(1, 8),
                ("target", Some(n)) => c.target = n.clamp(40, 240),
                ("tune", Some(n)) => c.tune = n.clamp(-6, 6),
                ("decay", Some(n)) => c.decay = n.clamp(40, 200),
                ("swing", Some(n)) => c.swing = n.clamp(0, 100),
                ("human", Some(n)) => c.human = n.clamp(0, 100),
                ("fills", Some(n)) => c.fills = n.clamp(0, 3),
                ("slot", Some(n)) => c.slot = n.clamp(0, USER_SLOTS as i32 - 1) as usize,
                ("mix", _) => list::<i32>(v).take(CHANNELS).enumerate().for_each(|(i, x)| c.mix[i] = x.clamp(0, 100)),
                ("mute", _) => list::<i32>(v).take(CHANNELS).enumerate().for_each(|(i, x)| c.mute[i] = x != 0),
                ("ulen", _) => list::<i32>(v).take(USER_SLOTS).enumerate().for_each(|(i, x)| c.ulen[i] = if x <= 12 { 12 } else { 16 }),
                (k, _) if k.len() == 2 && k.starts_with('u') => {
                    if let Some(u) = k[1..].parse::<usize>().ok().filter(|u| *u < USER_SLOTS) {
                        let rows: Vec<u32> = v.split(',').filter_map(|x| u32::from_str_radix(x.trim(), 16).ok()).collect();
                        if rows.len() == 8 {
                            c.users[u].copy_from_slice(&rows);
                        }
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn save(&self, root: &Path) {
        let join = |it: &mut dyn Iterator<Item = String>| it.collect::<Vec<_>>().join(",");
        let mut txt = format!(
            "bpm={}\npattern={}\nsig={}\ncountin={}\nsync={}\ntrainer={}\nstep={}\nbars={}\ntarget={}\n",
            self.bpm, self.pattern, self.sig, self.count_in as i32, self.sync as i32, self.trainer as i32, self.step, self.bars, self.target
        );
        txt += &format!(
            "tune={}\ndecay={}\nswing={}\nhuman={}\nfills={}\nslot={}\n",
            self.tune, self.decay, self.swing, self.human, self.fills, self.slot
        );
        txt += &format!("mix={}\n", join(&mut self.mix.iter().map(|x| x.to_string())));
        txt += &format!("mute={}\n", join(&mut self.mute.iter().map(|x| (*x as i32).to_string())));
        txt += &format!("ulen={}\n", join(&mut self.ulen.iter().map(|x| x.to_string())));
        for (u, rows) in self.users.iter().enumerate() {
            txt += &format!("u{u}={}\n", join(&mut rows.iter().map(|r| format!("{r:08x}"))));
        }
        let _ = std::fs::write(root.join("practice.conf"), txt);
    }

    pub fn user(&self) -> bool {
        self.pattern as usize >= USER
    }

    /// (steps in a bar, steps per beat lamp, steps per accent group)
    pub fn shape(&self) -> (usize, usize, usize) {
        dp::bar_shape(self.pattern as usize, self.sig as usize, self.ulen[self.slot] as usize)
    }

    /// The grid as it plays: packed rows and bar length.
    fn grid(&self) -> ([u32; 8], usize) {
        if self.user() {
            (self.users[self.slot], self.ulen[self.slot] as usize)
        } else {
            dp::groove_grid(self.pattern as usize, self.sig as usize)
        }
    }

    /// Click on a user cell: `v` < 0 cycles rest → normal → accent → ghost → rest.
    pub fn cell(&mut self, row: usize, step: usize, v: i32) {
        if !self.user() || row >= USER_ROWS.len() {
            return;
        }
        let r = &mut self.users[self.slot][row];
        let next = if v < 0 {
            match dp::row_cell(*r, step) {
                0 => 2,
                2 => 3,
                3 => 1,
                _ => 0,
            }
        } else {
            v.clamp(0, 3) as u32
        };
        *r = dp::row_set(*r, step, next);
    }

    /// Start editing the shown preset in the current user slot.
    pub fn copy_to_user(&mut self) {
        if !self.user() {
            let (rows, len) = dp::groove_rows(self.pattern as usize, self.sig as usize);
            self.users[self.slot] = rows;
            self.ulen[self.slot] = len as i32;
        }
        self.pattern = USER as i32;
    }

    /// Push every setting to the DSP parameters and the UI's plain properties.
    pub fn apply(&self, d: &Params, ui: &MainWindow) {
        for (k, v) in [
            (P::Bpm, self.bpm as f32),
            (P::DrumPattern, self.pattern as f32),
            (P::TimeSig, self.sig as f32),
            (P::CountIn, self.count_in as i32 as f32),
            (P::LoopSync, self.sync as i32 as f32),
            (P::TrainerOn, self.trainer as i32 as f32),
            (P::TrainerStep, self.step as f32),
            (P::TrainerBars, self.bars as f32),
            (P::TrainerTarget, self.target as f32),
            (P::DrumTune, self.tune as f32),
            (P::DrumDecay, self.decay as f32 / 100.0),
            (P::DrumSwing, self.swing as f32 / 100.0),
            (P::DrumHuman, self.human as f32 / 100.0),
            (P::DrumFills, self.fills as f32),
            (P::DrumUserLen, self.ulen[self.slot] as f32),
        ] {
            d.set(k, v);
        }
        for i in 0..CHANNELS {
            d.set(ch_param(i), if self.mute[i] { 0.0 } else { gain(self.mix[i]) });
        }
        for (r, row) in self.users[self.slot].iter().enumerate() {
            d.set_raw(P::DrumUser0, r, *row);
        }
        let (n, beat, group) = self.shape();
        ui.set_bpm(self.bpm);
        ui.set_drum_pattern(self.pattern);
        ui.set_time_sig(self.sig);
        ui.set_bar_len(n as i32);
        ui.set_beat_steps(beat as i32);
        ui.set_bar_group(group as i32);
        ui.set_count_in(self.count_in);
        ui.set_loop_sync(self.sync);
        ui.set_trainer_on(self.trainer);
        ui.set_trainer_step(self.step);
        ui.set_trainer_bars(self.bars);
        ui.set_trainer_target(self.target);
        ui.set_user_slot(self.slot as i32);
        ui.set_user_len(self.ulen[self.slot]);
        ui.set_kit_tune(self.tune);
        ui.set_kit_decay(self.decay);
        ui.set_kit_swing(self.swing);
        ui.set_kit_human(self.human);
        ui.set_kit_fills(self.fills);
    }
}

/// The page's models: mixer strips and drum grid rows, plus meter state.
pub struct PracticeUi {
    bus: Rc<VecModel<MixCh>>,
    kit: Rc<VecModel<MixCh>>,
    rows: Rc<VecModel<DrumRow>>,
    /// meter positions as shown (guitar, loop, drums), for the fall-back ballistics
    shown: [f32; 3],
}

impl PracticeUi {
    pub fn new(ui: &MainWindow) -> Self {
        let ch = |name: &str, meter: bool| MixCh { name: s(name), pos: 0.71, db: s("0.0"), peak: 0.0, mute: false, meter };
        let bus = Rc::new(VecModel::from(BUS.iter().map(|n| ch(n, true)).collect::<Vec<_>>()));
        let kit = Rc::new(VecModel::from(KIT_GROUPS.iter().map(|n| ch(n, false)).collect::<Vec<_>>()));
        let rows = Rc::new(VecModel::from(
            USER_ROWS.iter().map(|n| DrumRow { name: s(n), cells: ModelRc::new(VecModel::from(vec![0; 16])) }).collect::<Vec<_>>(),
        ));
        let names = |r: std::ops::Range<usize>| ModelRc::new(VecModel::from(PATTERNS[r].iter().map(|p| s(p)).collect::<Vec<_>>()));
        ui.set_groove_names(names(0..8));
        ui.set_groove_names_b(names(8..PATTERNS.len()));
        ui.set_mix_bus(ModelRc::from(bus.clone()));
        ui.set_mix_kit(ModelRc::from(kit.clone()));
        ui.set_drum_rows(ModelRc::from(rows.clone()));
        PracticeUi { bus, kit, rows, shown: [0.0; 3] }
    }

    /// Fader positions, dB texts, mutes and the grid of the groove that plays.
    pub fn show(&self, c: &PracticeCfg) {
        for i in 0..CHANNELS {
            let (m, k) = if i < 3 { (&self.bus, i) } else { (&self.kit, i - 3) };
            let Some(mut row) = m.row_data(k) else { continue };
            let (pos, db) = (c.mix[i] as f32 / 100.0, s(db_text(c.mix[i])));
            if row.pos != pos || row.db != db || row.mute != c.mute[i] {
                (row.pos, row.db, row.mute) = (pos, db, c.mute[i]);
                m.set_row_data(k, row);
            }
        }
        let (grid, len) = c.grid();
        for (r, bits) in grid.iter().enumerate() {
            let Some(row) = self.rows.row_data(r) else { continue };
            let cells: Vec<i32> = (0..len).map(|st| dp::row_cell(*bits, st) as i32).collect();
            let same = row.cells.row_count() == cells.len() && cells.iter().enumerate().all(|(i, v)| row.cells.row_data(i) == Some(*v));
            if !same {
                self.rows.set_row_data(r, DrumRow { name: row.name, cells: ModelRc::new(VecModel::from(cells)) });
            }
        }
    }

    /// Mixer meters at 30 Hz: instant rise, ~300 ms fall, writes only when a bar moves. The
    /// peaks are always taken (so they do not pile up), drawn only while the page shows.
    pub fn meters(&mut self, m: &Meters, visible: bool) {
        let peaks = [Meters::take_peak(&m.gtr_peak), Meters::take_peak(&m.loop_peak), Meters::take_peak(&m.drum_peak)];
        if !visible {
            self.shown = [0.0; 3];
            return;
        }
        for (i, p) in peaks.into_iter().enumerate() {
            let pos = if p > 1e-6 { ((20.0 * p.log10() + 60.0) / 60.0).clamp(0.0, 1.0) } else { 0.0 };
            let cur = self.shown[i];
            let next = if pos > cur { pos } else if cur < 0.01 { 0.0 } else { (cur - 0.025).max(pos) };
            if (next - cur).abs() > 0.004 || (next == 0.0 && cur != 0.0) {
                self.shown[i] = next;
                if let Some(mut row) = self.bus.row_data(i) {
                    row.peak = next;
                    self.bus.set_row_data(i, row);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_law() {
        assert!((20.0 * gain(UNITY).log10()).abs() < 0.05, "71 is 0 dB");
        assert!((20.0 * gain(100).log10() - 5.95).abs() < 0.05, "full throw +6 dB");
        assert_eq!(gain(0), 0.0);
        assert_eq!(db_text(0), "−∞");
        assert_eq!(db_text(UNITY), "0.0");
        assert_eq!(db_text(50), "−6.1", "square law: half travel is −6 dB");
    }

    #[test]
    fn conf_round_trip() {
        let root = std::env::temp_dir().join(format!("haze-practice-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut c = PracticeCfg::default();
        c.pattern = USER as i32;
        c.slot = 2;
        c.mix[4] = 33;
        c.mute[0] = true;
        c.tune = -3;
        c.fills = 0;
        c.ulen[2] = 12;
        c.cell(0, 5, -1);
        c.cell(0, 5, -1);
        assert_eq!(dp::row_cell(c.users[2][0], 5), 3, "rest → normal → accent");
        c.save(&root);
        assert_eq!(PracticeCfg::load(&root), c);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn editing_a_preset_copies_it_first() {
        let mut c = PracticeCfg { pattern: 8, ..PracticeCfg::default() };
        c.cell(0, 0, 0);
        assert_eq!(c.pattern, 8, "presets are read-only");
        c.slot = 3;
        c.copy_to_user();
        assert!(c.user());
        assert_eq!(c.users[3], dp::groove_rows(8, 0).0);
        // disco: kick on every beat
        assert!((0..16).step_by(4).all(|s| dp::row_cell(c.users[3][0], s) > 0));
    }
}
