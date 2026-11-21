//! Practice suite: stereo phrase looper and a synthesised groove drum machine / metronome.
//! Both run inside the audio callback without allocating: the looper's memory arrives once
//! through `Cmd::SetLoopBuffer`, drum voices are oscillators, filtered noise and envelopes.

use crate::filters::{flush, Biquad};
use std::f32::consts::TAU;

// ------------------------------------------------------------------ looper

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LoopState {
    Empty = 0,
    Recording = 1,
    Playing = 2,
    Overdubbing = 3,
    Stopped = 4,
}

impl LoopState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => LoopState::Recording,
            2 => LoopState::Playing,
            3 => LoopState::Overdubbing,
            4 => LoopState::Stopped,
            _ => LoopState::Empty,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopAction {
    /// REC → PLAY → DUB → PLAY … (one-button looper)
    Cycle,
    Stop,
    Play,
    /// drop the last overdub layer
    Undo,
    Clear,
}

/// Base loop + one overdub layer (single-level undo). Merging a layer into the base and
/// discarding an undone layer both happen incrementally, one sample per played sample,
/// so no operation ever touches the whole buffer at once.
pub struct Looper {
    /// interleaved stereo: [base L, base R, layer L, layer R] per frame
    buf: Box<[f32]>,
    pub state: LoopState,
    pub len: usize,
    pub pos: usize,
    layer_live: bool,
    merge_left: usize,
    wipe_left: usize,
}

impl Looper {
    pub fn new() -> Self {
        Looper { buf: Box::new([]), state: LoopState::Empty, len: 0, pos: 0, layer_live: false, merge_left: 0, wipe_left: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len() / 4
    }

    /// Install memory (UI thread allocates it); returns the old buffer for freeing elsewhere.
    pub fn set_buffer(&mut self, b: Box<[f32]>) -> Box<[f32]> {
        self.state = LoopState::Empty;
        self.len = 0;
        self.pos = 0;
        self.layer_live = false;
        std::mem::replace(&mut self.buf, b)
    }

    pub fn action(&mut self, a: LoopAction) {
        if self.capacity() == 0 {
            return;
        }
        use LoopState::*;
        match (a, self.state) {
            (LoopAction::Cycle, Empty) => {
                self.len = 0;
                self.pos = 0;
                self.state = Recording;
            }
            (LoopAction::Cycle, Recording) => self.close_loop(),
            (LoopAction::Cycle, Playing) | (LoopAction::Cycle, Stopped) => self.start_dub(),
            (LoopAction::Cycle, Overdubbing) => self.state = Playing,
            (LoopAction::Play, Stopped) => self.state = Playing,
            (LoopAction::Play, Recording) => self.close_loop(),
            (LoopAction::Stop, Recording) => {
                self.close_loop();
                self.state = Stopped;
            }
            (LoopAction::Stop, Playing | Overdubbing) => {
                self.state = Stopped;
                self.pos = 0;
            }
            (LoopAction::Undo, Playing | Overdubbing | Stopped) => {
                if self.layer_live {
                    self.layer_live = false;
                    self.merge_left = 0;
                    self.wipe_left = self.len;
                    if self.state == Overdubbing {
                        self.state = Playing;
                    }
                }
            }
            (LoopAction::Clear, _) => {
                self.state = Empty;
                self.len = 0;
                self.pos = 0;
                self.layer_live = false;
                self.merge_left = 0;
                self.wipe_left = 0;
            }
            _ => {}
        }
    }

    fn close_loop(&mut self) {
        if self.len < 2400 {
            // shorter than ~50 ms: treat as a mis-press
            self.state = LoopState::Empty;
            self.len = 0;
            return;
        }
        self.pos = 0;
        self.state = LoopState::Playing;
        // layer slots were zeroed while recording
        self.layer_live = false;
    }

    fn start_dub(&mut self) {
        if self.layer_live {
            // previous layer becomes part of the base as the loop plays on
            self.merge_left = self.len;
        }
        self.layer_live = true;
        self.state = LoopState::Overdubbing;
    }

    /// Process one frame: records/overdubs `(l, r)`, returns the loop playback to mix in.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let cap = self.capacity();
        match self.state {
            LoopState::Empty | LoopState::Stopped => {
                // keep incremental housekeeping going while stopped
                if self.state == LoopState::Stopped && self.len > 0 {
                    self.housekeep(self.pos);
                    self.pos = (self.pos + 1) % self.len;
                }
                (0.0, 0.0)
            }
            LoopState::Recording => {
                if self.len >= cap {
                    self.close_loop();
                    return (0.0, 0.0);
                }
                let i = self.len * 4;
                self.buf[i] = l;
                self.buf[i + 1] = r;
                self.buf[i + 2] = 0.0;
                self.buf[i + 3] = 0.0;
                self.len += 1;
                (0.0, 0.0)
            }
            LoopState::Playing | LoopState::Overdubbing => {
                let p = self.pos;
                self.housekeep(p);
                let i = p * 4;
                if self.state == LoopState::Overdubbing {
                    // overdub decays the existing layer a little, like a tape looper
                    self.buf[i + 2] = flush(self.buf[i + 2] * 0.98 + l);
                    self.buf[i + 3] = flush(self.buf[i + 3] * 0.98 + r);
                }
                let (ol, or) = if self.layer_live {
                    (self.buf[i] + self.buf[i + 2], self.buf[i + 1] + self.buf[i + 3])
                } else {
                    (self.buf[i], self.buf[i + 1])
                };
                self.pos = (p + 1) % self.len.max(1);
                (ol, or)
            }
        }
    }

    #[inline]
    fn housekeep(&mut self, p: usize) {
        let i = p * 4;
        if self.merge_left > 0 {
            self.buf[i] += self.buf[i + 2];
            self.buf[i + 1] += self.buf[i + 3];
            self.buf[i + 2] = 0.0;
            self.buf[i + 3] = 0.0;
            self.merge_left -= 1;
        } else if self.wipe_left > 0 {
            self.buf[i + 2] = 0.0;
            self.buf[i + 3] = 0.0;
            self.wipe_left -= 1;
        }
    }
}

impl Default for Looper {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ drum machine

/// Groove names in UI order; 0 is the metronome (its bar follows the time signature).
pub const PATTERNS: [&str; 15] = [
    "Metronome",
    "Rock",
    "Pop",
    "Funk",
    "Shuffle",
    "Half-time",
    "Metal",
    "6/8",
    "Disco",
    "Punk",
    "Blues 12/8",
    "Reggae",
    "Hip-hop",
    "Bossa",
    "Train",
];
/// `P::DrumPattern` value of the user groove, the one written into `P::DrumUser0..`.
pub const USER: usize = PATTERNS.len();
/// Metronome time signatures.
pub const SIGNATURES: [&str; 3] = ["4/4", "3/4", "6/8"];
/// Rows of the user groove's step grid, in `P::DrumUser0..` order (= the first eight voices).
pub const USER_ROWS: [&str; 8] = ["Kick", "Snare", "Hi-hat", "Open hat", "Ride", "Crash", "Tom hi", "Tom lo"];
/// Steps in a user row: a bar of 16ths (the first 12 of them in 3/4).
pub const USER_STEPS: usize = 16;
/// Mix groups of the kit (`P::DrumKickLv..`, in this order).
pub const KIT_GROUPS: [&str; 6] = ["Kick", "Snare", "Hats", "Cymbals", "Toms", "Click"];

const VOICES: usize = 10;
const KICK: usize = 0;
const SNARE: usize = 1;
const HAT: usize = 2;
const OPEN: usize = 3;
const RIDE: usize = 4;
const CRASH: usize = 5;
const TOM_HI: usize = 6;
const TOM_LO: usize = 7;
const CLICK_HI: usize = 8;
const CLICK: usize = 9;
/// mix group (`KIT_GROUPS`) of every voice
const GROUP: [usize; VOICES] = [0, 1, 2, 2, 3, 3, 4, 4, 5, 5];

/// One groove written as drum-tab lines on a 16th grid (12 steps for 6/8 and 3/4, or a
/// triplet grid for 12/8). `X` accent, `x` normal, `g` ghost note, `.` rest.
struct Groove {
    kick: &'static str,
    snare: &'static str,
    hat: &'static str,
    open: &'static str,
    ride: &'static str,
    /// fraction of a beat taken by the on-beat 8th (0.5 straight, 0.667 shuffle)
    swing: f32,
    /// steps per quarter-note beat: 4 (16ths) or 3 (8th-note triplets, 12/8)
    div: usize,
}

const GROOVES: [Groove; 15] = [
    // metronome placeholder: built from the time signature
    Groove { kick: "", snare: "", hat: "", open: "", ride: "", swing: 0.5, div: 4 },
    // Rock
    Groove {
        kick: "X.....x.X.x.....",
        snare: "....X.......X...",
        hat: "X.x.X.x.X.x.X.x.",
        open: "................",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Pop
    Groove {
        kick: "X......xX.......",
        snare: "....X..g....X...",
        hat: "XxxxXxxxXxxxXxx.",
        open: "...............x",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Funk
    Groove {
        kick: "X..x..x...X..x..",
        snare: "....X..g.g..X..g",
        hat: "XxXxXxXxXxXxXx.x",
        open: "..............x.",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Shuffle
    Groove {
        kick: "X.......X.x.....",
        snare: "....X.......X...",
        hat: "X.x.X.x.X.x.X.x.",
        open: "................",
        ride: "................",
        swing: 2.0 / 3.0,
        div: 4,
    },
    // Half-time
    Groove {
        kick: "X.......x.x.....",
        snare: "........X.......",
        hat: "X.x.x.x.X.x.x.x.",
        open: "................",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Metal
    Groove {
        kick: "XxXxXxXxXxXxXxXx",
        snare: "....X.......X...",
        hat: "................",
        open: "................",
        ride: "X.x.X.x.X.x.X.x.",
        swing: 0.5,
        div: 4,
    },
    // 6/8
    Groove {
        kick: "X.....x.....",
        snare: "......X.....",
        hat: "X.x.x.X.x.x.",
        open: "............",
        ride: "............",
        swing: 0.5,
        div: 4,
    },
    // Disco: four on the floor, open hat on the off-beats
    Groove {
        kick: "X...X...X...X...",
        snare: "....X.......X...",
        hat: "x.x.x.x.x.x.x.x.",
        open: "..x...x...x...x.",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Punk: driving 8ths, kick on every "and" of 2 and 4
    Groove {
        kick: "X.x...x.X.x...x.",
        snare: "....X.......X...",
        hat: "X.X.X.X.X.X.X.X.",
        open: "................",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Blues 12/8: triplet grid, ride on the shuffle
    Groove {
        kick: "X.....X..x..",
        snare: "...X.....X..",
        hat: "............",
        open: "............",
        ride: "X.xX.xX.xX.x",
        swing: 0.5,
        div: 3,
    },
    // Reggae one drop: kick and snare together on 3
    Groove {
        kick: "........X.......",
        snare: "........X.......",
        hat: "x.X.x.X.x.X.x.X.",
        open: "................",
        ride: "................",
        swing: 0.56,
        div: 4,
    },
    // Hip-hop: lazy swung 16ths
    Groove {
        kick: "X......x..X.....",
        snare: "....X.......X..g",
        hat: "X.x.X.x.X.x.X.x.",
        open: "................",
        ride: "................",
        swing: 0.6,
        div: 4,
    },
    // Bossa: surdo kick and a clave-like rim on the snare
    Groove {
        kick: "X..xX..xX..xX..x",
        snare: "x..x..x...x..x..",
        hat: "x.x.x.x.x.x.x.x.",
        open: "................",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
    // Train: country two-step, ghosted 16ths on the snare
    Groove {
        kick: "X.......X.......",
        snare: "gxgxXgxggxgxXgxg",
        hat: "................",
        open: "................",
        ride: "................",
        swing: 0.5,
        div: 4,
    },
];

fn vel(c: u8) -> f32 {
    match c {
        b'X' => 1.0,
        b'x' => 0.72,
        b'g' => 0.3,
        _ => 0.0,
    }
}

/// Velocity of a user grid cell: 0 rest, 1 ghost, 2 normal, 3 accent.
pub fn cell_vel(c: u32) -> f32 {
    match c & 3 {
        1 => 0.3,
        2 => 0.72,
        3 => 1.0,
        _ => 0.0,
    }
}

fn vel_cell(v: f32) -> u32 {
    if v >= 0.9 {
        3
    } else if v >= 0.5 {
        2
    } else if v > 0.0 {
        1
    } else {
        0
    }
}

/// Cell `step` of a packed user row (2 bits per step).
pub fn row_cell(row: u32, step: usize) -> u32 {
    if step >= USER_STEPS {
        return 0;
    }
    (row >> (2 * step)) & 3
}

/// The row with cell `step` set to `c` (0..3).
pub fn row_set(row: u32, step: usize, c: u32) -> u32 {
    if step >= USER_STEPS {
        return row;
    }
    let sh = 2 * step;
    (row & !(3 << sh)) | ((c & 3) << sh)
}

/// Steps in a bar and the steps that make one counted beat group, for the UI's step lights
/// and beat lamps: (bar steps, steps per beat lamp, steps per accent group).
pub fn bar_shape(pattern: usize, sig: usize, user_len: usize) -> (usize, usize, usize) {
    match pattern {
        0 => match sig {
            1 => (12, 4, 12),
            2 => (12, 2, 6),
            _ => (16, 4, 16),
        },
        7 => (12, 2, 6),
        p if p >= USER => {
            let n = if user_len <= 12 { 12 } else { 16 };
            (n, 4, n)
        }
        p => {
            let g = &GROOVES[p.min(GROOVES.len() - 1)];
            (g.kick.len(), g.div, g.kick.len())
        }
    }
}

/// A preset groove as user rows (to start editing from): packed rows and the bar length.
/// The metronome comes out as a hi-hat on every beat with the downbeat accented; a 12/8
/// triplet bar does not fit a 16th grid, so its four beats are stretched over 16 steps.
pub fn groove_rows(pattern: usize, sig: usize) -> ([u32; 8], usize) {
    rows_of(pattern, sig, true)
}

/// A preset groove as it plays, step for step (for showing it): packed rows and bar length.
pub fn groove_grid(pattern: usize, sig: usize) -> ([u32; 8], usize) {
    rows_of(pattern, sig, false)
}

fn rows_of(pattern: usize, sig: usize, fit: bool) -> ([u32; 8], usize) {
    let mut rows = [0u32; 8];
    let bar = if pattern == 0 { metronome(sig.min(2)) } else { parse(&GROOVES[pattern.min(GROOVES.len() - 1)]) };
    let stretch = fit && bar.div == 3;
    let len = if stretch { 16 } else { bar.len.min(USER_STEPS) };
    for (i, st) in bar.steps.iter().enumerate().take(bar.len) {
        let at = if stretch { i * 4 / 3 } else { i };
        if at >= USER_STEPS {
            continue;
        }
        for (r, row) in rows.iter_mut().enumerate() {
            *row = row_set(*row, at, vel_cell(st[r]).max(row_cell(*row, at)));
        }
        if st[CLICK_HI] > 0.0 || st[CLICK] > 0.0 {
            let c = if st[CLICK_HI] > 0.0 { 3 } else { 2 };
            rows[HAT] = row_set(rows[HAT], at, c);
        }
    }
    (rows, len)
}

/// A pattern ready to play: per step, the velocity of each voice.
struct Bar {
    steps: Vec<[f32; VOICES]>,
    /// steps in use (the user bar keeps all 16 allocated and plays 12 or 16)
    len: usize,
    div: usize,
    swing: f32,
    fills: bool,
}

fn metronome(sig: usize) -> Bar {
    // 4/4: four quarters; 3/4: three; 6/8: six 8ths, accents on 1 and 4
    let (n, hits): (usize, &[(usize, usize)]) = match sig {
        1 => (12, &[(0, CLICK_HI), (4, CLICK), (8, CLICK)]),
        2 => (12, &[(0, CLICK_HI), (2, CLICK), (4, CLICK), (6, CLICK_HI), (8, CLICK), (10, CLICK)]),
        _ => (16, &[(0, CLICK_HI), (4, CLICK), (8, CLICK), (12, CLICK)]),
    };
    let mut steps = vec![[0.0; VOICES]; n];
    for &(i, v) in hits {
        steps[i][v] = if sig == 2 && i == 6 { 0.75 } else { 1.0 };
    }
    Bar { steps, len: n, div: 4, swing: 0.5, fills: false }
}

fn parse(g: &Groove) -> Bar {
    let n = g.kick.len();
    let mut steps = vec![[0.0; VOICES]; n];
    for (voice, line) in [(KICK, g.kick), (SNARE, g.snare), (HAT, g.hat), (OPEN, g.open), (RIDE, g.ride)] {
        for (i, c) in line.bytes().enumerate().take(n) {
            steps[i][voice] = vel(c);
        }
    }
    Bar { steps, len: n, div: g.div, swing: g.swing, fills: n > 0 }
}

/// Feel and mix of the kit, set once per audio block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kit {
    /// kick, snare and toms, in semitones (−6..+6)
    pub tune: f32,
    /// ring length multiplier of drums and cymbals (0.4 tight … 2 roomy)
    pub decay: f32,
    /// 0..1: velocity spread and late-hit timing drift
    pub human: f32,
    /// fill every 8 / 4 / 2 bars for 1 / 2 / 3, none for 0
    pub fills: u32,
    /// 0..1: extra swing on the off-beat 16ths (straight → triplet feel), on top of the groove's
    pub swing: f32,
    /// linear gain per `KIT_GROUPS` entry
    pub levels: [f32; 6],
}

impl Default for Kit {
    fn default() -> Self {
        Kit { tune: 0.0, decay: 1.0, human: 0.3, fills: 2, swing: 0.0, levels: [1.0; 6] }
    }
}

/// One percussive voice: amplitude envelope, pitch envelope and oscillator phases.
#[derive(Clone, Copy, Default)]
struct Voice {
    amp: f32,
    pitch: f32,
    snap: f32,
    ph: [f32; 6],
}

/// Synthesised kit, every voice designed after the classic drum machines rather than a beep:
/// - kick: sine body with a fast pitch drop (≈140 → 45 Hz) plus a short beater click, soft clip
/// - snare: two drum-head modes (180 / 330 Hz) and band-limited noise for the wires
/// - hats / ride / crash: six detuned square oscillators (the 808 cymbal recipe) through
///   band-pass + high-pass, with noise for sizzle; the closed hat chokes the open one
/// - toms: pitch-swept sines for the fills
/// Velocity per hit with a little random variation, stereo placement like a drum riser,
/// sample-accurate 16th grid with optional swing, a fill every few bars. `Kit` tunes, damps,
/// humanises and mixes it; the user groove is a 16-step grid written through `set_user`.
pub struct Drums {
    fs: f32,
    pub step: usize,
    /// bars started since reset
    pub bar: u32,
    bar_started: bool,
    /// the next tick opens a bar (step 0, on the grid)
    downbeat: bool,
    /// samples into the current step (fraction carried over, so bars average exact length)
    phase: f64,
    /// the current step's hits are still to be played
    fire: bool,
    /// humanised timing: the step's hits land this many samples after its grid point
    fire_at: f64,
    bars: Vec<Bar>,
    meters: [Bar; 3],
    user: Bar,
    user_rows: [u32; 8],
    kit: Kit,
    tune: f32,
    v: [Voice; VOICES],
    // per-voice decay multipliers (per sample)
    k_amp: [f32; VOICES],
    hat_bp: Biquad,
    hat_hp: Biquad,
    open_bp: Biquad,
    open_hp: Biquad,
    ride_bp: Biquad,
    crash_hp: Biquad,
    crash_lp: Biquad,
    snare_hp: Biquad,
    snare_lp: Biquad,
    click_hp: Biquad,
    seed: u32,
}

const METAL_HZ: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

impl Drums {
    pub fn new(fs: f32) -> Self {
        let bp = |f: f32, q: f32| {
            let mut b = Biquad::new();
            b.bandpass(f, q, fs);
            b
        };
        let hp = |f: f32| {
            let mut b = Biquad::new();
            b.highpass(f, 0.707, fs);
            b
        };
        let lp = |f: f32| {
            let mut b = Biquad::new();
            b.lowpass(f, 0.707, fs);
            b
        };
        let mut d = Drums {
            fs,
            step: 0,
            bar: 0,
            bar_started: false,
            downbeat: true,
            phase: 0.0,
            fire: true,
            fire_at: 0.0,
            bars: GROOVES.iter().map(parse).collect(),
            meters: [metronome(0), metronome(1), metronome(2)],
            user: Bar { steps: vec![[0.0; VOICES]; USER_STEPS], len: USER_STEPS, div: 4, swing: 0.5, fills: true },
            user_rows: [0; 8],
            kit: Kit::default(),
            tune: 1.0,
            v: [Voice::default(); VOICES],
            k_amp: [0.0; VOICES],
            hat_bp: bp(9500.0, 1.1),
            hat_hp: hp(7000.0),
            open_bp: bp(8500.0, 0.9),
            open_hp: hp(6000.0),
            ride_bp: bp(5200.0, 1.6),
            crash_hp: hp(3800.0),
            crash_lp: lp(12000.0),
            snare_hp: hp(1400.0),
            snare_lp: lp(9000.0),
            click_hp: hp(1500.0),
            seed: 0x2545_F491,
        };
        d.decays();
        d
    }

    /// Envelope constants for the kit's decay setting (a handful of `exp`, on change only).
    fn decays(&mut self) {
        let fs = self.fs;
        let k = self.kit.decay.clamp(0.25, 3.0);
        let tau = |s: f32| (-1.0 / (s * fs)).exp();
        self.k_amp[KICK] = tau(0.30 * k);
        self.k_amp[SNARE] = tau(0.12 * k);
        self.k_amp[HAT] = tau(0.035 * k.sqrt());
        self.k_amp[OPEN] = tau(0.32 * k);
        self.k_amp[RIDE] = tau(0.9 * k);
        self.k_amp[CRASH] = tau(1.25 * k);
        self.k_amp[TOM_HI] = tau(0.32 * k);
        self.k_amp[TOM_LO] = tau(0.4 * k);
        self.k_amp[CLICK_HI] = tau(0.012);
        self.k_amp[CLICK] = tau(0.012);
    }

    /// Feel and mix (cheap when unchanged: called every audio block).
    pub fn set_kit(&mut self, k: &Kit) {
        if *k == self.kit {
            return;
        }
        let decay_moved = k.decay != self.kit.decay;
        self.kit = *k;
        self.tune = 2f32.powf(k.tune.clamp(-12.0, 12.0) / 12.0);
        if decay_moved {
            self.decays();
        }
    }

    /// The user groove: packed rows (`row_cell`) and 12 or 16 steps. Rebuilt in place, only
    /// when something changed, so it is safe to call every audio block.
    pub fn set_user(&mut self, rows: [u32; 8], len: usize) {
        let len = if len <= 12 { 12 } else { USER_STEPS };
        if rows == self.user_rows && len == self.user.len {
            return;
        }
        self.user_rows = rows;
        self.user.len = len;
        for (s, st) in self.user.steps.iter_mut().enumerate() {
            *st = [0.0; VOICES];
            for (r, row) in rows.iter().enumerate() {
                st[r] = cell_vel(row_cell(*row, s));
            }
        }
    }

    pub fn reset(&mut self) {
        self.step = 0;
        self.bar = 0;
        self.phase = 0.0;
        self.fire = true;
        self.fire_at = 0.0;
        self.downbeat = true;
    }

    /// True for the one sample at which a new bar began (step 0 triggered).
    pub fn bar_started(&self) -> bool {
        self.bar_started
    }

    /// Steps in a bar of this pattern / time signature.
    pub fn bar_len(&self, pattern: usize, sig: usize) -> usize {
        self.pick(pattern, sig).len
    }

    fn pick(&self, pattern: usize, sig: usize) -> &Bar {
        if pattern == 0 {
            &self.meters[sig.min(2)]
        } else if pattern >= USER {
            &self.user
        } else {
            &self.bars[pattern.min(self.bars.len() - 1)]
        }
    }

    #[inline]
    fn noise(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn hit(&mut self, voice: usize, vel: f32) {
        if vel <= 0.0 {
            return;
        }
        // a little spread so repeated hits are not machine-identical (±8 % at the default feel)
        let spread = 0.02 + 0.2 * self.kit.human.clamp(0.0, 1.0);
        let v = (vel * (1.0 + spread * self.noise())).clamp(0.0, 1.0);
        let s = &mut self.v[voice];
        s.amp = v;
        s.pitch = 1.0;
        s.snap = v;
        if voice == HAT {
            self.v[OPEN].amp *= 0.05; // choke
        }
    }

    fn trigger(&mut self, pattern: usize, sig: usize) {
        let (n, fills) = {
            let b = self.pick(pattern, sig);
            (b.len, b.fills)
        };
        let st = self.step;
        let every = match self.kit.fills {
            1 => 8,
            2 => 4,
            3 => 2,
            _ => 0,
        };
        let fills = fills && every > 0;
        let beat = if n == 12 { 3 } else { 4 };
        // bar 4, 8, …: last beat is a fill; the bar after it opens with a crash
        let fill = fills && self.bar % every == 0 && st >= n - beat;
        if fills && self.bar > 1 && self.bar % every == 1 && st == 0 {
            self.hit(CRASH, 0.9);
            self.hit(KICK, 1.0);
            return;
        }
        if fill {
            let k = st - (n - beat);
            match (beat, k) {
                (_, 0) => {
                    self.hit(SNARE, 0.8);
                    self.hit(KICK, 0.8);
                }
                (4, 1) => self.hit(SNARE, 0.6),
                (4, 2) | (3, 1) => self.hit(TOM_HI, 0.85),
                _ => self.hit(TOM_LO, 0.95),
            }
            return;
        }
        let row = self.pick(pattern, sig).steps[st];
        for (voice, vel) in row.iter().enumerate() {
            self.hit(voice, *vel);
        }
    }

    /// Stereo drum bus sample. `sig` is the metronome's time signature (0 4/4, 1 3/4, 2 6/8).
    #[inline]
    pub fn tick(&mut self, bpm: f32, pattern: usize, sig: usize) -> (f32, f32) {
        self.bar_started = false;
        let (n, swing, div) = {
            let b = self.pick(pattern, sig);
            (b.len, b.swing, b.div)
        };
        if n == 0 {
            return self.render();
        }
        if self.step >= n {
            // a shorter groove picked mid-bar: start its bar now
            self.step = 0;
            self.phase = 0.0;
            self.fire = true;
            self.fire_at = 0.0;
            self.downbeat = true;
        }
        if self.downbeat {
            self.downbeat = false;
            self.bar_started = true;
            self.bar += 1;
        }
        let beat = 60.0 / bpm.clamp(30.0, 300.0) as f64 * self.fs as f64;
        let len = if div == 3 {
            beat / 3.0
        } else {
            // swing moves the off-beat 8th: steps 0,1 share `swing` of the beat, steps 2,3 the rest
            let sw = swing.max(0.5 + 0.1667 * self.kit.swing.clamp(0.0, 1.0)) as f64;
            if self.step % 4 < 2 {
                beat * sw / 2.0
            } else {
                beat * (1.0 - sw) / 2.0
            }
        };
        if self.fire && self.phase >= self.fire_at {
            self.fire = false;
            self.trigger(pattern, sig);
        }
        self.phase += 1.0;
        if self.phase >= len {
            self.phase = (self.phase - len).min(len);
            if self.fire {
                // the drift pushed the hits past the step: play them now, never drop them
                self.fire = false;
                self.trigger(pattern, sig);
            }
            self.step = (self.step + 1) % n;
            self.fire = true;
            // up to ~10 ms late at full humanise, never on the downbeat
            let h = self.kit.human.clamp(0.0, 1.0);
            self.fire_at = if self.step == 0 || h <= 0.0 { 0.0 } else { (0.5 + 0.5 * self.noise()) as f64 * (h * h) as f64 * 0.010 * self.fs as f64 };
            self.downbeat = self.step == 0;
        }
        self.render()
    }

    #[inline]
    fn render(&mut self) -> (f32, f32) {
        let fs = self.fs;
        let lv = self.kit.levels;
        let tune = self.tune;
        let (mut l, mut r) = (0.0f32, 0.0f32);
        let pan = |x: f32, p: f32, l: &mut f32, r: &mut f32| {
            *l += x * (1.0 - p);
            *r += x * (1.0 + p);
        };
        // kick
        if self.v[KICK].amp > 1e-4 {
            let n = self.noise();
            let c = self.click_hp.tick(n) * self.v[KICK].snap;
            let k = &mut self.v[KICK];
            let f = (45.0 + 95.0 * k.pitch) * tune;
            k.ph[0] = (k.ph[0] + f / fs).fract();
            let body = (k.ph[0] * TAU).sin() * k.amp;
            k.pitch *= 0.99935; // ≈ 32 ms
            k.snap *= 0.9945; // ≈ 4 ms
            k.amp *= self.k_amp[KICK];
            let y = (1.7 * (body + 0.45 * c)).tanh() * 0.85;
            pan(y * lv[GROUP[KICK]], 0.0, &mut l, &mut r);
        }
        // snare
        if self.v[SNARE].amp > 1e-4 {
            let n = self.noise();
            let wires = self.snare_lp.tick(self.snare_hp.tick(n));
            let s = &mut self.v[SNARE];
            let drop = (1.0 + 0.12 * s.pitch) * tune;
            s.ph[0] = (s.ph[0] + 180.0 * drop / fs).fract();
            s.ph[1] = (s.ph[1] + 330.0 * drop / fs).fract();
            let head = ((s.ph[0] * TAU).sin() + 0.6 * (s.ph[1] * TAU).sin()) * s.snap;
            let y = head * 0.55 + wires * s.amp * 1.1;
            s.pitch *= 0.9985;
            s.snap *= 0.99965; // ≈ 60 ms head ring
            s.amp *= self.k_amp[SNARE];
            pan(y * 0.75 * lv[GROUP[SNARE]], 0.0, &mut l, &mut r);
        }
        // cymbals share the six-oscillator metal source
        let cym = self.v[HAT].amp + self.v[OPEN].amp + self.v[RIDE].amp + self.v[CRASH].amp;
        if cym > 1e-4 {
            let src = &mut self.v[HAT].ph;
            let mut m = 0.0;
            for (p, f) in src.iter_mut().zip(METAL_HZ) {
                *p = (*p + f * 1.7 / fs).fract();
                m += if *p < 0.5 { 1.0 } else { -1.0 };
            }
            let n = self.noise();
            let metal = m / 6.0 + 0.35 * n;
            if self.v[HAT].amp > 1e-4 {
                let y = self.hat_hp.tick(self.hat_bp.tick(metal)) * self.v[HAT].amp * 0.7;
                self.v[HAT].amp *= self.k_amp[HAT];
                pan(y * lv[GROUP[HAT]], 0.3, &mut l, &mut r);
            }
            if self.v[OPEN].amp > 1e-4 {
                let y = self.open_hp.tick(self.open_bp.tick(metal)) * self.v[OPEN].amp * 0.55;
                self.v[OPEN].amp *= self.k_amp[OPEN];
                pan(y * lv[GROUP[OPEN]], 0.3, &mut l, &mut r);
            }
            if self.v[RIDE].amp > 1e-4 {
                let rd = &mut self.v[RIDE];
                rd.ph[0] = (rd.ph[0] + 3150.0 / fs).fract();
                let bell = (rd.ph[0] * TAU).sin() * rd.snap * 0.25;
                rd.snap *= 0.99985;
                let y = (self.ride_bp.tick(metal) * 0.8 + bell) * self.v[RIDE].amp * 0.45;
                self.v[RIDE].amp *= self.k_amp[RIDE];
                pan(y * lv[GROUP[RIDE]], -0.35, &mut l, &mut r);
            }
            if self.v[CRASH].amp > 1e-4 {
                let y = self.crash_lp.tick(self.crash_hp.tick(metal * 0.6 + n * 0.6)) * self.v[CRASH].amp * 0.5;
                self.v[CRASH].amp *= self.k_amp[CRASH];
                pan(y * lv[GROUP[CRASH]], -0.4, &mut l, &mut r);
            }
        }
        // toms
        for (voice, base, p) in [(TOM_HI, 165.0f32, -0.3f32), (TOM_LO, 105.0, 0.3)] {
            if self.v[voice].amp > 1e-4 {
                let t = &mut self.v[voice];
                let f = base * (1.0 + 0.5 * t.pitch) * tune;
                t.ph[0] = (t.ph[0] + f / fs).fract();
                let y = (t.ph[0] * TAU).sin() * t.amp * 0.8;
                t.pitch *= 0.9993;
                t.amp *= self.k_amp[voice];
                pan(y * lv[GROUP[voice]], p, &mut l, &mut r);
            }
        }
        // metronome clicks: a woodblock-like ping
        for (voice, f) in [(CLICK_HI, 1760.0f32), (CLICK, 1320.0)] {
            if self.v[voice].amp > 1e-4 {
                let c = &mut self.v[voice];
                c.ph[0] = (c.ph[0] + f / fs).fract();
                let y = (c.ph[0] * TAU).sin() * c.amp * 1.1;
                c.amp *= self.k_amp[voice];
                pan(y * lv[GROUP[voice]], 0.0, &mut l, &mut r);
            }
        }
        (flush(l * 0.42), flush(r * 0.42))
    }
}
