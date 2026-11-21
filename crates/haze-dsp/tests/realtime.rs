//! Real-time guarantees and signal sanity of the chain.

use haze_dsp::chain::{Chain, Cmd};
use haze_dsp::conv::{builtin_cab, Convolver, IrSpectrum, B};
use haze_dsp::practice::{LoopState, Looper};
use haze_dsp::{EqKey, LoopAction, Meters, Params, Tuner, P};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

/// Counts allocations, reallocations and frees made by the armed thread only.
struct Counting;
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static EVENTS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    // try_with: the allocator also runs during thread teardown
    let _ = ARMED.try_with(|a| {
        if a.get() {
            let _ = EVENTS.try_with(|e| e.set(e.get() + 1));
        }
    });
}

fn events() -> usize {
    EVENTS.with(|e| e.get())
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn everything_on(p: &Params) {
    for (k, v) in [
        (P::Source, 1.0),
        (P::DriveOn, 1.0),
        (P::CabOn, 1.0),
        (P::CompOn, 1.0),
        (P::Delay2On, 1.0),
        (P::Delay2Type, 6.0),
        (P::ModOn, 1.0),
        (P::DelayOn, 1.0),
        (P::ReverbOn, 1.0),
        (P::Limiter, 1.0),
        (P::AmpVoicing, 5.0),
        (P::AmpGain, 0.8),
    ] {
        p.set(k, v);
    }
    for slot in 0..haze_dsp::params::EQ_SLOTS {
        p.set_i(Params::eq(slot, EqKey::On), 1.0);
        p.set_i(Params::eq(slot, EqKey::Pre), (slot % 2) as f32);
        p.set_i(Params::eq(slot, EqKey::Kind), (slot == 2) as i32 as f32);
        p.set_i(Params::eq(slot, EqKey::V(0)), 80.0);
        p.set_i(Params::eq(slot, EqKey::V(4)), 4.0);
        p.set_i(Params::eq(slot, EqKey::V(9)), 8000.0);
    }
    for slot in 0..haze_dsp::params::FX_SLOTS {
        p.set_i(Params::fx(slot, EqKey::On), 1.0);
        p.set_i(Params::fx(slot, EqKey::Pre), (slot % 2) as f32);
        p.set_i(Params::fx(slot, EqKey::Kind), 2.0);
        for (k, v) in [7.0, 0.8, -12.0, 0.6, 1.0, 0.5, 20.0, 1.0, 0.0, 0.0].into_iter().enumerate() {
            p.set_i(Params::fx(slot, EqKey::V(k)), v);
        }
    }
    p.set(P::PitchMidi, 57.0);
    for k in [P::PcHarmOn, P::PcRhyOn, P::PcShimOn, P::PcFrzOn, P::PcLofiOn] {
        p.set(k, 1.0);
    }
    p.set(P::PcHarmV2, -3.0);
}

#[test]
fn process_never_allocates() {
    let fs = 48000.0;
    let params = Arc::new(Params::default());
    everything_on(&params);
    let (mut chain, mut h) = Chain::new(fs, params.clone(), Arc::new(Meters::default()), Some(rtrb::RingBuffer::new(4096).0));
    let input = vec![0.1f32; 128];
    let mut l = vec![0f32; 128];
    let mut r = vec![0f32; 128];
    // IR swap prepared off the "audio thread", applied inside process
    let ir = Box::new(IrSpectrum::new(&builtin_cab(0, fs), "test"));
    h.cmds.push(Cmd::SetIr(3, ir)).ok().unwrap();
    params.set(P::CabIr, 3.0);
    h.cmds.push(Cmd::SetLoopBuffer(vec![0f32; 4 * 48000].into_boxed_slice())).ok().unwrap();
    params.set(P::DrumOn, 1.0);
    ARMED.with(|a| a.set(true));
    for i in 0..400 {
        // record, overdub, undo, clear while armed
        match i {
            10 | 60 | 120 | 200 => {
                let _ = h.cmds.push(Cmd::Loop(LoopAction::Cycle));
            }
            150 => {
                let _ = h.cmds.push(Cmd::Loop(LoopAction::Undo));
            }
            300 => {
                let _ = h.cmds.push(Cmd::Loop(LoopAction::Clear));
            }
            _ => {}
        }
        params.set(P::DrumPattern, ((i / 13) % 8) as f32);
        params.set(P::TimeSig, ((i / 29) % 3) as f32);
        params.set(P::TrainerOn, ((i / 50) % 2) as f32);
        // walk every model / type while armed
        params.set(P::DriveModel, (i % 6) as f32);
        params.set(P::AmpVoicing, (i % 7) as f32);
        params.set(P::ModType, ((i / 7) % 5) as f32);
        params.set(P::ReverbType, [1.0, 3.0, 4.0, 5.0, 6.0][(i / 11) % 5]);
        params.set(P::DelayType, [0.0, 1.0, 2.0, 6.0, 7.0, 8.0, 9.0, 10.0][(i / 17) % 8]);
        params.set(P::CompPre, ((i / 23) % 2) as f32);
        params.set_i(Params::fx(0, EqKey::Kind), (1 + (i / 13) % 8) as f32);
        params.set(P::PcRhyMode, ((i / 29) % 3) as f32);
        params.set(P::PcStopOn, ((i / 31) % 2) as f32);
        params.set(P::PcFrzOn, ((i / 37) % 2) as f32);
        params.set_i(Params::fx(1, EqKey::Kind), (1 + (i / 19) % 8) as f32);
        params.set_i(Params::eq(0, EqKey::V(1)), (i % 5) as f32);
        chain.process(&input, &mut l, &mut r);
    }
    ARMED.with(|a| a.set(false));
    assert_eq!(events(), 0, "allocator touched inside Chain::process");
    // the replaced IR came back for freeing on this side
    assert!(h.trash.pop().is_ok());
}

#[test]
fn every_block_stays_finite() {
    let fs = 48000.0;
    for model in 0..9 {
        for voicing in 0..7 {
            let p = Arc::new(Params::default());
            everything_on(&p);
            p.set(P::DriveModel, model as f32);
            p.set(P::AmpVoicing, voicing as f32);
            p.set(P::ModType, (model % 5) as f32);
            p.set_i(Params::fx(0, EqKey::Kind), (1 + (model + voicing) % 8) as f32);
            p.set(P::ReverbType, [1.0, 3.0, 4.0, 5.0, 6.0][voicing % 5]);
            p.set(P::DelayType, [0.0, 1.0, 6.0, 7.0, 8.0, 9.0, 10.0][(model + voicing) % 7]);
            let out = haze_dsp::render_offline(p, fs, 1.5);
            assert!(out.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
            let rms = (out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32).sqrt();
            assert!(rms > 0.005, "model {model} voicing {voicing}: silent (rms {rms})");
        }
    }
}

#[test]
fn convolver_reproduces_ir() {
    let fs = 48000.0;
    let ir = builtin_cab(2, fs);
    let mut c = Convolver::new(Box::new(IrSpectrum::new(&ir, "x")));
    let mut out = vec![];
    for i in 0..(ir.len() + 2 * B) {
        out.push(c.tick(if i == 0 { 1.0 } else { 0.0 }));
    }
    let err = ir.iter().zip(&out[B..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(err < 1e-4, "max error {err}");
}

#[test]
fn tuner_finds_open_a_string() {
    let fs = 48000.0;
    let mut t = Tuner::new(fs);
    for i in 0..(fs as usize / 4) {
        let s = (i as f32 * 110.0 / fs * std::f32::consts::TAU).sin() * 0.3;
        t.push(s + 0.3 * (2.0 * i as f32 * 110.0 / fs * std::f32::consts::TAU).sin());
    }
    let p = t.estimate().expect("pitch");
    assert_eq!(p.note, "A");
    assert_eq!(p.octave, 2);
    assert!(p.cents.abs() < 3.0, "{p:?}");
}

#[test]
fn realtime_factor() {
    let fs = 48000.0;
    let p = Arc::new(Params::default());
    everything_on(&p);
    let t = std::time::Instant::now();
    let _ = haze_dsp::render_offline(p, fs, 10.0);
    let el = t.elapsed().as_secs_f32();
    eprintln!("10 s of audio, full chain, rendered in {el:.3} s ({:.1}% of one core)", el / 10.0 * 100.0);
}

#[test]
fn allocation_counter_is_live() {
    // guards the zero-allocation test against a dead counter
    let before = events();
    ARMED.with(|a| a.set(true));
    let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(64));
    drop(v);
    ARMED.with(|a| a.set(false));
    assert!(events() >= before + 2);
}

#[test]
fn looper_record_overdub_undo() {
    let mut lp = Looper::new();
    let _ = lp.set_buffer(vec![0f32; 4 * 10_000].into_boxed_slice());
    lp.action(LoopAction::Cycle); // REC
    for _ in 0..5000 {
        lp.tick(0.25, -0.25);
    }
    lp.action(LoopAction::Cycle); // PLAY
    assert_eq!(lp.state, LoopState::Playing);
    assert_eq!(lp.len, 5000);
    let (l, r) = lp.tick(0.0, 0.0);
    assert!((l - 0.25).abs() < 1e-6 && (r + 0.25).abs() < 1e-6);
    // finish the pass, then overdub one full pass of 0.5
    for _ in 1..5000 {
        lp.tick(0.0, 0.0);
    }
    lp.action(LoopAction::Cycle); // DUB
    for _ in 0..5000 {
        lp.tick(0.5, 0.5);
    }
    lp.action(LoopAction::Cycle); // PLAY
    let (l, _) = lp.tick(0.0, 0.0);
    assert!((l - 0.75).abs() < 1e-3, "base + layer, got {l}");
    lp.action(LoopAction::Undo);
    let (l, _) = lp.tick(0.0, 0.0);
    assert!((l - 0.25).abs() < 1e-6, "undo drops the layer, got {l}");
    lp.action(LoopAction::Clear);
    assert_eq!(lp.tick(0.3, 0.3), (0.0, 0.0));
    assert_eq!(lp.state, LoopState::Empty);
}

#[test]
fn drums_keep_tempo() {
    // 120 BPM metronome: clicks every 0.5 s
    let fs = 48000.0;
    let mut d = haze_dsp::practice::Drums::new(fs);
    let mut onsets = vec![];
    let mut prev = 0f32;
    for i in 0..(fs as usize * 2) {
        let y = d.tick(120.0, 0, 0).0.abs();
        if y > 0.05 && prev <= 0.05 && onsets.last().is_none_or(|&o: &usize| i - o > 2000) {
            onsets.push(i);
        }
        prev = y;
    }
    assert_eq!(onsets.len(), 4, "{onsets:?}");
    for w in onsets.windows(2) {
        assert!((w[1] - w[0]).abs_diff(24000) < 50, "{onsets:?}");
    }
}

/// Run `input` through a fresh chain (interface source), mono out (left channel).
fn run_input(p: Arc<Params>, input: &[f32]) -> Vec<f32> {
    p.set(P::Source, 0.0);
    let (mut chain, _h) = Chain::new(48000.0, p, Arc::new(Meters::default()), None);
    let mut out = vec![0f32; input.len()];
    let mut r = vec![0f32; 128];
    for (i, o) in input.chunks(128).zip(out.chunks_mut(128)) {
        chain.process(i, o, &mut r[..i.len()]);
    }
    out
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

/// Ground-loop hum measured on a real USB guitar interface: 50 Hz at −32 dBFS plus odd
/// harmonics. Through a crunch amp it used to come out as a constant buzz; the hum notch
/// has to bring it under the gate so the idle output is silent.
#[test]
fn mains_hum_does_not_reach_the_output() {
    let fs = 48000.0;
    let hum: Vec<f32> = (0..(fs as usize * 2))
        .map(|n| {
            let t = n as f32 / fs;
            let w = std::f32::consts::TAU * 50.0 * t;
            0.025 * w.sin() + 0.0025 * (3.0 * w).sin() + 0.0014 * (5.0 * w).sin()
        })
        .collect();
    let p = Arc::new(Params::default());
    p.set(P::AmpVoicing, 3.0);
    p.set(P::AmpGain, 0.7);
    let on = run_input(p.clone(), &hum);
    p.set(P::HumHz, 0.0);
    let off = run_input(p, &hum);
    let tail = fs as usize..;
    let (on, off) = (rms(&on[tail.clone()]), rms(&off[tail]));
    eprintln!("idle hum out: filter off {:.1} dBFS, on {:.1} dBFS", 20.0 * off.log10(), 20.0 * (on + 1e-9).log10());
    assert!(off > 0.01, "without the filter the hum is audible (sanity): {off}");
    assert!(on < off * 0.01, "hum filter must cut the idle buzz by 40 dB: {on} vs {off}");
}

/// Low E (82.4 Hz) sits between the 50 and 100 Hz notches: it must pass nearly untouched.
#[test]
fn hum_filter_keeps_guitar_notes() {
    let fs = 48000.0;
    let note: Vec<f32> = (0..fs as usize).map(|n| 0.3 * (std::f32::consts::TAU * 82.4 * n as f32 / fs).sin()).collect();
    let mut f = haze_dsp::blocks::HumFilter::new(fs);
    let y: Vec<f32> = note.iter().map(|x| f.tick(*x, 50.0)).collect();
    let loss = 20.0 * (rms(&y[24000..]) / rms(&note[24000..])).log10();
    assert!(loss > -1.0, "E2 loses {loss:.2} dB");
}

/// Engine start must not blast: the output gain starts where the settings are, not at 0 dB.
#[test]
fn engine_start_has_no_blast() {
    let fs = 48000.0;
    let p = Arc::new(Params::default());
    p.set(P::OutGainDb, -40.0);
    p.set(P::Source, 1.0);
    let (mut chain, _h) = Chain::new(fs, p, Arc::new(Meters::default()), None);
    let mut peak_first = 0f32;
    let mut peak_steady = 0f32;
    let (mut l, mut r) = (vec![0f32; 128], vec![0f32; 128]);
    let silence = vec![0f32; 128];
    for blk in 0..(fs as usize * 4 / 128) {
        chain.process(&silence, &mut l, &mut r);
        let pk = l.iter().chain(r.iter()).fold(0f32, |a, x| a.max(x.abs()));
        if blk < 100 {
            peak_first = peak_first.max(pk);
        } else {
            peak_steady = peak_steady.max(pk);
        }
    }
    assert!(peak_first <= peak_steady * 1.5 + 1e-4, "start-up peak {peak_first} vs steady {peak_steady}");
}

/// A cheap USB interface as measured: hum plus hiss at −52 dBFS. The patch gate threshold
/// (−62 dB) sits under that; the adaptive floor must close the gate between notes and still
/// let a plucked note through.
#[test]
fn noisy_interface_gate_closes_between_notes() {
    let fs = 48000.0;
    let mut seed = 0x1234_5678u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 - 0.5) * 2.0
    };
    let n = fs as usize * 4;
    let pluck_at = fs as usize * 3;
    let input: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / fs;
            let hum = 0.025 * (std::f32::consts::TAU * 50.0 * t).sin();
            let hiss = 0.0035 * noise();
            let note = if i >= pluck_at {
                let tt = (i - pluck_at) as f32 / fs;
                0.1 * (-tt * 3.0).exp() * (std::f32::consts::TAU * 110.0 * tt).sin()
            } else {
                0.0
            };
            hum + hiss + note
        })
        .collect();
    let p = Arc::new(Params::default());
    p.set(P::AmpVoicing, 4.0);
    p.set(P::AmpGain, 0.8);
    p.set(P::GateThreshDb, -62.0);
    let out = run_input(p, &input);
    let idle = rms(&out[..pluck_at]);
    let played = rms(&out[pluck_at..pluck_at + fs as usize / 4]);
    eprintln!("idle {:.1} dBFS, note {:.1} dBFS", 20.0 * (idle + 1e-9).log10(), 20.0 * played.log10());
    assert!(idle < 1e-4, "idle output must be silent: {idle}");
    assert!(played > 0.05, "the note must come through: {played}");
}

/// Every reverb type has to die out once the input stops. The patch below is a real one
/// (from the PC engine's debug log): Shimmer used to feed its pitch-shifted tail into all
/// eight FDN lines on top of the unitary mix, a loop gain of ~1.7; the output grew 60 dB a
/// second, squealed, then overflowed to inf.
#[test]
fn reverb_tails_decay_for_every_type() {
    let fs = 48000.0;
    for ty in [1, 3, 4, 5, 6] {
        let p = Arc::new(Params::default());
        for (k, v) in [
            // a fixed input level: the leveller would change it while the test measures
            (P::InputAuto, 0.0),
            (P::GateOn, 0.0), (P::AmpVoicing, 2.0), (P::AmpGain, 0.525), (P::AmpBass, 0.47), (P::AmpMid, 0.7),
            (P::AmpTreble, 0.3), (P::AmpPresence, 0.58), (P::AmpMaster, 0.55), (P::AmpSag, 0.25), (P::CabIr, 1.0),
            (P::DriveOn, 1.0), (P::DriveModel, 1.0), (P::DriveAmt, 1.0), (P::DelayOn, 1.0), (P::DelayMs, 400.0),
            (P::DelayFeedback, 0.288), (P::DelayMix, 0.32), (P::DelayType, 1.0), (P::ReverbOn, 1.0),
            (P::ReverbType, ty as f32), (P::ReverbTime, 7.4), (P::ReverbMix, 0.423),
            (P::Delay2On, 1.0), (P::Delay2Type, 6.0), (P::Delay2Feedback, 0.53), (P::Delay2Mix, 0.66),
        ] {
            p.set(k, v);
        }
        // 2 s of loud strumming-like noise bursts, then 20 s of silence
        let mut seed = 0x9e37_79b9u32;
        let input: Vec<f32> = (0..(fs as usize * 22))
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                if i < fs as usize * 2 { 0.3 * (seed as f32 / u32::MAX as f32 - 0.5) } else { 0.0 }
            })
            .collect();
        let out = run_input(p, &input);
        assert!(out.iter().all(|x| x.is_finite()), "type {ty}: non-finite output");
        let s = fs as usize;
        let during = rms(&out[s..2 * s]);
        let early = rms(&out[2 * s..3 * s]);
        let late = rms(&out[20 * s..22 * s]);
        // a runaway saturates the limiter (and the NaN guard would later wipe it): the tail
        // has to be quieter than the playing that fed it
        assert!(early < during * 0.5, "type {ty}: tail louder than the input ({early} vs {during})");
        eprintln!("reverb {ty}: 1 s after the input {:.1} dBFS, 18 s after {:.1} dBFS", 20.0 * (early + 1e-12).log10(), 20.0 * (late + 1e-12).log10());
        assert!(late < early * 0.01, "type {ty}: tail must decay by 40 dB ({early} → {late})");
    }
}

/// Measured on the user's interface: input 1 carries only steady hum (−27 dBFS), the guitar
/// arrives on input 2 at −41 dBFS and rings out. The picker must move to input 2, and must
/// stay on input 1 when the guitar is there (hum included) and input 2 is silent.
#[test]
fn input_channel_follows_the_guitar() {
    use haze_dsp::engine::ChannelPick;
    let fs = 48000.0;
    let hum = |i: usize| 0.045 * (std::f32::consts::TAU * 50.0 * i as f32 / fs).sin();
    let pluck = |i: usize| {
        // a note every 2 s, −41 dBFS peak, 1.5 s decay
        let t = (i % (2 * fs as usize)) as f32 / fs;
        0.009 * (-t * 3.0).exp() * (std::f32::consts::TAU * 196.0 * t).sin()
    };
    let run = |a: &dyn Fn(usize) -> f32, b: &dyn Fn(usize) -> f32| {
        let mut p = ChannelPick::new(fs);
        let mut ch = 0;
        for blk in 0..(fs as usize * 6 / 256) {
            ch = p.update((blk * 256..blk * 256 + 256).map(|i| (a(i), b(i))));
        }
        ch
    };
    assert_eq!(run(&hum, &pluck), 1, "guitar on input 2 next to a humming input 1");
    assert_eq!(run(&|i| hum(i) + 4.0 * pluck(i), &|_| 0.0), 0, "guitar (with hum) on input 1, input 2 silent");
    assert_eq!(run(&|_| 0.0, &|_| 0.0), 0, "silence: stay on input 1");
    // seen live: guitar at −22 dBFS on input 1 over −27 dBFS hum, input 2 silent (−76) but
    // catching −30 dB of crosstalk from it
    let noise = |i: usize| 1.5e-4 * ((i as f32 * 12.9898).sin() * 43758.547).fract();
    assert_eq!(run(&|i| hum(i) + 4.0 * pluck(i), &|i| noise(i) + 0.03 * 4.0 * pluck(i)), 0, "crosstalk on input 2 must not win");
}
