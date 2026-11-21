//! Distortion report: a steady sine through the whole chain (interface input path), and how
//! much of the output is neither the note nor its harmonics: aliasing, intermodulation, noise.
//! `cargo run --release -p haze-dsp --example distortion` (`amp`, `comp`: single stages)

use haze_dsp::chain::Chain;
use haze_dsp::{Meters, Params, P};
use realfft::RealFftPlanner;
use std::sync::Arc;

const FS: f32 = 48000.0;

fn run(p: Arc<Params>, f: f32, amp: f32) -> Vec<f32> {
    p.set(P::Source, 0.0);
    p.set(P::InputAuto, 0.0);
    p.set(P::GateOn, 0.0);
    p.set(P::HumHz, 0.0);
    let n = (3.0 * FS) as usize;
    let input: Vec<f32> = (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / FS).sin()).collect();
    let (mut chain, _h) = Chain::new(FS, p, Arc::new(Meters::default()), None);
    let mut out = vec![0f32; n];
    let mut r = vec![0f32; 128];
    for (i, o) in input.chunks(128).zip(out.chunks_mut(128)) {
        chain.process(i, o, &mut r[..i.len()]);
    }
    out
}

/// (non-harmonic energy dB re total, output peak dBFS) of the last second; f must be an integer.
fn nhr(y: &[f32], f: f32) -> (f32, f32) {
    let n = FS as usize;
    let seg = &y[y.len() - n..];
    let mut buf: Vec<f32> = seg.iter().enumerate().map(|(i, v)| v * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
    let mut spec = RealFftPlanner::<f32>::new().plan_fft_forward(n).make_output_vec();
    RealFftPlanner::<f32>::new().plan_fft_forward(n).process(&mut buf, &mut spec).unwrap();
    let (mut harm, mut other, mut fund) = (0f64, 0f64, 0f64);
    for (k, c) in spec.iter().enumerate().skip(20) {
        let e = c.norm_sqr() as f64;
        let h = (k as f32 / f).round() * f;
        if h >= f && (k as f32 - h).abs() <= 3.0 {
            harm += e;
            if h == f {
                fund += e;
            }
        } else {
            other += e;
        }
    }
    let peak = seg.iter().fold(0f32, |m, v| m.max(v.abs()));
    let thd = 10.0 * ((harm - fund) / fund).log10();
    eprint!("THD {thd:+6.1} dB  ");
    ((10.0 * (other / (harm + other)).log10()) as f32, 20.0 * peak.log10())
}

fn amp_thd(v: i32, gain: f32, master: f32, sag: f32, f: f32, amp: f32) -> f32 {
    let mut a = haze_dsp::blocks::Amp::new(FS);
    let n = (3.0 * FS) as usize;
    let y: Vec<f32> = (0..n)
        .map(|i| a.tick(amp * (std::f32::consts::TAU * f * i as f32 / FS).sin(), v, gain, 0.45, 0.55, 0.62, 0.58, 0.5, master, sag))
        .collect();
    nhr(&y, f);
    0.0
}

fn main() {
    if std::env::args().any(|a| a == "amp") {
        for (sag, m) in [(0.25, 0.55), (0.0, 0.55), (0.25, 0.3), (0.0, 0.3), (0.0, 0.1)] {
            for g in [0.3f32, 0.1] {
                eprint!("clean g {g} m {m} sag {sag} 82 Hz -6 dBFS: ");
                amp_thd(1, g, m, sag, 82.0, 0.5);
                eprintln!();
            }
        }
        return;
    }
    if std::env::args().any(|a| a == "comp") {
        // Katana COMP init (SUSTAIN 50, ATTACK 50) and LIMITER init, as the studio maps them
        let comps: [(&str, [f32; 5], bool); 4] = [
            ("comp pre", [-27.5, 6.0, 27.0, 180.0, 7.9], true),
            ("comp post", [-27.5, 6.0, 27.0, 180.0, 7.9], false),
            ("limiter pre", [-12.0, 5.0, 0.1, 118.0, -6.0], true),
            ("limiter post", [-12.0, 5.0, 0.1, 118.0, -6.0], false),
        ];
        for (name, c, pre) in comps {
            for f in [82.0, 196.0, 659.0] {
                for amp in [0.5f32, 0.15] {
                    let p = Arc::new(Params::default());
                    p.set(P::AmpGain, 0.3);
                    p.set(P::AmpMaster, 0.55);
                    p.set(P::CabIr, 1.0);
                    p.set(P::CompOn, 1.0);
                    p.set(P::CompPre, pre as i32 as f32);
                    p.set(P::CompThreshDb, c[0]);
                    p.set(P::CompRatio, c[1]);
                    p.set(P::CompAttackMs, c[2]);
                    p.set(P::CompReleaseMs, c[3]);
                    p.set(P::CompMakeupDb, c[4]);
                    let (r, pk) = nhr(&run(p, f, amp), f);
                    println!("{name:<13} {f:>5} Hz in {:+5.1} dBFS: non-harmonic {r:+6.1} dB  out peak {pk:+5.1}", 20.0 * amp.log10());
                }
            }
        }
        return;
    }
    let rigs: [(&str, f32, f32, f32, bool); 6] = [
        ("clean starter", 1.0, 0.30, 0.55, false),
        ("clean hot", 1.0, 0.70, 0.75, false),
        ("acoustic", 0.0, 0.30, 0.55, false),
        ("pushed", 2.0, 0.35, 0.52, false),
        ("crunch", 3.0, 0.50, 0.55, false),
        ("lead + T-Scream", 4.0, 0.75, 0.50, true),
    ];
    for (name, v, g, m, drive) in rigs {
        for f in [82.0, 196.0, 659.0, 1319.0] {
            for amp in [0.5f32, 0.15] {
                let p = Arc::new(Params::default());
                p.set(P::AmpVoicing, v);
                p.set(P::AmpGain, g);
                p.set(P::AmpMaster, m);
                p.set(P::CabIr, match v as i32 { 0 => 0.0, 1 | 2 => 1.0, _ => 2.0 });
                p.set(P::DriveOn, drive as i32 as f32);
                let (r, pk) = nhr(&run(p, f, amp), f);
                println!("{name:<16} {f:>5} Hz in {:+5.1} dBFS: non-harmonic {r:+6.1} dB  out peak {pk:+5.1}", 20.0 * amp.log10());
            }
        }
    }
}
