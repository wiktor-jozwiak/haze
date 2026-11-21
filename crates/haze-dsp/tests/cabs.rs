//! Built-in cabinets: the IR has the designed speaker curve and a tight (minimum-phase) attack.

use haze_dsp::conv::{builtin_cab, cab_target_db, CABS};

const FS: f32 = 48000.0;

fn response_db(ir: &[f32], hz: f32) -> f32 {
    let w = std::f64::consts::TAU * hz as f64 / FS as f64;
    let (mut re, mut im) = (0f64, 0f64);
    for (n, &v) in ir.iter().enumerate() {
        re += v as f64 * (w * n as f64).cos();
        im -= v as f64 * (w * n as f64).sin();
    }
    (20.0 * re.hypot(im).log10()) as f32
}

#[test]
fn cabs_follow_their_speaker_curves() {
    for (k, name) in CABS.iter().enumerate() {
        let ir = builtin_cab(k, FS);
        // compare shapes: both curves relative to their 1 kHz value
        let at = |hz: f32| response_db(&ir, hz) - response_db(&ir, 1000.0);
        let want = |hz: f32| cab_target_db(k, hz) - cab_target_db(k, 1000.0);
        let mut worst = 0f32;
        for hz in [80.0, 100.0, 150.0, 250.0, 400.0, 700.0, 1500.0, 2200.0, 2800.0, 3500.0, 4500.0, 6000.0, 8000.0] {
            let e = (at(hz) - want(hz)).abs();
            worst = worst.max(e);
            assert!(e < 1.5, "{name} @ {hz} Hz: IR {:.1} dB, target {:.1} dB", at(hz), want(hz));
        }
        eprintln!("{name}: worst {worst:.2} dB from the curve");
    }
}

#[test]
fn cabs_have_a_tight_attack() {
    for (k, name) in CABS.iter().enumerate() {
        let ir = builtin_cab(k, FS);
        let total: f32 = ir.iter().map(|x| x * x).sum();
        let early: f32 = ir[..(0.005 * FS) as usize].iter().map(|x| x * x).sum();
        eprintln!("{name}: {:.1} % of the energy in the first 5 ms", early / total * 100.0);
        assert!(early / total > 0.9, "{name}: smeared attack");
        let peak = ir.iter().position(|x| x.abs() == ir.iter().fold(0f32, |m, v| m.max(v.abs()))).unwrap();
        assert!(peak < 48, "{name}: peak at sample {peak}");
    }
}

#[test]
fn fizz_is_cut() {
    // amp-sim fizz lives at 6–10 kHz: a guitar cab is ≥ 25 dB down there from its presence peak
    for (k, name) in CABS.iter().enumerate() {
        let ir = builtin_cab(k, FS);
        let presence = [2000.0, 2500.0, 3000.0, 3500.0].map(|f| response_db(&ir, f)).into_iter().fold(f32::MIN, f32::max);
        assert!(presence - response_db(&ir, 9000.0) > 25.0, "{name}");
    }
}
