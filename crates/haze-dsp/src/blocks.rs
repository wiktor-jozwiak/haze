//! The effect blocks. Each owns its state, preallocated in `new(fs)`; `process` never allocates.

use crate::filters::{db, flush, Biquad, DelayLine, Oversampler4, Smooth};
use crate::params::EQ_FREQS;
use std::f32::consts::{PI, TAU};

// ------------------------------------------------------------------ noise gate

/// Envelope gate with hysteresis (opens at threshold, closes 6 dB below) and smooth gain.
pub struct Gate {
    env: f32,
    gain: f32,
    open: bool,
    att: f32,
    rel_env: f32,
    rel_gain: f32,
    hold: u32,
    hold_max: u32,
}

impl Gate {
    pub fn new(fs: f32) -> Self {
        let k = |ms: f32| 1.0 - (-1.0 / (ms * 0.001 * fs)).exp();
        Gate { env: 0.0, gain: 0.0, open: false, att: k(0.5), rel_env: k(30.0), rel_gain: k(60.0), hold: 0, hold_max: (0.04 * fs) as u32 }
    }

    #[inline]
    pub fn tick(&mut self, x: f32, thresh_db: f32) -> f32 {
        let a = x.abs();
        self.env += (a - self.env) * if a > self.env { self.att } else { self.rel_env };
        let open_t = db(thresh_db);
        if self.env > open_t {
            self.open = true;
            self.hold = self.hold_max;
        } else if self.env < open_t * 0.5 {
            if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.open = false;
            }
        }
        let target = if self.open { 1.0 } else { 0.0 };
        self.gain += (target - self.gain) * if self.open { 0.2 } else { self.rel_gain };
        x * self.gain
    }
}

// ------------------------------------------------------------------ drive pedals

pub const DRIVE_MODELS: [&str; 6] = ["T-Scream", "Klon", "Blues Driver", "RAT", "Muff", "Metal Zone"];

/// Overdrive / distortion / fuzz: pre-emphasis, 4x oversampled waveshaper, tone, level.
pub struct Drive {
    fs: f32,
    os: Oversampler4,
    pre: Biquad,
    pre2: Biquad,
    post: Biquad,
    post2: Biquad,
    dc: Biquad,
    model: i32,
    tone: f32,
}

impl Drive {
    pub fn new(fs: f32) -> Self {
        let mut d = Drive {
            fs,
            os: Oversampler4::new(fs),
            pre: Biquad::new(),
            pre2: Biquad::new(),
            post: Biquad::new(),
            post2: Biquad::new(),
            dc: Biquad::new(),
            model: -1,
            tone: -1.0,
        };
        d.dc.highpass(20.0, 0.707, fs);
        d
    }

    fn configure(&mut self, model: i32, tone: f32) {
        if model == self.model && (tone - self.tone).abs() < 1e-3 {
            return;
        }
        self.model = model;
        self.tone = tone;
        let fs = self.fs;
        let tf = 800.0 * 10f32.powf(tone * 1.1); // 800 Hz .. ~10 kHz
        match model {
            // T-Scream: bass cut before clipping, mid hump
            0 => {
                self.pre.highpass(720.0, 0.707, fs);
                self.pre2.peak(900.0, 0.8, 6.0, fs);
                self.post.lowpass(tf.min(6000.0), 0.707, fs);
                self.post2.peak(1200.0, 0.7, 2.0, fs);
            }
            // Klon: wide-band, clean blend handled in shaper mix
            1 => {
                self.pre.highpass(160.0, 0.707, fs);
                self.pre2.peak(1000.0, 0.5, 3.0, fs);
                self.post.lowpass(tf, 0.707, fs);
                self.post2.high_shelf(3000.0, (tone - 0.5) * 8.0, fs);
            }
            // Blues Driver: transparent, slight low cut
            2 => {
                self.pre.highpass(120.0, 0.707, fs);
                self.pre2.peak(700.0, 0.6, 2.0, fs);
                self.post.lowpass(tf, 0.6, fs);
                self.post2.high_shelf(2500.0, (tone - 0.5) * 10.0, fs);
            }
            // RAT: hard clip, "filter" knob is a variable low-pass
            3 => {
                self.pre.highpass(60.0, 0.707, fs);
                self.pre2.peak(1800.0, 0.7, 4.0, fs);
                self.post.lowpass(475.0 + tone * 9000.0, 0.707, fs);
                self.post2.peak(200.0, 0.7, 0.0, fs);
            }
            // Muff: scooped mids
            4 => {
                self.pre.highpass(80.0, 0.707, fs);
                self.pre2.peak(300.0, 0.6, 3.0, fs);
                self.post.lowpass(tf.min(5000.0), 0.5, fs);
                self.post2.peak(800.0, 0.5, -9.0, fs);
            }
            // Metal Zone: tight bass, aggressive upper mids
            _ => {
                self.pre.highpass(150.0, 0.707, fs);
                self.pre2.peak(2200.0, 1.0, 9.0, fs);
                self.post.lowpass(tf.max(4000.0), 0.707, fs);
                self.post2.peak(600.0, 0.8, -4.0, fs);
            }
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32, model: i32, amt: f32, tone: f32, level: f32) -> f32 {
        self.configure(model, tone);
        let g = match model {
            0 => db(6.0 + amt * 34.0),
            1 => db(amt * 30.0),
            2 => db(3.0 + amt * 34.0),
            3 => db(10.0 + amt * 44.0),
            4 => db(18.0 + amt * 40.0),
            _ => db(20.0 + amt * 42.0),
        };
        let pre = self.pre2.tick(self.pre.tick(x)) * g;
        let y = self.os.process(pre, |u| match model {
            // soft symmetric diode pair
            0 => u.tanh(),
            // germanium-ish soft clip
            1 => u / (1.0 + u.abs()),
            // asymmetric soft clip
            2 => if u > 0.0 { u.tanh() } else { (u * 0.8).tanh() * 1.1 },
            // op-amp into hard silicon clip
            3 => u.clamp(-0.7, 0.7) * 1.3,
            // two cascaded fuzz stages
            4 => ((u * 2.0).tanh() * 3.0).tanh(),
            _ => (u * 1.5).clamp(-1.0, 1.0).tanh() * 1.2,
        });
        let y = self.post2.tick(self.post.tick(y));
        let y = if model == 1 { y * 0.7 + x * (1.0 - amt) * 1.2 } else { y };
        flush(self.dc.tick(y) * db(-12.0 + level * 24.0) * 0.6)
    }
}

// ------------------------------------------------------------------ amp

pub const VOICINGS: [&str; 7] = ["Acoustic", "Clean", "Pushed", "Crunch", "Lead", "Brown", "Modern Metal"];

/// Preamp (1–2 oversampled triode-like stages), passive-style tone stack, power amp with
/// sag compression, presence and resonance shelving.
pub struct Amp {
    fs: f32,
    os1: Oversampler4,
    os2: Oversampler4,
    hp_in: Biquad,
    voice: Biquad,
    inter: Biquad,
    bass: Biquad,
    mid: Biquad,
    treble: Biquad,
    presence: Biquad,
    resonance: Biquad,
    dc: Biquad,
    sag_env: f32,
    sag_k: f32,
    key: [f32; 7],
}

impl Amp {
    pub fn new(fs: f32) -> Self {
        let mut a = Amp {
            fs,
            os1: Oversampler4::new(fs),
            os2: Oversampler4::new(fs),
            hp_in: Biquad::new(),
            voice: Biquad::new(),
            inter: Biquad::new(),
            bass: Biquad::new(),
            mid: Biquad::new(),
            treble: Biquad::new(),
            presence: Biquad::new(),
            resonance: Biquad::new(),
            dc: Biquad::new(),
            sag_env: 0.0,
            sag_k: 1.0 - (-1.0 / (0.08 * fs)).exp(),
            key: [-1.0; 7],
        };
        a.dc.highpass(15.0, 0.707, fs);
        a
    }

    #[allow(clippy::too_many_arguments)]
    fn configure(&mut self, v: i32, bass: f32, mid: f32, treble: f32, pres: f32, res: f32) {
        let key = [v as f32, bass, mid, treble, pres, res, 0.0];
        if key.iter().zip(self.key.iter()).all(|(a, b)| (a - b).abs() < 1e-3) {
            return;
        }
        self.key = key;
        let fs = self.fs;
        let (hp, vf, vq, vdb) = match v {
            0 => (40.0, 3000.0, 0.5, 4.0),
            1 => (70.0, 2000.0, 0.6, 2.0),
            2 => (90.0, 1200.0, 0.7, 3.0),
            3 => (110.0, 900.0, 0.8, 4.0),
            4 => (140.0, 1400.0, 0.9, 5.0),
            5 => (120.0, 700.0, 0.7, 6.0),
            _ => (180.0, 1800.0, 1.0, 7.0),
        };
        self.hp_in.highpass(hp, 0.707, fs);
        self.voice.peak(vf, vq, vdb, fs);
        self.inter.lowpass(if v >= 4 { 6500.0 } else { 9000.0 }, 0.707, fs);
        let mid_f = if v >= 5 { 650.0 } else { 500.0 };
        self.bass.low_shelf(120.0, (bass - 0.5) * 24.0, fs);
        self.mid.peak(mid_f, 0.7, (mid - 0.5) * 20.0, fs);
        self.treble.high_shelf(2800.0, (treble - 0.5) * 24.0, fs);
        self.presence.high_shelf(4500.0, (pres - 0.5) * 16.0, fs);
        self.resonance.peak(95.0, 1.2, (res - 0.5) * 14.0, fs);
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn tick(&mut self, x: f32, v: i32, gain: f32, bass: f32, mid: f32, treble: f32, pres: f32, res: f32, master: f32, sag: f32) -> f32 {
        self.configure(v, bass, mid, treble, pres, res);
        let (g_lo, g_hi, two_stage, bias) = match v {
            0 => (0.0, 12.0, false, 0.0),
            1 => (0.0, 22.0, false, 0.05),
            2 => (8.0, 30.0, false, 0.1),
            3 => (12.0, 38.0, true, 0.15),
            4 => (16.0, 46.0, true, 0.2),
            5 => (20.0, 50.0, true, 0.25),
            _ => (24.0, 56.0, true, 0.2),
        };
        let g = db(g_lo + (g_hi - g_lo) * gain);
        let u = self.voice.tick(self.hp_in.tick(x)) * g;
        // triode: asymmetric soft clip with grid bias
        let mut y = self.os1.process(u, |s| (s + bias).tanh() - bias.tanh());
        if two_stage {
            let s2 = self.inter.tick(y) * db(6.0 + gain * 14.0);
            y = self.os2.process(s2, |s| (s * 1.2 + bias * 0.5).tanh() - (bias * 0.5).tanh());
        }
        // tone stack
        y = self.treble.tick(self.mid.tick(self.bass.tick(y)));
        // power amp with supply sag
        self.sag_env += (y.abs() - self.sag_env) * self.sag_k;
        let sag_gain = 1.0 / (1.0 + sag * self.sag_env * 3.0);
        let p = y * db(-6.0 + master * 22.0) * sag_gain;
        let p = (p * 0.9).tanh();
        let p = self.resonance.tick(self.presence.tick(p));
        flush(self.dc.tick(p) * 0.7)
    }
}

// ------------------------------------------------------------------ 10-band EQ

pub struct Eq10 {
    fs: f32,
    bands: [Biquad; 10],
    gains: [f32; 10],
}

impl Eq10 {
    pub fn new(fs: f32) -> Self {
        Eq10 { fs, bands: [Biquad::new(); 10], gains: [f32::NAN; 10] }
    }
    pub fn set(&mut self, i: usize, gain_db: f32) {
        if self.gains[i] != gain_db {
            self.gains[i] = gain_db;
            let f = EQ_FREQS[i].min(self.fs * 0.45);
            self.bands[i].peak(f, 1.4, gain_db, self.fs);
        }
    }
    #[inline]
    pub fn tick(&mut self, mut x: f32) -> f32 {
        for (b, g) in self.bands.iter_mut().zip(self.gains) {
            if g != 0.0 {
                x = b.tick(x);
            }
        }
        x
    }
}

// ------------------------------------------------------------------ modulation

pub const MOD_TYPES: [&str; 5] = ["Chorus", "Flanger", "Phaser", "Tremolo", "Rotary"];

/// Mono in, stereo out: chorus / flanger (modulated delay), phaser (6 allpasses),
/// tremolo (amplitude), rotary (doppler + panning).
pub struct Modulation {
    fs: f32,
    phase: f32,
    dl: DelayLine,
    fb: f32,
    ap_l: [Biquad; 6],
    ap_r: [Biquad; 6],
    ph_fb_l: f32,
    ph_fb_r: f32,
    ap_tick: u32,
}

impl Modulation {
    pub fn new(fs: f32) -> Self {
        Modulation {
            fs,
            phase: 0.0,
            dl: DelayLine::new((0.05 * fs) as usize),
            fb: 0.0,
            ap_l: [Biquad::new(); 6],
            ap_r: [Biquad::new(); 6],
            ph_fb_l: 0.0,
            ph_fb_r: 0.0,
            ap_tick: 0,
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f32, ty: i32, rate: f32, depth: f32, mix: f32) -> (f32, f32) {
        let fs = self.fs;
        self.phase = (self.phase + rate / fs).fract();
        let sl = (self.phase * TAU).sin();
        let sr = ((self.phase + 0.25) * TAU).sin();
        match ty {
            0 | 1 => {
                let (base, span, fbk) = if ty == 0 { (0.012, 0.006, 0.0) } else { (0.0015, 0.0035, 0.55) };
                self.dl.push(x + self.fb * fbk);
                let dlf = (base + span * depth * (0.5 + 0.5 * sl)) * fs;
                let drf = (base + span * depth * (0.5 + 0.5 * sr)) * fs;
                let wl = self.dl.read(dlf);
                let wr = self.dl.read(drf);
                self.fb = flush(wl);
                (x * (1.0 - mix * 0.5) + wl * mix, x * (1.0 - mix * 0.5) + wr * mix)
            }
            2 => {
                // retune the allpasses every 16 samples: sweep 200 Hz .. 2.2 kHz
                if self.ap_tick % 16 == 0 {
                    let fl = 200.0 * 11f32.powf(depth * (0.5 + 0.5 * sl));
                    let fr = 200.0 * 11f32.powf(depth * (0.5 + 0.5 * sr));
                    for (a, b) in self.ap_l.iter_mut().zip(self.ap_r.iter_mut()) {
                        a.allpass(fl, 0.7, fs);
                        b.allpass(fr, 0.7, fs);
                    }
                }
                self.ap_tick = self.ap_tick.wrapping_add(1);
                let mut l = x + self.ph_fb_l * 0.4;
                let mut r = x + self.ph_fb_r * 0.4;
                for (a, b) in self.ap_l.iter_mut().zip(self.ap_r.iter_mut()) {
                    l = a.tick(l);
                    r = b.tick(r);
                }
                self.ph_fb_l = flush(l);
                self.ph_fb_r = flush(r);
                (x * (1.0 - mix * 0.5) + l * mix, x * (1.0 - mix * 0.5) + r * mix)
            }
            3 => {
                let g = 1.0 - depth * (0.5 - 0.5 * sl);
                (x * g, x * g)
            }
            _ => {
                // rotary horn: doppler from a short modulated delay, opposite-phase panning
                self.dl.push(x);
                let w = self.dl.read((0.0008 + 0.0006 * depth * sl) * fs);
                let pan = 0.5 + 0.5 * sl * depth;
                let am = 1.0 - 0.3 * depth * (0.5 - 0.5 * sr);
                let y = (x * (1.0 - mix) + w * mix) * am;
                (y * (1.0 - pan) * 1.4, y * pan * 1.4)
            }
        }
    }
}

// ------------------------------------------------------------------ delay

/// Stereo delay, up to 2 s, damped feedback; ping-pong crosses the feedback between sides.
pub struct Delay {
    fs: f32,
    l: DelayLine,
    r: DelayLine,
    damp_l: Biquad,
    damp_r: Biquad,
    time: Smooth,
    lfo: f32,
}

impl Delay {
    pub fn new(fs: f32) -> Self {
        let mut d = Delay {
            fs,
            l: DelayLine::new((2.1 * fs) as usize),
            r: DelayLine::new((2.1 * fs) as usize),
            damp_l: Biquad::new(),
            damp_r: Biquad::new(),
            time: Smooth::new(0.38 * fs, 120.0, fs),
            lfo: 0.0,
        };
        d.damp_l.lowpass(4500.0, 0.6, fs);
        d.damp_r.lowpass(4500.0, 0.6, fs);
        d
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, ms: f32, fb: f32, mix: f32, ping: bool) -> (f32, f32) {
        self.lfo = (self.lfo + 0.6 / self.fs).fract();
        let t = self.time.next(ms.clamp(10.0, 2000.0) * 0.001 * self.fs) + (self.lfo * TAU).sin() * 4.0;
        let dl = self.l.read(t);
        let dr = self.r.read(t);
        let fb = fb.clamp(0.0, 0.95);
        if ping {
            self.l.push(flush((l + r) * 0.5 + self.damp_r.tick(dr) * fb));
            self.r.push(flush(self.damp_l.tick(dl) * fb));
        } else {
            self.l.push(flush(l + self.damp_l.tick(dl) * fb));
            self.r.push(flush(r + self.damp_r.tick(dr) * fb));
        }
        (l + dl * mix, r + dr * mix)
    }
}

// ------------------------------------------------------------------ reverb

pub const REVERB_TYPES: [&str; 4] = ["Plate", "Hall", "Spring", "Shimmer"];

const FDN_N: usize = 8;
const FDN_LEN: [usize; FDN_N] = [1031, 1327, 1523, 1789, 2053, 2311, 2617, 2903];

/// 8-line feedback delay network (Hadamard mixing) with input diffusion. Types scale line
/// lengths, damping and pre-delay; shimmer feeds an octave-up pitch shifter into the loop.
pub struct Reverb {
    fs: f32,
    lines: [DelayLine; FDN_N],
    damp: [Biquad; FDN_N],
    diff: [DelayLine; 4],
    pre: DelayLine,
    shim: DelayLine,
    shim_ph: f32,
    ty: i32,
    scale: f32,
    chirp: [Biquad; 3],
}

impl Reverb {
    pub fn new(fs: f32) -> Self {
        let k = fs / 48000.0 * 1.6;
        Reverb {
            fs,
            lines: std::array::from_fn(|i| DelayLine::new((FDN_LEN[i] as f32 * k) as usize + 8)),
            damp: [Biquad::new(); FDN_N],
            diff: std::array::from_fn(|i| DelayLine::new([142, 107, 379, 277][i] * 2)),
            pre: DelayLine::new((0.1 * fs) as usize),
            shim: DelayLine::new((0.2 * fs) as usize),
            shim_ph: 0.0,
            ty: -1,
            scale: 1.0,
            chirp: [Biquad::new(); 3],
        }
    }

    fn configure(&mut self, ty: i32) {
        if ty == self.ty {
            return;
        }
        self.ty = ty;
        let fs = self.fs;
        let (scale, damp) = match ty {
            0 => (0.8, 7000.0),
            1 => (1.5, 4500.0),
            2 => (0.45, 3500.0),
            _ => (1.4, 6000.0),
        };
        self.scale = scale * fs / 48000.0;
        for d in &mut self.damp {
            d.lowpass(damp, 0.6, fs);
        }
        for c in &mut self.chirp {
            c.allpass(1800.0, 0.3, fs);
        }
        for l in &mut self.lines {
            l.clear();
        }
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r: f32, ty: i32, decay: f32, mix: f32) -> (f32, f32) {
        self.configure(ty);
        let fs = self.fs;
        let mut x = (l + r) * 0.5;
        // pre-delay
        self.pre.push(x);
        x = self.pre.read(match ty { 1 => 0.03, 0 => 0.008, _ => 0.002 } * fs);
        // spring: dispersive chirp
        if ty == 2 {
            for c in &mut self.chirp {
                x = c.tick(x);
            }
        }
        // input diffusion: 4 Schroeder allpasses
        for (i, d) in self.diff.iter_mut().enumerate() {
            let len = [142.0, 107.0, 379.0, 277.0][i];
            let dl = d.read(len - 1.0);
            let v = x + dl * 0.6;
            d.push(flush(v));
            x = dl - v * 0.6;
        }
        let rt = 0.4 + decay * decay * 7.0; // seconds
        let mut outs = [0f32; FDN_N];
        for (i, o) in outs.iter_mut().enumerate() {
            let len = FDN_LEN[i] as f32 * self.scale;
            let g = 10f32.powf(-3.0 * len / (rt * fs));
            *o = self.damp[i].tick(self.lines[i].read(len)) * g;
        }
        // shimmer: two-tap crossfaded pitch shifter (+12 st) on the network output
        let mut sh = 0.0;
        if ty == 3 {
            self.shim.push(outs[0] + outs[3]);
            let w = 0.08 * fs;
            self.shim_ph = (self.shim_ph + 1.0 / w).fract();
            let d1 = w * (1.0 - self.shim_ph);
            let d2 = w * (1.0 - (self.shim_ph + 0.5).fract());
            let x1 = (PI * self.shim_ph).sin();
            let x2 = (PI * (self.shim_ph + 0.5).fract()).sin();
            sh = (self.shim.read(d1) * x1 + self.shim.read(d2) * x2) * 0.35;
        }
        // Hadamard 8x8 (fast Walsh-Hadamard), normalised
        let mut h = outs;
        let mut step = 1;
        while step < FDN_N {
            for i in (0..FDN_N).step_by(step * 2) {
                for j in i..i + step {
                    let a = h[j];
                    let b = h[j + step];
                    h[j] = a + b;
                    h[j + step] = a - b;
                }
            }
            step *= 2;
        }
        let norm = 1.0 / (FDN_N as f32).sqrt();
        for i in 0..FDN_N {
            self.lines[i].push(flush(h[i] * norm + x * 0.35 + sh));
        }
        let wl = outs[0] + outs[2] + outs[4] + outs[6];
        let wr = outs[1] + outs[3] + outs[5] + outs[7];
        (l + wl * mix * 0.5, r + wr * mix * 0.5)
    }
}

// ------------------------------------------------------------------ master limiter

/// Peak limiter: instant attack, 80 ms release, ceiling -0.3 dBFS.
pub struct Limiter {
    gain: f32,
    rel: f32,
}

impl Limiter {
    pub fn new(fs: f32) -> Self {
        Limiter { gain: 1.0, rel: 1.0 - (-1.0 / (0.08 * fs)).exp() }
    }
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let ceil = 0.966;
        let peak = l.abs().max(r.abs());
        let want = if peak * self.gain > ceil { ceil / peak } else { 1.0 };
        if want < self.gain {
            self.gain = want;
        } else {
            self.gain += (want - self.gain) * self.rel;
        }
        (l * self.gain, r * self.gain)
    }
}
