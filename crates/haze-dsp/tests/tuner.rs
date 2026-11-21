//! Tuner accuracy on synthetic plucked strings: harmonics with a weak fundamental (as on a
//! low string through a pickup), string inharmonicity, decay and noise.

use haze_dsp::tuner::{nearest_string, Tuner, TUNINGS};

const FS: f32 = 48000.0;

/// Plucked-string model: 10 partials, f_n = n f0 sqrt(1 + B n²), 2nd harmonic louder than
/// the fundamental, each partial decaying faster than the one below it.
fn pluck(hz: f32, secs: f32, b: f32, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    let n = (FS * secs) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / FS;
            let mut y = 0.0;
            for k in 1..=10 {
                let kf = k as f32;
                let f = kf * hz * (1.0 + b * kf * kf).sqrt();
                if f > FS * 0.45 {
                    break;
                }
                let amp = if k == 1 { 0.35 } else if k == 2 { 1.0 } else { 0.8 / kf };
                y += amp * (std::f32::consts::TAU * f * t + kf).sin() * (-t * (1.0 + 0.6 * kf)).exp();
            }
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            0.12 * y + 0.002 * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0)
        })
        .collect()
}

fn read(hz: f32, b: f32) -> haze_dsp::Pitch {
    let mut t = Tuner::new(FS);
    let x = pluck(hz, 0.6, b, hz.to_bits());
    let mut last = None;
    // estimates at ~15 Hz from 0.15 s on, like the UI
    for (i, v) in x.iter().enumerate() {
        t.push(*v);
        if i > 7200 && i % 3200 == 0 {
            last = t.estimate().or(last);
        }
    }
    last.unwrap_or_else(|| panic!("no pitch for {hz} Hz"))
}

#[test]
fn every_string_of_every_tuning_reads_right() {
    let mut worst = 0f32;
    for tu in &TUNINGS {
        for &m in tu.strings {
            let hz = 440.0 * 2f32.powf((m as f32 - 69.0) / 12.0);
            let p = read(hz, 0.0);
            assert_eq!(p.midi, m, "{}: string {m} ({hz:.2} Hz) read as {p:?}", tu.name);
            worst = worst.max(p.cents.abs());
            assert!(p.cents.abs() < 0.25, "{}: {hz:.2} Hz off by {:.2} cents", tu.name, p.cents);
            // a real wound string's partials run sharp (inharmonicity B ≈ 1e-4 lifts the 3rd
            // by ~0.6 cent): the periodicity reading follows a little, as the ear does, but
            // must never leave the note
            let q = read(hz, 0.00008);
            assert_eq!(q.midi, m, "{}: inharmonic string {m} read as {q:?}", tu.name);
            assert!(q.cents.abs() < 1.0, "{}: inharmonic {hz:.2} Hz off by {:.2} cents", tu.name, q.cents);
        }
    }
    eprintln!("worst error over all strings: {worst:.3} cents");
}

#[test]
fn detuned_strings_read_their_cents() {
    for (hz, cents) in [(82.41, -12.0), (110.0, 7.5), (146.83, -3.0), (196.0, 1.0), (246.94, 20.0), (329.63, -0.5)] {
        let f = hz * 2f32.powf(cents / 1200.0);
        let p = read(f, 0.0);
        assert!((p.cents - cents).abs() < 0.3, "{f:.2} Hz: want {cents} cents, got {:.2}", p.cents);
    }
}

#[test]
fn reference_pitch_moves_the_scale() {
    let mut t = Tuner::new(FS);
    t.set_reference(432.0);
    for v in pluck(432.0 / 4.0, 0.5, 0.0, 7) {
        t.push(v);
    }
    let p = t.estimate().expect("pitch");
    assert_eq!((p.note, p.octave), ("A", 2));
    assert!(p.cents.abs() < 0.3, "{p:?}");
}

#[test]
fn silence_and_noise_give_no_reading() {
    let mut t = Tuner::new(FS);
    for _ in 0..24000 {
        t.push(0.0);
    }
    assert!(t.estimate().is_none());
    let mut s = 1u32;
    for _ in 0..24000 {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        t.push(0.3 * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0));
    }
    assert!(t.estimate().is_none(), "white noise must not read as a note");
}

#[test]
fn string_matching_picks_the_closest() {
    let std = &TUNINGS[0];
    assert_eq!(nearest_string(std, 40.3), 0);
    assert_eq!(nearest_string(std, 58.6), 4);
    assert_eq!(nearest_string(std, 63.9), 5);
}

#[test]
fn estimate_is_cheap() {
    let mut t = Tuner::new(FS);
    for v in pluck(82.41, 0.2, 0.0, 3) {
        t.push(v);
    }
    let t0 = std::time::Instant::now();
    for _ in 0..30 {
        t.estimate_raw();
    }
    let per = t0.elapsed().as_secs_f32() / 30.0 * 1000.0;
    eprintln!("estimate: {per:.2} ms");
    // 15 estimates per second on the UI thread: stay well under 1 % of a core in release
    assert!(per < if cfg!(debug_assertions) { 60.0 } else { 1.0 }, "{per:.2} ms per estimate");
}

/// A real string through a pickup: pitch wobbling ±2 cents (beating partials, string drift)
/// at a few Hz, random 1-cent steps every 30 ms, hum-free noise. Frequency `hz(t)`.
fn wobbly(hz: impl Fn(f32) -> f32, secs: f32, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let n = (FS * secs) as usize;
    let mut ph = [0f32; 6];
    let mut step = 0.0;
    (0..n)
        .map(|i| {
            let t = i as f32 / FS;
            if i % 1440 == 0 {
                step = rnd();
            }
            let cents = 2.0 * (std::f32::consts::TAU * 4.3 * t).sin() + 1.2 * (std::f32::consts::TAU * 1.7 * t).sin() + step;
            let f = hz(t) * 2f32.powf(cents / 1200.0);
            let mut y = 0.0;
            for (k, p) in ph.iter_mut().enumerate() {
                let kf = (k + 1) as f32;
                *p = (*p + kf * f / FS).fract();
                let amp = if k == 0 { 0.4 } else if k == 1 { 1.0 } else { 0.7 / kf };
                y += amp * (std::f32::consts::TAU * *p).sin();
            }
            0.1 * y * (-t * 0.4).exp() + 0.003 * rnd()
        })
        .collect()
}

/// Readouts at the UI's 15 Hz from `from` s on: (smoothed, raw) cents against `ref_hz(t)`.
fn readouts(x: &[f32], from: f32, ref_hz: impl Fn(f32) -> f32) -> (Vec<(f32, f32)>, Vec<f32>) {
    let mut t = Tuner::new(FS);
    let (mut sm, mut raw) = (vec![], vec![]);
    for (i, v) in x.iter().enumerate() {
        t.push(*v);
        if i % 3200 == 0 && i as f32 / FS >= from {
            let now = i as f32 / FS;
            let to_cents = |hz: f32| 1200.0 * (hz / ref_hz(now)).log2();
            if let Some(p) = t.estimate_raw() {
                raw.push(to_cents(p.hz));
            }
            if let Some(p) = t.estimate() {
                sm.push((now, to_cents(p.hz)));
            }
        }
    }
    (sm, raw)
}

fn spread(v: &[f32]) -> f32 {
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
}

#[test]
fn needle_stands_still_on_a_wobbling_string() {
    for hz in [82.41f32, 110.0, 196.0, 329.63] {
        let x = wobbly(|_| hz, 4.0, hz.to_bits());
        let (sm, raw) = readouts(&x, 0.6, |_| hz);
        let smv: Vec<f32> = sm.iter().map(|p| p.1).collect();
        let (s, r) = (spread(&smv), spread(&raw));
        let worst = smv.iter().fold(0f32, |a, c| a.max(c.abs()));
        eprintln!("{hz} Hz: raw spread {r:.2} cents, readout spread {s:.2}, worst {worst:.2}");
        assert!(s < 0.6, "{hz} Hz: readout wobbles by {s:.2} cents");
        assert!(worst < 2.0, "{hz} Hz: readout strays {worst:.2} cents");
        assert_eq!(sm.len(), raw.len(), "{hz} Hz: readout dropped out");
    }
}

#[test]
fn needle_follows_a_turning_peg() {
    // tuning up 25 cents over 0.4 s from 1.5 s on: within 1.5 cents of the string 0.4 s later
    let target = 110.0;
    let f = |t: f32| target * 2f32.powf(((t - 1.5) / 0.4).clamp(0.0, 1.0) * 25.0 / 1200.0 - 25.0 / 1200.0);
    let x = wobbly(f, 3.5, 77);
    let (sm, _) = readouts(&x, 0.6, f);
    let late: Vec<f32> = sm.iter().filter(|p| p.0 > 2.3).map(|p| p.1).collect();
    let worst = late.iter().fold(0f32, |a, c| a.max(c.abs()));
    eprintln!("after the peg: worst {worst:.2} cents");
    assert!(worst < 1.5, "readout lags the peg by {worst:.2} cents");
}

#[test]
fn short_dropout_keeps_the_reading() {
    let mut x = wobbly(|_| 146.83, 2.0, 5);
    // 200 ms of near-silence in the middle of the ring
    for v in &mut x[48000..57600] {
        *v *= 0.005;
    }
    let mut t = Tuner::new(FS);
    let mut blank = 0;
    for (i, v) in x.iter().enumerate() {
        t.push(*v);
        if i > 24000 && i % 3200 == 0 && t.estimate().is_none() {
            blank += 1;
        }
    }
    assert_eq!(blank, 0, "the display blanked {blank} times through a 200 ms dropout");
}
