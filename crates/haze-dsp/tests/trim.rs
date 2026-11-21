//! Input leveller: a quiet interface reaches the amp at the voiced level, dynamics survive,
//! silence never winds the gain up.

use haze_dsp::blocks::Trim;

const FS: f32 = 48000.0;

fn db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

/// A chord every 0.5 s with peak `amp`, plus hiss; the first 2 s are hiss only (the app
/// was open a moment before the playing starts).
fn strum(t: f32, amp: f32, n: &mut u32) -> f32 {
    let amp = if t < 2.0 { 0.0 } else { amp };
    *n ^= *n << 13;
    *n ^= *n >> 17;
    *n ^= *n << 5;
    let hiss = 0.0003 * ((*n as f32 / u32::MAX as f32) * 2.0 - 1.0);
    let tt = t % 0.5;
    let env = amp * (-tt / 0.4).exp();
    env * [110.0, 165.0, 220.0, 277.0].iter().map(|f| (std::f32::consts::TAU * f * t).sin() * 0.25).sum::<f32>() + hiss
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

#[test]
fn quiet_interface_reaches_the_voiced_level() {
    // guitar peaking at −32 dBFS, as on a cautious interface gain
    let mut t = Trim::new(FS);
    let mut n = 1;
    let y: Vec<f32> = (0..(FS * 14.0) as usize).map(|i| t.tick(strum(i as f32 / FS, 0.025, &mut n))).collect();
    let late = db(peak(&y[(FS * 12.0) as usize..]));
    eprintln!("peaks after 10 s: {late:.1} dBFS, gain {:.1} dB", db(t.gain()));
    assert!((-11.0..-7.0).contains(&late));
}

#[test]
fn picking_dynamics_survive() {
    // loud for 10 s, then 12 dB softer: the soft part stays ~12 dB down for a long while
    let mut t = Trim::new(FS);
    let mut n = 2;
    let y: Vec<f32> = (0..(FS * 30.0) as usize)
        .map(|i| {
            let s = i as f32 / FS;
            t.tick(strum(s, if s < 12.0 { 0.025 } else { 0.0063 }, &mut n))
        })
        .collect();
    let loud = peak(&y[(FS * 10.0) as usize..(FS * 12.0) as usize]);
    let soft = peak(&y[(FS * 28.0) as usize..]);
    eprintln!("soft passage {:.1} dB under the loud one after 18 s", db(loud / soft));
    assert!(db(loud / soft) > 10.0);
}

#[test]
fn silence_never_winds_the_gain_up() {
    let mut t = Trim::new(FS);
    let mut n = 3;
    for i in 0..(FS * 10.0) as usize {
        t.tick(strum(i as f32 / FS, 0.025, &mut n));
    }
    let before = t.gain();
    // a minute of hiss only
    for _ in 0..(FS * 60.0) as usize {
        n ^= n << 13;
        n ^= n >> 17;
        n ^= n << 5;
        t.tick(0.0003 * ((n as f32 / u32::MAX as f32) * 2.0 - 1.0));
    }
    eprintln!("gain {:.1} dB before the break, {:.1} dB after a minute of hiss", db(before), db(t.gain()));
    assert!((db(t.gain()) - db(before)).abs() < 0.5);
}
