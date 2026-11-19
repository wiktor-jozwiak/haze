//! Cabinet impulse responses: uniformly partitioned overlap-save FFT convolution (UPOLS).
//!
//! Partition size B = 64 samples, FFT size 2B. Latency = B samples (1.3 ms at 48 kHz).
//! IRs are converted to partition spectra off the audio thread (`IrSpectrum::new`) and handed
//! over as a `Box`; the convolver swaps them in without allocating.

use crate::filters::Biquad;
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

/// Built-in cabinets, synthesised once at start-up (no shipped audio files): an impulse plus
/// a short decaying noise tail, band-limited like a guitar speaker, with cabinet resonances.
pub const CABS: [&str; 3] = ["1x12 Open", "2x12 Open", "4x12 Closed"];

pub fn builtin_cab(kind: usize, fs: f32) -> Vec<f32> {
    let len = (0.04 * fs) as usize;
    let mut seed = 0x9E37_79B9u32.wrapping_add(kind as u32 * 7919);
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let (decay_ms, hp, lp, res_f, res_db, notch_f, breakup_f) = match kind {
        0 => (3.5, 90.0, 5200.0, 140.0, 4.0, 1400.0, 2600.0),
        1 => (5.0, 75.0, 5000.0, 115.0, 6.0, 1100.0, 2400.0),
        _ => (7.0, 65.0, 4400.0, 100.0, 8.0, 900.0, 2100.0),
    };
    let mut x: Vec<f32> = (0..len)
        .map(|i| {
            let t = i as f32 / fs * 1000.0;
            let tail = rnd() * (-t / decay_ms).exp() * 0.35;
            if i == 0 { 1.0 + tail } else { tail }
        })
        .collect();
    let mut f = [Biquad::new(); 7];
    f[0].highpass(hp, 0.8, fs);
    f[1].highpass(hp, 0.8, fs);
    f[2].lowpass(lp, 0.9, fs);
    f[3].lowpass(lp * 1.2, 0.6, fs);
    f[4].peak(res_f, 1.6, res_db, fs);
    f[5].peak(notch_f, 1.2, -6.0, fs);
    f[6].peak(breakup_f, 2.0, 5.0, fs);
    for s in x.iter_mut() {
        let mut v = *s;
        for b in f.iter_mut() {
            v = b.tick(v);
        }
        *s = v;
    }
    normalize(&mut x);
    x
}

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
