//! Drum machine and practice tools: levels, time signatures, fills, loop sync, count-in,
//! speed trainer.

use haze_dsp::chain::{Chain, Cmd};
use haze_dsp::practice::{Drums, LoopState, PATTERNS};
use haze_dsp::{LoopAction, Meters, Params, P};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

const FS: f32 = 48000.0;

#[test]
fn every_groove_is_loud_enough_and_never_clips() {
    for (i, name) in PATTERNS.iter().enumerate() {
        let mut d = Drums::new(FS);
        let (mut peak, mut sum, mut n) = (0f32, 0f64, 0usize);
        // 8 bars at 120 BPM: includes two fills and the crash after them
        for _ in 0..(FS as usize * 16) {
            let (l, r) = d.tick(120.0, i, 0);
            assert!(l.is_finite() && r.is_finite(), "{name}");
            peak = peak.max(l.abs()).max(r.abs());
            sum += (l * l + r * r) as f64;
            n += 2;
        }
        let rms = (sum / n as f64).sqrt() as f32;
        eprintln!("{name:<10} peak {:.2}  rms {:.3}", peak, rms);
        assert!(peak < 0.98, "{name}: peak {peak}");
        assert!(peak > 0.25, "{name}: too quiet, peak {peak}");
    }
}

#[test]
fn grooves_are_stereo_metronome_is_mono() {
    let side = |pattern: usize| {
        let mut d = Drums::new(FS);
        let mut s = 0f64;
        for _ in 0..(FS as usize * 4) {
            let (l, r) = d.tick(120.0, pattern, 0);
            s += ((l - r) as f64).powi(2);
        }
        s
    };
    assert!(side(1) > 1.0, "rock kit has hats and cymbals off centre");
    assert!(side(0) < 1e-9, "the metronome sits in the middle");
}

#[test]
fn time_signatures_set_the_bar() {
    let d = Drums::new(FS);
    assert_eq!(d.bar_len(0, 0), 16);
    assert_eq!(d.bar_len(0, 1), 12);
    assert_eq!(d.bar_len(0, 2), 12);
    // 3/4 at 120 BPM: a bar every 1.5 s
    let mut d = Drums::new(FS);
    let mut bars = vec![];
    for i in 0..(FS as usize * 6) {
        d.tick(120.0, 0, 1);
        if d.bar_started() {
            bars.push(i);
        }
    }
    for w in bars.windows(2) {
        assert!((w[1] - w[0]).abs_diff(72000) < 8, "{bars:?}");
    }
}

#[test]
fn shuffle_swings_the_offbeat() {
    // with swing 2/3 the off-beat 8th (step 2) starts 2/3 into the beat
    let mut d = Drums::new(FS);
    let mut at: Option<usize> = None;
    for i in 0..48000 {
        d.tick(120.0, 4, 0);
        if d.step == 2 && at.is_none() {
            at = Some(i);
        }
    }
    // beat = 24000 samples; 2/3 of it = 16000
    assert!(at.unwrap().abs_diff(16000) < 4, "{at:?}");
}

fn chain(p: &Arc<Params>) -> (Chain, haze_dsp::chain::ChainHandles, Arc<Meters>) {
    let m = Arc::new(Meters::default());
    let (mut c, mut h) = Chain::new(FS, p.clone(), m.clone(), None);
    h.cmds.push(Cmd::SetLoopBuffer(vec![0f32; 4 * 48000 * 20].into_boxed_slice())).ok().unwrap();
    let mut l = [0f32; 64];
    let mut r = [0f32; 64];
    c.process(&[0.0; 64], &mut l, &mut r);
    (c, h, m)
}

fn run(c: &mut Chain, frames: usize) {
    let mut l = [0f32; 64];
    let mut r = [0f32; 64];
    for _ in 0..frames / 64 {
        c.process(&[0.0; 64], &mut l, &mut r);
    }
}

#[test]
fn synced_loop_is_whole_bars() {
    let p = Arc::new(Params::default());
    p.set(P::DrumOn, 1.0);
    p.set(P::DrumPattern, 1.0);
    p.set(P::Bpm, 120.0);
    let (mut c, mut h, m) = chain(&p);
    // press REC in the middle of the first bar
    run(&mut c, 30016);
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    run(&mut c, 64);
    assert_eq!(m.loop_wait.load(Relaxed), 1, "REC waits for the bar line");
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Empty);
    run(&mut c, 96000 - 30080 + 640);
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Recording);
    // close a bit after the start of bar 3: the loop runs on to the end of that bar
    run(&mut c, 96000 + 20000);
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    run(&mut c, 96000);
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Playing);
    let len = m.loop_len.load(Relaxed) as usize;
    assert_eq!(len % 96000, 0, "loop of {len} frames is not whole 2 s bars");
    assert_eq!(len, 2 * 96000);
}

#[test]
fn count_in_then_record() {
    let p = Arc::new(Params::default());
    p.set(P::Bpm, 120.0);
    let (mut c, mut h, m) = chain(&p);
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    run(&mut c, 64 * 10);
    assert_eq!(m.loop_wait.load(Relaxed), 2, "counting in");
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Empty);
    // one 4/4 bar at 120 BPM = 2 s
    run(&mut c, 96000);
    assert_eq!(m.loop_wait.load(Relaxed), 0);
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Recording);
    // counting clicks were audible: the output was not silent during the count bar
}

#[test]
fn count_in_is_audible() {
    let p = Arc::new(Params::default());
    p.set(P::Bpm, 120.0);
    p.set(P::OutGainDb, 0.0);
    let (mut c, mut h, _) = chain(&p);
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    let mut l = [0f32; 64];
    let mut r = [0f32; 64];
    let mut peak = 0f32;
    for _ in 0..(96000 / 64) {
        c.process(&[0.0; 64], &mut l, &mut r);
        peak = l.iter().fold(peak, |a, x| a.max(x.abs()));
    }
    assert!(peak > 0.05, "count-in clicks missing (peak {peak})");
}

#[test]
fn speed_trainer_raises_tempo_to_target() {
    let p = Arc::new(Params::default());
    for (k, v) in [(P::DrumOn, 1.0), (P::Bpm, 200.0), (P::TrainerOn, 1.0), (P::TrainerStep, 10.0), (P::TrainerBars, 1.0), (P::TrainerTarget, 230.0)] {
        p.set(k, v);
    }
    let (mut c, _h, _) = chain(&p);
    run(&mut c, 48000 * 8);
    assert_eq!(p.get(P::Bpm), 230.0, "stops at the target");
}

/// RMS in dBFS of `secs` of the chain's output (DI riff as the source), skipping the first 0.5 s.
fn out_rms(p: &Arc<Params>, secs: f32) -> f32 {
    let out = haze_dsp::render_offline(p.clone(), FS, secs);
    let skip = FS as usize;
    let ms: f64 = out[skip..].iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / (out.len() - skip) as f64;
    10.0 * ms.log10() as f32
}

#[test]
fn mixer_balance_drums_against_guitar() {
    // guitar alone at its fader's 0 dB, clean amp
    let p = Arc::new(Params::default());
    let guitar = out_rms(&p, 8.0);
    // the rock groove alone at the drum fader's 0 dB
    let p = Arc::new(Params::default());
    p.set(P::GuitarLevel, 0.0);
    p.set(P::DrumOn, 1.0);
    p.set(P::DrumPattern, 1.0);
    p.set(P::DrumLevel, 1.0);
    p.set(P::Bpm, 110.0);
    let drums = out_rms(&p, 8.0);
    eprintln!("guitar {guitar:.1} dBFS, drums {drums:.1} dBFS (makeup {:.1} dB)", 20.0 * haze_dsp::chain::DRUM_MAKEUP.log10());
    assert!((drums - (guitar - 3.0)).abs() < 2.5, "drums {drums:.1} vs guitar {guitar:.1}");
}

fn energy(d: &mut Drums, pattern: usize, secs: f32) -> f64 {
    (0..(FS * secs) as usize).map(|_| d.tick(120.0, pattern, 0)).map(|(l, r)| (l * l + r * r) as f64).sum()
}

#[test]
fn user_groove_plays_what_the_grid_says() {
    use haze_dsp::practice::{groove_rows, row_cell, row_set, Kit, USER};
    let mut d = Drums::new(FS);
    d.set_kit(&Kit { fills: 0, ..Kit::default() });
    assert!(energy(&mut d, USER, 2.0) < 1e-9, "an empty grid is silent");
    // a kick on every beat
    let kick = (0..16).step_by(4).fold(0, |r, s| row_set(r, s, 3));
    assert_eq!(row_cell(kick, 4), 3);
    assert_eq!(row_cell(kick, 5), 0);
    d.set_user([kick, 0, 0, 0, 0, 0, 0, 0], 16);
    assert!(energy(&mut d, USER, 2.0) > 1.0, "the kick row sounds");
    // the rock preset copied into the grid sounds like the preset (fills off on both)
    let (rows, len) = groove_rows(1, 0);
    assert_eq!(len, 16);
    let mut a = Drums::new(FS);
    let mut b = Drums::new(FS);
    a.set_kit(&Kit { fills: 0, human: 0.0, ..Kit::default() });
    b.set_kit(&Kit { fills: 0, human: 0.0, ..Kit::default() });
    b.set_user(rows, len);
    let (ea, eb) = (energy(&mut a, 1, 4.0), energy(&mut b, USER, 4.0));
    assert!((ea / eb - 1.0).abs() < 0.05, "preset {ea:.1} vs its copy {eb:.1}");
}

#[test]
fn kit_levels_mute_their_voices() {
    use haze_dsp::practice::Kit;
    let mut on = Drums::new(FS);
    let mut off = Drums::new(FS);
    off.set_kit(&Kit { levels: [0.0; 6], ..Kit::default() });
    assert!(energy(&mut on, 1, 2.0) > 1.0);
    assert!(energy(&mut off, 1, 2.0) < 1e-9, "every group at 0 silences the kit");
}

#[test]
fn humanise_keeps_the_bar_line_on_the_grid() {
    use haze_dsp::practice::Kit;
    let mut d = Drums::new(FS);
    d.set_kit(&Kit { human: 1.0, swing: 1.0, ..Kit::default() });
    let mut bars = vec![];
    for i in 0..(FS as usize * 9) {
        d.tick(120.0, 1, 0);
        if d.bar_started() {
            bars.push(i);
        }
    }
    assert_eq!(bars[0], 0);
    for w in bars.windows(2) {
        assert_eq!(w[1] - w[0], 96000, "{bars:?}");
    }
}

#[test]
fn guitar_fader_is_a_monitor_level_the_looper_records_pre_fader() {
    let p = Arc::new(Params::default());
    p.set(P::Source, 1.0);
    p.set(P::OutGainDb, 0.0);
    p.set(P::CountIn, 0.0);
    p.set(P::GuitarLevel, 0.0);
    let (mut c, mut h, m) = chain(&p);
    let mut l = [0f32; 64];
    let mut r = [0f32; 64];
    let mut peak = 0f32;
    // record 1 s of the DI riff with the guitar fader down: silent output
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    for _ in 0..750 {
        c.process(&[0.0; 64], &mut l, &mut r);
        peak = l.iter().chain(r.iter()).fold(peak, |a, s| a.max(s.abs()));
    }
    assert!(peak < 1e-4, "guitar fader down: {peak}");
    h.cmds.push(Cmd::Loop(LoopAction::Cycle)).ok().unwrap();
    p.set(P::Source, 0.0);
    run(&mut c, 64 * 750);
    assert_eq!(LoopState::from_u32(m.loop_state.load(Relaxed)), LoopState::Playing);
    let lp = haze_dsp::Meters::take_peak(&m.loop_peak);
    assert!(lp > 0.05, "the loop holds the guitar and plays it: {lp}");
}
