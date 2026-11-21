//! The Katana MOD / FX types played on the PC: each does what its name says.

use haze_dsp::blocks::Drive;
use haze_dsp::fxunit::{diatonic, FxUnit};

const FS: f32 = 48000.0;

fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / FS).sin()).collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

/// Magnitude of the `f` Hz component, averaged over 2048-sample blocks (shorter than a
/// pitch shifter grain, whose phase restarts every grain).
fn bin(x: &[f32], f: f32) -> f32 {
    let c: Vec<f32> = x.chunks_exact(2048).map(|b| bin1(b, f)).collect();
    c.iter().sum::<f32>() / c.len().max(1) as f32
}

fn bin1(x: &[f32], f: f32) -> f32 {
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for (i, v) in x.iter().enumerate() {
        let p = std::f32::consts::TAU * f * i as f32 / FS;
        re += v * p.cos();
        im += v * p.sin();
    }
    (re * re + im * im).sqrt() * 2.0 / x.len() as f32
}

fn run(kind: i32, v: [f32; 10], x: &[f32], midi: f32) -> Vec<f32> {
    let mut u = FxUnit::new(FS);
    x.iter().map(|s| u.tick(*s, kind, &v, midi)).collect()
}

#[test]
fn pitch_shifter_lands_on_the_interval() {
    let x = sine(220.0, 0.3, FS as usize);
    for (semis, want) in [(-12.0, 110.0), (7.0, 329.63), (12.0, 440.0)] {
        let y = run(2, [semis, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], &x, -1.0);
        let t = &y[FS as usize / 4..];
        let (hit, src) = (bin(t, want), bin(t, 220.0));
        eprintln!("{semis:+} st: {want} Hz {:.1} dB, 220 Hz {:.1} dB", db(hit), db(src));
        assert!(hit > 0.15 && src < hit * 0.2, "{semis:+} semitones: wanted {want} Hz");
    }
}

#[test]
fn diatonic_steps_follow_the_scale() {
    // C major: a third above C is E (+4), above E is G (+3), above B is D (+3)
    assert_eq!(diatonic(60.0, 0, false, 2), 4.0);
    assert_eq!(diatonic(64.0, 0, false, 2), 3.0);
    assert_eq!(diatonic(71.0, 0, false, 2), 3.0);
    // a fifth above D in C major is A (+7), below is G (−7); octave is always ±12
    assert_eq!(diatonic(62.0, 0, false, 4), 7.0);
    assert_eq!(diatonic(62.0, 0, false, -4), -7.0);
    assert_eq!(diatonic(62.0, 0, false, -3), -5.0);
    assert_eq!(diatonic(69.0, 9, true, 7), 12.0);
    // A minor: a third above A is C (+3)
    assert_eq!(diatonic(57.0, 9, true, 2), 3.0);
}

#[test]
fn harmonist_follows_the_played_note() {
    // E4 in C major, +3rd → G4 (+3 semitones), heard through the played-note input
    let x = sine(329.63, 0.3, FS as usize);
    let y = run(2, [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0], &x, 64.0);
    let t = &y[FS as usize / 4..];
    let (g, gs) = (bin(t, 392.0), bin(t, 415.3));
    eprintln!("E4 + third: G4 {:.1} dB, G#4 {:.1} dB", db(g), db(gs));
    assert!(g > gs * 4.0);
}

#[test]
fn touch_wah_opens_with_the_pick() {
    // high-frequency share of a harmonic-rich tone: soft vs hard picking
    let saw = |amp: f32| -> Vec<f32> { (0..FS as usize / 2).map(|i| amp * ((i as f32 * 110.0 / FS).fract() * 2.0 - 1.0)).collect() };
    let bright = |y: &[f32]| bin(y, 1650.0) / bin(y, 330.0).max(1e-6);
    let v = [0.0, 0.1, 0.6, 0.7, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    let (soft, hard) = (run(1, v, &saw(0.003), -1.0), run(1, v, &saw(0.3), -1.0));
    let (s, h) = (bright(&soft[12000..]), bright(&hard[12000..]));
    eprintln!("1.65 kHz / 330 Hz: soft {s:.3}, hard {h:.3}");
    assert!(h > s * 3.0, "harder picking must open the wah");
}

#[test]
fn slicer_chops() {
    let x = sine(440.0, 0.3, FS as usize);
    // pattern 1 (every other step), 8 steps a second
    let y = run(7, [1.0, 8.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0], &x, -1.0);
    let blocks: Vec<f32> = y.chunks(1500).map(rms).collect();
    let (lo, hi) = blocks.iter().fold((f32::MAX, 0f32), |(a, b), v| (a.min(*v), b.max(*v)));
    eprintln!("slicer blocks: quietest {:.1} dB, loudest {:.1} dB", db(lo), db(hi));
    assert!(hi / lo.max(1e-6) > 30.0);
}

#[test]
fn slow_gear_swells_in() {
    let mut x = vec![0.0; FS as usize / 2];
    x.extend((0..FS as usize).map(|i| 0.3 * (-(i as f32) / FS / 1.5).exp() * (std::f32::consts::TAU * 196.0 * i as f32 / FS).sin()));
    let y = run(3, [0.6, 0.6, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], &x, -1.0);
    let at = FS as usize / 2;
    let (first, later) = (rms(&y[at..at + 2400]), rms(&y[at + 24000..at + 26400]));
    let (in1, in2) = (rms(&x[at..at + 2400]), rms(&x[at + 24000..at + 26400]));
    eprintln!("first 50 ms {:.1} dB re input, 0.5 s on {:.1} dB", db(first / in1), db(later / in2));
    assert!(first / in1 < 0.1 && later / in2 > 0.7);
}

#[test]
fn boosts_stay_clean() {
    // a clean boost at noon adds level, not distortion: 3rd harmonic well under −40 dB
    let x = sine(220.0, 0.2, FS as usize / 2);
    for m in [6, 7] {
        let mut d = Drive::new(FS);
        let y: Vec<f32> = x.iter().map(|s| d.tick(*s, m, 0.5, 0.5, 0.5)).collect();
        let t = &y[FS as usize / 8..];
        let h3 = db(bin1(t, 660.0) / bin1(t, 220.0));
        eprintln!("model {m}: 3rd harmonic {h3:.1} dB, level {:.1} dB re input", db(rms(t) / rms(&x)));
        assert!(h3 < -40.0, "model {m} distorts ({h3:.1} dB)");
    }
}
