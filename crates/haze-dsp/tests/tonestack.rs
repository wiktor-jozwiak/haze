//! The TMB tone stack: digital filter matches the circuit, and the knobs act like an amp's.

use haze_dsp::tonestack::{analog_response, ToneStack, BASSMAN, FENDER_BF, MARSHALL, MODERN};

const FS: f32 = 48000.0;

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

fn circuit_db(p: &haze_dsp::tonestack::StackParts, t: f64, b: f64, m: f64, hz: f64) -> f64 {
    let (re, im) = analog_response(p, t, b, m, 2.0 * std::f64::consts::PI * hz);
    db(re.hypot(im))
}

#[test]
fn digital_filter_matches_the_circuit() {
    let mut worst = 0f64;
    for p in [FENDER_BF, BASSMAN, MARSHALL, MODERN] {
        for (t, b, m) in [(0.5, 0.5, 0.5), (0.2, 0.8, 0.3), (0.9, 0.1, 1.0), (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)] {
            let mut s = ToneStack::default();
            s.design(&p, t as f32, b as f32, m as f32, FS);
            for hz in [40.0, 80.0, 160.0, 320.0, 640.0, 1280.0, 2560.0, 5120.0, 10000.0] {
                let want = circuit_db(&p, t, b, m, hz);
                let got = db(s.gain_at(hz as f32, FS));
                // bilinear warping bends the very top; everything a guitar uses must agree
                let tol = if hz > 6000.0 { 1.5 } else { 0.5 };
                worst = worst.max((want - got).abs());
                assert!((want - got).abs() < tol, "{p:?} t{t} b{b} m{m} @ {hz} Hz: circuit {want:.2} dB, filter {got:.2} dB");
            }
        }
    }
    eprintln!("worst deviation from the circuit: {worst:.3} dB");
}

#[test]
fn fender_scoops_the_mids() {
    let lo = circuit_db(&FENDER_BF, 0.5, 0.5, 0.5, 100.0);
    let mid = circuit_db(&FENDER_BF, 0.5, 0.5, 0.5, 500.0);
    let hi = circuit_db(&FENDER_BF, 0.5, 0.5, 0.5, 3000.0);
    eprintln!("fender noon: 100 Hz {lo:.1} dB, 500 Hz {mid:.1} dB, 3 kHz {hi:.1} dB");
    assert!(mid < lo - 3.0 && mid < hi - 3.0, "no scoop");
}

#[test]
fn knobs_do_what_they_say() {
    for p in [FENDER_BF, BASSMAN, MARSHALL, MODERN] {
        let d = |t, b, m, hz| circuit_db(&p, t, b, m, hz);
        assert!(d(0.5, 0.9, 0.5, 80.0) > d(0.5, 0.1, 0.5, 80.0) + 6.0, "{p:?}: bass");
        assert!(d(0.9, 0.5, 0.5, 4000.0) > d(0.1, 0.5, 0.5, 4000.0) + 6.0, "{p:?}: treble");
        assert!(d(0.5, 0.5, 0.9, 600.0) > d(0.5, 0.5, 0.1, 600.0) + 3.0, "{p:?}: middle");
    }
}

#[test]
fn marshall_has_more_mids_than_fender() {
    let scoop = |p| circuit_db(p, 0.5, 0.5, 0.5, 500.0) - circuit_db(p, 0.5, 0.5, 0.5, 3000.0);
    assert!(scoop(&MARSHALL) > scoop(&FENDER_BF) + 2.0);
}

#[test]
fn filter_is_stable_everywhere() {
    for p in [FENDER_BF, BASSMAN, MARSHALL, MODERN] {
        for i in 0..=10 {
            let k = i as f32 / 10.0;
            let mut s = ToneStack::default();
            s.design(&p, k, 1.0 - k, k * 0.7, FS);
            let mut y = 0f32;
            for n in 0..96000 {
                y = s.tick(if n == 0 { 1.0 } else { 0.0 });
                assert!(y.is_finite());
            }
            assert!(y.abs() < 1e-6, "{p:?} k{k}: impulse response does not decay ({y})");
        }
    }
}

