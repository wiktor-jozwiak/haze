//! PC-only effects: each does the thing its card promises.

use haze_dsp::pcfx::{PcFx, PcVals};
use haze_dsp::{Params, P};

const FS: f32 = 48000.0;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

fn tone(f: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / FS).sin()).collect()
}

/// Magnitude of `f`, averaged over 2048-sample blocks.
fn bin(x: &[f32], f: f32) -> f32 {
    let one = |b: &[f32]| {
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, v) in b.iter().enumerate() {
            let p = std::f32::consts::TAU * f * i as f32 / FS;
            re += v * p.cos();
            im += v * p.sin();
        }
        (re * re + im * im).sqrt() * 2.0 / b.len() as f32
    };
    let c: Vec<f32> = x.chunks_exact(2048).map(one).collect();
    c.iter().sum::<f32>() / c.len().max(1) as f32
}

/// Left output of the stereo stage for a mono input, with `set` applied to fresh params.
fn run(input: &[f32], set: &[(P, f32)], mut each: impl FnMut(usize, &Params)) -> Vec<f32> {
    let p = Params::default();
    for (k, v) in set {
        p.set(*k, *v);
    }
    let mut fx = PcFx::new(FS);
    let mut out = Vec::with_capacity(input.len());
    for (i, x) in input.iter().enumerate() {
        each(i, &p);
        let v = PcVals::read(&p);
        fx.prepare(&v, &p);
        out.push(fx.stereo(*x, *x, &v).0);
    }
    out
}

#[test]
fn shimmer_tail_climbs_an_octave() {
    let mut x = tone(220.0, 0.3, FS as usize);
    x.resize(FS as usize * 20, 0.0);
    let tail = |amt: f32| {
        let y = run(&x, &[(P::PcShimOn, 1.0), (P::PcShimMix, 1.0), (P::PcShimAmt, amt), (P::PcShimDecay, 6.0)], |_, _| {});
        let t = &y[FS as usize * 2..FS as usize * 4];
        (bin(t, 440.0) / bin(t, 220.0).max(1e-9), rms(&y[FS as usize * 2..FS as usize * 3]), rms(&y[FS as usize * 18..]))
    };
    let ((off, _, _), (on, early, late)) = (tail(0.0), tail(1.0));
    eprintln!("440 / 220 Hz in the tail: shimmer amount 0 {off:.3}, 1 {on:.3}; tail {:.1} dB at 2 s, {:.1} dB at 18 s", db(early), db(late));
    assert!(on > off * 4.0 && on > 0.3, "the tail must grow an octave up");
    assert!(late < early * 0.1, "the shimmer must die away, not ring for ever");
}

#[test]
fn freeze_holds_the_note() {
    // a note, freeze pressed at 1 s, guitar silent from 1.1 s: the pad keeps sounding
    let mut x = tone(196.0, 0.3, FS as usize + FS as usize / 10);
    x.resize(FS as usize * 4, 0.0);
    let y = run(&x, &[(P::PcFrzMix, 1.0)], |i, p| p.set(P::PcFrzOn, (i >= FS as usize) as i32 as f32));
    let (note, held) = (rms(&x[FS as usize / 2..FS as usize]), rms(&y[FS as usize * 3..]));
    let blocks: Vec<f32> = y[FS as usize * 2..].chunks(4800).map(|b| db(rms(b))).collect();
    let swing = blocks.iter().cloned().fold(f32::MIN, f32::max) - blocks.iter().cloned().fold(f32::MAX, f32::min);
    eprintln!("frozen pad {:.1} dB re the note, level swing {swing:.1} dB", db(held / note));
    // random grains of one note beat a little against each other: a breathing pad, not a loop
    assert!(db(held / note) > -8.0 && swing < 9.0);
}

#[test]
fn multi_tap_lands_on_the_grid() {
    // 120 BPM, 1/8: echoes at 250, 500 and 750 ms
    let mut x = vec![0.0; FS as usize];
    x[0] = 1.0;
    let y = run(&x, &[(P::PcRhyOn, 1.0), (P::PcRhyMode, 0.0), (P::PcRhyDiv, 1.0), (P::Bpm, 120.0), (P::PcRhyAmt, 0.0), (P::PcRhyMix, 1.0)], |_, _| {});
    let peaks: Vec<usize> = (1..y.len()).filter(|i| y[*i].abs() > 0.2).collect();
    eprintln!("echoes at {:?} samples", peaks);
    assert_eq!(peaks, vec![12000, 36000]);
    // the 2nd tap goes right
}

#[test]
fn slicer_follows_the_tempo() {
    let x = tone(440.0, 0.3, FS as usize * 2);
    let y = run(&x, &[(P::PcRhyOn, 1.0), (P::PcRhyMode, 1.0), (P::PcRhyDiv, 3.0), (P::Bpm, 120.0), (P::PcRhyAmt, 0.5), (P::PcRhyMix, 1.0)], |_, _| {});
    // 1/16 at 120 BPM = 125 ms: on for the first 62 ms of each
    let step = 6000;
    let on = rms(&y[step * 4 + 500..step * 4 + 2500]);
    let off = rms(&y[step * 4 + 3500..step * 4 + 5500]);
    eprintln!("slice on {:.1} dB, off {:.1} dB", db(on), db(off));
    assert!(on > 0.15 && off < 0.002);
}

#[test]
fn lofi_crushes_and_band_limits() {
    let x = tone(1000.0, 0.5, FS as usize);
    let y = run(&x, &[(P::PcLofiOn, 1.0), (P::PcLofiBits, 3.0), (P::PcLofiRate, 4000.0), (P::PcLofiWow, 0.0), (P::PcLofiHiss, 0.0)], |_, _| {});
    let t = &y[FS as usize / 2..];
    let fund = bin(t, 1000.0);
    // the 4 kHz hold makes images at 3 and 5 kHz; 3 bits leave harmonics
    let junk = bin(t, 3000.0) + bin(t, 5000.0) + bin(t, 3000.0 * 1.0);
    eprintln!("lo-fi: 1 kHz {:.1} dB, images {:.1} dB", db(fund), db(junk));
    assert!(fund > 0.1 && junk > fund * 0.05);
}

#[test]
fn tape_stop_winds_down_to_silence() {
    let x = tone(440.0, 0.3, FS as usize * 3);
    let y = run(&x, &[(P::PcStopTime, 1.0)], |i, p| p.set(P::PcStopOn, (i >= FS as usize && i < FS as usize * 5 / 2) as i32 as f32));
    // zero crossings per 100 ms: 88 before, fewer while winding down, silence after
    let zc = |a: usize| y[a..a + 4800].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let (before, early, late) = (zc(FS as usize / 2), zc(FS as usize + 4800), zc(FS as usize + 30000));
    let stopped = rms(&y[FS as usize * 2 + 4800..FS as usize * 2 + 19200]);
    let back = rms(&y[FS as usize * 5 / 2 + 4800..FS as usize * 5 / 2 + 9600]);
    eprintln!("cycles / 100 ms: {before} before, {early} at 0.1 s, {late} at 0.6 s; stopped {:.1} dB; back {:.1} dB", db(stopped), db(back));
    assert!(before > 40 && early < before && late < early && stopped < 1e-4 && back > 0.15);
}
