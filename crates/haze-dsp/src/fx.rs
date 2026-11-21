//! Effect blocks voiced after the Katana's own, so a patch made on the amp keeps its
//! character on the PC engine: compressor / limiter, parametric and graphic EQ slots, typed
//! delays (digital, pan, stereo, analog, tape echo, reverse, modulate, SDE-3000) and a
//! modulated 8-line FDN reverb (room, hall, plate, spring, modulate).
//!
//! Every block takes its settings in `set`, called once per audio buffer; coefficients are
//! rebuilt only when a value changed. `tick` runs per sample. Nothing allocates after `new`.

use crate::filters::{db, flush, Biquad, DelayLine, Smooth};
use std::f32::consts::{PI, TAU};

/// Biquad that passes the signal unchanged (a 0 dB peak has b = a).
fn identity(b: &mut Biquad, fs: f32) {
    b.peak(1000.0, 1.0, 0.0, fs);
}

/// tanh-like soft clip, exact at 0, ±1 at ±3 and beyond.
#[inline]
fn soft(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

// ------------------------------------------------------------------ compressor / limiter

/// Feed-forward compressor: peak detector, 6 dB soft knee, attack / release on the gain
/// reduction, make-up gain and a high-shelf tone control. A limiter is the same block with a
/// high ratio and a short attack.
pub struct Comp {
    fs: f32,
    env: f32,
    det_a: f32,
    det_r: f32,
    /// smoothed gain change in dB, ≤ 0
    gr: f32,
    att: f32,
    rel: f32,
    thresh: f32,
    slope: f32,
    makeup: f32,
    tone: Biquad,
    tone_on: bool,
    key: [f32; 6],
}

const KNEE_DB: f32 = 6.0;

impl Comp {
    pub fn new(fs: f32) -> Self {
        let c = |ms: f32| 1.0 - (-1.0 / (ms * 0.001 * fs)).exp();
        Comp {
            fs,
            env: 0.0,
            det_a: c(0.3),
            det_r: c(12.0),
            gr: 0.0,
            att: 0.0,
            rel: 0.0,
            thresh: 0.0,
            slope: 0.0,
            makeup: 1.0,
            tone: Biquad::new(),
            tone_on: false,
            key: [f32::NAN; 6],
        }
    }

    pub fn set(&mut self, thresh_db: f32, ratio: f32, attack_ms: f32, release_ms: f32, makeup_db: f32, tone_db: f32) {
        let k = [thresh_db, ratio, attack_ms, release_ms, makeup_db, tone_db];
        if k == self.key {
            return;
        }
        self.key = k;
        let fs = self.fs;
        let c = |ms: f32| 1.0 - (-1.0 / (ms.max(0.05) * 0.001 * fs)).exp();
        self.thresh = thresh_db;
        self.slope = 1.0 - 1.0 / ratio.max(1.0);
        self.att = c(attack_ms);
        self.rel = c(release_ms);
        self.makeup = db(makeup_db);
        self.tone_on = tone_db.abs() > 0.05;
        self.tone.high_shelf(2500.0, tone_db, fs);
    }

    /// Gain reduction of the last sample, dB (≤ 0), for tests and meters.
    pub fn reduction_db(&self) -> f32 {
        self.gr
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let a = x.abs();
        self.env += (a - self.env) * if a > self.env { self.det_a } else { self.det_r };
        self.env = flush(self.env);
        let over = 20.0 * (self.env + 1e-9).log10() - self.thresh;
        let red = if over <= -KNEE_DB * 0.5 {
            0.0
        } else if over >= KNEE_DB * 0.5 {
            over * self.slope
        } else {
            let o = over + KNEE_DB * 0.5;
            self.slope * o * o / (2.0 * KNEE_DB)
        };
        let target = -red;
        self.gr += (target - self.gr) * if target < self.gr { self.att } else { self.rel };
        let y = x * db(self.gr) * self.makeup;
        if self.tone_on {
            self.tone.tick(y)
        } else {
            y
        }
    }
}

// ------------------------------------------------------------------ EQ slot

const EQ_N: usize = 10;

/// One Katana EQ: PARAMETRIC (low cut, low shelf, two peaks, high shelf, high cut, level)
/// or GE-10 (ten peaks at 31.25 Hz · 2^i, level). Values as in `params::EqKey`. Every filter
/// keeps its place whatever the gains, so dragging a knob through 0 dB never shifts state
/// from one filter to another.
pub struct EqSlot {
    fs: f32,
    f: [Biquad; EQ_N],
    n: usize,
    level: f32,
    kind: i32,
    key: [f32; 11],
}

impl EqSlot {
    pub fn new(fs: f32) -> Self {
        EqSlot { fs, f: [Biquad::new(); EQ_N], n: 0, level: 1.0, kind: -1, key: [f32::NAN; 11] }
    }

    pub fn set(&mut self, kind: i32, v: &[f32; 11]) {
        if kind == self.kind && *v == self.key {
            return;
        }
        if kind != self.kind {
            for f in &mut self.f {
                f.reset();
            }
        }
        self.kind = kind;
        self.key = *v;
        let fs = self.fs;
        let top = 0.45 * fs;
        if kind == 1 {
            for (i, f) in self.f.iter_mut().enumerate() {
                f.peak((31.25 * 2f32.powi(i as i32)).min(top), 1.4, v[i].clamp(-24.0, 24.0), fs);
            }
            self.n = 10;
        } else {
            let [hp1, hp2, ls, lm, hm, hs, lp1, lp2, ..] = &mut self.f;
            if v[0] > 0.0 {
                hp1.highpass(v[0].min(top), 0.541, fs);
                hp2.highpass(v[0].min(top), 1.307, fs);
            } else {
                identity(hp1, fs);
                identity(hp2, fs);
            }
            ls.low_shelf(120.0, v[1].clamp(-20.0, 20.0), fs);
            lm.peak(v[2].clamp(20.0, top), v[3].clamp(0.3, 16.0), v[4].clamp(-20.0, 20.0), fs);
            hm.peak(v[5].clamp(20.0, top), v[6].clamp(0.3, 16.0), v[7].clamp(-20.0, 20.0), fs);
            hs.high_shelf(6000.0, v[8].clamp(-20.0, 20.0), fs);
            if v[9] > 0.0 && v[9] < top {
                lp1.lowpass(v[9], 0.541, fs);
                lp2.lowpass(v[9], 1.307, fs);
            } else {
                identity(lp1, fs);
                identity(lp2, fs);
            }
            self.n = 8;
        }
        self.level = db(v[10].clamp(-24.0, 24.0));
    }

    #[inline]
    pub fn tick(&mut self, mut x: f32) -> f32 {
        for f in &mut self.f[..self.n] {
            x = f.tick(x);
        }
        x * self.level
    }
}

// ------------------------------------------------------------------ delay

/// Katana delay types (`PRM_DLY_TYPE` codes) the engine voices; other codes play DIGITAL.
pub const DELAY_TYPES: [(i32, &str); 8] = [
    (0, "Digital"),
    (1, "Pan"),
    (2, "Stereo"),
    (6, "Reverse"),
    (7, "Analog"),
    (8, "Tape Echo"),
    (9, "Modulate"),
    (10, "SDE-3000"),
];

const REVERSE: i32 = 6;
const ANALOG: i32 = 7;
const TAPE: i32 = 8;
const MODULATE: i32 = 9;
const SDE: i32 = 10;

/// Stereo delay up to 2 s. The record side carries the type's colour (band limits,
/// saturation), so the first repeat is already dark on ANALOG / TAPE and every pass darkens
/// further; the read side carries its motion (tape wow and flutter, BBD drift, chorus).
/// REVERSE plays each delay-time chunk backwards through two crossfaded heads.
pub struct Echo {
    fs: f32,
    /// left line holds 4.2 s: the reverse heads read up to twice the delay time back
    l: DelayLine,
    r: DelayLine,
    time: Smooth,
    lp: [Biquad; 2],
    hp: [Biquad; 2],
    ty: i32,
    hicut: f32,
    sat: bool,
    ph_slow: f32,
    ph_fast: f32,
    rev: f32,
    /// the time smoother starts at the first delay time asked for, not gliding from a default
    primed: bool,
}

impl Echo {
    pub fn new(fs: f32) -> Self {
        Echo {
            fs,
            l: DelayLine::new((4.2 * fs) as usize),
            r: DelayLine::new((2.1 * fs) as usize),
            time: Smooth::new(0.38 * fs, 120.0, fs),
            lp: [Biquad::new(); 2],
            hp: [Biquad::new(); 2],
            ty: -1,
            hicut: f32::NAN,
            sat: false,
            ph_slow: 0.0,
            ph_fast: 0.0,
            rev: 0.0,
            primed: false,
        }
    }

    /// Silence both lines (after a non-finite sample).
    pub fn reset(&mut self) {
        self.l.clear();
        self.r.clear();
        for f in self.lp.iter_mut().chain(self.hp.iter_mut()) {
            f.reset();
        }
    }

    /// `hicut`: the patch's HIGH CUT in Hz, 0 = flat.
    pub fn set(&mut self, ty: i32, hicut: f32) {
        if ty == self.ty && hicut == self.hicut {
            return;
        }
        if ty != self.ty {
            self.rev = 0.0;
        }
        self.ty = ty;
        self.hicut = hicut;
        let fs = self.fs;
        let cut = if hicut > 0.0 { hicut } else { 20000.0 };
        let (lp, hp, sat) = match ty {
            ANALOG => (cut.min(2800.0), 90.0, true),
            TAPE => (cut.min(4200.0), 160.0, true),
            SDE => (cut.min(12000.0), 40.0, false),
            _ => (cut.min(16000.0), 25.0, false),
        };
        for (l, h) in self.lp.iter_mut().zip(self.hp.iter_mut()) {
            l.lowpass(lp.min(0.45 * fs), 0.6, fs);
            h.highpass(hp, 0.6, fs);
        }
        self.sat = sat;
    }

    /// `fb` 0..1 of the Katana range, `wet` / `direct` linear (1 = unity).
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, ms: f32, fb: f32, wet: f32, direct: f32, mod_hz: f32, mod_depth: f32) -> (f32, f32) {
        let fs = self.fs;
        let want = ms.clamp(1.0, 2000.0) * 0.001 * fs;
        if !self.primed {
            self.primed = true;
            self.time.v = want;
        }
        let n = self.time.next(want);
        let fb = fb.clamp(0.0, 1.0) * 0.98;
        if self.ty == REVERSE {
            return self.reverse(l, r, n, fb, wet, direct);
        }
        // read-side motion, in samples
        let (ml, mr) = match self.ty {
            TAPE => {
                self.ph_slow = (self.ph_slow + 0.55 / fs).fract();
                self.ph_fast = (self.ph_fast + 6.5 / fs).fract();
                let m = 0.0005 * fs * (self.ph_slow * TAU).sin() + 0.00005 * fs * (self.ph_fast * TAU).sin();
                (m, m)
            }
            ANALOG | SDE => {
                self.ph_slow = (self.ph_slow + if self.ty == ANALOG { 0.3 } else { 0.45 } / fs).fract();
                let a = if self.ty == ANALOG { 0.0003 } else { 0.00015 } * fs;
                (a * (self.ph_slow * TAU).sin(), a * ((self.ph_slow + 0.25) * TAU).sin())
            }
            MODULATE => {
                self.ph_slow = (self.ph_slow + mod_hz.clamp(0.05, 10.0) / fs).fract();
                let a = 0.003 * fs * mod_depth.clamp(0.0, 1.0);
                (a * (1.0 + (self.ph_slow * TAU).sin()), a * (1.0 + ((self.ph_slow + 0.25) * TAU).sin()))
            }
            _ => (0.0, 0.0),
        };
        let dl = self.l.read(n + ml);
        // STEREO: the right side a touch longer, so the repeats spread instead of stacking
        let dr = self.r.read(if self.ty == 2 { n * 1.012 } else { n } + mr);
        let (wl, wr) = if self.ty == 1 { ((l + r) * 0.5 + dr * fb, dl * fb) } else { (l + dl * fb, r + dr * fb) };
        let (wl, wr) = (self.color(0, wl), self.color(1, wr));
        self.l.push(flush(wl));
        self.r.push(flush(wr));
        (direct * l + wet * dl, direct * r + wet * dr)
    }

    #[inline]
    fn color(&mut self, ch: usize, x: f32) -> f32 {
        let y = self.lp[ch].tick(self.hp[ch].tick(x));
        if self.sat {
            soft(y * 1.25) * 0.8
        } else {
            y
        }
    }

    #[inline]
    fn reverse(&mut self, l: f32, r: f32, n: f32, fb: f32, wet: f32, direct: f32) -> (f32, f32) {
        let n = n.max(16.0);
        self.rev += 1.0;
        while self.rev >= n {
            self.rev -= n;
        }
        // head A reads the previous chunk backwards (delay 0 → 2n over the chunk), head B
        // the same half a chunk later; sin² windows sum to one
        let pa = self.rev / n;
        let pb = (pa + 0.5).fract();
        let wa = (PI * pa).sin();
        let wb = (PI * pb).sin();
        let y = self.l.read(2.0 * pa * n) * wa * wa + self.l.read(2.0 * pb * n) * wb * wb;
        let x = self.color(0, (l + r) * 0.5 + y * fb);
        self.l.push(flush(x));
        (direct * l + wet * y, direct * r + wet * y)
    }
}

// ------------------------------------------------------------------ reverb

/// Katana reverb types (`PRM_REVERB_TYPE` codes) the engine voices; 0 plays ROOM, 2 HALL.
pub const REVERB_TYPES: [(i32, &str); 5] = [(1, "Room"), (3, "Hall"), (4, "Plate"), (5, "Spring"), (6, "Modulate")];

const FDN_N: usize = 8;
const FDN_LEN: [f32; FDN_N] = [1031.0, 1327.0, 1523.0, 1789.0, 2053.0, 2311.0, 2617.0, 2903.0];
const DIFF_LEN: [f32; 4] = [142.0, 107.0, 379.0, 277.0];
/// input polarity per line: decorrelates the lines from the first Hadamard row
const INJ_SIGN: [f32; FDN_N] = [1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0];
const INJ: f32 = 0.35;
/// longest line scale of any type, plus room for the modulation swing
const MAX_SCALE: f32 = 1.4;

/// 8-line feedback delay network, Hadamard mixing, 4 input diffusers, stereo pre-delay.
/// Every line's length drifts slowly (a few samples, ~0.5 Hz, a different rate per line):
/// that breaks the fixed comb resonances that make a static FDN ring metallic. MODULATE
/// swings the lengths by ~0.6 ms for the Katana's chorused, washy tail.
///
/// Level: at EFFECT LEVEL 100 the tail of steady playing sits 6 dB under the dry signal for a
/// 2 s reverb time, louder for longer times (the energy of a real room grows with RT60).
pub struct Verb {
    fs: f32,
    lines: [DelayLine; FDN_N],
    len: [f32; FDN_N],
    g: [f32; FDN_N],
    damp: [f32; FDN_N],
    damp_a: f32,
    diff: [DelayLine; 4],
    diff_len: [f32; 4],
    diff_g: f32,
    pre_l: DelayLine,
    pre_r: DelayLine,
    chirp: [Biquad; 4],
    /// per-line quadrature oscillators (cos, sin) and their per-sample rotation
    osc: [(f32, f32); FDN_N],
    rot: [(f32, f32); FDN_N],
    osc_n: u32,
    depth: f32,
    cut_l: [Biquad; 2],
    cut_r: [Biquad; 2],
    norm: f32,
    ty: i32,
    key: [f32; 5],
}

impl Verb {
    pub fn new(fs: f32) -> Self {
        let k = fs / 48000.0;
        Verb {
            fs,
            lines: std::array::from_fn(|i| DelayLine::new((FDN_LEN[i] * MAX_SCALE * k) as usize + 64)),
            len: [1.0; FDN_N],
            g: [0.0; FDN_N],
            damp: [0.0; FDN_N],
            damp_a: 0.0,
            diff: std::array::from_fn(|i| DelayLine::new((DIFF_LEN[i] * k) as usize + 4)),
            diff_len: std::array::from_fn(|i| (DIFF_LEN[i] * k).round()),
            diff_g: 0.6,
            pre_l: DelayLine::new((0.52 * fs) as usize),
            pre_r: DelayLine::new((0.52 * fs) as usize),
            chirp: [Biquad::new(); 4],
            osc: std::array::from_fn(|i| ((i as f32 * 0.9).cos(), (i as f32 * 0.9).sin())),
            rot: [(1.0, 0.0); FDN_N],
            osc_n: 0,
            depth: 0.0,
            cut_l: [Biquad::new(); 2],
            cut_r: [Biquad::new(); 2],
            norm: 0.0,
            ty: -1,
            key: [f32::NAN; 5],
        }
    }

    /// Silence every line (after a non-finite sample: never let NaN/inf ring on).
    pub fn reset(&mut self) {
        for l in self.lines.iter_mut().chain(self.diff.iter_mut()) {
            l.clear();
        }
        self.pre_l.clear();
        self.pre_r.clear();
        self.damp = [0.0; FDN_N];
        for b in self.chirp.iter_mut().chain(self.cut_l.iter_mut()).chain(self.cut_r.iter_mut()) {
            b.reset();
        }
    }

    /// `rt` RT60 in s, `density` 0..1, cuts in Hz (0 = flat).
    pub fn set(&mut self, ty: i32, rt: f32, density: f32, low_cut: f32, high_cut: f32) {
        let key = [ty as f32, rt, density, low_cut, high_cut];
        if key == self.key {
            return;
        }
        self.key = key;
        let fs = self.fs;
        let k = fs / 48000.0;
        if ty != self.ty {
            self.ty = ty;
            for l in &mut self.lines {
                l.clear();
            }
            self.damp = [0.0; FDN_N];
        }
        // (line scale, in-loop damping Hz, length swing in samples at 48 kHz, LFO Hz)
        let (scale, damp_hz, swing, lfo) = match ty {
            0 | 1 => (0.55, 6500.0, 3.0, 0.7),
            2 | 3 => (1.25, 5000.0, 5.0, 0.5),
            4 => (0.85, 9000.0, 4.0, 0.9),
            5 => (0.42, 4200.0, 2.0, 0.6),
            _ => (1.3, 6000.0, 28.0, 0.55),
        };
        let rt = rt.clamp(0.1, 10.0);
        for i in 0..FDN_N {
            self.len[i] = FDN_LEN[i] * scale * k;
            self.g[i] = 10f32.powf(-3.0 * self.len[i] / (rt * fs));
            let w = TAU * lfo * (0.75 + 0.09 * i as f32) / fs;
            self.rot[i] = (w.cos(), w.sin());
        }
        self.depth = swing * k;
        self.damp_a = (-TAU * damp_hz / fs).exp();
        let plate = ty == 4;
        self.diff_g = (0.35 + 0.35 * density.clamp(0.0, 1.0) + if plate { 0.05 } else { 0.0 }).min(0.75);
        for c in &mut self.chirp {
            c.allpass(1700.0, 0.35, fs);
        }
        for lc in [&mut self.cut_l[0], &mut self.cut_r[0]] {
            if low_cut > 0.0 {
                lc.highpass(low_cut, 0.707, fs);
            } else {
                identity(lc, fs);
            }
        }
        for hc in [&mut self.cut_l[1], &mut self.cut_r[1]] {
            if high_cut > 0.0 && high_cut < 0.45 * fs {
                hc.lowpass(high_cut, 0.707, fs);
            } else {
                identity(hc, fs);
            }
        }
        // steady-state energy of a unitary FDN fed INJ·x per line: INJ² / (1 − g²) per line,
        // four lines summed per side
        let g2 = self.g.iter().map(|g| g * g).sum::<f32>() / FDN_N as f32;
        let raw = 2.0 * INJ / (1.0 - g2).max(1e-4).sqrt();
        // ×1.3: the in-loop damping drains ~2.3 dB of a guitar-band signal (measured)
        self.norm = 1.3 * 0.5 * (rt / 2.0).sqrt() / raw;
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, pre_ms: f32, wet: f32, direct: f32) -> (f32, f32) {
        let fs = self.fs;
        self.pre_l.push(l);
        self.pre_r.push(r);
        let d = (pre_ms * 0.001 * fs).clamp(0.0, 0.5 * fs);
        let (xl, xr) = (self.pre_l.read(d), self.pre_r.read(d));
        let mut x = (xl + xr) * 0.5;
        let side = (xl - xr) * 0.5;
        if self.ty == 5 {
            for c in &mut self.chirp {
                x = c.tick(x);
            }
        }
        for i in 0..4 {
            let dl = self.diff[i].read(self.diff_len[i] - 1.0);
            let v = x + dl * self.diff_g;
            self.diff[i].push(flush(v));
            x = dl - v * self.diff_g;
        }
        // advance the length oscillators; renormalise now and then against rounding drift
        self.osc_n = self.osc_n.wrapping_add(1);
        let renorm = self.osc_n % 4096 == 0;
        let mut outs = [0f32; FDN_N];
        for i in 0..FDN_N {
            let (c, s) = self.osc[i];
            let (rc, rs) = self.rot[i];
            let (mut c2, mut s2) = (c * rc - s * rs, c * rs + s * rc);
            if renorm {
                let m = 1.0 / (c2 * c2 + s2 * s2).sqrt();
                c2 *= m;
                s2 *= m;
            }
            self.osc[i] = (c2, s2);
            let y = self.lines[i].read(self.len[i] + self.depth * (1.0 + s2));
            self.damp[i] = flush(y + (self.damp[i] - y) * self.damp_a);
            outs[i] = self.damp[i] * self.g[i];
        }
        // Hadamard 8x8 (fast Walsh-Hadamard), normalised
        let mut h = outs;
        let mut step = 1;
        while step < FDN_N {
            for i in (0..FDN_N).step_by(step * 2) {
                for j in i..i + step {
                    let (a, b) = (h[j], h[j + step]);
                    h[j] = a + b;
                    h[j + step] = a - b;
                }
            }
            step *= 2;
        }
        let hn = 1.0 / (FDN_N as f32).sqrt();
        for i in 0..FDN_N {
            let side_in = if i % 2 == 0 { side } else { -side };
            self.lines[i].push(flush(h[i] * hn + (x * INJ_SIGN[i] + side_in * 0.5) * INJ));
        }
        let mut wl = (outs[0] + outs[2] + outs[4] + outs[6]) * self.norm;
        let mut wr = (outs[1] + outs[3] + outs[5] + outs[7]) * self.norm;
        for f in &mut self.cut_l {
            wl = f.tick(wl);
        }
        for f in &mut self.cut_r {
            wr = f.tick(wr);
        }
        (direct * l + wet * wl, direct * r + wet * wr)
    }
}
