//! Signal chain of the KATANA MkII.
//!
//! The firmware does not allow free ordering: there are 7 chain patterns (`PRM_CHAIN_PTN`)
//! for BOOSTER / MOD / AMP / FX / DELAY, plus three position switches (EQ amp-in/out,
//! PEDAL FX input/post-amp, SEND/RETURN post-amp/post-reverb). DELAY 2 and REVERB are always last.
//! Order per pattern taken from the BTS chain diagrams.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Node {
    Ns,
    PedalFx,
    Booster,
    Mod,
    Eq,
    Amp,
    SendReturn,
    Fx,
    Delay,
    Delay2,
    Reverb,
}

use Node::*;

pub const PATTERNS: [[Node; 5]; 7] = [
    [Booster, Amp, Mod, Fx, Delay],  // CHAIN 1
    [Booster, Mod, Amp, Fx, Delay],  // CHAIN 2-1
    [Booster, Mod, Fx, Amp, Delay],  // CHAIN 3-1
    [Booster, Mod, Fx, Delay, Amp],  // CHAIN 4-1
    [Mod, Booster, Amp, Fx, Delay],  // CHAIN 2-2
    [Mod, Booster, Fx, Amp, Delay],  // CHAIN 3-2
    [Mod, Booster, Fx, Delay, Amp],  // CHAIN 4-2
];
pub const PATTERN_NAMES: [&str; 7] = ["CHAIN 1", "CHAIN 2-1", "CHAIN 3-1", "CHAIN 4-1", "CHAIN 2-2", "CHAIN 3-2", "CHAIN 4-2"];

/// Raw routing state as stored in the patch (Patch_2 bytes 0..=3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Routing {
    pub pattern: u8,
    /// PRM_POSITION_SEND_RETURN: false = POST AMP, true = POST REVERB
    pub sr_post_rev: bool,
    /// PRM_POSITION_EQ: false = AMP IN, true = AMP OUT
    pub eq_post: bool,
    /// PRM_POSITION_PEDAL_FX: false = INPUT, true = POST AMP
    pub pfx_post: bool,
}

impl Routing {
    pub fn from_bytes(b: &[u8]) -> Routing {
        Routing { pattern: b[0].min(6), sr_post_rev: b[1] != 0, eq_post: b[2] != 0, pfx_post: b[3] != 0 }
    }
    pub fn to_bytes(self) -> [u8; 4] {
        [self.pattern, self.sr_post_rev as u8, self.eq_post as u8, self.pfx_post as u8]
    }

    /// Left-to-right signal flow.
    pub fn layout(self) -> Vec<Node> {
        let mut v = vec![Ns];
        if !self.pfx_post {
            v.push(PedalFx);
        }
        for n in PATTERNS[self.pattern as usize] {
            if n == Amp {
                if !self.eq_post {
                    v.push(Eq);
                }
                v.push(Amp);
                if self.eq_post {
                    v.push(Eq);
                }
                if self.pfx_post {
                    v.push(PedalFx);
                }
                if !self.sr_post_rev {
                    v.push(SendReturn);
                }
            } else {
                v.push(n);
            }
        }
        v.extend([Delay2, Reverb]);
        if self.sr_post_rev {
            v.push(SendReturn);
        }
        v
    }

    /// Derive a routing from an arbitrary order, if the hardware can do it.
    pub fn from_order(order: &[Node]) -> Option<Routing> {
        let pos = |n: Node| order.iter().position(|x| *x == n);
        let core: Vec<Node> = order.iter().copied().filter(|n| matches!(n, Booster | Mod | Amp | Fx | Delay)).collect();
        let pattern = PATTERNS.iter().position(|p| p[..] == core[..])? as u8;
        let ia = pos(Amp)?;
        let anchors = |n: &Node| matches!(n, Booster | Mod | Fx | Delay | Delay2 | Reverb);
        let prev = order[..ia].iter().rposition(anchors).map(|i| i as isize).unwrap_or(-1);
        let next = order[ia + 1..].iter().position(anchors).map(|i| i + ia + 1).unwrap_or(order.len());
        let first_core = order.iter().position(|n| matches!(n, Booster | Mod | Amp | Fx | Delay))?;
        // DELAY 2 and REVERB never move; they must stay after every core block, in order.
        let (d2, rv) = (pos(Delay2)?, pos(Reverb)?);
        if !(core.iter().all(|n| pos(*n).unwrap() < d2) && d2 < rv) {
            return None;
        }
        let near_amp = |i: usize| -> Option<bool> {
            if (i as isize) > prev && i < ia {
                Some(false)
            } else if i > ia && i < next {
                Some(true)
            } else {
                None
            }
        };
        let eq_post = near_amp(pos(Eq)?)?;
        let ip = pos(PedalFx)?;
        let pfx_post = if ip < first_core { false } else { near_amp(ip).filter(|p| *p)? };
        let is = pos(SendReturn)?;
        let sr_post_rev = if is > rv { true } else { !near_amp(is).filter(|p| *p)? };
        Some(Routing { pattern, sr_post_rev, eq_post, pfx_post })
    }

    /// Move node `from` (index in `layout()`) into gap `gap` (0..=len). None = not possible on the amp.
    pub fn moved(self, from: usize, gap: usize) -> Option<Routing> {
        let mut v = self.layout();
        if from >= v.len() || gap > v.len() || !movable(v[from]) {
            return None;
        }
        let n = v.remove(from);
        let g = if gap > from { gap - 1 } else { gap };
        v.insert(g.min(v.len()), n);
        Routing::from_order(&v)
    }

    /// For a drag of `from`, which gaps are valid drop targets (and actually change something).
    pub fn valid_gaps(self, from: usize) -> Vec<bool> {
        let len = self.layout().len();
        (0..=len).map(|g| g != from && g != from + 1 && self.moved(from, g).is_some_and(|r| r != self)).collect()
    }
}

/// A named chain layout the user can apply in one click. Only the order changes: which
/// blocks are on and how they sound stays as it is.
pub struct ChainPreset {
    pub name: &'static str,
    pub blurb: &'static str,
    pub routing: Routing,
}

const fn rt(pattern: u8, eq_post: bool, pfx_post: bool, sr_post_rev: bool) -> Routing {
    Routing { pattern, sr_post_rev, eq_post, pfx_post }
}

/// Ready-made orders, all of them patterns the Katana runs natively.
pub const CHAIN_PRESETS: [ChainPreset; 6] = [
    ChainPreset {
        name: "Standard",
        blurb: "Wah and drive into the amp, modulation, delay and reverb after it",
        routing: rt(0, false, false, false),
    },
    ChainPreset {
        name: "Modern high gain",
        blurb: "EQ after the amp to tighten and shape the distorted tone",
        routing: rt(0, true, false, false),
    },
    ChainPreset {
        name: "Compressor first",
        blurb: "Mod slot (compressor) before the booster, for even, squashed drive",
        routing: rt(4, false, false, false),
    },
    ChainPreset {
        name: "Vintage front end",
        blurb: "Booster and modulation both hit the front of the amp",
        routing: rt(1, false, false, false),
    },
    ChainPreset {
        name: "Pedal platform",
        blurb: "Every effect before the amp, like a pedalboard into a clean amp",
        routing: rt(3, false, false, false),
    },
    ChainPreset {
        name: "Loop at the end",
        blurb: "FX loop after the reverb, for external delays and loopers",
        routing: rt(0, false, false, true),
    },
];

pub fn movable(n: Node) -> bool {
    matches!(n, PedalFx | Booster | Mod | Eq | Amp | SendReturn)
}

#[cfg(test)]
mod tests {

    #[test]
    fn chain_presets_are_valid_and_distinct() {
        for (i, p) in super::CHAIN_PRESETS.iter().enumerate() {
            let back = Routing::from_order(&p.routing.layout());
            assert_eq!(back, Some(p.routing), "{} does not round-trip", p.name);
            for q in &super::CHAIN_PRESETS[i + 1..] {
                assert_ne!(p.routing, q.routing, "{} == {}", p.name, q.name);
            }
        }
    }

    use super::*;

    #[test]
    fn every_routing_roundtrips() {
        for pattern in 0..7 {
            for bits in 0..8u8 {
                let r = Routing { pattern, sr_post_rev: bits & 1 != 0, eq_post: bits & 2 != 0, pfx_post: bits & 4 != 0 };
                assert_eq!(Routing::from_order(&r.layout()), Some(r), "{r:?} {:?}", r.layout());
            }
        }
    }

    #[test]
    fn drag_booster_after_mod_switches_pattern() {
        let r = Routing { pattern: 1, ..Default::default() }; // B M A F D
        let l = r.layout();
        let b = l.iter().position(|n| *n == Booster).unwrap();
        let m = l.iter().position(|n| *n == Mod).unwrap();
        let r2 = r.moved(b, m + 1).unwrap();
        assert_eq!(r2.pattern, 4); // M B A F D
    }

    #[test]
    fn impossible_orders_rejected() {
        let r = Routing { pattern: 1, ..Default::default() };
        let l = r.layout();
        let b = l.iter().position(|n| *n == Booster).unwrap();
        // booster after reverb: not a hardware pattern
        assert!(r.moved(b, l.len()).is_none());
        // fixed blocks cannot be dragged
        let d2 = l.iter().position(|n| *n == Delay2).unwrap();
        assert!(r.moved(d2, 0).is_none());
    }

    #[test]
    fn eq_moves_across_amp() {
        let r = Routing { pattern: 1, ..Default::default() };
        let l = r.layout();
        let e = l.iter().position(|n| *n == Eq).unwrap();
        let a = l.iter().position(|n| *n == Amp).unwrap();
        assert!(r.moved(e, a + 1).unwrap().eq_post);
        assert!(r.valid_gaps(e).iter().filter(|x| **x).count() >= 1);
    }
}
