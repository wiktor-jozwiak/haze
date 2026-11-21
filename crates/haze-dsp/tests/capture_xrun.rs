//! A capture overrun must not end the input stream. cpal 0.15.3 re-prepared an ALSA capture
//! after an xrun but never started it again: the PCM sat in PREPARED, the callback never ran
//! again and the guitar went silent with no error. Patched in `vendor/cpal`.
//!
//! Needs a real capture device, so it is ignored by default:
//!   HAZE_XRUN_DEV=front:CARD=CODEC,DEV=0 cargo test -p haze-dsp --test capture_xrun -- --ignored
//! (default device: `default`, i.e. PipeWire's ALSA plugin).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
#[ignore = "needs a capture device"]
fn capture_survives_overrun() {
    let want = std::env::var("HAZE_XRUN_DEV").unwrap_or_else(|_| "default".into());
    let host = cpal::default_host();
    let dev = host.input_devices().unwrap().find(|d| d.name().is_ok_and(|n| n == want)).unwrap_or_else(|| panic!("no input {want}"));
    let sup = dev.default_input_config().unwrap();
    let sc = cpal::StreamConfig { channels: sup.channels(), sample_rate: sup.sample_rate(), buffer_size: cpal::BufferSize::Fixed(512) };
    let calls = Arc::new(AtomicU32::new(0));
    let errs = Arc::new(AtomicU32::new(0));
    let (c, e) = (calls.clone(), errs.clone());
    let s = dev
        .build_input_stream(
            &sc,
            move |_: &[i16], _| {
                // stall once for 300 ms against a ~11 ms buffer: a guaranteed overrun
                if c.fetch_add(1, Ordering::Relaxed) == 20 {
                    std::thread::sleep(Duration::from_millis(300));
                }
            },
            move |err| {
                e.fetch_add(1, Ordering::Relaxed);
                eprintln!("input error: {err}");
            },
            None,
        )
        .unwrap();
    s.play().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let before = calls.load(Ordering::Relaxed);
    assert!(before > 20, "stream never reached the stall ({before} callbacks)");
    std::thread::sleep(Duration::from_millis(500));
    let after = calls.load(Ordering::Relaxed);
    eprintln!("callbacks: {before} then {after}, errors {}", errs.load(Ordering::Relaxed));
    assert!(after > before + 10, "capture stopped after the overrun ({before} -> {after} callbacks)");
}

/// The output thread runs SCHED_FIFO (or at least nice −11) whatever niceness the launcher
/// passed down. Needs a playback device and an rtprio / nice limit (group `audio`), so ignored
/// by default: `cargo test -p haze-dsp --test capture_xrun -- --ignored audio_thread`
#[test]
#[ignore = "needs a playback device"]
fn audio_thread_gets_realtime_priority() {
    let p = Arc::new(haze_dsp::Params::default());
    p.set(haze_dsp::P::Mute, 1.0);
    let e = haze_dsp::Engine::start(p, &haze_dsp::Config::default()).expect("output device");
    std::thread::sleep(Duration::from_millis(400));
    let rt = e.meters.rt.load(Ordering::Relaxed);
    eprintln!("audio thread scheduling bits: {rt:#06b}");
    assert!(rt & 0b0101 != 0, "output thread kept timesharing at the launcher's niceness");
}
