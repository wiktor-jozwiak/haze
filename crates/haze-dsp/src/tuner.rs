//! Chromatic tuner (YIN) running on the UI thread from samples the audio thread pushes into a
//! ring. Input is decimated 4x before analysis to keep it cheap.

pub struct Tuner {
    fs: f32,
    buf: Vec<f32>,
    acc: f32,
    acc_n: usize,
    diff: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pitch {
    pub hz: f32,
    pub note: &'static str,
    pub octave: i32,
    pub cents: f32,
}

const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const WIN: usize = 1024;

impl Tuner {
    pub fn new(fs: f32) -> Self {
        Tuner { fs: fs / 4.0, buf: Vec::with_capacity(WIN * 2), acc: 0.0, acc_n: 0, diff: vec![0.0; WIN / 2] }
    }

    /// Feed raw samples at the engine rate.
    pub fn push(&mut self, x: f32) {
        self.acc += x;
        self.acc_n += 1;
        if self.acc_n == 4 {
            if self.buf.len() >= WIN * 2 {
                self.buf.drain(..WIN);
            }
            self.buf.push(self.acc * 0.25);
            self.acc = 0.0;
            self.acc_n = 0;
        }
    }

    /// Pitch of the latest window, or None when quiet / unpitched.
    pub fn estimate(&mut self) -> Option<Pitch> {
        if self.buf.len() < WIN {
            return None;
        }
        let w = &self.buf[self.buf.len() - WIN..];
        let rms = (w.iter().map(|x| x * x).sum::<f32>() / WIN as f32).sqrt();
        if rms < 0.003 {
            return None;
        }
        let half = WIN / 2;
        let min_tau = (self.fs / 1400.0) as usize;
        let max_tau = ((self.fs / 38.0) as usize).min(half - 1);
        for tau in 1..half {
            let mut s = 0.0;
            for j in 0..half {
                let d = w[j] - w[j + tau];
                s += d * d;
            }
            self.diff[tau] = s;
        }
        // cumulative mean normalised difference
        let mut run = 0.0;
        self.diff[0] = 1.0;
        for tau in 1..half {
            run += self.diff[tau];
            self.diff[tau] = if run > 0.0 { self.diff[tau] * tau as f32 / run } else { 1.0 };
        }
        let mut tau = min_tau.max(2);
        let mut found = None;
        while tau < max_tau {
            if self.diff[tau] < 0.15 {
                while tau + 1 < max_tau && self.diff[tau + 1] < self.diff[tau] {
                    tau += 1;
                }
                found = Some(tau);
                break;
            }
            tau += 1;
        }
        let t = found?;
        // parabolic interpolation
        let (a, b, c) = (self.diff[t - 1], self.diff[t], self.diff[t + 1]);
        let den = a + c - 2.0 * b;
        let tf = if den.abs() > 1e-9 { t as f32 + 0.5 * (a - c) / den } else { t as f32 };
        let hz = self.fs / tf;
        let midi = 69.0 + 12.0 * (hz / 440.0).log2();
        let n = midi.round();
        Some(Pitch { hz, note: NAMES[(n as i32).rem_euclid(12) as usize], octave: n as i32 / 12 - 1, cents: (midi - n) * 100.0 })
    }
}
