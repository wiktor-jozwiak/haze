//! Cabinet impulse responses: uniformly partitioned overlap-save FFT convolution (UPOLS).
//!
//! Partition size B = 64 samples, FFT size 2B. Latency = B samples (1.3 ms at 48 kHz).
//! IRs are converted to partition spectra off the audio thread (`IrSpectrum::new`) and handed
//! over as a `Box`; the convolver swaps them in without allocating.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const B: usize = 64;
const N: usize = 2 * B;
const BINS: usize = B + 1;
/// Longest IR kept (samples): ~170 ms at 48 kHz, plenty for a cabinet.
pub const MAX_IR: usize = 8192;
pub const MAX_PARTS: usize = MAX_IR / B;

pub struct IrSpectrum {
    parts: Vec<Complex<f32>>,
    count: usize,
    pub name: String,
}

impl IrSpectrum {
    /// Off the audio thread: split the IR into partitions and transform each.
    pub fn new(ir: &[f32], name: impl Into<String>) -> Self {
        let ir = &ir[..ir.len().min(MAX_IR)];
        let count = ir.len().div_ceil(B).max(1);
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(N);
        let mut parts = vec![Complex::default(); count * BINS];
        let mut buf = vec![0f32; N];
        for p in 0..count {
            buf.iter_mut().for_each(|x| *x = 0.0);
            let s = p * B;
            let e = (s + B).min(ir.len());
            buf[..e - s].copy_from_slice(&ir[s..e]);
            fwd.process(&mut buf, &mut parts[p * BINS..(p + 1) * BINS]).expect("fft");
        }
        IrSpectrum { parts, count, name: name.into() }
    }
}

pub struct Convolver {
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    ir: Box<IrSpectrum>,
    /// frequency-domain delay line of input frames
    fdl: Vec<Complex<f32>>,
    fdl_pos: usize,
    frame: Vec<f32>,
    time: Vec<f32>,
    spec: Vec<Complex<f32>>,
    acc: Vec<Complex<f32>>,
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    inbuf: [f32; B],
    outbuf: [f32; B],
    n: usize,
}

impl Convolver {
    pub fn new(ir: Box<IrSpectrum>) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(N);
        let inv = planner.plan_fft_inverse(N);
        let scratch_f = fwd.make_scratch_vec();
        let scratch_i = inv.make_scratch_vec();
        Convolver {
            fwd,
            inv,
            ir,
            fdl: vec![Complex::default(); MAX_PARTS * BINS],
            fdl_pos: 0,
            frame: vec![0.0; N],
            time: vec![0.0; N],
            spec: vec![Complex::default(); BINS],
            acc: vec![Complex::default(); BINS],
            scratch_f,
            scratch_i,
            inbuf: [0.0; B],
            outbuf: [0.0; B],
            n: 0,
        }
    }

    /// Swap the IR; returns the old one so the caller can drop it off the audio thread.
    pub fn swap_ir(&mut self, ir: Box<IrSpectrum>) -> Box<IrSpectrum> {
        std::mem::replace(&mut self.ir, ir)
    }

    pub fn ir_name(&self) -> &str {
        &self.ir.name
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.outbuf[self.n];
        self.inbuf[self.n] = x;
        self.n += 1;
        if self.n == B {
            self.n = 0;
            self.block();
        }
        y
    }

    fn block(&mut self) {
        // sliding window: [previous block | current block]
        self.frame.copy_within(B..N, 0);
        self.frame[B..].copy_from_slice(&self.inbuf);
        self.time.copy_from_slice(&self.frame);
        let _ = self.fwd.process_with_scratch(&mut self.time, &mut self.spec, &mut self.scratch_f);
        self.fdl_pos = (self.fdl_pos + 1) % MAX_PARTS;
        let at = self.fdl_pos * BINS;
        self.fdl[at..at + BINS].copy_from_slice(&self.spec);
        self.acc.iter_mut().for_each(|c| *c = Complex::default());
        for p in 0..self.ir.count {
            let slot = (self.fdl_pos + MAX_PARTS - p) % MAX_PARTS;
            let x = &self.fdl[slot * BINS..(slot + 1) * BINS];
            let h = &self.ir.parts[p * BINS..(p + 1) * BINS];
            for ((a, x), h) in self.acc.iter_mut().zip(x).zip(h) {
                *a += x * h;
            }
        }
        // real IFFT needs purely real DC and Nyquist bins
        self.acc[0].im = 0.0;
        self.acc[B].im = 0.0;
        let _ = self.inv.process_with_scratch(&mut self.acc, &mut self.time, &mut self.scratch_i);
        let scale = 1.0 / N as f32;
        for (o, t) in self.outbuf.iter_mut().zip(&self.time[B..]) {
            *o = t * scale;
        }
    }
}

/// Built-in cabinets, designed once at start-up (no shipped audio files). Each is the
/// minimum-phase impulse response of a measured-style speaker curve: the low resonance of
/// the cabinet (open backs roll off earlier), the midrange dip, the cone's presence peaks
/// around 2–3.5 kHz and the steep rolloff above ~5 kHz that makes a guitar speaker sound
/// like one, plus a little cone-breakup ripple. Minimum phase keeps the attack tight, as a
/// close-miked speaker is, instead of the smeared tail a noise-based IR has.
pub const CABS: [&str; 3] = ["1x12 Open", "2x12 Open", "4x12 Closed"];

/// Magnitude targets (Hz, dB), interpolated on a log-frequency axis.
const CAB_CURVES: [&[(f32, f32)]; 3] = [
    // 1x12 open back, American ceramic speaker: loose lows, smooth highs
    &[
        (20.0, -30.0), (60.0, -14.0), (90.0, -4.0), (120.0, 0.0), (200.0, -1.0), (400.0, -2.0), (700.0, -3.0),
        (1200.0, -1.0), (2000.0, 2.0), (2800.0, 4.0), (3500.0, 1.0), (4200.0, -2.0), (5000.0, -8.0),
        (6000.0, -16.0), (8000.0, -30.0), (12000.0, -45.0), (24000.0, -60.0),
    ],
    // 2x12 open back, British alnico: chime peak near 3 kHz
    &[
        (20.0, -28.0), (55.0, -12.0), (85.0, -2.0), (110.0, 1.0), (200.0, 0.0), (450.0, -2.0), (800.0, -1.0),
        (1500.0, 1.0), (2500.0, 4.0), (3200.0, 6.0), (4000.0, 2.0), (5000.0, -4.0), (6500.0, -15.0),
        (8000.0, -28.0), (12000.0, -44.0), (24000.0, -60.0),
    ],
    // 4x12 closed back, British ceramic (V30 type): tight thump, upper-mid bark, fizz cut
    &[
        (20.0, -30.0), (50.0, -14.0), (75.0, -3.0), (105.0, 3.0), (140.0, 2.0), (250.0, 0.0), (500.0, -3.0),
        (800.0, -2.0), (1500.0, 1.0), (2200.0, 3.0), (2800.0, 6.0), (3500.0, 2.0), (4000.0, -1.0),
        (4800.0, 2.0), (5500.0, -6.0), (7000.0, -20.0), (9000.0, -34.0), (13000.0, -48.0), (24000.0, -60.0),
    ],
];

/// Target response of built-in cabinet `kind` at `hz`, in dB (before normalisation).
pub fn cab_target_db(kind: usize, hz: f32) -> f32 {
    let c = CAB_CURVES[kind.min(2)];
    let f = hz.clamp(c[0].0, c[c.len() - 1].0);
    let i = c.windows(2).position(|w| f <= w[1].0).unwrap_or(c.len() - 2);
    let ((f0, d0), (f1, d1)) = (c[i], c[i + 1]);
    let t = (f / f0).ln() / (f1 / f0).ln();
    let base = d0 + (d1 - d0) * t;
    // cone breakup: a gentle ripple that grows from 1 to 5 kHz
    let w = ((hz - 1000.0) / 4000.0).clamp(0.0, 1.0);
    let ripple = 1.2 * w * (hz.log2() * 5.3 + kind as f32 * 1.7).sin();
    base + ripple
}

/// Length of the built-in cabinet IRs (samples at 48 kHz: ~43 ms).
const CAB_LEN: usize = 2048;

pub fn builtin_cab(kind: usize, fs: f32) -> Vec<f32> {
    // minimum phase from the magnitude via the real cepstrum:
    // c = IFFT(ln|H|); fold the anticausal half onto the causal one; H_min = exp(FFT(c))
    let n = 16384;
    let bins = n / 2 + 1;
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut spec: Vec<Complex<f64>> = (0..bins)
        .map(|k| {
            let hz = (k as f32 * fs / n as f32).max(1.0);
            let db = cab_target_db(kind, hz) as f64;
            Complex::new(db / 20.0 * std::f64::consts::LN_10, 0.0)
        })
        .collect();
    let mut cep = vec![0f64; n];
    inv.process(&mut spec, &mut cep).expect("ifft");
    cep.iter_mut().for_each(|x| *x /= n as f64);
    for (i, x) in cep.iter_mut().enumerate() {
        *x *= match i {
            0 => 1.0,
            i if i < n / 2 => 2.0,
            i if i == n / 2 => 1.0,
            _ => 0.0,
        };
    }
    fwd.process(&mut cep, &mut spec).expect("fft");
    for c in spec.iter_mut() {
        let m = c.re.exp();
        *c = Complex::new(m * c.im.cos(), m * c.im.sin());
    }
    // the inverse real FFT wants a real DC and Nyquist bin
    spec[0].im = 0.0;
    spec[bins - 1].im = 0.0;
    let mut ir = vec![0f64; n];
    inv.process(&mut spec, &mut ir).expect("ifft");
    let len = ((CAB_LEN as f32 * fs / 48000.0) as usize).min(MAX_IR);
    let fade = len / 4;
    let mut x: Vec<f32> = (0..len)
        .map(|i| {
            let w = if i + fade >= len { 0.5 + 0.5 * (std::f64::consts::PI * (i + fade - len) as f64 / fade as f64).cos() } else { 1.0 };
            (ir[i] / n as f64 * w) as f32
        })
        .collect();
    // level by the guitar's band, not by total energy: the old noise-tail cabs gave about
    // +4 dB there on a clean note, and more on distortion, whose fizz they let through; +7 dB
    // splits the difference so patches keep roughly their loudness
    let bands = [150.0f64, 300.0, 600.0, 1200.0, 2400.0];
    let mean_db = bands
        .iter()
        .map(|&hz| {
            let w = std::f64::consts::TAU * hz / fs as f64;
            let (re, im) = x.iter().enumerate().fold((0.0, 0.0), |(re, im), (k, &v)| (re + v as f64 * (w * k as f64).cos(), im - v as f64 * (w * k as f64).sin()));
            20.0 * re.hypot(im).log10()
        })
        .sum::<f64>()
        / bands.len() as f64;
    let g = 10f64.powf((CAB_BAND_DB - mean_db) / 20.0) as f32;
    x.iter_mut().for_each(|v| *v *= g);
    x
}

/// In-band gain of the built-in cabinets (mean over 150 Hz–2.4 kHz), dB.
const CAB_BAND_DB: f64 = 7.0;

/// Unit energy, so switching IRs keeps the level.
pub fn normalize(x: &mut [f32]) {
    let e: f32 = x.iter().map(|v| v * v).sum::<f32>().sqrt();
    if e > 1e-9 {
        x.iter_mut().for_each(|v| *v /= e * 1.6);
    }
}

/// Mono samples of a .wav at the engine rate (linear resampling), any bit depth.
pub fn load_wav(path: &std::path::Path, fs: f32, max_len: Option<usize>) -> Result<Vec<f32>, String> {
    let mut r = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let k = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * k).collect()
        }
    };
    let mono: Vec<f32> = raw.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect();
    if mono.is_empty() {
        return Err("pusty plik WAV".into());
    }
    let ratio = spec.sample_rate as f32 / fs;
    let out_len = ((mono.len() as f32) / ratio) as usize;
    let out_len = max_len.map_or(out_len, |m| out_len.min(m));
    Ok((0..out_len)
        .map(|i| {
            let p = i as f32 * ratio;
            let j = p as usize;
            let f = p - j as f32;
            let a = mono[j.min(mono.len() - 1)];
            let b = mono[(j + 1).min(mono.len() - 1)];
            a + (b - a) * f
        })
        .collect())
}
