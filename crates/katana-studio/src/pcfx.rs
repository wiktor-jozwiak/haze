//! PC FX page: the cards, their controls' ranges and texts, and `pcfx.conf`.
//!
//! The settings are the app's, not a patch's: the PC effects sit after whatever patch plays,
//! like a pedalboard into the amp's return. Freeze and tape stop are latches and always start
//! released.

use std::path::Path;

use haze_dsp::{Params, P};
use std::rc::Rc;

use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::{PcCard, PcCtl};

const NOTES: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];
/// Harmony voices: label, scale steps.
const VOICES: [(&str, f32); 11] = [
    ("Off", 0.0),
    ("−Octave", -7.0),
    ("−6th", -5.0),
    ("−5th", -4.0),
    ("−4th", -3.0),
    ("−3rd", -2.0),
    ("+3rd", 2.0),
    ("+4th", 3.0),
    ("+5th", 4.0),
    ("+6th", 5.0),
    ("+Octave", 7.0),
];

#[derive(Clone, Copy)]
enum Fmt {
    Pct,
    Sec,
    Bits,
    Khz,
}

#[derive(Clone, Copy)]
enum Ctl {
    /// fader from `lo` to `hi`, exponential when `log`
    Fader { p: P, label: &'static str, lo: f32, hi: f32, log: bool, fmt: Fmt },
    /// segmented or stepper choice: options and the value each one stores
    Pick { p: P, label: &'static str, opts: &'static [(&'static str, f32)], stepper: bool },
}

struct CardDef {
    title: &'static str,
    blurb: &'static str,
    on: P,
    latch: Option<&'static str>,
    hue: u32,
    ctls: &'static [Ctl],
}

const KEYS: [(&str, f32); 12] = [
    ("C", 0.0),
    ("C♯", 1.0),
    ("D", 2.0),
    ("E♭", 3.0),
    ("E", 4.0),
    ("F", 5.0),
    ("F♯", 6.0),
    ("G", 7.0),
    ("A♭", 8.0),
    ("A", 9.0),
    ("B♭", 10.0),
    ("B", 11.0),
];

const CARDS: [CardDef; 6] = [
    CardDef {
        title: "Harmonizer",
        blurb: "Up to two voices that stay in key, following each note you play.",
        on: P::PcHarmOn,
        latch: None,
        hue: 0x7C6CF0,
        ctls: &[
            Ctl::Pick { p: P::PcHarmKey, label: "Key", opts: &KEYS, stepper: true },
            Ctl::Pick { p: P::PcHarmMinor, label: "Scale", opts: &[("Major", 0.0), ("Minor", 1.0)], stepper: false },
            Ctl::Pick { p: P::PcHarmV1, label: "Voice 1", opts: &VOICES_1, stepper: true },
            Ctl::Pick { p: P::PcHarmV2, label: "Voice 2", opts: &VOICES, stepper: true },
            Ctl::Fader { p: P::PcHarmMix, label: "Level", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
        ],
    },
    CardDef {
        title: "Rhythm",
        blurb: "Echoes, slices or tremolo on the beat of the practice drums.",
        on: P::PcRhyOn,
        latch: None,
        hue: 0xE8A33D,
        ctls: &[
            Ctl::Pick { p: P::PcRhyMode, label: "Mode", opts: &[("Multi-tap", 0.0), ("Slicer", 1.0), ("Tremolo", 2.0)], stepper: false },
            Ctl::Pick { p: P::PcRhyDiv, label: "Note", opts: &[("1/4", 0.0), ("1/8", 1.0), ("1/8 .", 2.0), ("1/16", 3.0), ("1/8 T", 4.0)], stepper: false },
            Ctl::Fader { p: P::PcRhyAmt, label: "Amount", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
            Ctl::Fader { p: P::PcRhyMix, label: "Mix", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
        ],
    },
    CardDef {
        title: "Shimmer",
        blurb: "A vast hall whose tail keeps climbing in pitch. Ambient pads from one chord.",
        on: P::PcShimOn,
        latch: None,
        hue: 0x4FC3E8,
        ctls: &[
            Ctl::Pick { p: P::PcShimPitch, label: "Interval", opts: &[("+Octave", 12.0), ("+5th", 7.0), ("+2 Oct", 24.0)], stepper: false },
            Ctl::Fader { p: P::PcShimDecay, label: "Decay", lo: 1.0, hi: 10.0, log: true, fmt: Fmt::Sec },
            Ctl::Fader { p: P::PcShimAmt, label: "Shimmer", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
            Ctl::Fader { p: P::PcShimMix, label: "Mix", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
        ],
    },
    CardDef {
        title: "Freeze",
        blurb: "Hit it while a chord rings: the moment holds as an endless pad to play over.",
        on: P::PcFrzOn,
        latch: Some("FREEZE"),
        hue: 0x8FD3FF,
        ctls: &[
            Ctl::Fader { p: P::PcFrzTexture, label: "Texture", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
            Ctl::Fader { p: P::PcFrzMix, label: "Level", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
        ],
    },
    CardDef {
        title: "Lo-fi",
        blurb: "Bit crusher, low sample rate and a worn cassette's wow and hiss.",
        on: P::PcLofiOn,
        latch: None,
        hue: 0xE0736B,
        ctls: &[
            Ctl::Fader { p: P::PcLofiBits, label: "Bits", lo: 16.0, hi: 3.0, log: false, fmt: Fmt::Bits },
            Ctl::Fader { p: P::PcLofiRate, label: "Rate", lo: 48000.0, hi: 1500.0, log: true, fmt: Fmt::Khz },
            Ctl::Fader { p: P::PcLofiWow, label: "Wow", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
            Ctl::Fader { p: P::PcLofiHiss, label: "Hiss", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
            Ctl::Fader { p: P::PcLofiMix, label: "Mix", lo: 0.0, hi: 1.0, log: false, fmt: Fmt::Pct },
        ],
    },
    CardDef {
        title: "Tape stop",
        blurb: "The tape winds down to a halt, everything with it. Hit again to start.",
        on: P::PcStopOn,
        latch: Some("STOP"),
        hue: 0xC9B07A,
        ctls: &[Ctl::Fader { p: P::PcStopTime, label: "Time", lo: 0.1, hi: 3.0, log: true, fmt: Fmt::Sec }],
    },
];

/// Voice 1 has no "Off": the card switch is its off.
const VOICES_1: [(&str, f32); 10] = [
    ("−Octave", -7.0),
    ("−6th", -5.0),
    ("−5th", -4.0),
    ("−4th", -3.0),
    ("−3rd", -2.0),
    ("+3rd", 2.0),
    ("+4th", 3.0),
    ("+5th", 4.0),
    ("+6th", 5.0),
    ("+Octave", 7.0),
];

fn ctl_param(c: &Ctl) -> P {
    match c {
        Ctl::Fader { p, .. } | Ctl::Pick { p, .. } => *p,
    }
}

/// Every persisted parameter: the switches (not the latches) and all controls.
fn saved() -> impl Iterator<Item = P> {
    CARDS.iter().flat_map(|c| c.latch.is_none().then_some(c.on).into_iter().chain(c.ctls.iter().map(ctl_param)))
}

fn name(p: P) -> String {
    format!("{p:?}")
}

/// Load `pcfx.conf` into the engine's parameters (unknown keys and bad values ignored).
pub fn load(root: &Path, d: &Params) {
    let text = std::fs::read_to_string(root.join("pcfx.conf")).unwrap_or_default();
    for l in text.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let Ok(v) = v.trim().parse::<f32>() else { continue };
        if let Some(p) = saved().find(|p| name(*p) == k.trim()) {
            if v.is_finite() {
                d.set(p, v);
            }
        }
    }
}

pub fn save(root: &Path, d: &Params) {
    let text: String = saved().map(|p| format!("{}={}\n", name(p), d.get(p))).collect();
    let _ = std::fs::write(root.join("pcfx.conf"), text);
}

fn to_norm(c: &Ctl, v: f32) -> f32 {
    match *c {
        Ctl::Fader { lo, hi, log: true, .. } => ((v / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0),
        Ctl::Fader { lo, hi, .. } => ((v - lo) / (hi - lo)).clamp(0.0, 1.0),
        Ctl::Pick { .. } => 0.0,
    }
}

fn from_norm(c: &Ctl, n: f32) -> f32 {
    match *c {
        Ctl::Fader { lo, hi, log: true, .. } => lo * (hi / lo).powf(n),
        Ctl::Fader { lo, hi, fmt: Fmt::Bits, .. } => (lo + (hi - lo) * n).round(),
        Ctl::Fader { lo, hi, .. } => lo + (hi - lo) * n,
        Ctl::Pick { .. } => 0.0,
    }
}

fn text(fmt: Fmt, v: f32) -> String {
    match fmt {
        Fmt::Pct => format!("{:.0} %", v * 100.0),
        Fmt::Sec => format!("{v:.1} s"),
        Fmt::Bits => format!("{v:.0} bit"),
        Fmt::Khz => format!("{:.1} kHz", v / 1000.0),
    }
}

fn ctl_row(k: &Ctl, d: &Params) -> PcCtl {
    match k {
        Ctl::Fader { p, label, fmt, .. } => {
            let x = d.get(*p);
            PcCtl { label: (*label).into(), kind: 0, value: to_norm(k, x), text: text(*fmt, x).into(), options: ModelRc::default(), index: 0 }
        }
        Ctl::Pick { p, label, opts, stepper } => {
            let x = d.get(*p);
            let index = opts.iter().position(|(_, o)| (o - x).abs() < 1e-3).unwrap_or(0);
            let o: Vec<SharedString> = opts.iter().map(|(n, _)| SharedString::from(*n)).collect();
            PcCtl { label: (*label).into(), kind: if *stepper { 2 } else { 1 }, value: 0.0, text: SharedString::default(), options: ModelRc::new(VecModel::from(o)), index: index as i32 }
        }
    }
}

fn blurb(c: &CardDef, d: &Params) -> String {
    if c.on == P::PcHarmOn {
        let key = NOTES[(d.get(P::PcHarmKey).max(0.0) as usize).min(11)];
        let scale = if d.on(P::PcHarmMinor) { "minor" } else { "major" };
        return format!("Up to two voices in {key} {scale}, following each note you play.");
    }
    c.blurb.to_string()
}

/// The page's model. Rows are updated in place: a fader being dragged keeps its element.
pub struct PcFxUi {
    cards: Rc<VecModel<PcCard>>,
    ctls: Vec<Rc<VecModel<PcCtl>>>,
}

impl PcFxUi {
    pub fn new(d: &Params) -> Self {
        let ctls: Vec<Rc<VecModel<PcCtl>>> = CARDS.iter().map(|c| Rc::new(VecModel::from(c.ctls.iter().map(|k| ctl_row(k, d)).collect::<Vec<_>>()))).collect();
        let cards = CARDS.iter().zip(&ctls).map(|(c, m)| Self::card_row(c, m, d)).collect::<Vec<_>>();
        PcFxUi { cards: Rc::new(VecModel::from(cards)), ctls }
    }

    fn card_row(c: &CardDef, m: &Rc<VecModel<PcCtl>>, d: &Params) -> PcCard {
        PcCard {
            title: c.title.into(),
            blurb: blurb(c, d).into(),
            on: d.on(c.on),
            latch: c.latch.is_some(),
            latch_text: c.latch.unwrap_or("").into(),
            hue: Color::from_rgb_u8((c.hue >> 16) as u8, (c.hue >> 8) as u8, c.hue as u8),
            ctls: ModelRc::from(m.clone()),
        }
    }

    pub fn model(&self) -> ModelRc<PcCard> {
        ModelRc::from(self.cards.clone())
    }

    /// Re-read the parameters; only rows that changed are written.
    pub fn refresh(&self, d: &Params) {
        for (i, (c, m)) in CARDS.iter().zip(&self.ctls).enumerate() {
            for (j, k) in c.ctls.iter().enumerate() {
                let row = ctl_row(k, d);
                let old = m.row_data(j);
                // a choice's option list never changes: compare without it
                if old.as_ref().is_none_or(|o| o.value != row.value || o.text != row.text || o.index != row.index) {
                    m.set_row_data(j, row);
                }
            }
            let row = Self::card_row(c, m, d);
            if self.cards.row_data(i).is_none_or(|o| o.on != row.on || o.blurb != row.blurb) {
                self.cards.set_row_data(i, row);
            }
        }
    }
}

/// Card switch / latch pressed. Returns whether the change is worth saving.
pub fn toggle(d: &Params, card: usize) -> bool {
    let Some(c) = CARDS.get(card) else { return false };
    d.set(c.on, if d.on(c.on) { 0.0 } else { 1.0 });
    c.latch.is_none()
}

/// Control `ctl` of `card` moved: `v` is 0..1 for a fader, the option index for a choice.
pub fn set(d: &Params, card: usize, ctl: usize, v: f32) {
    let Some(k) = CARDS.get(card).and_then(|c| c.ctls.get(ctl)) else { return };
    match k {
        Ctl::Fader { p, .. } => d.set(*p, from_norm(k, v)),
        Ctl::Pick { p, opts, .. } => {
            if let Some((_, x)) = opts.get(v.max(0.0) as usize) {
                d.set(*p, *x);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_round_trip_through_pcfx_conf() {
        let dir = std::env::temp_dir().join(format!("haze-pcfx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = Params::default();
        // shimmer on, decay fader at the top, rhythm on 1/16, voice 2 a 6th down
        assert!(toggle(&d, 2));
        set(&d, 2, 1, 1.0);
        set(&d, 1, 1, 3.0);
        set(&d, 0, 3, 2.0);
        // freeze is a latch: pressed, but never saved
        assert!(!toggle(&d, 3));
        save(&dir, &d);
        let e = Params::default();
        load(&dir, &e);
        assert!(e.on(P::PcShimOn));
        assert!((e.get(P::PcShimDecay) - 10.0).abs() < 1e-3);
        assert_eq!(e.get(P::PcRhyDiv), 3.0);
        assert_eq!(e.get(P::PcHarmV2), -5.0);
        assert!(!e.on(P::PcFrzOn));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn faders_map_both_ways() {
        for c in CARDS.iter().flat_map(|c| c.ctls.iter()) {
            if let Ctl::Fader { .. } = c {
                for n in [0.0, 0.25, 0.5, 1.0] {
                    let back = to_norm(c, from_norm(c, n));
                    // the bit-depth fader snaps to whole bits
                    assert!((back - n).abs() < 0.04, "{n} -> {back}");
                }
            }
        }
    }
}
