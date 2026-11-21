//! The Katana MOD / FX types the modulation block does not cover: wahs, pitch shifter,
//! octave and harmonist, slow gear, vibrato, ring mod, humanizer, slicer and the acoustic /
//! guitar body simulators. One `FxUnit` per slot (MOD, FX), mono in and out, so it sits
//! ahead of the amp or after the cab like the patch's chain pattern says.
//!
//! Values come through `Params::fx(slot, EqKey::V(i))`; what each `V(i)` means per kind is
//! listed on `FX_KINDS`. Everything is allocated in `new`; `tick` never allocates.

use std::f32::consts::{PI, TAU};

use crate::filters::{db, flush, Biquad, DelayLine};

/// Kinds of an effect unit, with their values:
/// - 1 Wah: V0 mode (0 touch, 1 auto, 2 pedal), V1 frequency 0..1, V2 peak 0..1,
///   V3 sensitivity 0..1 (touch) or rate Hz (auto), V4 depth 0..1, V5 polarity (1 = down),
///   V6 effect level, V7 direct level, V8 / V9 pedal range 0..1.
/// - 2 Pitch: V0 / V2 voice 1 / 2 in semitones (or scale steps when V7 = 1), V1 / V3 their
///   levels, V4 direct level, V5 feedback 0..1, V6 pre-delay ms, V7 diatonic, V8 key
///   (0 = C), V9 minor scale.
/// - 3 Slow gear: V0 sensitivity 0..1, V1 rise time s, V2 level.
/// - 4 Vibrato: V0 rate Hz, V1 depth 0..1, V2 level.
/// - 5 Ring mod: V0 frequency Hz (or ratio to the played note when V1 = 1), V2 effect
///   level, V3 direct level.
/// - 6 Humanizer: V0 / V1 vowels (a e i o u), V2 rate Hz, V3 depth 0..1, V4 mode (0 picking,
///   1 auto), V5 sensitivity 0..1, V6 manual 0..1, V7 level.
/// - 7 Slicer: V0 pattern 0..19, V1 steps per second, V2 effect level, V3 direct level,
///   V4 step length 0..1.
/// - 8 Body: V0 low dB, V1 body (250 Hz) dB, V2 top dB, V3 level, V4 mid scoop dB.
pub const FX_KINDS: [&str; 9] = ["Off", "Wah", "Pitch", "Slow gear", "Vibrato", "Ring mod", "Humanizer", "Slicer", "Body"];

/// One read head pair of a granular pitch shifter: two taps sweeping a 45 ms window half a
/// cycle apart, crossfaded with sin² so their sum stays at unity.
#[derive(Clone, Copy, Default)]
pub struct Voice {
    ph: f32,
}

impl Voice {
    #[inline]
    pub fn tick(&mut self, dl: &DelayLine, ratio: f32, win: f32, pre: f32) -> f32 {
        self.ph = (self.ph + (1.0 - ratio) / win).rem_euclid(1.0);
        let p2 = (self.ph + 0.5).fract();
        let a = (PI * self.ph).sin();
        let b = (PI * p2).sin();
        dl.read(pre + 1.0 + self.ph * win) * a * a + dl.read(pre + 1.0 + p2 * win) * b * b
    }
}

/// Semitones from `midi` to the note `steps` scale degrees away in `key` (major or natural
/// minor). Notes outside the scale count from the scale note below them.
pub fn diatonic(midi: f32, key: i32, minor: bool, steps: i32) -> f32 {
    const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
    const MINOR: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];
    let sc = if minor { &MINOR } else { &MAJOR };
    let m = midi.round() as i32;
    let rel = m - key;
    let (oct, pc) = (rel.div_euclid(12), rel.rem_euclid(12));
    let deg = sc.iter().rposition(|s| *s <= pc).unwrap_or(0) as i32;
    let off = pc - sc[deg as usize];
    let t = deg + steps;
    let (to, td) = (t.div_euclid(7), t.rem_euclid(7));
    let target = key + (oct + to) * 12 + sc[td as usize] + off;
    (target - m) as f32
}

/// Topology-preserving state-variable filter (resonant low / band pass).
#[derive(Clone, Copy, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl Svf {
    fn set(&mut self, f: f32, q: f32, fs: f32) {
        let g = (PI * f.clamp(20.0, fs * 0.45) / fs).tan();
        self.k = 1.0 / q.max(0.3);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }
    /// (low pass, normalised band pass)
    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32) {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = flush(2.0 * v1 - self.ic1);
        self.ic2 = flush(2.0 * v2 - self.ic2);
        (v2, v1 * self.k)
    }
}

/// Slicer patterns: 16 steps each, bit i = step i sounds (BOSS SL-20 style mixes).
const SLICES: [u16; 20] = [
    0xFFFF, 0x5555, 0x7777, 0x6DB6, 0x3333, 0xB6DB, 0x7575, 0x5B5B, 0x9249, 0xEEEE, 0x6666, 0xD5D5, 0x1111, 0xF0F0, 0xAAAA,
    0x7B7B, 0x4924, 0xDDDD, 0xE6E6, 0x5AD6,
];

/// Formants (F1, F2) of the vowels a e i o u.
const VOWELS: [(f32, f32); 5] = [(730.0, 1090.0), (530.0, 1840.0), (300.0, 2250.0), (570.0, 840.0), (320.0, 870.0)];

pub struct FxUnit {
    fs: f32,
    kind: i32,
    dl: DelayLine,
    v1: Voice,
    v2: Voice,
    fb: f32,
    env: f32,
    env_a: f32,
    env_r: f32,
    phase: f32,
    svf: Svf,
    svf2: Svf,
    tick_n: u32,
    gain: f32,
    armed: bool,
    shift1: f32,
    shift2: f32,
    note: f32,
    eq: [Biquad; 4],
    eq_key: [f32; 5],
    slice: f32,
    /// wah make-up gain: target set with the filter, glided per sample
    wah_g: f32,
    wah_want: f32,
}

impl FxUnit {
    pub fn new(fs: f32) -> Self {
        FxUnit {
            fs,
            kind: 0,
            dl: DelayLine::new((0.35 * fs) as usize),
            v1: Voice::default(),
            v2: Voice { ph: 0.25 },
            fb: 0.0,
            env: 0.0,
            env_a: 1.0 - (-1.0 / (0.002 * fs)).exp(),
            env_r: 1.0 - (-1.0 / (0.09 * fs)).exp(),
            phase: 0.0,
            svf: Svf::default(),
            svf2: Svf::default(),
            tick_n: 0,
            gain: 0.0,
            armed: true,
            shift1: 0.0,
            shift2: 0.0,
            note: 60.0,
            eq: [Biquad::new(); 4],
            eq_key: [f32::NAN; 5],
            slice: 0.0,
            wah_g: 1.0,
            wah_want: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.dl.clear();
        self.fb = 0.0;
        self.env = 0.0;
        self.gain = 0.0;
        self.svf = Svf::default();
        self.svf2 = Svf::default();
        self.eq.iter_mut().for_each(Biquad::reset);
        self.wah_g = 1.0;
        self.wah_want = 1.0;
    }

    /// One sample. `v` = the slot's values, `midi` = the played note (−1 none).
    #[inline]
    pub fn tick(&mut self, x: f32, kind: i32, v: &[f32; 10], midi: f32) -> f32 {
        if kind != self.kind {
            self.kind = kind;
            self.reset();
        }
        let fs = self.fs;
        // input envelope: wah, slow gear, humanizer
        let ax = x.abs();
        self.env += (ax - self.env) * if ax > self.env { self.env_a } else { self.env_r };
        let n = self.tick_n;
        self.tick_n = n.wrapping_add(1);
        match kind {
            1 => {
                let pos = match v[0] as i32 {
                    0 => {
                        // −40 dBFS .. −10 dBFS of envelope sweeps the full range at max sens
                        let e = (self.env * (1.0 + 60.0 * v[3] * v[3])).min(1.0) * v[4].max(0.05) * 1.6;
                        let e = e.min(1.0);
                        (if v[5] >= 0.5 { v[1] * (1.0 - e) } else { v[1] + (1.0 - v[1]) * e }).clamp(0.0, 1.0)
                    }
                    1 => {
                        self.phase = (self.phase + v[3].max(0.05) / fs).fract();
                        (v[1] + v[4] * 0.5 * (self.phase * TAU).sin()).clamp(0.0, 1.0)
                    }
                    _ => v[8] + (v[9] - v[8]) * v[1],
                };
                if n % 8 == 0 {
                    // 350 Hz .. 2.2 kHz, the Cry Baby's sweep
                    let q = (2.0 + 10.0 * v[2]).max(0.5);
                    self.svf.set(350.0 * 6.3f32.powf(pos), q, fs);
                    // the normalised band pass keeps a narrow slice of the guitar: heel down at
                    // high peak lost 22 dB. Make-up by position and Q (measured on the DI riff
                    // through the clean amp) keeps the sweep within ~3 dB of bypass
                    self.wah_want = db(13.0 * (1.0 - pos).powf(1.3) + 7.0 * (q * 0.5).log10() - 1.0);
                }
                self.wah_g += (self.wah_want - self.wah_g) * 0.01;
                let (lp, bp) = self.svf.tick(x);
                flush((bp * 1.8 + lp * 0.12) * self.wah_g * v[6] + x * v[7])
            }
            2 => {
                let pre = v[6] * 0.001 * fs;
                self.dl.push(x + self.fb * v[5] * 0.9);
                let (s1, s2) = if v[7] >= 0.5 {
                    if midi >= 0.0 {
                        self.note = midi;
                    }
                    let (k, mi) = (v[8] as i32, v[9] >= 0.5);
                    (diatonic(self.note, k, mi, v[0] as i32), diatonic(self.note, k, mi, v[2] as i32))
                } else {
                    (v[0], v[2])
                };
                // glide 8 ms between intervals: a jump would click
                let k = 1.0 - (-1.0 / (0.008 * fs)).exp();
                self.shift1 += (s1 - self.shift1) * k;
                self.shift2 += (s2 - self.shift2) * k;
                let win = 0.045 * fs;
                let a = if v[1] > 0.0 { self.v1.tick(&self.dl, 2f32.powf(self.shift1 / 12.0), win, pre) * v[1] } else { 0.0 };
                let b = if v[3] > 0.0 { self.v2.tick(&self.dl, 2f32.powf(self.shift2 / 12.0), win, pre) * v[3] } else { 0.0 };
                self.fb = flush(a + b);
                x * v[4] + a + b
            }
            3 => {
                // a pick attack above the threshold re-arms a fade-in over the rise time
                let thr = db(-20.0 - 40.0 * v[0]);
                if self.env < thr * 0.3 {
                    self.armed = true;
                    self.gain *= 1.0 - 1.0 / (0.03 * fs);
                }
                if self.armed && self.env > thr {
                    self.armed = false;
                    self.gain = 0.0;
                }
                if !self.armed {
                    self.gain = (self.gain + 1.0 / (v[1].max(0.05) * fs)).min(1.0);
                }
                x * self.gain * self.gain * v[2]
            }
            4 => {
                self.phase = (self.phase + v[0].max(0.1) / fs).fract();
                self.dl.push(x);
                let d = (0.002 + 0.004 * v[1] * (0.5 + 0.5 * (self.phase * TAU).sin())) * fs;
                self.dl.read(d) * v[2]
            }
            5 => {
                let f = if v[1] >= 0.5 && midi >= 0.0 { 440.0 * 2f32.powf((midi - 69.0) / 12.0) * v[0] } else { v[0] };
                self.phase = (self.phase + f / fs).fract();
                x * (self.phase * TAU).sin() * v[2] + x * v[3]
            }
            6 => {
                let m = if v[4] >= 0.5 {
                    self.phase = (self.phase + v[2].max(0.05) / fs).fract();
                    v[6] + v[3] * 0.5 * (self.phase * TAU).sin()
                } else {
                    (self.env * (1.0 + 60.0 * v[5] * v[5])).min(1.0)
                }
                .clamp(0.0, 1.0);
                if n % 16 == 0 {
                    let (a, b) = (VOWELS[(v[0] as usize).min(4)], VOWELS[(v[1] as usize).min(4)]);
                    self.svf.set(a.0 + (b.0 - a.0) * m, 7.0, fs);
                    self.svf2.set(a.1 + (b.1 - a.1) * m, 9.0, fs);
                }
                let f1 = self.svf.tick(x).1;
                let f2 = self.svf2.tick(x).1;
                // two narrow formants pass little of the guitar: +10 dB brings level 1 to
                // about bypass loudness (it was 10–15 dB under)
                flush((f1 * 1.6 + f2 * 1.0) * 3.16 * v[7])
            }
            7 => {
                self.phase += v[1].max(0.5) / fs;
                if self.phase >= 16.0 {
                    self.phase -= 16.0;
                }
                let step = self.phase as usize;
                let within = self.phase.fract();
                let on = SLICES[(v[0] as usize).min(19)] >> step & 1 == 1 && within < v[4].clamp(0.1, 1.0);
                // 2 ms ramps: no clicks on the slice edges
                let k = 1.0 - (-1.0 / (0.002 * fs)).exp();
                self.slice += ((on as i32 as f32) - self.slice) * k;
                x * self.slice * v[2] + x * v[3]
            }
            8 => {
                let key = [v[0], v[1], v[2], v[4], 0.0];
                if key.iter().zip(self.eq_key.iter()).any(|(a, b)| a != b) {
                    self.eq_key = key;
                    self.eq[0].low_shelf(160.0, v[0], fs);
                    self.eq[1].peak(250.0, 1.1, v[1], fs);
                    self.eq[2].peak(700.0, 0.8, v[4], fs);
                    self.eq[3].high_shelf(4500.0, v[2], fs);
                }
                let mut y = x;
                for b in &mut self.eq {
                    y = b.tick(y);
                }
                y * v[3]
            }
            _ => x,
        }
    }
}
