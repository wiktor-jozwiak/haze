//! Editor model: amp-head knobs, pedal (chain node) definitions and inspector sections.
//! Everything is resolved against the generated parameter map once, at startup.

use katana_core::chain::Node;
use katana_core::map::{fx_group_types, Key, ParamDef, ParamRef, PATCH_BLOCKS};
use katana_core::patch::{blk, Patch};
use slint::Color;

pub struct KnobDef {
    pub label: &'static str,
    pub r: ParamRef,
}

/// A parameter range of one block shown in the inspector.
#[derive(Clone)]
pub struct Section {
    pub name: &'static str,
    pub block: usize,
    pub idx: std::ops::Range<u16>,
    pub filter: Filter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    None,
    /// MOD/FX: only params of the current FX TYPE
    FxType,
    /// PEDAL FX: only params of the current pedal type
    PedalType,
    /// EQ: parametric bands for PARAMETRIC EQ, graphic bands for GE-10
    EqType,
}

/// Three colour slots (GREEN/RED/YELLOW) of a panel effect knob.
#[derive(Clone, Copy)]
pub struct Variant {
    /// Patch_2 offset of the GREEN type assignment (RED = +1, YELLOW = +2)
    pub asgn: usize,
    /// Patch_2 offset of the selected colour
    pub sel: usize,
    /// Status offset of the panel LED
    pub led: usize,
}

pub struct NodeDef {
    pub node: Node,
    pub title: &'static str,
    pub hue: Color,
    pub sw: Option<ParamRef>,
    pub ty: Option<ParamRef>,
    pub variant: Option<Variant>,
    pub sections: Vec<Section>,
    /// preferred knobs on the pedal face (by param name), max 3; the rest is filled in
    /// from the first numeric params of the block's current type
    pub knob_names: &'static [&'static str],
}

pub struct Panel {
    pub knobs: Vec<KnobDef>,
    pub amp_type: ParamRef,
    pub nodes: Vec<NodeDef>,
    pub routing_block: usize,
}

fn p(block: &str, prm: &'static str) -> ParamRef {
    ParamRef::find(block, Key::Prm(prm)).unwrap_or_else(|| panic!("param {block}/{prm} missing from map"))
}
fn bi(k: &str) -> usize {
    PATCH_BLOCKS.iter().position(|x| x.key == k).unwrap()
}
fn sec(name: &'static str, block: &str, idx: std::ops::Range<u16>, filter: Filter) -> Section {
    Section { name, block: bi(block), idx, filter }
}
const fn rgb(c: u32) -> Color {
    Color::from_argb_u8(255, (c >> 16) as u8, (c >> 8) as u8, c as u8)
}

impl Panel {
    pub fn new() -> Panel {
        let knobs = vec![
            KnobDef { label: "GAIN", r: p("Patch_0", "PRM_PREAMP_A_GAIN") },
            KnobDef { label: "BASS", r: p("Patch_0", "PRM_PREAMP_A_BASS") },
            KnobDef { label: "MIDDLE", r: p("Patch_0", "PRM_PREAMP_A_MIDDLE") },
            KnobDef { label: "TREBLE", r: p("Patch_0", "PRM_PREAMP_A_TREBLE") },
            KnobDef { label: "PRESENCE", r: p("Patch_0", "PRM_PREAMP_A_PRESENCE") },
            KnobDef { label: "VOLUME", r: p("Patch_0", "PRM_PREAMP_A_LEVEL") },
        ];
        use Node::*;
        let nodes = vec![
            NodeDef {
                node: Ns, title: "GATE", hue: rgb(0x5d6166),
                sw: Some(p("Patch_1", "PRM_NS1_SW")), ty: None, variant: None,
                sections: vec![sec("NOISE SUPPRESSOR", "Patch_1", 34..38, Filter::None)],
                knob_names: &["THRESHOLD", "RELEASE"],
            },
            NodeDef {
                node: PedalFx, title: "PEDAL FX", hue: rgb(0xa8322f),
                sw: Some(p("Patch_1", "PRM_PEDAL_FX_SW")), ty: Some(p("Patch_1", "PRM_PEDAL_FX_TYPE")), variant: None,
                sections: vec![sec("PEDAL FX", "Patch_1", 12..30, Filter::PedalType)],
                knob_names: &["PEDAL POS", "EFFECT LEVEL", "PITCH"],
            },
            NodeDef {
                node: Booster, title: "BOOSTER", hue: rgb(0xd8742a),
                sw: Some(p("Patch_0", "PRM_ODDS_SW")), ty: Some(p("Patch_0", "PRM_ODDS_TYPE")),
                variant: Some(Variant { asgn: 4, sel: 25, led: 13 }),
                sections: vec![sec("BOOSTER", "Patch_0", 0..15, Filter::None)],
                knob_names: &["DRIVE", "TONE", "EFFECT LEVEL"],
            },
            NodeDef {
                node: Mod, title: "MOD", hue: rgb(0x3474cf),
                sw: Some(p("Fx(1)", "PRM_FX1_SW")), ty: Some(p("Fx(1)", "PRM_FX1_FXTYPE")),
                variant: Some(Variant { asgn: 7, sel: 26, led: 14 }),
                sections: vec![sec("MOD", "Fx(1)", 0..999, Filter::FxType)],
                knob_names: &[],
            },
            NodeDef {
                node: Eq, title: "EQ", hue: rgb(0xb9b2a4),
                sw: Some(p("Patch_0", "PRM_EQ_SW")), ty: Some(p("Patch_0", "PRM_EQ_TYPE")), variant: None,
                sections: vec![sec("EQ", "Patch_0", 48..999, Filter::EqType), sec("EQ 2", "Eq(2)", 0..999, Filter::EqType)],
                knob_names: &["LOW GAIN", "HIGH GAIN", "LEVEL"],
            },
            NodeDef {
                node: Amp, title: "AMP", hue: rgb(0x1a1714),
                sw: None, ty: None, variant: None,
                sections: vec![
                    sec("AMP", "Patch_0", 16..48, Filter::None),
                    sec("SOLO", "Patch_Mk2V2", 0..999, Filter::None),
                    sec("CONTOUR", "Contour(1)", 0..999, Filter::None),
                    sec("GLOBAL", "Patch_1", 38..999, Filter::None),
                ],
                knob_names: &[],
            },
            NodeDef {
                // FX LOOP: the Katana's SEND/RETURN jacks
                node: SendReturn, title: "FX LOOP", hue: rgb(0x46505c),
                sw: Some(p("Patch_1", "PRM_SEND_RETURN_SW")), ty: Some(p("Patch_1", "PRM_SEND_RETURN_MODE")), variant: None,
                sections: vec![sec("SEND/RETURN", "Patch_1", 30..34, Filter::None)],
                knob_names: &["SEND LEVEL", "RETURN LEVEL"],
            },
            NodeDef {
                node: Fx, title: "FX", hue: rgb(0x8250cc),
                sw: Some(p("Fx(2)", "PRM_FX1_SW")), ty: Some(p("Fx(2)", "PRM_FX1_FXTYPE")),
                variant: Some(Variant { asgn: 10, sel: 27, led: 15 }),
                sections: vec![sec("FX", "Fx(2)", 0..999, Filter::FxType)],
                knob_names: &[],
            },
            NodeDef {
                node: Delay, title: "DELAY", hue: rgb(0x2a9a70),
                sw: Some(p("Delay(1)", "PRM_DLY_SW")), ty: Some(p("Delay(1)", "PRM_DLY_TYPE")),
                variant: Some(Variant { asgn: 13, sel: 28, led: 16 }),
                sections: vec![sec("DELAY", "Delay(1)", 0..999, Filter::None)],
                knob_names: &["DELAY TIME", "FEEDBACK", "EFFECT LEVEL"],
            },
            NodeDef {
                node: Delay2, title: "DELAY 2", hue: rgb(0x1f7660),
                sw: Some(p("Delay(2)", "PRM_DLY_SW")), ty: Some(p("Delay(2)", "PRM_DLY_TYPE")),
                variant: Some(Variant { asgn: 19, sel: 28, led: 16 }),
                sections: vec![sec("DELAY 2", "Delay(2)", 0..999, Filter::None)],
                knob_names: &["DELAY TIME", "FEEDBACK", "EFFECT LEVEL"],
            },
            NodeDef {
                node: Reverb, title: "REVERB", hue: rgb(0x2f9db3),
                sw: Some(p("Patch_1", "PRM_REVERB_SW")), ty: Some(p("Patch_1", "PRM_REVERB_TYPE")),
                variant: Some(Variant { asgn: 16, sel: 29, led: 17 }),
                sections: vec![sec("REVERB", "Patch_1", 0..12, Filter::None)],
                knob_names: &["REVERB TIME", "EFFECT LEVEL", "PRE DELAY"],
            },
        ];
        Panel { knobs, amp_type: p("Patch_0", "PRM_PREAMP_A_TYPE"), nodes, routing_block: bi("Patch_2") }
    }

    /// Up to three params for the pedal face.
    pub fn pedal_knobs(&self, d: &NodeDef, patch: &Patch) -> Vec<ParamRef> {
        if d.node == Node::Amp {
            // amp head: gain, middle, volume
            return [0, 2, 5].iter().map(|&i| self.knobs[i].r).collect();
        }
        let all = self.section_params(&d.sections[0], patch);
        let numeric: Vec<ParamRef> =
            all.into_iter().filter(|r| r.param.labels.is_empty() && r.param.max - r.param.min >= 10 && r.param.name != "SW").collect();
        let mut out: Vec<ParamRef> = vec![];
        for name in d.knob_names {
            if let Some(r) = numeric.iter().find(|r| r.param.name == *name && !out.iter().any(|o| std::ptr::eq(o.param, r.param))) {
                out.push(*r);
            }
        }
        for r in &numeric {
            if out.len() >= 3 {
                break;
            }
            if !out.iter().any(|o| std::ptr::eq(o.param, r.param)) && r.param.name != "DIRECT MIX" {
                out.push(*r);
            }
        }
        out.truncate(3);
        out
    }

    pub fn node(&self, n: Node) -> &NodeDef {
        self.nodes.iter().find(|d| d.node == n).expect("node def")
    }

    /// Editable, named params of a section for the given patch.
    pub fn section_params(&self, s: &Section, patch: &Patch) -> Vec<ParamRef> {
        let ty = |prm: &str| {
            PATCH_BLOCKS[s.block].params.iter().find(|d| d.prm == prm).map(|d| patch.get(ParamRef { block: s.block, param: d })).unwrap_or(0)
        };
        let fx_type = ty("PRM_FX1_FXTYPE");
        let pedal = ty("PRM_PEDAL_FX_TYPE");
        let eq = ty("PRM_EQ_TYPE");
        PATCH_BLOCKS[s.block]
            .params
            .iter()
            .filter(|d| s.idx.contains(&d.idx) && !d.name.is_empty() && d.min < d.max)
            .filter(|d| match s.filter {
                Filter::None => true,
                Filter::FxType => fx_group_types(d.prm).map_or(true, |t| t.contains(&fx_type)),
                // GE-10 owns PRM_EQ_GEQ_*, the parametric EQ every other PRM_EQ_* band
                Filter::EqType => d.prm.contains("_GEQ_") == (eq == 1),
                Filter::PedalType => {
                    let g = if d.prm.contains("_WAH_") { Some(0) } else if d.prm.contains("_PEDAL_BEND_") { Some(1) } else if d.prm.contains("_EVH95_") { Some(2) } else { None };
                    g.map_or(true, |g| g == pedal)
                }
            })
            .filter(|d| !d.prm.ends_with("_SW") || !matches!(s.filter, Filter::FxType))
            .map(|d: &'static ParamDef| ParamRef { block: s.block, param: d })
            .collect()
    }
}

/// Human group name from a BTS param id: PRM_PREAMP_A_GAIN → "AMP".
pub fn group_of(d: &ParamDef) -> String {
    let s = d.prm.strip_prefix("PRM_").unwrap_or(d.prm);
    let s = s.strip_prefix("FX1_").unwrap_or(s);
    let g = s.split('_').next().unwrap_or("");
    match g {
        "2" | "2x2CHORUS" => "2x2 CHORUS".into(),
        "EVH" => "MXR / EVH".into(),
        "ODDS" => "BOOSTER".into(),
        "DLY" => "DELAY".into(),
        "PREAMP" => "PREAMP".into(),
        "NS1" => "NOISE SUPPRESSOR".into(),
        other => other.replace("ADCOMP", "COMP").replace("ACPROCESS", "AC.PROCESSOR"),
    }
}

/// Short label under a pedal knob (fits ~30px at 9px).
pub fn short(name: &str) -> String {
    match name {
        "EFFECT LEVEL" | "LEVEL" => "Level".into(),
        "DELAY TIME" | "REVERB TIME" => "Time".into(),
        "PEDAL POS" => "Pedal".into(),
        "SEND LEVEL" => "Send".into(),
        "RETURN LEVEL" => "Return".into(),
        "THRESHOLD" => "Thresh".into(),
        "SUSTAIN" => "Sust".into(),
        "GAIN" => "Gain".into(),
        "MIDDLE" => "Mid".into(),
        "ATTACK" => "Attack".into(),
        "FEEDBACK" => "Fdbk".into(),
        "FREQUENCY" => "Freq".into(),
        "RELEASE" => "Release".into(),
        "PRE DELAY" => "Pre".into(),
        "LOW GAIN" => "Low".into(),
        "HIGH GAIN" => "High".into(),
        "LOW-MID GAIN" => "Lo-Mid".into(),
        "HIGH-MID GAIN" => "Hi-Mid".into(),
        "VOLUME" => "Vol".into(),
        other => {
            // multi-word names: the last word carries the meaning ("PEDAL MIN" → "Min")
            let w = other.split([' ', '_']).filter(|w| !w.is_empty()).last().unwrap_or(other);
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c.flat_map(|x| x.to_lowercase())).take(7).collect(),
                None => String::new(),
            }
        }
    }
}

pub const STATUS: usize = blk::STATUS;

#[cfg(test)]
mod tests {
    use super::*;
    use katana_core::Patch;

    fn names(p: &Panel, patch: &Patch) -> Vec<&'static str> {
        let d = p.node(Node::Eq);
        p.section_params(&d.sections[0], patch).iter().map(|r| r.param.name).collect()
    }

    #[test]
    fn eq_shows_only_bands_of_its_type() {
        let p = Panel::new();
        let mut patch = Patch::default();
        let ty = p.node(Node::Eq).ty.unwrap();

        patch.set(ty, 0);
        let peq = names(&p, &patch);
        assert!(peq.contains(&"LOW-MID FREQ") && peq.contains(&"HIGH CUT"));
        assert!(!peq.contains(&"31Hz"), "{peq:?}");

        patch.set(ty, 1);
        let geq = names(&p, &patch);
        assert!(geq.contains(&"31Hz") && geq.contains(&"16KHz"));
        assert!(!geq.contains(&"LOW-MID FREQ"), "{geq:?}");
        assert_eq!(geq.iter().filter(|n| **n == "LEVEL").count(), 1);
    }
}
