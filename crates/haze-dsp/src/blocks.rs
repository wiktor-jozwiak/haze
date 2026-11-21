//! The effect blocks. Each owns its state, preallocated in `new(fs)`; `process` never allocates.

use crate::filters::{db, flush, Biquad, DelayLine, Oversampler4};
use crate::tonestack::{StackParts, ToneStack, BASSMAN, FENDER_BF, MARSHALL, MODERN};
use std::f32::consts::{PI, TAU};

// ------------------------------------------------------------------ noise gate

/// Envelope gate with hysteresis (opens at threshold, closes 9 dB below) and smooth gain.
///
/// The threshold adapts to the interface: a noise-floor tracker (minimum of the envelope
/// over the last ~2 s) lifts it to floor + 16 dB, never above −35 dBFS. Patch thresholds
/// are written for the Katana's own quiet input; a cheap USB interface sits at −50 dBFS,
/// where a −65 dB gate never closes and the amp's gain turns the hiss into a buzz.
///
/// The floor falls at once but rises by at most 0.5 dB a second. Continuous playing has no
/// gaps, so the 2 s minimum is then the quiet part of the playing, not noise: followed
/// freely, it lifted the gate to −35 dBFS within seconds and shut a guitar peaking at −34
/// (a softer passage after a loud one went silent). Real noise that grows still gets there.
///
/// A patch threshold assumes the Katana's input, where a guitar peaks near −12 dBFS; it moves
/// down by however much quieter this interface hears the guitar (`peak`, up to 30 dB).
pub struct Gate {
    env: f32,
    gain: f32,
    open: bool,
    att: f32,
    rel_env: f32,
    rel_gain: f32,
    hold: u32,
    hold_max: u32,
    /// minimum envelope of each of the last finished blocks (MAX = not measured yet)
    mins: [f32; FLOOR_BLOCKS],
    cur_min: f32,
    count: u32,
    block_len: u32,
    slot: usize,
    /// samples left in the start-up hold: the gate stays shut while the hum notches settle
    /// (τ ≈ 80 ms, unfiltered hum would open it) and the envelope rises from 0
    warm: u32,
    /// rate-limited noise floor (MAX = not measured yet)
    floor: f32,
    /// playing level: instant peak of the envelope, released over ~20 s
    peak: f32,
    peak_rel: f32,
}

const FLOOR_BLOCKS: usize = 4;
/// most the floor may rise per half-second block: +0.25 dB (0.5 dB/s)
const FLOOR_RISE: f32 = 1.029;
/// how far above the noise floor the gate opens, and the most the floor may lift it to
/// (the envelope of plain hiss swings ~13 dB min to max, plus 6 dB of hysteresis)
const FLOOR_MARGIN: f32 = 6.3; // +16 dB
const FLOOR_CAP: f32 = 0.0178; // −35 dBFS
/// where a guitar peaks on the Katana's input, the level patch thresholds are written for
const KATANA_PEAK: f32 = 0.25; // −12 dBFS
/// close at a third of the open level: decaying notes ring 9 dB under the threshold
const CLOSE_RATIO: f32 = 0.35;

impl Gate {
    pub fn new(fs: f32) -> Self {
        let k = |ms: f32| 1.0 - (-1.0 / (ms * 0.001 * fs)).exp();
        Gate {
            env: 0.0,
            gain: 0.0,
            open: false,
            att: k(0.5),
            rel_env: k(30.0),
            rel_gain: k(120.0),
            hold: 0,
            hold_max: (0.04 * fs) as u32,
            mins: [f32::MAX; FLOOR_BLOCKS],
            cur_min: f32::MAX,
            count: 0,
            block_len: (0.5 * fs) as u32,
            slot: 0,
            warm: (0.25 * fs) as u32,
            floor: f32::MAX,
            peak: 0.0,
            peak_rel: 1.0 - (-1.0 / (20.0 * fs)).exp(),
        }
    }

    /// Noise floor: smallest envelope seen over the last FLOOR_BLOCKS half-second blocks,
    /// rising no faster than FLOOR_RISE per block. Unknown (MAX) until the first block
    /// completes: the gate then sits at the cap.
    #[inline]
    fn floor(&self) -> f32 {
        self.floor
    }

    #[inline]
    pub fn tick(&mut self, x: f32, thresh_db: f32) -> f32 {
        let a = x.abs();
        self.env += (a - self.env) * if a > self.env { self.att } else { self.rel_env };
        if self.warm > 0 {
            self.warm -= 1;
            return 0.0;
        }
        self.cur_min = self.cur_min.min(self.env);
        self.peak = if self.env > self.peak { self.env } else { self.peak - self.peak * self.peak_rel };
        self.count += 1;
        if self.count >= self.block_len {
            self.mins[self.slot] = self.cur_min;
            self.slot = (self.slot + 1) % FLOOR_BLOCKS;
            let m = self.mins.iter().fold(f32::MAX, |a, m| a.min(*m));
            self.floor = if self.floor == f32::MAX { m } else { m.min(self.floor * FLOOR_RISE) };
            self.cur_min = f32::MAX;
            self.count = 0;
        }
        self.apply(x, thresh_db)
    }

    /// Level that opens the gate now (linear), and the envelope: for tests and meters.
    pub fn state(&self, thresh_db: f32) -> (f32, f32) {
        (self.open_level(thresh_db), self.env)
    }

    #[inline]
    fn open_level(&self, thresh_db: f32) -> f32 {
        // patch threshold, moved down as far as this interface hears the guitar quieter than
        // the Katana would (nothing played yet: the full 30 dB)
        let shift = (self.peak / KATANA_PEAK).clamp(0.0316, 1.0);
        (db(thresh_db) * shift).max((self.floor() * FLOOR_MARGIN).min(FLOOR_CAP))
    }

    #[inline]
    fn apply(&mut self, x: f32, thresh_db: f32) -> f32 {
        let open_t = self.open_level(thresh_db);
        if self.env > open_t {
            self.open = true;
            self.hold = self.hold_max;
        } else if self.env < open_t * CLOSE_RATIO {
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

// ------------------------------------------------------------------ hum filter

/// Mains hum remover: narrow notches on the mains frequency and its first harmonics.
/// A ground loop between the PC's USB and powered speakers (or single-coil pickups) puts
/// 50/60 Hz on the guitar input at −30 dBFS, far above any gate threshold; the amp's gain
/// then turns it into a constant buzz. Notches are ~4 Hz wide above the fundamental, so the
/// guitar's own notes lose at most a dB or two on a fundamental that sits right next to one.
pub struct HumFilter {
    fs: f32,
    hz: f32,
    notch: [Biquad; 5],
}

impl HumFilter {
    pub fn new(fs: f32) -> Self {
        HumFilter { fs, hz: 0.0, notch: [Biquad::new(); 5] }
    }

    /// `hz`: mains frequency (50 / 60), 0 = bypass. Coefficients rebuild only on change.
    #[inline]
    pub fn tick(&mut self, x: f32, hz: f32) -> f32 {
        if hz <= 0.0 {
            return x;
        }
        if hz != self.hz {
            self.hz = hz;
            for (k, n) in self.notch.iter_mut().enumerate() {
                let f = hz * (k + 1) as f32;
                // fundamental a bit wider: mains drifts ±0.2 Hz and it carries most energy
                let q = if k == 0 { 8.0 } else { f / 4.0 };
                n.notch(f, q, self.fs);
                n.reset();
            }
        }
        let mut y = x;
        for n in self.notch.iter_mut() {
            y = n.tick(y);
        }
        flush(y)
    }
}

// ------------------------------------------------------------------ input leveller

/// Peak the leveller brings the guitar to ahead of the gate. The amp models are voiced for the
/// built-in DI riff's −6 dBFS; the last 3 dB (`TRIM_MAKEUP`) come after the gate, so idle
/// residue (hum the notches leave, hiss) stays under the gate's −35 dBFS cap.
const TRIM_TARGET: f32 = 0.35;
/// gain after the gate up to the voiced level: without it a guitar reached the linear clean
/// voicings 3 dB quieter than the riff they were levelled on
pub const TRIM_MAKEUP: f32 = 0.5 / TRIM_TARGET;
/// at most +30 dB: past that it is hiss, not a guitar
const TRIM_MAX: f32 = 31.6;
/// below this the input is not playing
const TRIM_FLOOR: f32 = 0.003;
/// the interface's noise may come up to −50 dBFS RMS at most: the gate's adaptive floor
/// still closes on it there
const TRIM_NOISE: f32 = 0.00316;

/// Input calibration, not a compressor: follows the loudest playing of the last minutes
/// (instant up, −20 dB over ~5 min of playing down) and sets one slow gain from it, so the guitar
/// reaches the amp at the level the models were voiced for whatever the interface gain.
/// Picking dynamics pass untouched. The gain is also capped by the measured noise floor
/// (quietest 50 ms, creeping up 6 dB a minute), so a noisy interface is not made noisier
/// than the gate can handle: there the guitar only comes up as far as the hiss allows.
pub struct Trim {
    peak: f32,
    rel: f32,
    gain: f32,
    up: f32,
    down: f32,
    blk: usize,
    blk_n: usize,
    blk_sum: f32,
    floor: f32,
    floor_rise: f32,
}

impl Trim {
    pub fn new(fs: f32) -> Self {
        Trim {
            peak: 0.0,
            // −20 dB in 300 s
            rel: 0.1f32.powf(1.0 / (300.0 * fs)),
            gain: 1.0,
            up: 1.0 / (2.0 * fs),
            down: 1.0 / (0.05 * fs),
            blk: (0.05 * fs) as usize,
            blk_n: 0,
            blk_sum: 0.0,
            floor: f32::MAX,
            // +6 dB a minute, per 50 ms block
            floor_rise: 10f32.powf(6.0 / 20.0 / 1200.0),
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let a = x.abs();
        // the reference only sinks while the guitar plays (within 20 dB of it): silence
        // between songs never winds the gain up for the next attack to blast through
        if a > self.peak {
            self.peak = a;
        } else if a > self.peak * 0.1 {
            self.peak = (self.peak * self.rel).max(TRIM_FLOOR);
        }
        self.blk_sum += x * x;
        self.blk_n += 1;
        if self.blk_n >= self.blk {
            let rms = (self.blk_sum / self.blk_n as f32).sqrt();
            self.floor = if self.floor == f32::MAX { rms } else { rms.min(self.floor * self.floor_rise) };
            self.blk_n = 0;
            self.blk_sum = 0.0;
        }
        let max = if self.floor == f32::MAX { 1.0 } else { (TRIM_NOISE / self.floor.max(1e-7)).clamp(1.0, TRIM_MAX) };
        let want = if self.peak > TRIM_FLOOR { (TRIM_TARGET / self.peak).clamp(1.0, max) } else { self.gain.min(max) };
        // rises over seconds, falls in 50 ms when a harder note arrives
        let k = if want > self.gain { self.up } else { self.down };
        self.gain += (want - self.gain) * k;
        x * self.gain
    }
}

// ------------------------------------------------------------------ drive pedals

pub const DRIVE_MODELS: [&str; 9] = ["T-Scream", "Klon", "Blues Driver", "RAT", "Muff", "Metal Zone", "Clean boost", "Mid boost", "Treble boost"];

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
            // clean boost: flat, a touch of low cut
            6 => {
                self.pre.highpass(40.0, 0.707, fs);
                self.pre2.peak(1000.0, 0.7, 0.0, fs);
                self.post.lowpass(tf.max(6000.0), 0.6, fs);
                self.post2.high_shelf(3000.0, (tone - 0.5) * 8.0, fs);
            }
            // mid boost: lows off, a broad hump at 800 Hz (tightens a high-gain amp)
            7 => {
                self.pre.highpass(160.0, 0.707, fs);
                self.pre2.peak(800.0, 0.6, 9.0, fs);
                self.post.lowpass(tf.max(5000.0), 0.6, fs);
                self.post2.high_shelf(3000.0, (tone - 0.5) * 6.0, fs);
            }
            // treble booster: everything under ~1 kHz rolled off
            8 => {
                self.pre.highpass(600.0, 0.6, fs);
                self.pre2.peak(3200.0, 0.7, 4.0, fs);
                self.post.lowpass(tf.max(7000.0), 0.6, fs);
                self.post2.high_shelf(2500.0, (tone - 0.5) * 6.0, fs);
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
            6..=8 => db(amt * 24.0),
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
            // op-amp boost: clean until the rails, ±3
            6..=8 => (u * 0.33).tanh() * 3.0,
            _ => (u * 1.5).clamp(-1.0, 1.0).tanh() * 1.2,
        });
        let y = self.post2.tick(self.post.tick(y));
        let y = if model == 1 { y * 0.7 + x * (1.0 - amt) * 1.2 } else { y };
        // per-model trim: at drive / level noon every overdrive / distortion comes out ~10 dB
        // over its input (K-weighted, DI riff; they spread from +4 to +16 dB). The boosts keep
        // their own gain: their DRIVE knob is the boost
        const TRIM_DB: [f32; 9] = [-4.8, 6.0, -1.2, -6.0, 0.4, -5.3, 0.0, 0.0, 0.0];
        let trim = db(TRIM_DB[model.clamp(0, 8) as usize]);
        flush(self.dc.tick(y) * db(-12.0 + level * 24.0) * 0.6 * trim)
    }
}

// ------------------------------------------------------------------ amp

pub const VOICINGS: [&str; 7] = ["Acoustic", "Clean", "Pushed", "Crunch", "Lead", "Brown", "Modern Metal"];

/// Preamp (1–2 oversampled triode-like stages), the voicing's passive TMB tone stack
/// (Fender / Bassman / Marshall / modern, see `tonestack`), power amp with sag compression,
/// presence and resonance shelving. The acoustic voicing keeps gentle shelving EQ.
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
    stack: ToneStack,
    /// voicing whose noon level `makeup` was measured for
    stack_v: i32,
    makeup: f64,
    presence: Biquad,
    resonance: Biquad,
    dc: Biquad,
    sag_env: f32,
    sag_k: f32,
    key: [f32; 7],
    /// clean leveller: peak envelope of the input, smoothed gain, counter for the gain update
    lev_env: f32,
    lev_gain: f32,
    lev_want: f32,
    lev_a: f32,
    lev_r: f32,
    lev_k: f32,
    lev_n: u32,
}

/// Clean leveller threshold (input peak, dBFS) and ratio per voicing (0 = off). The low-gain
/// voicings are linear, so a guitar played 6 dB softer came out 6 dB down while the saturated
/// ones barely moved; 2:1 above the threshold halves that, like a clean amp pushed at volume.
const LEV_T: f32 = -26.0;
const LEV_RATIO: [f32; 7] = [2.0, 2.0, 1.6, 0.0, 0.0, 0.0, 0.0];

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
            stack: ToneStack::default(),
            stack_v: -1,
            makeup: 1.0,
            presence: Biquad::new(),
            resonance: Biquad::new(),
            dc: Biquad::new(),
            sag_env: 0.0,
            sag_k: 1.0 - (-1.0 / (0.08 * fs)).exp(),
            key: [-1.0; 7],
            lev_env: 0.0,
            lev_gain: 1.0,
            lev_want: 1.0,
            lev_a: 1.0 - (-1.0 / (0.003 * fs)).exp(),
            lev_r: 1.0 - (-1.0 / (0.25 * fs)).exp(),
            lev_k: 1.0 - (-1.0 / (0.004 * fs)).exp(),
            lev_n: 0,
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
        if let Some(parts) = stack_parts(v) {
            if self.stack_v != v {
                // the passive stack loses 10–20 dB; bring its noon setting back to unity over
                // the guitar's core range so switching voicings keeps the level
                let mut noon = ToneStack::default();
                noon.design(&parts, 0.5, 0.5, 0.5, fs);
                let bands = [110.0, 250.0, 500.0, 1000.0, 2000.0, 3500.0];
                let mean_db = bands.iter().map(|&f| 20.0 * noon.gain_at(f, fs).log10()).sum::<f64>() / bands.len() as f64;
                self.makeup = 10f64.powf(-mean_db / 20.0);
                self.stack_v = v;
                self.stack.reset();
            }
            self.stack.design(&parts, treble, bass, mid, fs);
            self.stack.makeup = self.makeup;
        }
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
        let x = self.level(x, LEV_RATIO[v.clamp(0, 6) as usize]);
        let u = self.voice.tick(self.hp_in.tick(x)) * g;
        // triode: asymmetric soft clip with grid bias
        let mut y = self.os1.process(u, |s| (s + bias).tanh() - bias.tanh());
        if two_stage {
            let s2 = self.inter.tick(y) * db(6.0 + gain * 14.0);
            y = self.os2.process(s2, |s| (s * 1.2 + bias * 0.5).tanh() - (bias * 0.5).tanh());
        }
        // tone stack
        y = if v == 0 { self.treble.tick(self.mid.tick(self.bass.tick(y))) } else { self.stack.tick(y) };
        // power amp with supply sag
        self.sag_env += (y.abs() - self.sag_env) * self.sag_k;
        let sag_gain = 1.0 / (1.0 + sag * self.sag_env * 3.0);
        let p = y * db(-6.0 + master * 22.0) * sag_gain;
        let p = (p * 0.9).tanh();
        let p = self.resonance.tick(self.presence.tick(p));
        // clean voicings stay linear (and levelled) and come out quieter than saturated ones
        // at the same knob settings; even them out on the DI riff at typical starter knobs,
        // the clean ones ~0.5 dB on top: a guitar mostly plays under its loudest peaks
        const OUT_DB: [f32; 7] = [12.6, 6.8, 3.6, 0.3, 0.0, 0.5, -0.8];
        flush(self.dc.tick(p) * 0.7 * db(OUT_DB[v.clamp(0, 6) as usize]))
    }
}

impl Amp {
    /// Clean leveller: soft-knee (6 dB) gain reduction above `LEV_T`, the gain recomputed
    /// every 8 samples and glided over 4 ms. Unity below the knee: soft picking is untouched.
    #[inline]
    fn level(&mut self, x: f32, ratio: f32) -> f32 {
        if ratio <= 1.0 {
            self.lev_env = 0.0;
            self.lev_gain = 1.0;
            return x;
        }
        let a = x.abs();
        self.lev_env += (a - self.lev_env) * if a > self.lev_env { self.lev_a } else { self.lev_r };
        self.lev_env = flush(self.lev_env);
        if self.lev_n % 8 == 0 {
            let over = 20.0 * (self.lev_env + 1e-9).log10() - LEV_T;
            let slope = 1.0 - 1.0 / ratio;
            let red = if over <= -3.0 {
                0.0
            } else if over >= 3.0 {
                over * slope
            } else {
                slope * (over + 3.0) * (over + 3.0) / 12.0
            };
            self.lev_want = db(-red);
        }
        self.lev_n = self.lev_n.wrapping_add(1);
        self.lev_gain += (self.lev_want - self.lev_gain) * self.lev_k;
        x * self.lev_gain
    }
}

/// Tone stack circuit per voicing (None: acoustic, shelving EQ).
fn stack_parts(v: i32) -> Option<StackParts> {
    match v {
        0 => None,
        1 => Some(FENDER_BF),
        2 => Some(BASSMAN),
        3..=5 => Some(MARSHALL),
        _ => Some(MODERN),
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
                // chorus keeps the dry on top; the flanger mixes equal parts at 0.5 so its comb
                // notches go all the way down. Both normalised to the power of the dry signal
                let dry = if ty == 0 { 1.0 - mix * 0.5 } else { 1.0 - mix };
                let n = 1.0 / (dry * dry + mix * mix).sqrt().max(0.5);
                ((x * dry + wl * mix) * n, (x * dry + wr * mix) * n)
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
                // equal parts at 0.5: the notches cancel fully (dry 0.75 left them at −12 dB)
                let n = 1.0 / ((1.0 - mix) * (1.0 - mix) + mix * mix).sqrt();
                ((x * (1.0 - mix) + l * mix) * n, (x * (1.0 - mix) + r * mix) * n)
            }
            3 => {
                // gain swings 1 − depth .. 1; scaled back to the dry signal's power
                let h = depth * 0.5;
                let g = (1.0 - h + h * sl) / ((1.0 - h) * (1.0 - h) + h * h * 0.5).sqrt();
                (x * g, x * g)
            }
            _ => {
                // rotary horn: doppler from a short modulated delay, opposite-phase panning
                self.dl.push(x);
                let w = self.dl.read((0.0008 + 0.0006 * depth * sl) * fs);
                // a horn has no dry path: dry + a 0.8 ms copy combed out 625 Hz and lost 6 dB.
                // Equal-power pan keeps the level while the horn swings (`mix` unused)
                let _ = mix;
                let pan = (0.5 + 0.5 * sl * depth) * PI * 0.5;
                let am = 1.0 - 0.3 * depth * (0.5 - 0.5 * sr);
                let y = w * am * std::f32::consts::SQRT_2;
                (y * pan.cos(), y * pan.sin())
            }
        }
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
        if !peak.is_finite() {
            self.gain = 1.0;
            return (0.0, 0.0);
        }
        let want = if peak * self.gain > ceil { ceil / peak } else { 1.0 };
        if want < self.gain {
            self.gain = want;
        } else {
            self.gain += (want - self.gain) * self.rel;
        }
        (l * self.gain, r * self.gain)
    }
}
