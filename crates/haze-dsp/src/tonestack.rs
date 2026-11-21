//! Passive treble / bass / middle tone stack ("TMB", Fender – Marshall – Mesa) as a
//! 3rd-order IIR, the way the knobs of a real amp interact: one network, not three
//! independent shelves. Turning bass up also moves the mid scoop, treble also lifts the
//! upper mids, and everything at zero gives almost no output, exactly like the circuit.
//!
//! Coefficients come from the circuit itself. Nodal analysis gives the exact response at a
//! handful of frequencies; the network is a 3rd-order rational function
//! H(s) = (b1 s + b2 s² + b3 s³) / (1 + a1 s + a2 s² + a3 s³), so a least-squares solve on
//! those points recovers b and a, and the bilinear transform turns them into a digital
//! filter. Runs only when a knob or the voicing changes (a few µs, no allocation).
//!
//! Circuit (5F6A Bassman / JCM800 topology):
//! ```text
//!  Vin ──C1── T ─┬─(1−t)·R1─┐
//!   │             (treble pot) Out (wiper)
//!   R4 (slope)               └── t·R1 ── J
//!   │                                    │
//!   X ──C2──────────────────────────── J
//!   X ──C3── M                           │
//!            │ ← l·R2 (bass) ────────────┘
//!            m·R3 (middle) to ground
//! ```

/// Component values of one amp's stack.
#[derive(Clone, Copy, Debug)]
pub struct StackParts {
    /// treble pot, bass pot, middle pot, slope resistor (Ω)
    pub r1: f64,
    pub r2: f64,
    pub r3: f64,
    pub r4: f64,
    /// treble, bass, middle caps (F)
    pub c1: f64,
    pub c2: f64,
    pub c3: f64,
}

/// Blackface Fender (AB763 Twin / Deluxe Reverb): scooped, glassy.
pub const FENDER_BF: StackParts = StackParts { r1: 250e3, r2: 250e3, r3: 10e3, r4: 100e3, c1: 250e-12, c2: 100e-9, c3: 47e-9 };
/// Tweed Bassman 5F6A: the original, fuller mids.
pub const BASSMAN: StackParts = StackParts { r1: 250e3, r2: 1e6, r3: 25e3, r4: 56e3, c1: 250e-12, c2: 20e-9, c3: 20e-9 };
/// Marshall JCM800 / Plexi: less scoop, mids forward.
pub const MARSHALL: StackParts = StackParts { r1: 220e3, r2: 1e6, r3: 22e3, r4: 33e3, c1: 470e-12, c2: 22e-9, c3: 22e-9 };
/// Modern high gain (5150 / Rectifier family): tighter lows, more treble range.
pub const MODERN: StackParts = StackParts { r1: 250e3, r2: 250e3, r3: 25e3, r4: 39e3, c1: 500e-12, c2: 22e-9, c3: 22e-9 };

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn new(re: f64, im: f64) -> C {
        C { re, im }
    }
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
    fn div(self, o: C) -> C {
        let d = o.re * o.re + o.im * o.im;
        C::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
}

/// Audio-taper pot: 10 % of the resistance at half rotation, like an "A" pot.
fn log_taper(p: f64) -> f64 {
    (10f64.powf(2.0 * p.clamp(0.0, 1.0)) - 1.0) / 99.0
}

/// Exact response of the network at angular frequency `w` (rad/s), unloaded output.
/// Knob positions 0..1: treble (linear pot), bass (audio taper), middle (linear).
pub fn analog_response(p: &StackParts, treble: f64, bass: f64, mid: f64, w: f64) -> (f64, f64) {
    let h = analog(p, treble, bass, mid, C::new(0.0, w));
    (h.re, h.im)
}

fn analog(p: &StackParts, treble: f64, bass: f64, mid: f64, s: C) -> C {
    // real pots keep a residual track resistance at the end stops (~0.5–1 %); it also keeps
    // the network 3rd-order, which the fit below relies on
    let t = treble.clamp(0.01, 0.99);
    let rb = log_taper(bass) * p.r2 + 0.005 * p.r2;
    let rm = mid.clamp(0.0, 1.0) * p.r3 + 0.01 * p.r3;
    let g = |r: f64| C::new(1.0 / r, 0.0);
    let y = |c: f64| s.mul(C::new(c, 0.0));
    let (g_top, g_bot) = (g((1.0 - t) * p.r1), g(t * p.r1));
    let (g4, gb, gm) = (g(p.r4), g(rb), g(rm));
    let (y1, y2, y3) = (y(p.c1), y(p.c2), y(p.c3));
    // unknowns: X, T, J, M, Out; Vin = 1
    let z = C::default();
    let mut a = [
        // X: from Vin via R4, to J via C2, to M via C3
        [g4.add(y2).add(y3), z, z.sub(y2), z.sub(y3), z, g4],
        // T: from Vin via C1, to Out via (1−t)R1
        [z, y1.add(g_top), z, z, z.sub(g_top), y1],
        // J: from X via C2, to Out via tR1, to M via bass
        [z.sub(y2), z, y2.add(g_bot).add(gb), z.sub(gb), z.sub(g_bot), z],
        // M: from X via C3, from J via bass, to ground via middle
        [z.sub(y3), z, z.sub(gb), y3.add(gb).add(gm), z, z],
        // Out: unloaded wiper between T and J
        [z, z.sub(g_top), z.sub(g_bot), z, g_top.add(g_bot), z],
    ];
    solve5(&mut a)[4]
}

/// Gaussian elimination with partial pivoting on a 5×5 complex system (last column = RHS).
fn solve5(a: &mut [[C; 6]; 5]) -> [C; 5] {
    for col in 0..5 {
        let piv = (col..5).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap_or(col);
        a.swap(col, piv);
        let d = a[col][col];
        for row in col + 1..5 {
            let f = a[row][col].div(d);
            for k in col..6 {
                a[row][k] = a[row][k].sub(f.mul(a[col][k]));
            }
        }
    }
    let mut x = [C::default(); 5];
    for row in (0..5).rev() {
        let mut acc = a[row][5];
        for k in row + 1..5 {
            acc = acc.sub(a[row][k].mul(x[k]));
        }
        x[row] = acc.div(a[row][row]);
    }
    x
}

/// Analog coefficients (b1, b2, b3, a1, a2, a3) of H(s) with s normalised by `W0`.
fn fit(p: &StackParts, treble: f64, bass: f64, mid: f64) -> [f64; 6] {
    // H(s)(1 + a1 s + a2 s² + a3 s³) = b1 s + b2 s² + b3 s³, linear in the six unknowns.
    // Real and imaginary parts at 6 log-spaced frequencies: 12 equations, normal equations.
    let mut ata = [[0f64; 6]; 6];
    let mut atb = [0f64; 6];
    for k in 0..6 {
        let w = 2.0 * std::f64::consts::PI * 40.0 * 2f64.powf(k as f64 * 1.6) / W0;
        let s = C::new(0.0, w);
        let h = analog(p, treble, bass, mid, C::new(0.0, w * W0));
        let s2 = s.mul(s);
        let s3 = s2.mul(s);
        // unknown order: b1 b2 b3 a1 a2 a3; equation: b·[s s² s³] − a·[Hs Hs² Hs³] = H
        let row = [s, s2, s3, C::default().sub(h.mul(s)), C::default().sub(h.mul(s2)), C::default().sub(h.mul(s3))];
        // weight each frequency by 1/|H| so the fit is relative (dB-like), not absolute
        let wgt = 1.0 / h.abs().max(1e-6);
        for part in 0..2 {
            let r: [f64; 6] = std::array::from_fn(|i| if part == 0 { row[i].re } else { row[i].im } * wgt);
            let rhs = if part == 0 { h.re } else { h.im } * wgt;
            for i in 0..6 {
                for j in 0..6 {
                    ata[i][j] += r[i] * r[j];
                }
                atb[i] += r[i] * rhs;
            }
        }
    }
    solve_real6(ata, atb)
}

fn solve_real6(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> [f64; 6] {
    for col in 0..6 {
        let piv = (col..6).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap_or(col);
        a.swap(col, piv);
        b.swap(col, piv);
        for row in col + 1..6 {
            let f = a[row][col] / a[col][col];
            for k in col..6 {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0f64; 6];
    for row in (0..6).rev() {
        let mut acc = b[row];
        for k in row + 1..6 {
            acc -= a[row][k] * x[k];
        }
        x[row] = acc / a[row][row];
    }
    x
}

/// frequency scale of the fit (rad/s): keeps the normal equations well conditioned
const W0: f64 = 2.0 * std::f64::consts::PI * 1000.0;

/// Jury test for z³ + a1 z² + a2 z + a3 (all poles inside the unit circle).
fn stable(a: &[f64; 4]) -> bool {
    let (a1, a2, a3) = (a[1], a[2], a[3]);
    a.iter().all(|x| x.is_finite()) && a3.abs() < 1.0 && 1.0 + a1 + a2 + a3 > 0.0 && 1.0 - a1 + a2 - a3 > 0.0 && (a3 * a3 - 1.0).abs() > (a3 * a1 - a2).abs()
}

/// The stack as a digital filter (transposed direct form II, f64 state).
#[derive(Clone, Copy, Debug)]
pub struct ToneStack {
    b: [f64; 4],
    a: [f64; 4],
    z: [f64; 3],
    /// output gain that brings noon settings back to unity at 1 kHz-ish loudness
    pub makeup: f64,
}

impl Default for ToneStack {
    fn default() -> Self {
        ToneStack { b: [1.0, 0.0, 0.0, 0.0], a: [1.0, 0.0, 0.0, 0.0], z: [0.0; 3], makeup: 1.0 }
    }
}

impl ToneStack {
    /// Set knobs (0..1) for a circuit at sample rate `fs`.
    pub fn design(&mut self, p: &StackParts, treble: f32, bass: f32, mid: f32, fs: f32) {
        let [b1, b2, b3, a1, a2, a3] = fit(p, treble as f64, bass as f64, mid as f64);
        // bilinear transform, prewarped at 1 kHz so the scoop sits where the circuit has it
        let wc = 2.0 * std::f64::consts::PI * 1000.0;
        let k = wc / (wc / (2.0 * fs as f64)).tan() / W0;
        let (k2, k3) = (k * k, k * k * k);
        // s = K(1−z⁻¹)/(1+z⁻¹), both sides × (1+z⁻¹)³:
        //   1 → [1 3 3 1], s → K[1 1 −1 −1], s² → K²[1 −1 −1 1], s³ → K³[1 −3 3 −1]
        let bz = [b1 * k + b2 * k2 + b3 * k3, b1 * k - b2 * k2 - 3.0 * b3 * k3, -b1 * k - b2 * k2 + 3.0 * b3 * k3, -b1 * k + b2 * k2 - b3 * k3];
        let az = [
            1.0 + a1 * k + a2 * k2 + a3 * k3,
            3.0 + a1 * k - a2 * k2 - 3.0 * a3 * k3,
            3.0 - a1 * k - a2 * k2 + 3.0 * a3 * k3,
            1.0 - a1 * k + a2 * k2 - a3 * k3,
        ];
        let n = az[0];
        let (b, a) = (bz.map(|x| x / n), az.map(|x| x / n));
        // never install a filter that could blow up: keep the previous one instead
        if stable(&a) && b.iter().all(|x| x.is_finite()) {
            self.b = b;
            self.a = a;
        }
    }

    /// Magnitude of the digital filter at `hz`.
    pub fn gain_at(&self, hz: f32, fs: f32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * hz as f64 / fs as f64;
        let ev = |c: &[f64; 4]| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, v) in c.iter().enumerate() {
                re += v * (w * n as f64).cos();
                im -= v * (w * n as f64).sin();
            }
            re.hypot(im)
        };
        ev(&self.b) / ev(&self.a)
    }

    pub fn reset(&mut self) {
        self.z = [0.0; 3];
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[1] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[2] * y + self.z[2];
        self.z[2] = self.b[3] * x - self.a[3] * y;
        let y = y * self.makeup;
        if y.abs() < 1e-20 {
            0.0
        } else {
            y as f32
        }
    }
}
