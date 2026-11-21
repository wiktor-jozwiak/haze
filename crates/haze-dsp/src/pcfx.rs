//! Effects only the PC engine has, after the patch's own chain: harmonizer in key, rhythmic
//! effects locked to the practice tempo, shimmer reverb, freeze, lo-fi and tape stop.
//!
//! All buffers are allocated in `new` (about 3.5 MB at 48 kHz); a block that is off costs
//! nothing but its branch, apart from tape stop's two rings, which always record so a stop
//! can wind down what was just played.

use std::f32::consts::{PI, TAU};

use crate::filters::{flush, Biquad, DelayLine};
use crate::fx::Verb;
use crate::fxunit::{FxUnit, Voice};
use crate::params::{Params, P};

/// Rhythm divisions in beats: 1/4, 1/8, dotted 1/8, 1/16, 1/8 triplet.
pub const DIVS: [f32; 5] = [1.0, 0.5, 0.75, 0.25, 1.0 / 3.0];

/// Values of one buffer, read once per `Chain::process`.
#[derive(Clone, Copy, Default)]
pub struct PcVals {
    pub harm: bool,
    harm_v: [f32; 10],
    pub rhy: bool,
    rhy_mode: i32,
    rhy_beats: f32,
    rhy_mix: f32,
    rhy_amt: f32,
    bpm: f32,
    pub shim: bool,
    shim_mix: f32,
    shim_amt: f32,
    shim_ratio: f32,
    pub frz: bool,
    frz_mix: f32,
    frz_tex: f32,
    pub lofi: bool,
    bits: f32,
    rate: f32,
    wow: f32,
    hiss: f32,
    lofi_mix: f32,
    pub stop: bool,
    stop_time: f32,
}

impl PcVals {
    pub fn read(p: &Params) -> Self {
        let harm_mix = p.get(P::PcHarmMix);
        let v2 = p.get(P::PcHarmV2);
        PcVals {
            harm: p.on(P::PcHarmOn),
            harm_v: [
                p.get(P::PcHarmV1),
                harm_mix,
                v2,
                if v2 != 0.0 { harm_mix * 0.8 } else { 0.0 },
                1.0,
                0.0,
                0.0,
                1.0,
                p.get(P::PcHarmKey),
                p.get(P::PcHarmMinor),
            ],
            rhy: p.on(P::PcRhyOn),
            rhy_mode: p.get(P::PcRhyMode) as i32,
            rhy_beats: DIVS[(p.get(P::PcRhyDiv).max(0.0) as usize).min(DIVS.len() - 1)],
            rhy_mix: p.get(P::PcRhyMix),
            rhy_amt: p.get(P::PcRhyAmt),
            bpm: p.get(P::Bpm).clamp(30.0, 300.0),
            shim: p.on(P::PcShimOn),
            shim_mix: p.get(P::PcShimMix),
            shim_amt: p.get(P::PcShimAmt),
            shim_ratio: 2f32.powf(p.get(P::PcShimPitch) / 12.0),
            frz: p.on(P::PcFrzOn),
            frz_mix: p.get(P::PcFrzMix),
            frz_tex: p.get(P::PcFrzTexture),
            lofi: p.on(P::PcLofiOn),
            bits: p.get(P::PcLofiBits).clamp(2.0, 16.0),
            rate: p.get(P::PcLofiRate).clamp(500.0, 48000.0),
            wow: p.get(P::PcLofiWow),
            hiss: p.get(P::PcLofiHiss),
            lofi_mix: p.get(P::PcLofiMix),
            stop: p.on(P::PcStopOn),
            stop_time: p.get(P::PcStopTime).clamp(0.1, 3.0),
        }
    }
}

const FRZ_LEN: f32 = 2.0;
const GRAINS: usize = 4;
const STOP_LEN: f32 = 3.2;

pub struct PcFx {
    fs: f32,
    harm: FxUnit,
    // rhythm
    rhy_dl: DelayLine,
    beat: f32,
    gate: f32,
    // shimmer
    verb: Verb,
    shim_dl: DelayLine,
    shim_voice: Voice,
    shim_fb: f32,
    shim_env: f32,
    // freeze
    frz_dl: DelayLine,
    frozen: bool,
    frz_env: f32,
    grain_ph: [f32; GRAINS],
    grain_at: [f32; GRAINS],
    grain_pos: [f32; GRAINS],
    rng: u32,
    // lo-fi
    hold: (f32, f32),
    hold_acc: f32,
    wow_dl: (DelayLine, DelayLine),
    wow_ph: (f32, f32),
    tape: [Biquad; 4],
    hiss_lp: f32,
    // tape stop
    stop_dl: (DelayLine, DelayLine),
    stop_t: f32,
    stop_d: f32,
    stop_env: f32,
    stop_silent: bool,
}

impl PcFx {
    pub fn new(fs: f32) -> Self {
        let mut tape = [Biquad::new(); 4];
        tape[0].lowpass(5200.0, 0.6, fs);
        tape[1].lowpass(5200.0, 0.6, fs);
        tape[2].highpass(110.0, 0.7, fs);
        tape[3].highpass(110.0, 0.7, fs);
        let mut verb = Verb::new(fs);
        verb.set(3, 6.0, 0.9, 180.0, 9000.0);
        PcFx {
            fs,
            harm: FxUnit::new(fs),
            rhy_dl: DelayLine::new((5.0 * fs) as usize),
            beat: 0.0,
            gate: 1.0,
            verb,
            shim_dl: DelayLine::new((0.12 * fs) as usize),
            shim_voice: Voice::default(),
            shim_fb: 0.0,
            shim_env: 0.0,
            frz_dl: DelayLine::new((FRZ_LEN * fs) as usize),
            frozen: false,
            frz_env: 0.0,
            grain_ph: [0.0, 0.25, 0.5, 0.75],
            grain_at: [0.3, 0.6, 0.9, 1.2],
            grain_pos: [0.0; GRAINS],
            rng: 0x9E37_79B9,
            hold: (0.0, 0.0),
            hold_acc: 1.0,
            wow_dl: (DelayLine::new((0.05 * fs) as usize), DelayLine::new((0.05 * fs) as usize)),
            wow_ph: (0.0, 0.3),
            tape,
            hiss_lp: 0.0,
            stop_dl: (DelayLine::new((STOP_LEN * fs) as usize), DelayLine::new((STOP_LEN * fs) as usize)),
            stop_t: 0.0,
            stop_d: 0.0,
            stop_env: 1.0,
            stop_silent: false,
        }
    }

    pub fn reset(&mut self) {
        self.harm.reset();
        self.rhy_dl.clear();
        self.verb.reset();
        self.shim_dl.clear();
        self.shim_fb = 0.0;
        self.frz_dl.clear();
        self.frozen = false;
        self.frz_env = 0.0;
        self.stop_dl.0.clear();
        self.stop_dl.1.clear();
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng as f32 / u32::MAX as f32
    }

    /// Per-buffer settings that cost a filter design (only when they change).
    pub fn prepare(&mut self, v: &PcVals, p: &Params) {
        if v.shim {
            self.verb.set(3, p.get(P::PcShimDecay).clamp(1.0, 10.0), 0.9, 180.0, 9000.0);
        }
    }

    /// Mono part: the harmonizer, after the cab and before the modulation.
    #[inline]
    pub fn mono(&mut self, x: f32, v: &PcVals, midi: f32) -> f32 {
        if v.harm {
            self.harm.tick(x, 2, &v.harm_v, midi)
        } else {
            x
        }
    }

    /// Restart the rhythm on the drums' bar line.
    pub fn bar(&mut self) {
        self.beat = 0.0;
    }

    /// Stereo part, after the reverb.
    #[inline]
    pub fn stereo(&mut self, mut l: f32, mut r: f32, v: &PcVals) -> (f32, f32) {
        let fs = self.fs;
        if v.rhy {
            (l, r) = self.rhythm(l, r, v);
        }
        if v.shim || self.shim_env > 1e-4 {
            // the tail keeps ringing after the switch, like a pedal with trails
            let k = 1.0 / (0.02 * fs);
            self.shim_env = if v.shim { (self.shim_env + k).min(1.0) } else { self.shim_env * (1.0 - 1.0 / (3.0 * fs)) };
            let m = 0.5 * (l + r) * if v.shim { 1.0 } else { 0.0 };
            let (wl, wr) = self.verb.tick(m + self.shim_fb, m + self.shim_fb, 25.0, 1.0, 0.0);
            self.shim_dl.push(0.5 * (wl + wr));
            let up = self.shim_voice.tick(&self.shim_dl, v.shim_ratio, 0.05 * fs, 0.0);
            // soft-clipped: the loop can never run away
            self.shim_fb = flush((up * v.shim_amt * 1.8).tanh());
            let g = v.shim_mix * self.shim_env * 1.4;
            l += wl * g;
            r += wr * g;
        }
        if v.frz || self.frz_env > 0.0 {
            (l, r) = self.freeze(l, r, v);
        } else {
            self.frz_dl.push(0.5 * (l + r));
        }
        if v.lofi {
            (l, r) = self.lofi(l, r, v);
        }
        self.tape_stop(l, r, v)
    }

    fn rhythm(&mut self, l: f32, r: f32, v: &PcVals) -> (f32, f32) {
        let fs = self.fs;
        self.beat += v.bpm / 60.0 / fs;
        if self.beat >= 64.0 {
            self.beat -= 64.0;
        }
        let ph = (self.beat / v.rhy_beats).fract();
        match v.rhy_mode {
            0 => {
                // three taps: one division left, two right, three centre; the last feeds back
                let t = v.rhy_beats * 60.0 / v.bpm * fs;
                let (a, b, c) = (self.rhy_dl.read(t - 1.0), self.rhy_dl.read(2.0 * t - 1.0), self.rhy_dl.read(3.0 * t - 1.0));
                self.rhy_dl.push(flush(0.5 * (l + r) + c * v.rhy_amt * 0.75));
                (l + v.rhy_mix * (a * 0.85 + c * 0.4), r + v.rhy_mix * (b * 0.7 + c * 0.4))
            }
            1 => {
                let on = ph < v.rhy_amt.clamp(0.1, 0.95);
                self.gate += ((on as i32 as f32) - self.gate) / (0.002 * fs);
                let g = 1.0 - v.rhy_mix + v.rhy_mix * self.gate;
                (l * g, r * g)
            }
            _ => {
                // sine tremolo, the right side half a cycle later at full mix: it pans too
                let gl = 1.0 - v.rhy_amt * (0.5 - 0.5 * (ph * TAU).cos());
                let gr = 1.0 - v.rhy_amt * (0.5 + 0.5 * (ph * TAU).cos() * (1.0 - 2.0 * v.rhy_mix).max(-1.0));
                let w = v.rhy_mix.min(1.0);
                (l * (1.0 - w + w * gl), r * (1.0 - w + w * gr))
            }
        }
    }

    fn freeze(&mut self, l: f32, r: f32, v: &PcVals) -> (f32, f32) {
        let fs = self.fs;
        if v.frz && !self.frozen {
            self.frozen = true;
        }
        if self.frozen {
            self.frz_env = if v.frz { (self.frz_env + 1.0 / (0.35 * fs)).min(1.0) } else { self.frz_env - 1.0 / (1.5 * fs) };
        }
        if self.frz_env <= 0.0 {
            // released and faded out: record again
            self.frz_env = 0.0;
            self.frozen = false;
            self.frz_dl.push(0.5 * (l + r));
            return (l, r);
        }
        // four Hann grains a quarter apart, each from a random spot of the last second,
        // detuned by a few cents so the pad breathes instead of buzzing
        let glen = (0.12 + 0.38 * v.frz_tex) * fs;
        let span = 0.95 * fs - glen;
        const DETUNE: [f32; GRAINS] = [1.0, 1.0019, 0.9981, 1.0007];
        let (mut gl, mut gr) = (0.0, 0.0);
        for g in 0..GRAINS {
            self.grain_ph[g] += 1.0 / glen;
            if self.grain_ph[g] >= 1.0 {
                self.grain_ph[g] -= 1.0;
                self.grain_at[g] = 0.03 * fs + self.rand() * span;
                self.grain_pos[g] = 0.0;
            }
            self.grain_pos[g] += DETUNE[g];
            let w = (PI * self.grain_ph[g]).sin();
            let s = self.frz_dl.read(self.grain_at[g] + glen - self.grain_pos[g]) * w * w;
            if g % 2 == 0 {
                gl += s * 0.8;
                gr += s * 0.2;
            } else {
                gl += s * 0.2;
                gr += s * 0.8;
            }
        }
        let k = v.frz_mix * self.frz_env * 0.9;
        (l + gl * k, r + gr * k)
    }

    fn lofi(&mut self, l: f32, r: f32, v: &PcVals) -> (f32, f32) {
        let fs = self.fs;
        self.hold_acc += v.rate / fs;
        if self.hold_acc >= 1.0 {
            self.hold_acc -= 1.0;
            self.hold = (l, r);
        }
        let q = 2f32.powf(v.bits - 1.0);
        let crush = |x: f32| (x * q).round() / q;
        let (cl, cr) = (crush(self.hold.0), crush(self.hold.1));
        // cassette: wow 0.5 Hz, flutter 7 Hz, band-limited, hiss
        self.wow_ph.0 = (self.wow_ph.0 + 0.5 / fs).fract();
        self.wow_ph.1 = (self.wow_ph.1 + 7.0 / fs).fract();
        let d = (0.012 + v.wow * (0.0035 * (self.wow_ph.0 * TAU).sin() + 0.0004 * (self.wow_ph.1 * TAU).sin())) * fs;
        self.wow_dl.0.push(cl);
        self.wow_dl.1.push(cr);
        let (wl, wr) = (self.wow_dl.0.read(d), self.wow_dl.1.read(d));
        let n = self.rand() * 2.0 - 1.0;
        self.hiss_lp += (n - self.hiss_lp) * 0.35;
        let h = self.hiss_lp * v.hiss * 0.004;
        let (a, b) = (self.tape[0].tick(wl), self.tape[1].tick(wr));
        let tl = self.tape[2].tick(a) + h;
        let tr = self.tape[3].tick(b) + h;
        let m = v.lofi_mix;
        (l * (1.0 - m) + tl * m, r * (1.0 - m) + tr * m)
    }

    fn tape_stop(&mut self, l: f32, r: f32, v: &PcVals) -> (f32, f32) {
        let fs = self.fs;
        self.stop_dl.0.push(l);
        self.stop_dl.1.push(r);
        if v.stop {
            // speed falls on a curve, so the pitch dives faster at the end, like a real reel
            self.stop_t = (self.stop_t + 1.0 / (v.stop_time * fs)).min(1.0);
            let speed = (1.0 - self.stop_t).powf(1.6);
            self.stop_d = (self.stop_d + 1.0 - speed).min(STOP_LEN * fs - 4.0);
            self.stop_env = if self.stop_t >= 1.0 { (self.stop_env - 1.0 / (0.01 * fs)).max(0.0) } else { 1.0 };
            let g = self.stop_env;
            (self.stop_dl.0.read(self.stop_d) * g, self.stop_dl.1.read(self.stop_d) * g)
        } else if self.stop_d > 0.0 {
            // back to the live signal over 25 ms: crossfaded from a tape still moving, faded
            // in from the silence of one that had stopped (its last sample would be a step)
            if self.stop_t >= 1.0 {
                self.stop_t = 0.0;
                self.stop_env = 1.0;
                self.stop_silent = true;
            }
            self.stop_env = (self.stop_env - 1.0 / (0.025 * fs)).max(0.0);
            let k = self.stop_env;
            let held = if self.stop_silent { (0.0, 0.0) } else { (self.stop_dl.0.read(self.stop_d), self.stop_dl.1.read(self.stop_d)) };
            if k <= 0.0 {
                self.stop_d = 0.0;
                self.stop_t = 0.0;
                self.stop_env = 1.0;
                self.stop_silent = false;
            }
            (held.0 * k + l * (1.0 - k), held.1 * k + r * (1.0 - k))
        } else {
            (l, r)
        }
    }
}
