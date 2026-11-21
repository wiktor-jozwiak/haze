//! Noise gate on a quiet interface: the guitar peaks at −34 dBFS over −66 dBFS hiss (the
//! user's M-Audio after lowering its gain). Long passages and ringing notes must pass
//! untouched; the hiss between songs must not.

use haze_dsp::blocks::Gate;

const FS: f32 = 48000.0;

struct Noise(u32);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * (x + 1e-12).log10()
}

/// A strummed chord every `every` s: peak −34 dBFS, decaying with time constant `tau`.
fn strum(t: f32, every: f32, tau: f32) -> f32 {
    let tt = t % every;
    let env = 0.02 * (-tt / tau).exp();
    env * [110.0, 165.0, 220.0, 277.0].iter().map(|f| (std::f32::consts::TAU * f * t).sin() * 0.5).sum::<f32>()
}

/// Gate gain in 100 ms blocks: output level against input level, dB.
fn gain_blocks(input: &[f32], thresh_db: f32) -> Vec<f32> {
    let mut g = Gate::new(FS);
    let out: Vec<f32> = input.iter().map(|x| g.tick(*x, thresh_db)).collect();
    let blk = FS as usize / 10;
    input.chunks(blk).zip(out.chunks(blk)).map(|(i, o)| db(rms(o) / rms(i))).collect()
}

#[test]
fn long_passage_never_drops_out() {
    // 8 s of hard strumming, then a softer passage 12 dB down, no gap anywhere: the gate
    // never sees the real noise floor, and must not take the loud part's quiet moments for it
    for thresh in [-77.5, -55.0, -48.0] {
        let mut n = Noise(7);
        let input: Vec<f32> = (0..(FS * 17.0) as usize)
            .map(|i| {
                // the app was open a moment before the playing starts
                let t = i as f32 / FS - 2.0;
                let g = if t < 0.0 { 0.0 } else if t < 8.0 { 1.0 } else { 0.25 };
                strum(t.max(0.0), 0.5, 0.4) * g + 0.0005 * n.next()
            })
            .collect();
        let g = gain_blocks(&input, thresh);
        let worst = g[21..].iter().cloned().fold(f32::MAX, f32::min);
        let at = g[21..].iter().position(|x| *x == worst).unwrap() + 21;
        eprintln!("patch threshold {thresh} dB: worst block {worst:.1} dB at {:.1} s", at as f32 / 10.0);
        assert!(worst > -3.0, "threshold {thresh}: the gate cut the playing by {worst:.1} dB at {:.1} s", at as f32 / 10.0);
    }
}

#[test]
fn ringing_note_fades_naturally() {
    // one strum after a bar of quiet, ringing out over 6 s: the gate may only close once the
    // note is within ~10 dB of the hiss
    let mut n = Noise(3);
    let pluck = FS as usize * 2;
    let input: Vec<f32> = (0..(FS * 8.0) as usize)
        .map(|i| {
            let note = if i >= pluck { strum((i - pluck) as f32 / FS, 100.0, 1.2) } else { 0.0 };
            note + 0.0005 * n.next()
        })
        .collect();
    let g = gain_blocks(&input, -77.5);
    let blk = FS as usize / 10;
    let cut = (pluck / blk + 1..g.len()).find(|b| g[*b] < -6.0).expect("gate closes in the end");
    let level = db(rms(&input[cut * blk..(cut + 1) * blk]));
    eprintln!("note gated at {:.1} s after the pluck, input there {level:.1} dBFS", (cut * blk - pluck) as f32 / FS);
    assert!(level < -56.0, "the gate cut a note still at {level:.1} dBFS");
}

#[test]
fn hiss_between_songs_stays_shut() {
    let mut n = Noise(5);
    let input: Vec<f32> = (0..(FS * 10.0) as usize)
        .map(|i| {
            let t = i as f32 / FS;
            let note = if t < 4.0 { strum(t, 0.5, 0.4) } else { 0.0 };
            note + 0.0005 * n.next()
        })
        .collect();
    let mut g = Gate::new(FS);
    let out: Vec<f32> = input.iter().map(|x| g.tick(*x, -77.5)).collect();
    let quiet = &out[(FS * 7.0) as usize..];
    eprintln!("hiss after playing: {:.1} dBFS in, {:.1} dBFS out", db(rms(&input[(FS * 7.0) as usize..])), db(rms(quiet)));
    assert!(db(rms(quiet)) < -85.0, "hiss leaks through the gate");
}

