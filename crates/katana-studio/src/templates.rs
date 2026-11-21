//! Starter rigs: ready-made chains (amp, effects, order) the user can drop into the library
//! with one click. Built through the same rules the editor follows (effect colour slots, panel
//! LEDs in the status block, amp family bytes), so a starter behaves like a tone saved on the amp.

use crate::panel::{Panel, STATUS};
use katana_core::chain::Node;
use katana_core::map::{Key, ParamRef, AMP_PANEL};
use katana_core::Patch;

pub struct Template {
    pub name: &'static str,
    pub style: &'static str,
    pub blurb: &'static str,
    /// card artwork colour
    pub hue: u32,
    build: fn(&mut Rig),
}

impl Template {
    pub fn build(&self, panel: &Panel) -> Patch {
        let mut r = Rig { p: Patch::default(), panel };
        r.p.set_name(self.name);
        // every switchable block starts off; the template turns on what it uses
        for d in &panel.nodes {
            if d.sw.is_some() {
                r.off(d.node);
            }
        }
        (self.build)(&mut r);
        r.p
    }
}

/// Patch builder used by the templates.
pub struct Rig<'a> {
    p: Patch,
    panel: &'a Panel,
}

impl Rig<'_> {
    fn prm(&mut self, block: &str, prm: &'static str, v: i16) {
        let r = ParamRef::find(block, Key::Prm(prm)).unwrap_or_else(|| panic!("{block}/{prm} missing"));
        self.p.set(r, v.clamp(r.param.min, r.param.max));
    }
    fn raw(&mut self, block: usize, off: usize, v: u8) {
        let mut b = self.p.block(block).to_vec();
        b[off] = v;
        self.p.set_block(block, &b);
    }

    /// Panel amp type (family 0..4 = Acoustic, Clean, Crunch, Lead, Brown) and its knobs:
    /// gain, bass, middle, treble, presence, volume.
    fn amp(&mut self, family: usize, variation: bool, k: [i16; 6]) {
        let (_, a, b) = AMP_PANEL[family];
        let at = self.panel.amp_type;
        self.p.set(at, if variation { b } else { a });
        self.raw(STATUS, 0, family as u8);
        self.raw(STATUS, 12, variation as u8);
        for (i, v) in k.iter().enumerate() {
            let r = self.panel.knobs[i].r;
            self.p.set(r, *v);
        }
    }

    fn off(&mut self, n: Node) {
        let d = self.panel.node(n);
        if let Some(sw) = d.sw {
            self.p.set(sw, 0);
        }
        if let Some(va) = d.variant {
            self.raw(STATUS, va.led, 0);
        }
    }

    /// Turn a block on with the given type (by its panel label), on the GREEN colour slot.
    fn on(&mut self, n: Node, ty: &str) {
        let d = self.panel.node(n);
        let (sw, tyr, va) = (d.sw, d.ty, d.variant);
        if let Some(tyr) = tyr {
            let v = tyr.param.labels.iter().find(|(_, l)| *l == ty).map(|(v, _)| *v).unwrap_or_else(|| panic!("type {ty} missing"));
            self.p.set(tyr, v);
            if let Some(va) = va {
                let rb = self.panel.routing_block;
                self.raw(rb, va.sel, 0);
                self.raw(rb, va.asgn, v as u8);
            }
        }
        if let Some(sw) = sw {
            self.p.set(sw, 1);
        }
        if let Some(va) = va {
            self.raw(STATUS, va.led, 1);
        }
    }

    fn booster(&mut self, ty: &str, drive: i16, tone: i16, level: i16) {
        self.on(Node::Booster, ty);
        self.prm("Patch_0", "PRM_ODDS_DRIVE", drive);
        self.prm("Patch_0", "PRM_ODDS_TONE", tone);
        self.prm("Patch_0", "PRM_ODDS_EFFECT_LEVEL", level);
    }
    fn delay(&mut self, ty: &str, ms: i16, feedback: i16, level: i16) {
        self.on(Node::Delay, ty);
        self.prm("Delay(1)", "PRM_DLY_COMMON_DLY_TIME", ms);
        self.prm("Delay(1)", "PRM_DLY_COMMON_FEEDBACK", feedback);
        self.prm("Delay(1)", "PRM_DLY_COMMON_EFFECT_LEVEL", level);
    }
    fn reverb(&mut self, ty: &str, time: i16, level: i16) {
        self.on(Node::Reverb, ty);
        self.prm("Patch_1", "PRM_REVERB_TIME", time);
        self.prm("Patch_1", "PRM_REVERB_EFFECT_LEVEL", level);
    }
    fn gate(&mut self, threshold: i16) {
        self.on(Node::Ns, "");
        self.prm("Patch_1", "PRM_NS1_THRESHOLD", threshold);
    }
    /// Parametric EQ: low cut (index), low-mid (freq index, Q index, dB), high-mid (same),
    /// high cut (index). Indexes as the Katana stores them (`eqcurve`).
    fn peq(&mut self, low_cut: i16, lo_mid: (i16, i16, i16), hi_mid: (i16, i16, i16), high_cut: i16) {
        self.on(Node::Eq, "PARAMETRIC EQ");
        for (prm, v) in [
            ("PRM_EQ_LOW_CUT", low_cut),
            ("PRM_EQ_LOWMID_FREQ", lo_mid.0),
            ("PRM_EQ_LOWMID_Q", lo_mid.1),
            ("PRM_EQ_LOWMID_GAIN", lo_mid.2),
            ("PRM_EQ_HIGHMID_FREQ", hi_mid.0),
            ("PRM_EQ_HIGHMID_Q", hi_mid.1),
            ("PRM_EQ_HIGHMID_GAIN", hi_mid.2),
            ("PRM_EQ_HIGH_CUT", high_cut),
        ] {
            self.prm("Patch_0", prm, v);
        }
    }
    /// Chain pattern 0..6 (CHAIN 1, 2-1, 3-1, 4-1, 2-2, 3-2, 4-2).
    fn chain(&mut self, pattern: u8) {
        let rb = self.panel.routing_block;
        self.raw(rb, 0, pattern);
    }
}

const ACOUSTIC: usize = 0;
const CLEAN: usize = 1;
const CRUNCH: usize = 2;
const LEAD: usize = 3;
const BROWN: usize = 4;

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "Glass Clean",
        style: "Clean",
        blurb: "Sparkling clean with light compression, chorus shimmer and a plate tail.",
        hue: 0x3FC8E0,
        build: |r| {
            r.amp(CLEAN, false, [30, 45, 55, 62, 58, 55]);
            r.on(Node::Fx, "COMP");
            r.on(Node::Mod, "CHORUS");
            r.delay("DIGITAL", 380, 18, 28);
            r.reverb("PLATE", 32, 35);
            r.chain(2);
        },
    },
    Template {
        name: "Blues Breaker",
        style: "Crunch",
        blurb: "Edge-of-breakup crunch pushed by a blues overdrive into spring reverb.",
        hue: 0x4C8DFF,
        build: |r| {
            r.amp(CRUNCH, false, [42, 50, 60, 55, 50, 55]);
            r.booster("BLUES DRIVE", 40, 0, 60);
            r.reverb("SPRING", 28, 30);
        },
    },
    Template {
        name: "Classic Rock",
        style: "Crunch",
        blurb: "Hot crunch with a T-Scream in front, analog slapback and a small room.",
        hue: 0xFF9F1C,
        build: |r| {
            r.amp(CRUNCH, true, [65, 55, 62, 58, 55, 52]);
            r.booster("T-SCREAM", 20, 10, 70);
            r.delay("ANALOG", 320, 18, 24);
            r.reverb("ROOM", 25, 25);
        },
    },
    Template {
        name: "80s Lead",
        style: "Lead",
        blurb: "Singing lead: Turbo OD, wide chorus, long digital repeats and hall.",
        hue: 0xA070FF,
        build: |r| {
            r.amp(LEAD, false, [75, 55, 58, 60, 62, 50]);
            r.booster("TURBO OD", 30, 5, 65);
            r.on(Node::Mod, "CHORUS");
            r.delay("DIGITAL", 450, 30, 34);
            r.reverb("HALL", 45, 35);
        },
    },
    Template {
        name: "Modern Metal",
        style: "Hi-Gain",
        blurb: "Tight high gain: clean boost style screamer, gate and a dry room.",
        hue: 0xFF3B4E,
        build: |r| {
            r.amp(BROWN, true, [85, 58, 40, 62, 60, 50]);
            r.booster("T-SCREAM", 0, 20, 80);
            r.gate(45);
            r.reverb("ROOM", 18, 14);
        },
    },
    Template {
        name: "Ambient Swell",
        style: "Ambient",
        blurb: "Volume swells into modulated repeats and a long hall wash.",
        hue: 0x2FD69A,
        build: |r| {
            r.amp(CLEAN, true, [35, 50, 50, 55, 50, 52]);
            r.on(Node::Mod, "SLOW GEAR");
            r.delay("MODULATE", 600, 55, 50);
            r.reverb("HALL", 80, 55);
        },
    },
    Template {
        name: "Funk Rhythm",
        style: "Clean",
        blurb: "Snappy clean with touch wah and compression for tight rhythm work.",
        hue: 0xFFC23A,
        build: |r| {
            r.amp(CLEAN, false, [25, 45, 58, 62, 60, 55]);
            r.on(Node::Mod, "T.WAH");
            r.on(Node::Fx, "COMP");
            r.reverb("ROOM", 15, 18);
            r.chain(4);
        },
    },
    Template {
        name: "Surf Spring",
        style: "Clean",
        blurb: "Bright clean, tremolo, tape slap and a drenched spring tank.",
        hue: 0x00E5FF,
        build: |r| {
            r.amp(CLEAN, false, [35, 48, 52, 66, 60, 55]);
            r.on(Node::Mod, "TREMOLO");
            r.delay("TAPE ECHO", 140, 10, 30);
            r.reverb("SPRING", 60, 55);
        },
    },
    Template {
        name: "Stoner Fuzz",
        style: "Hi-Gain",
        blurb: "Thick fuzz into a brown amp with dark analog echoes.",
        hue: 0xD8742A,
        build: |r| {
            r.amp(BROWN, false, [55, 65, 45, 50, 45, 52]);
            r.booster("MUFF FUZZ", 70, -5, 55);
            r.delay("ANALOG", 520, 35, 25);
            r.reverb("ROOM", 30, 22);
        },
    },
    Template {
        name: "Acoustic Stage",
        style: "Acoustic",
        blurb: "Acoustic simulation with gentle compression, chorus and a hall.",
        hue: 0xB9B2A4,
        build: |r| {
            r.amp(ACOUSTIC, false, [30, 45, 50, 55, 50, 55]);
            r.on(Node::Fx, "COMP");
            r.on(Node::Mod, "CHORUS");
            r.reverb("HALL", 35, 30);
        },
    },
    Template {
        name: "November Rain",
        style: "Lead",
        blurb: "Slash live: Les Paul neck pickup into a cranked Marshall, mids forward, eighth echo at 80 BPM, arena hall. Single coils? Switch FX on.",
        hue: 0xC8402A,
        build: |r| {
            // CHAIN 3-1: the guitar sim (FX) sits ahead of the amp, where pickups would
            r.chain(2);
            // JCM800 / Silver Jubilee lead channel: gain past noon, mids up, presence moderate
            r.amp(LEAD, false, [72, 52, 68, 54, 52, 50]);
            // a touch of mid push for the neck pickup's singing sustain, no extra fuzz
            r.booster("MID BOOST", 30, -5, 60);
            // humbucker neck pickup on a single-coil guitar: GUITAR SIM S->H, left off
            r.on(Node::Fx, "GUITAR SIM");
            r.prm("Fx(2)", "PRM_FX1_GTRSIM_TYPE", 0);
            r.prm("Fx(2)", "PRM_FX1_GTRSIM_LOW", 8);
            r.prm("Fx(2)", "PRM_FX1_GTRSIM_HIGH", -6);
            r.prm("Fx(2)", "PRM_FX1_GTRSIM_BODY", 60);
            r.prm("Fx(2)", "PRM_FX1_GTRSIM_LEVEL", 70);
            r.off(Node::Fx);
            // 80 Hz low cut, −3 dB of neck-pickup woof at 250 Hz, +3 dB vocal bite at 1.6 kHz,
            // fizz off above 6.3 kHz
            r.peq(7, (11, 1, -3), (19, 1, 3), 10);
            // EQ after the amp: it shapes the voice, not how hard the amp is driven
            let rb = r.panel.routing_block;
            r.raw(rb, 2, 1);
            // the song runs at about 80 BPM: eighth-note echo, darkened repeats, low in the mix
            r.delay("DIGITAL", 375, 22, 28);
            r.prm("Delay(1)", "PRM_DLY_COMMON_HICUT", 9);
            // arena hall: 2.6 s, 40 ms pre-delay keeps the pick attack clear
            r.reverb("HALL", 26, 24);
            r.prm("Patch_1", "PRM_REVERB_PREDELAY", 40);
            r.prm("Patch_1", "PRM_REVERB_LOWCUT", 11);
            r.prm("Patch_1", "PRM_REVERB_HICUT", 9);
            // long notes must ring out: the gate only catches the hiss between phrases
            r.gate(12);
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use katana_core::chain::Routing;

    #[test]
    fn every_template_builds_with_consistent_slots() {
        let panel = Panel::new();
        for t in TEMPLATES {
            let p = t.build(&panel);
            assert_eq!(p.name().trim(), t.name);
            let rb = panel.routing_block;
            assert!(Routing::from_bytes(p.block(rb)).pattern <= 6);
            for d in &panel.nodes {
                if let (Some(sw), Some(va), Some(ty)) = (d.sw, d.variant, d.ty) {
                    let on = p.get(sw) != 0;
                    // DELAY and DELAY 2 share one panel LED: lit when either is on
                    let shared_on = panel.nodes.iter().any(|o| o.variant.is_some_and(|v| v.led == va.led) && o.sw.is_some_and(|s| p.get(s) != 0));
                    assert_eq!(p.block(STATUS)[va.led] != 0, shared_on, "{} {}: LED follows switch", t.name, d.title);
                    if on {
                        assert_eq!(p.block(rb)[va.asgn] as i16, p.get(ty), "{} {}: green slot holds the type", t.name, d.title);
                    }
                }
            }
        }
    }
}
