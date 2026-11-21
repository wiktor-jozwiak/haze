//! Chromatic / string tuner on the UI thread, fed from samples the audio thread pushes into a
//! ring.
//!
//! Pitch: McLeod Pitch Method (normalised square difference function, "key maxima" peak
//! picking) on a 2048-sample window of the input decimated 2x through a 4th-order low-pass
//! (≈ 85 ms at 24 kHz: five periods of a 7-string's low B). MPM keeps the octave on guitar
//! notes whose 2nd harmonic is louder than the fundamental, which plain YIN thresholds miss.
//! Parabolic interpolation of the NSDF peak gives ~0.1 cent resolution.
//!
//! Readout (`estimate`), built against a real pickup's wobble (beating partials, the string's
//! own pitch drift, sympathetic strings), which the bare estimates follow by ±2–3 cents:
//! - median of the last three estimates within the note;
//! - adaptive smoothing: near the current reading it settles hard (the needle stands still on
//!   a tuned string), a few cents away it follows fast (a turning peg);
//! - a different note needs two estimates in a row that agree, so a lone octave slip or a
//!   pick-attack harmonic never flips the display;
//! - short dropouts (clarity dips while a string rings out) keep the last reading for ~0.5 s
//!   instead of blanking the display and restarting the smoothing.

use crate::filters::Biquad;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pitch {
    pub hz: f32,
    pub note: &'static str,
    pub octave: i32,
    /// deviation from the nearest equal-tempered note, −50..+50
    pub cents: f32,
    /// nearest MIDI note (A4 = 69 at the reference pitch)
    pub midi: i32,
    /// fractional MIDI note number, unrounded
    pub midi_f: f32,
    /// NSDF peak height, 0..1: how periodic the signal is
    pub clarity: f32,
}

pub const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Name of a MIDI note without octave.
pub fn note_name(midi: i32) -> &'static str {
    NAMES[midi.rem_euclid(12) as usize]
}

/// A string set, lowest string first, as MIDI notes.
pub struct Tuning {
    pub name: &'static str,
    pub strings: &'static [i32],
}

pub const TUNINGS: [Tuning; 10] = [
    Tuning { name: "Standard E", strings: &[40, 45, 50, 55, 59, 64] },
    Tuning { name: "Drop D", strings: &[38, 45, 50, 55, 59, 64] },
    Tuning { name: "E♭ Standard", strings: &[39, 44, 49, 54, 58, 63] },
    Tuning { name: "D Standard", strings: &[38, 43, 48, 53, 57, 62] },
    Tuning { name: "Drop C", strings: &[36, 43, 48, 53, 57, 62] },
    Tuning { name: "DADGAD", strings: &[38, 45, 50, 55, 57, 62] },
    Tuning { name: "Open G", strings: &[38, 43, 50, 55, 59, 62] },
    Tuning { name: "Open D", strings: &[38, 45, 50, 54, 57, 62] },
    Tuning { name: "7-string B", strings: &[35, 40, 45, 50, 55, 59, 64] },
    Tuning { name: "Bass E", strings: &[28, 33, 38, 43] },
];

/// Index of the string closest to a fractional MIDI note.
pub fn nearest_string(t: &Tuning, midi_f: f32) -> usize {
    let mut best = 0;
    for (i, &s) in t.strings.iter().enumerate() {
        if (midi_f - s as f32).abs() < (midi_f - t.strings[best] as f32).abs() {
            best = i;
        }
    }
    best
}

const WIN: usize = 2048;
const RING: usize = 4096;
/// lowest pitch searched: under a bass's low E (41.2 Hz)
const MIN_HZ: f32 = 38.0;
const MAX_HZ: f32 = 1400.0;
/// key-maximum threshold relative to the highest NSDF peak (MPM's k)
const K: f32 = 0.93;
/// below this the signal is noise or a chord: no reading
const MIN_CLARITY: f32 = 0.75;
/// −54 dBFS RMS
const MIN_RMS: f32 = 0.002;
/// estimates without a pitch that still show the last reading (~0.5 s at the UI's 15 Hz)
const HOLD_MISSES: u32 = 7;
/// smoothing weight when the estimate sits on the reading, and the most it grows to when the
/// estimate is `FOLLOW_CENTS` or more away
const SETTLE: f32 = 0.12;
const FOLLOW: f32 = 0.65;
const FOLLOW_CENTS: f32 = 12.0;

pub struct Tuner {
    /// rate after decimation
    fs: f32,
    a4: f32,
    aa: [Biquad; 2],
    odd: bool,
    ring: Box<[f32; RING]>,
    w: usize,
    filled: usize,
    win: Vec<f32>,
    nsdf: Vec<f32>,
    // readout smoothing
    hist: [f32; 3],
    hist_n: usize,
    smooth: Option<f32>,
    /// a different note seen once, waiting for a second estimate to confirm it
    pending: Option<f32>,
    misses: u32,
    last: Option<Pitch>,
}

impl Tuner {
    pub fn new(fs: f32) -> Self {
        let fs_d = fs / 2.0;
        let mut aa = [Biquad::new(); 2];
        // 4th-order Butterworth at 0.2 × the decimated rate: alias-free up to the tuner range
        aa[0].lowpass(fs_d * 0.2, 0.541, fs);
        aa[1].lowpass(fs_d * 0.2, 1.307, fs);
        let max_tau = (fs_d / MIN_HZ) as usize + 2;
        Tuner {
            fs: fs_d,
            a4: 440.0,
            aa,
            odd: false,
            ring: Box::new([0.0; RING]),
            w: 0,
            filled: 0,
            win: vec![0.0; WIN],
            nsdf: vec![0.0; max_tau.min(WIN / 2) + 1],
            hist: [0.0; 3],
            hist_n: 0,
            smooth: None,
            pending: None,
            misses: 0,
            last: None,
        }
    }

    /// Concert pitch of A4 in Hz (430..450).
    pub fn set_reference(&mut self, a4: f32) {
        self.a4 = a4.clamp(400.0, 480.0);
    }

    pub fn reference(&self) -> f32 {
        self.a4
    }

    /// Feed raw samples at the engine rate.
    #[inline]
    pub fn push(&mut self, x: f32) {
        let y0 = self.aa[0].tick(x);
        let y = self.aa[1].tick(y0);
        self.odd = !self.odd;
        if self.odd {
            return;
        }
        self.ring[self.w] = y;
        self.w = (self.w + 1) % RING;
        self.filled = (self.filled + 1).min(RING);
    }

    /// Forget the readout history (e.g. after a pause), keeping the audio.
    pub fn reset_readout(&mut self) {
        self.hist_n = 0;
        self.smooth = None;
        self.pending = None;
        self.misses = 0;
        self.last = None;
    }

    /// Steady pitch of the latest window for display, or None when quiet / unpitched.
    pub fn estimate(&mut self) -> Option<Pitch> {
        let Some(raw) = self.estimate_raw() else {
            self.misses += 1;
            if self.misses > HOLD_MISSES {
                self.reset_readout();
            }
            return self.last;
        };
        self.misses = 0;
        let x = raw.midi_f;
        if let Some(s) = self.smooth {
            if (x - s).abs() > 0.5 {
                // another note: believe it once a second estimate agrees
                match self.pending {
                    Some(p) if (x - p).abs() < 0.35 => {
                        self.hist_n = 0;
                        self.smooth = None;
                        self.pending = None;
                    }
                    _ => {
                        self.pending = Some(x);
                        return self.last;
                    }
                }
            } else {
                self.pending = None;
            }
        }
        self.hist[self.hist_n % 3] = x;
        self.hist_n += 1;
        let n = self.hist_n.min(3);
        let mut h = [self.hist[0], self.hist[1], self.hist[2]];
        h[..n].sort_by(|a, b| a.total_cmp(b));
        let med = h[n / 2];
        let s = match self.smooth {
            Some(s) => {
                let cents = (med - s).abs() * 100.0;
                // the first estimates of a note: a running mean, the fastest way off the
                // pick attack's first guess
                let a = (SETTLE + (FOLLOW - SETTLE) * (cents / FOLLOW_CENTS).clamp(0.0, 1.0)).max(1.0 / self.hist_n as f32);
                // a muddy window (chord, dying string) moves the reading less
                s + (med - s) * a * raw.clarity.clamp(0.6, 1.0)
            }
            None => med,
        };
        self.smooth = Some(s);
        let p = self.pitch_of(s, raw.clarity);
        self.last = Some(p);
        Some(p)
    }

    fn pitch_of(&self, midi_f: f32, clarity: f32) -> Pitch {
        let n = midi_f.round();
        let midi = n as i32;
        Pitch {
            hz: self.a4 * 2f32.powf((midi_f - 69.0) / 12.0),
            note: note_name(midi),
            octave: midi.div_euclid(12) - 1,
            cents: (midi_f - n) * 100.0,
            midi,
            midi_f,
            clarity,
        }
    }

    /// One MPM estimate of the latest window, unsmoothed.
    pub fn estimate_raw(&mut self) -> Option<Pitch> {
        if self.filled < WIN {
            return None;
        }
        for (i, v) in self.win.iter_mut().enumerate() {
            *v = self.ring[(self.w + RING - WIN + i) % RING];
        }
        let w = &self.win;
        let energy: f32 = w.iter().map(|x| x * x).sum();
        if (energy / WIN as f32).sqrt() < MIN_RMS {
            return None;
        }
        let min_tau = ((self.fs / MAX_HZ) as usize).max(2);
        let max_tau = self.nsdf.len() - 1;
        // NSDF: n(τ) = 2 r(τ) / m(τ), m(τ) = Σ x[j]² + x[j+τ]² over the overlap
        let mut m = 2.0 * energy;
        for tau in 0..=max_tau {
            if tau > 0 {
                m -= w[tau - 1] * w[tau - 1] + w[WIN - tau] * w[WIN - tau];
            }
            let mut r = 0.0f32;
            for j in 0..WIN - tau {
                r += w[j] * w[j + tau];
            }
            self.nsdf[tau] = if m > 1e-12 { 2.0 * r / m } else { 0.0 };
        }
        let d = &self.nsdf;
        // key maxima: the highest point between each positive-going zero crossing and the
        // next negative-going one
        let mut keys: [(usize, f32); 32] = [(0, 0.0); 32];
        let mut nk = 0;
        let mut tau = 1;
        while tau < max_tau && d[tau] > 0.0 {
            tau += 1;
        }
        while tau < max_tau && nk < keys.len() {
            while tau < max_tau && d[tau] <= 0.0 {
                tau += 1;
            }
            let mut best = (0, f32::MIN);
            while tau < max_tau && d[tau] > 0.0 {
                if d[tau] > best.1 && d[tau] >= d[tau - 1] && d[tau] >= d[tau + 1] {
                    best = (tau, d[tau]);
                }
                tau += 1;
            }
            if best.0 >= min_tau {
                keys[nk] = best;
                nk += 1;
            }
        }
        let top = keys[..nk].iter().map(|k| k.1).fold(0.0f32, f32::max);
        if top < MIN_CLARITY {
            return None;
        }
        let (t, _) = *keys[..nk].iter().find(|k| k.1 >= K * top)?;
        let (a, b, c) = (d[t - 1], d[t], d[t + 1]);
        let den = a + c - 2.0 * b;
        let (tf, peak) = if den.abs() > 1e-9 {
            let off = 0.5 * (a - c) / den;
            (t as f32 + off, b - 0.25 * (a - c) * off)
        } else {
            (t as f32, b)
        };
        let hz = self.fs / tf;
        let midi_f = 69.0 + 12.0 * (hz / self.a4).log2();
        Some(self.pitch_of(midi_f, peak.min(1.0)))
    }
}
