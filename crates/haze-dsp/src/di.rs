//! Built-in dry DI test riff: four bars of arpeggiated chords (Em–C–G–D, 100 BPM), plucked
//! with a Karplus–Strong string model at start-up. Lets you dial in tones without a guitar.

use crate::filters::Biquad;

pub const DI_NAME: &str = "Haze DI · Em–C–G–D arpeggio 100 BPM";

fn midi_hz(n: i32) -> f32 {
    440.0 * 2f32.powf((n - 69) as f32 / 12.0)
}

/// Mono loop at `fs`, peak about -6 dBFS (DI level of a single-coil guitar).
pub fn builtin_riff(fs: f32) -> Vec<f32> {
    let bpm = 100.0;
    let eighth = 60.0 / bpm / 2.0;
    // chord voicings, low to high (MIDI notes)
    let chords: [[i32; 6]; 4] = [
        [40, 47, 52, 55, 59, 64], // Em
        [48, 52, 55, 60, 64, 67], // C
        [43, 47, 50, 55, 59, 67], // G
        [50, 57, 62, 66, 69, 62], // D
    ];
    // 8 eighth notes per bar: string order of the arpeggio
    let pattern = [0usize, 2, 3, 4, 5, 4, 3, 2];
    let bar = 8.0 * eighth;
    let len = (4.0 * bar * fs) as usize;
    let mut out = vec![0f32; len];
    let mut seed = 0x1234_5678u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    for (b, chord) in chords.iter().enumerate() {
        for (k, &s) in pattern.iter().enumerate() {
            let start = ((b as f32 * bar + k as f32 * eighth) * fs) as usize;
            let vel = if k == 0 { 1.0 } else { 0.7 + 0.1 * (k % 2) as f32 };
            pluck(&mut out, start, midi_hz(chord[s]), vel, fs, &mut rnd);
        }
    }
    // pickup + cable: gentle low cut, treble roll-off
    let mut hp = Biquad::new();
    hp.highpass(70.0, 0.7, fs);
    let mut lp = Biquad::new();
    lp.lowpass(5500.0, 0.8, fs);
    for x in out.iter_mut() {
        *x = lp.tick(hp.tick(*x));
    }
    let peak = out.iter().fold(0f32, |m, x| m.max(x.abs())).max(1e-6);
    out.iter_mut().for_each(|x| *x *= 0.5 / peak);
    out
}

fn pluck(out: &mut [f32], start: usize, hz: f32, vel: f32, fs: f32, rnd: &mut impl FnMut() -> f32) {
    // string period rounded to whole samples: < 6 cents off up to 400 Hz at 48 kHz
    let n = ((fs / hz).round() as usize).max(2);
    let mut line: Vec<f32> = (0..n).map(|_| rnd() * vel).collect();
    // soften the pluck (pick position / finger)
    for i in 1..line.len() {
        line[i] = line[i] * 0.6 + line[i - 1] * 0.4;
    }
    let ring = (1.6 * fs) as usize;
    let decay = 0.9965f32.powf(110.0 / hz.max(60.0)).max(0.990);
    let mut idx = 0usize;
    let len = line.len();
    for t in 0..ring {
        let o = start + t;
        if o >= out.len() {
            break;
        }
        let a = line[idx % len];
        let b = line[(idx + 1) % len];
        out[o] += a;
        line[idx % len] = (a + b) * 0.5 * decay;
        idx += 1;
    }
}
