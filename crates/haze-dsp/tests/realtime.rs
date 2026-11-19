//! Real-time guarantees and signal sanity of the chain.

use haze_dsp::chain::{Chain, Cmd};
use haze_dsp::conv::{builtin_cab, Convolver, IrSpectrum, B};
use haze_dsp::practice::{LoopState, Looper};
use haze_dsp::{LoopAction, Meters, Params, Tuner, P};
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
        (P::EqOn, 1.0),
        (P::ModOn, 1.0),
        (P::DelayOn, 1.0),
        (P::ReverbOn, 1.0),
        (P::Limiter, 1.0),
        (P::AmpVoicing, 5.0),
        (P::AmpGain, 0.8),
    ] {
        p.set(k, v);
    }
    p.set(Params::eq_band(3), 4.0);
    p.set(Params::eq_band(7), -3.0);
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
        params.set(P::DrumPattern, ((i / 13) % 5) as f32);
        // walk every model / type while armed
        params.set(P::DriveModel, (i % 6) as f32);
        params.set(P::AmpVoicing, (i % 7) as f32);
        params.set(P::ModType, ((i / 7) % 5) as f32);
        params.set(P::ReverbType, ((i / 11) % 4) as f32);
        params.set(P::EqBand0, (i % 5) as f32);
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
    for model in 0..6 {
        for voicing in 0..7 {
            let p = Arc::new(Params::default());
            everything_on(&p);
            p.set(P::DriveModel, model as f32);
            p.set(P::AmpVoicing, voicing as f32);
            p.set(P::ModType, (model % 5) as f32);
            p.set(P::ReverbType, (voicing % 4) as f32);
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
        let y = d.tick(120.0, 0).abs();
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
