//! Practice suite: stereo phrase looper and a synthesised groove drum machine / metronome.
//! Both run inside the audio callback without allocating: the looper's memory arrives once
//! through `Cmd::SetLoopBuffer`, drum voices are pure oscillators + envelopes.

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

pub const PATTERNS: [&str; 5] = ["Metronom", "Rock", "Funk", "Shuffle", "Metal"];

// 16 steps per bar; bits: 1 kick, 2 snare, 4 hat, 8 accent click (metronome)
const PAT: [[u8; 16]; 5] = [
    [8, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0],
    [5, 0, 4, 0, 6, 0, 4, 0, 5, 0, 5, 0, 6, 0, 4, 0],
    [5, 0, 4, 1, 6, 0, 4, 4, 0, 1, 5, 0, 6, 0, 4, 1],
    // shuffle uses only steps 0 and 3 of each beat (swung 8ths, 2:1)
    [5, 0, 0, 4, 6, 0, 0, 4, 5, 0, 0, 5, 6, 0, 0, 4],
    [5, 1, 5, 1, 7, 1, 5, 1, 5, 1, 5, 1, 7, 1, 5, 1],
];

/// Kick (pitch-swept sine), snare (tone + filtered noise), closed hat (high-passed noise),
/// metronome clicks. Sample-accurate 16th-note grid.
pub struct Drums {
    fs: f32,
    pub step: usize,
    phase: f64,
    kick_env: f32,
    kick_ph: f32,
    snare_env: f32,
    snare_ph: f32,
    hat_env: f32,
    click_env: f32,
    click_hi: bool,
    click_ph: f32,
    hat_hp: Biquad,
    snare_bp: Biquad,
    seed: u32,
    shuffle: bool,
}

impl Drums {
    pub fn new(fs: f32) -> Self {
        let mut hat_hp = Biquad::new();
        hat_hp.highpass(7000.0, 0.7, fs);
        let mut snare_bp = Biquad::new();
        snare_bp.bandpass(1800.0, 0.8, fs);
        Drums {
            fs,
            step: 0,
            phase: 0.0,
            kick_env: 0.0,
            kick_ph: 0.0,
            snare_env: 0.0,
            snare_ph: 0.0,
            hat_env: 0.0,
            click_env: 0.0,
            click_hi: false,
            click_ph: 0.0,
            hat_hp,
            snare_bp,
            seed: 0x2545_F491,
            shuffle: false,
        }
    }

    pub fn reset(&mut self) {
        self.step = 0;
        self.phase = 0.0;
    }

    #[inline]
    fn noise(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn trigger(&mut self, pattern: usize) {
        let s = PAT[pattern.min(PAT.len() - 1)][self.step];
        if s & 1 != 0 {
            self.kick_env = 1.0;
            self.kick_ph = 0.0;
        }
        if s & 2 != 0 {
            if pattern == 0 {
                self.click_env = 1.0;
                self.click_hi = false;
            } else {
                self.snare_env = 1.0;
            }
        }
        if s & 4 != 0 {
            self.hat_env = if self.step % 4 == 0 { 0.9 } else { 0.55 };
        }
        if s & 8 != 0 {
            self.click_env = 1.0;
            self.click_hi = true;
        }
    }

    /// Mono drum bus sample.
    #[inline]
    pub fn tick(&mut self, bpm: f32, pattern: usize) -> f32 {
        self.shuffle = pattern == 3;
        // 16th length; shuffle: on-beat 8th lasts 2/3 of the beat, off-beat 8th 1/3
        let sixteenth = 60.0 / bpm.clamp(30.0, 300.0) as f64 / 4.0;
        let len = if self.shuffle { if self.step % 4 == 0 { sixteenth * 8.0 / 3.0 } else { sixteenth * 4.0 / 3.0 } } else { sixteenth };
        if self.phase <= 0.0 {
            self.trigger(pattern);
        }
        self.phase += 1.0 / self.fs as f64;
        if self.phase >= len {
            self.phase = 0.0;
            self.step = if self.shuffle && self.step % 4 == 0 { self.step + 3 } else { (self.step + 1) % 16 };
        }
        let fs = self.fs;
        let mut y = 0.0;
        if self.kick_env > 1e-4 {
            let f = 48.0 + 110.0 * self.kick_env * self.kick_env;
            self.kick_ph = (self.kick_ph + f / fs).fract();
            y += (self.kick_ph * TAU).sin() * self.kick_env * 0.9;
            self.kick_env *= 1.0 - 9.0 / fs;
        }
        if self.snare_env > 1e-4 {
            self.snare_ph = (self.snare_ph + 190.0 / fs).fract();
            let n = self.noise();
            let nb = self.snare_bp.tick(n);
            y += ((self.snare_ph * TAU).sin() * 0.4 + nb * 1.6) * self.snare_env * 0.6;
            self.snare_env *= 1.0 - 14.0 / fs;
        }
        if self.hat_env > 1e-4 {
            let n = self.noise();
            y += self.hat_hp.tick(n) * self.hat_env * 0.35;
            self.hat_env *= 1.0 - 60.0 / fs;
        }
        if self.click_env > 1e-4 {
            let f = if self.click_hi { 1760.0 } else { 1320.0 };
            self.click_ph = (self.click_ph + f / fs).fract();
            y += (self.click_ph * TAU).sin() * self.click_env * 0.5;
            self.click_env *= 1.0 - 60.0 / fs;
        }
        flush(y)
    }
}
