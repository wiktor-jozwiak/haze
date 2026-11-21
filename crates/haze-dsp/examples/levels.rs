//! Level report: renders the built-in DI riff through the chain and prints K-weighted loudness
//! (LUFS-like, no gating) and peak for every voicing at typical settings, a gain sweep, and every
//! effect against bypass. `cargo run --release -p haze-dsp --example levels`

use haze_dsp::filters::Biquad;
use haze_dsp::{EqKey, Params, P};
use std::sync::Arc;

const FS: f32 = 48000.0;
const SECS: f32 = 9.6;

/// K-weighted mean square of the stereo render, first 0.5 s skipped (fade-in, trim, tails).
fn loud(out: &[f32]) -> (f32, f32) {
    let mut ms = 0.0f64;
    let mut peak = 0f32;
    let skip = (0.5 * FS) as usize;
    for ch in 0..2 {
        let (mut hs, mut hp) = (Biquad::new(), Biquad::new());
        hs.high_shelf(1681.0, 4.0, FS);
        hp.highpass(38.0, 0.5, FS);
        for (i, s) in out.iter().skip(ch).step_by(2).enumerate() {
            let k = hp.tick(hs.tick(*s));
            if i >= skip {
                ms += (k * k) as f64;
                peak = peak.max(s.abs());
            }
        }
    }
    let n = (out.len() / 2 - skip) as f64;
    ((-0.691 + 10.0 * (ms / n).log10()) as f32, 20.0 * peak.log10())
}

fn base(voicing: f32, gain: f32, master: f32) -> Arc<Params> {
    let p = Arc::new(Params::default());
    p.set(P::AmpVoicing, voicing);
    p.set(P::AmpGain, gain);
    p.set(P::AmpMaster, master);
    p.set(P::CabIr, match voicing as i32 { 0 => 0.0, 1 | 2 => 1.0, _ => 2.0 });
    p.set(P::AmpSag, if voicing >= 3.0 { 0.45 } else { 0.25 });
    p.set(P::Limiter, 0.0);
    p
}

fn render(p: &Arc<Params>) -> Vec<f32> {
    haze_dsp::render_offline(p.clone(), FS, SECS)
}

/// Effect strength: level change vs bypass, and how much of the output is not the dry signal
/// (RMS of on − off over RMS of off, dB).
fn fx_row(name: &str, p: Arc<Params>, set: impl Fn(&Params)) {
    let off = render(&p);
    set(&p);
    let on = render(&p);
    let (lo, _) = loud(&off);
    let (ln, pk) = loud(&on);
    let skip = (FS as usize) * 2;
    let d: f64 = on[skip..].iter().zip(&off[skip..]).map(|(a, b)| ((a - b) * (a - b)) as f64).sum();
    let o: f64 = off[skip..].iter().map(|b| (b * b) as f64).sum();
    println!("{name:<22} level {:+5.1} dB   wet/diff {:+6.1} dB   peak {pk:+5.1}", ln - lo, 10.0 * (d / o).log10());
}

fn main() {
    if std::env::args().any(|a| a == "drive") {
        let riff = haze_dsp::di::builtin_riff(FS);
        let st = |x: &[f32]| -> Vec<f32> { x.iter().flat_map(|v| [*v, *v]).collect() };
        let (li, _) = loud(&st(&riff));
        for m in 0..9 {
            let mut d = haze_dsp::blocks::Drive::new(FS);
            let y: Vec<f32> = riff.iter().map(|x| d.tick(*x, m, 0.5, 0.5, 0.5)).collect();
            let (lo, pk) = loud(&st(&y));
            println!("drive {m}: {:+5.1} dB vs input, peak {pk:+5.1}", lo - li);
        }
        return;
    }
    if std::env::args().any(|a| a == "wah") {
        for peak in [0.0f32, 0.35, 0.5, 1.0] {
            for pos in [0.0f32, 0.35, 0.7, 1.0] {
                fx_row(&format!("wah pk {peak:.2} pos {pos:.2}"), base(1.0, 0.3, 0.55), |p| {
                    let v = [2.0, pos, peak, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
                    p.set_i(Params::fx(0, EqKey::On), 1.0);
                    p.set_i(Params::fx(0, EqKey::Kind), 1.0);
                    for (k, x) in v.iter().enumerate() {
                        p.set_i(Params::fx(0, EqKey::V(k)), *x);
                    }
                });
            }
        }
        for (a, b) in [(0.0f32, 2.0f32), (0.0, 4.0), (1.0, 3.0)] {
            fx_row(&format!("human {a}-{b} auto"), base(1.0, 0.3, 0.55), |p| {
                let v = [a, b, 4.1, 1.0, 1.0, 0.5, 0.5, 1.0, 0.0, 0.0];
                p.set_i(Params::fx(0, EqKey::On), 1.0);
                p.set_i(Params::fx(0, EqKey::Kind), 6.0);
                for (k, x) in v.iter().enumerate() {
                    p.set_i(Params::fx(0, EqKey::V(k)), *x);
                }
            });
        }
        return;
    }
    let riff = haze_dsp::di::builtin_riff(FS);
    let st: Vec<f32> = riff.iter().cycle().take((SECS * FS) as usize).flat_map(|x| [*x, *x]).collect();
    let (l, pk) = loud(&st);
    println!("== DI riff itself: {l:+6.1} LUFS  peak {pk:+5.1}");
    let names = ["Acoustic", "Clean", "Pushed", "Crunch", "Lead", "Brown", "Metal"];
    println!("== voicings at template-like knobs (gain, master) — limiter off, out -18 dB (default)");
    let typical = [(0.30, 0.55), (0.30, 0.55), (0.35, 0.52), (0.50, 0.55), (0.75, 0.50), (0.80, 0.50), (0.85, 0.50)];
    for (v, (g, m)) in typical.iter().enumerate() {
        let (l, pk) = loud(&render(&base(v as f32, *g, *m)));
        println!("{:<9} g {g:.2} m {m:.2}  {l:+6.1} LUFS  peak {pk:+5.1} dBFS  crest {:4.1}", names[v], pk - l);
    }
    for in_db in [-3.0f32, -6.0, -9.0] {
        println!("== template knobs, guitar {in_db} dB under the DI riff");
        for (v, (g, m)) in typical.iter().enumerate() {
            let p = base(v as f32, *g, *m);
            p.set(P::InputGainDb, in_db);
            let (l, pk) = loud(&render(&p));
            println!("{:<9} {l:+6.1} LUFS  peak {pk:+5.1}", names[v]);
        }
    }
    println!("== noon (gain 0.5, master 0.5)");
    for (v, n) in names.iter().enumerate() {
        let (l, pk) = loud(&render(&base(v as f32, 0.5, 0.5)));
        println!("{n:<9} {l:+6.1} LUFS  peak {pk:+5.1}");
    }
    println!("== clean gain sweep (master 0.5)");
    for g in [0.1, 0.3, 0.5, 0.7, 0.9] {
        let (l, pk) = loud(&render(&base(1.0, g, 0.5)));
        println!("clean g {g:.1}  {l:+6.1} LUFS  peak {pk:+5.1}");
    }
    println!("== master sweep (clean gain 0.3)");
    for m in [0.2, 0.4, 0.5, 0.6, 0.8, 1.0] {
        let (l, pk) = loud(&render(&base(1.0, 0.3, m)));
        println!("clean m {m:.1}  {l:+6.1} LUFS  peak {pk:+5.1}");
    }

    println!("== modulation on clean (Katana-mapped defaults: rate knob 30, depth 50)");
    let rate = 0.1 + 7.9 * 0.3f32.powi(2);
    for (t, n) in ["Chorus", "Flanger", "Phaser", "Tremolo", "Rotary"].iter().enumerate() {
        fx_row(n, base(1.0, 0.3, 0.55), |p| {
            p.set(P::ModOn, 1.0);
            p.set(P::ModType, t as f32);
            p.set(P::ModRate, rate);
            p.set(P::ModDepth, 0.5);
            p.set(P::ModMix, if t == 3 { 1.0 } else { 0.5 });
        });
    }
    println!("== effect units on clean, post amp (Katana init values)");
    let units: [(&str, i32, [f32; 10]); 9] = [
        ("T.Wah", 1, [0.0, 0.35, 0.35, 0.5, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
        ("Auto wah", 1, [1.0, 0.35, 0.5, 5.1, 0.6, 0.0, 1.0, 0.0, 0.0, 1.0]),
        ("Octave", 2, [-12.0, 0.62, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Slow gear", 3, [0.45, 1.3, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Vibrato", 4, [8.2, 0.45, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Ring mod", 5, [200.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Humanizer", 6, [0.0, 2.0, 8.1, 1.0, 1.0, 0.5, 0.5, 1.4, 0.0, 0.0]),
        ("Slicer", 7, [0.0, 13.0, 1.0, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Ac sim", 8, [-2.0, 4.0, 4.0, 1.0, -6.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ];
    for (n, kind, v) in units {
        fx_row(n, base(1.0, 0.3, 0.55), |p| {
            p.set_i(Params::fx(0, EqKey::On), 1.0);
            p.set_i(Params::fx(0, EqKey::Kind), kind as f32);
            p.set_i(Params::fx(0, EqKey::Pre), 0.0);
            for (k, x) in v.iter().enumerate() {
                p.set_i(Params::fx(0, EqKey::V(k)), *x);
            }
        });
    }
    println!("== delay / reverb on clean (param defaults)");
    fx_row("Delay", base(1.0, 0.3, 0.55), |p| p.set(P::DelayOn, 1.0));
    for (t, n) in [(1.0, "Room"), (3.0, "Hall"), (4.0, "Plate"), (5.0, "Spring"), (6.0, "Modulate")] {
        fx_row(n, base(1.0, 0.3, 0.55), |p| {
            p.set(P::ReverbOn, 1.0);
            p.set(P::ReverbType, t);
        });
    }
    println!("== comp on clean (thresh -24, ratio 4, makeup 0)");
    fx_row("Comp", base(1.0, 0.3, 0.55), |p| p.set(P::CompOn, 1.0));
    println!("== drive pedals into clean (amt .5 tone .5 level .5)");
    for (m, n) in ["T-Scream", "Klon", "Blues Drv", "RAT", "Muff", "Metal Zone", "Clean bst", "Mid bst", "Treble bst"].iter().enumerate() {
        fx_row(n, base(1.0, 0.3, 0.55), |p| {
            p.set(P::DriveOn, 1.0);
            p.set(P::DriveModel, m as f32);
        });
    }
}
