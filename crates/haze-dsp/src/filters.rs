//! Building blocks: RBJ biquads, one-pole smoothers, fractional delay lines, 4x oversampler.
//! Everything is fixed-size after construction; nothing here allocates while processing.

use std::f32::consts::PI;

#[derive(Clone, Copy, Default, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new() -> Self {
        Biquad { b0: 1.0, ..Default::default() }
    }

    fn set(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    fn w(f: f32, fs: f32) -> (f32, f32) {
        let w = 2.0 * PI * (f / fs).clamp(1e-5, 0.49);
        (w.cos(), w.sin())
    }

    pub fn lowpass(&mut self, f: f32, q: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let al = s / (2.0 * q);
        self.set((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    pub fn highpass(&mut self, f: f32, q: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let al = s / (2.0 * q);
        self.set((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    pub fn bandpass(&mut self, f: f32, q: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let al = s / (2.0 * q);
        self.set(al, 0.0, -al, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    pub fn peak(&mut self, f: f32, q: f32, db: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let a = 10f32.powf(db / 40.0);
        let al = s / (2.0 * q);
        self.set(1.0 + al * a, -2.0 * c, 1.0 - al * a, 1.0 + al / a, -2.0 * c, 1.0 - al / a);
    }

    pub fn low_shelf(&mut self, f: f32, db: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let a = 10f32.powf(db / 40.0);
        let al = s / 2.0 * std::f32::consts::SQRT_2;
        let sa = 2.0 * a.sqrt() * al;
        self.set(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        );
    }

    pub fn high_shelf(&mut self, f: f32, db: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let a = 10f32.powf(db / 40.0);
        let al = s / 2.0 * std::f32::consts::SQRT_2;
        let sa = 2.0 * a.sqrt() * al;
        self.set(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        );
    }

    /// Band-reject: unity everywhere except a narrow cut at `f` (bandwidth ≈ f / q).
    pub fn notch(&mut self, f: f32, q: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let al = s / (2.0 * q);
        self.set(1.0, -2.0 * c, 1.0, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    /// First-order-ish allpass via biquad (used by the phaser).
    pub fn allpass(&mut self, f: f32, q: f32, fs: f32) {
        let (c, s) = Self::w(f, fs);
        let al = s / (2.0 * q);
        self.set(1.0 - al, -2.0 * c, 1.0 + al, 1.0 + al, -2.0 * c, 1.0 - al);
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// One-pole parameter smoother (avoids zipper noise on knob moves).
#[derive(Clone, Copy, Debug)]
pub struct Smooth {
    pub v: f32,
    k: f32,
}

impl Smooth {
    pub fn new(v: f32, ms: f32, fs: f32) -> Self {
        Smooth { v, k: 1.0 - (-1.0 / (ms * 0.001 * fs)).exp() }
    }
    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        self.v += (target - self.v) * self.k;
        self.v
    }
}

/// Circular delay line with linear-interpolated fractional reads. Capacity is fixed at creation.
pub struct DelayLine {
    buf: Vec<f32>,
    w: usize,
}

impl DelayLine {
    pub fn new(max: usize) -> Self {
        DelayLine { buf: vec![0.0; max.next_power_of_two().max(4)], w: 0 }
    }
    #[inline]
    pub fn push(&mut self, x: f32) {
        self.w = (self.w + 1) & (self.buf.len() - 1);
        self.buf[self.w] = x;
    }
    /// Sample written `d` samples ago (fractional), clamped to the capacity.
    #[inline]
    pub fn read(&self, d: f32) -> f32 {
        let m = self.buf.len() - 1;
        let d = d.clamp(0.0, (m - 1) as f32);
        let i = d as usize;
        let f = d - i as f32;
        let a = self.buf[(self.w + self.buf.len() - i) & m];
        let b = self.buf[(self.w + self.buf.len() - i - 1) & m];
        a + (b - a) * f
    }
    pub fn clear(&mut self) {
        self.buf.iter_mut().for_each(|x| *x = 0.0);
    }
}

/// 4x oversampler: zero-stuffing up, 8th-order Butterworth (4 biquads) anti-image and
/// anti-alias filters at 0.42·fs, decimation by 4. Wraps a memoryless shaper.
pub struct Oversampler4 {
    up: [Biquad; 4],
    down: [Biquad; 4],
}

impl Oversampler4 {
    pub fn new(fs: f32) -> Self {
        // Butterworth 8th order: Q of the 4 sections
        const Q: [f32; 4] = [0.5098, 0.6013, 0.9000, 2.5629];
        let mk = || {
            let mut b = [Biquad::new(); 4];
            for (s, q) in b.iter_mut().zip(Q) {
                s.lowpass(0.42 * fs, q, fs * 4.0);
            }
            b
        };
        Oversampler4 { up: mk(), down: mk() }
    }

    #[inline]
    pub fn process(&mut self, x: f32, mut shaper: impl FnMut(f32) -> f32) -> f32 {
        let mut out = 0.0;
        for k in 0..4 {
            // zero-stuffed input, gain 4 restores level
            let mut u = if k == 0 { x * 4.0 } else { 0.0 };
            for s in &mut self.up {
                u = s.tick(u);
            }
            let mut y = shaper(u);
            for s in &mut self.down {
                y = s.tick(y);
            }
            if k == 3 {
                out = y;
            }
        }
        out
    }
}

#[inline]
pub fn db(x: f32) -> f32 {
    10f32.powf(x / 20.0)
}

#[inline]
pub fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}
