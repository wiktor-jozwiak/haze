//! The Katana-voiced effect blocks: levels, decay times and the character of each type.

use haze_dsp::fx::{Comp, Echo, EqSlot, Verb};

const FS: f32 = 48000.0;

fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            amp * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0)
        })
        .collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

/// Wet-only reverb output (direct 0, effect level 1) for a mono input.
fn verb_wet(ty: i32, rt: f32, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut v = Verb::new(FS);
    v.set(ty, rt, 1.0, 0.0, 0.0);
    input.iter().map(|x| v.tick(*x, *x, 0.0, 1.0, 0.0)).unzip()
}

#[test]
fn reverb_level_follows_the_katana_scale() {
    // steady noise: the tail sits ~6 dB under the dry signal at 2 s, louder for longer times
    // guitar-like spectrum: noise through two 2 kHz one-poles
    let mut x = noise(FS as usize * 8, 0.3, 9);
    let a = (-std::f32::consts::TAU * 2000.0 / FS).exp();
    let (mut s1, mut s2) = (0.0, 0.0);
    for v in &mut x {
        s1 += (*v - s1) * (1.0 - a);
        s2 += (s1 - s2) * (1.0 - a);
        *v = s2;
    }
    for ty in [1, 3, 4, 5, 6] {
        let (l, r) = verb_wet(ty, 2.0, &x);
        let s = FS as usize * 4;
        let rel = db(rms(&l[s..]).max(rms(&r[s..])) / rms(&x[s..]));
        eprintln!("type {ty}: wet at RT 2 s {rel:.1} dB re dry");
        assert!((-9.0..-3.0).contains(&rel), "type {ty}: wet {rel:.1} dB, want about −6");
    }
    let (l2, _) = verb_wet(6, 2.0, &x);
    let (l6, _) = verb_wet(6, 6.5, &x);
    let s = FS as usize * 6;
    assert!(rms(&l6[s..]) > rms(&l2[s..]) * 1.3, "a longer reverb must be a bigger wash");
}

#[test]
fn reverb_time_is_rt60() {
    // burst, then measure the slope of the tail energy between −10 and −40 dB
    for (ty, rt) in [(1, 1.2f32), (3, 3.0), (4, 2.0), (6, 6.5)] {
        let n = (FS * (rt * 1.5 + 0.5)) as usize;
        let mut x = noise(FS as usize / 4, 0.3, 3);
        x.resize(n, 0.0);
        let (l, r) = verb_wet(ty, rt, &x);
        let blk = (FS * 0.02) as usize;
        let env: Vec<f32> = l.chunks(blk).zip(r.chunks(blk)).map(|(a, b)| db((rms(a) + rms(b)) * 0.5)).collect();
        let start = FS as usize / 4 / blk + 2;
        let peak = env[start];
        let t10 = env[start..].iter().position(|e| *e < peak - 10.0).expect("−10 dB") as f32;
        let t40 = env[start..].iter().position(|e| *e < peak - 40.0).expect("−40 dB") as f32;
        let measured = (t40 - t10) * blk as f32 / FS * 2.0;
        eprintln!("type {ty}: RT60 set {rt} s, measured {measured:.2} s");
        assert!((measured / rt - 1.0).abs() < 0.3, "type {ty}: RT60 {measured:.2} s, set {rt} s");
    }
}

#[test]
fn reverb_is_wide() {
    let x = noise(FS as usize * 3, 0.1, 5);
    for ty in [1, 3, 4, 6] {
        let (l, r) = verb_wet(ty, 2.5, &x);
        let s = FS as usize;
        let (a, b) = (&l[s..], &r[s..]);
        let c = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (rms(a) * rms(b) * a.len() as f32);
        eprintln!("type {ty}: L/R correlation {c:.2}");
        assert!(c.abs() < 0.5, "type {ty}: tail is near mono (correlation {c:.2})");
    }
}

#[test]
fn modulate_reverb_moves_its_pitch() {
    // a steady sine through MODULATE comes back smeared over neighbouring frequencies,
    // through PLATE it stays a clean line
    let x: Vec<f32> = (0..FS as usize * 4).map(|i| 0.1 * (std::f32::consts::TAU * 440.0 * i as f32 / FS).sin()).collect();
    let smear = |ty: i32| {
        let (l, _) = verb_wet(ty, 4.0, &x);
        let s = &l[FS as usize * 2..];
        let at = |f: f32| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, v) in s.iter().enumerate() {
                let p = std::f32::consts::TAU * f * i as f32 / FS;
                re += v * p.cos();
                im += v * p.sin();
            }
            (re * re + im * im).sqrt()
        };
        (at(437.0) + at(443.0)) / at(440.0)
    };
    let (m, p) = (smear(6), smear(4));
    eprintln!("sidebands / carrier: modulate {m:.3}, plate {p:.3}");
    assert!(m > p * 2.0, "MODULATE must chorus the tail ({m:.3} vs plate {p:.3})");
}

#[test]
fn digital_delay_repeats_at_unity() {
    // a 50 ms 1 kHz burst comes back 300 ms later at the same level (EFFECT LEVEL 100)
    let mut e = Echo::new(FS);
    e.set(0, 0.0);
    let burst = (0.05 * FS) as usize;
    let x = |i: usize| if i < burst { 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / FS).sin() } else { 0.0 };
    let out: Vec<f32> = (0..(0.5 * FS) as usize).map(|i| e.tick(x(i), x(i), 300.0, 0.0, 1.0, 0.0, 1.0, 0.5).0).collect();
    let at = (0.3 * FS) as usize;
    let rep = rms(&out[at + 200..at + burst - 200]);
    let inp = rms(&(200..burst - 200).map(x).collect::<Vec<_>>());
    eprintln!("first repeat {:.2} dB re input", db(rep / inp));
    assert!(db(rep / inp).abs() < 0.5);
}

#[test]
fn tape_and_analog_repeats_are_darker() {
    // high-frequency share of the 2nd repeat, noise burst in
    let hf = |ty: i32| {
        let mut e = Echo::new(FS);
        e.set(ty, 0.0);
        let x = noise(2400, 0.3, 11);
        let out: Vec<f32> = (0..(FS * 0.7) as usize).map(|i| e.tick(*x.get(i).unwrap_or(&0.0), 0.0, 250.0, 0.6, 1.0, 0.0, 1.0, 0.5).0).collect();
        let rep = &out[(0.5 * FS) as usize..(0.5 * FS) as usize + 4000];
        let diff: Vec<f32> = rep.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&diff) / rms(rep)
    };
    let (d, t, a) = (hf(0), hf(8), hf(7));
    eprintln!("HF share of the 2nd repeat: digital {d:.3}, tape {t:.3}, analog {a:.3}");
    assert!(t < d * 0.6 && a < d * 0.6, "tape / analog repeats must lose their top end");
}

#[test]
fn reverse_delay_swells() {
    // a plucked note (fast attack, slow decay) comes back backwards: quiet first, loud last
    let ms = 400.0;
    let n = (ms * 0.001 * FS) as usize;
    let mut e = Echo::new(FS);
    e.set(6, 0.0);
    let pluck = |i: usize| if i < n { (-(i as f32) / (0.08 * FS)).exp() * (0.05 * i as f32).sin() } else { 0.0 };
    let out: Vec<f32> = (0..3 * n).map(|i| e.tick(pluck(i), pluck(i), ms, 0.0, 1.0, 0.0, 1.0, 0.5).0).collect();
    // the reversed chunk plays during the next chunk
    let chunk = &out[n..2 * n];
    let (early, late) = (rms(&chunk[n / 8..n / 2]), rms(&chunk[n / 2..n * 7 / 8]));
    eprintln!("reverse chunk: early {:.1} dB, late {:.1} dB", db(early), db(late));
    assert!(late > early * 3.0, "reverse delay must swell into the note");
}

#[test]
fn compressor_evens_out_dynamics() {
    let mut c = Comp::new(FS);
    c.set(-27.0, 6.0, 20.0, 180.0, 0.0, 0.0);
    let tone = |amp: f32, c: &mut Comp| {
        let y: Vec<f32> = (0..FS as usize).map(|i| c.tick(amp * (std::f32::consts::TAU * 220.0 * i as f32 / FS).sin())).collect();
        rms(&y[FS as usize / 2..])
    };
    let loud = tone(0.5, &mut c);
    let quiet = tone(0.02, &mut c);
    let range = db(loud) - db(quiet);
    eprintln!("28 dB in, {range:.1} dB out");
    assert!(range < 12.0, "compressor left {range:.1} dB of a 28 dB range");
}

#[test]
fn eq_slot_matches_its_settings() {
    let gain_at = |e: &mut EqSlot, f: f32| {
        let y: Vec<f32> = (0..FS as usize / 2).map(|i| e.tick((std::f32::consts::TAU * f * i as f32 / FS).sin())).collect();
        db(rms(&y[FS as usize / 4..]) * 2f32.sqrt())
    };
    // parametric: low cut 100 Hz, +6 dB peak at 800 Hz, high cut 5 kHz
    let mut e = EqSlot::new(FS);
    e.set(0, &[100.0, 0.0, 800.0, 1.0, 6.0, 3000.0, 1.0, 0.0, 0.0, 5000.0, 0.0]);
    let (lo, pk, mid, hi) = (gain_at(&mut e, 40.0), gain_at(&mut e, 800.0), gain_at(&mut e, 1600.0), gain_at(&mut e, 12000.0));
    eprintln!("PEQ: 40 Hz {lo:.1}, 800 Hz {pk:.1}, 1.6 kHz {mid:.1}, 12 kHz {hi:.1} dB");
    assert!(lo < -12.0 && (pk - 6.0).abs() < 0.7 && hi < -12.0);
    // GE-10: +8 dB at the 500 Hz band only
    let mut g = EqSlot::new(FS);
    let mut v = [0f32; 11];
    v[4] = 8.0;
    g.set(1, &v);
    let (b, off) = (gain_at(&mut g, 500.0), gain_at(&mut g, 4000.0));
    eprintln!("GEQ: 500 Hz {b:.1}, 4 kHz {off:.1} dB");
    assert!((b - 8.0).abs() < 1.0 && off.abs() < 0.5);
}
