//! Read-only smoke test against a real amp: `cargo run -p katana-midi --example probe`
use katana_midi::{Cmd, Device, Event};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

fn main() {
    let (tx, rx) = channel();
    let d = Device::spawn(move |e| { let _ = tx.send(e); });
    let t0 = Instant::now();
    d.send(Cmd::Connect);
    while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::TempPatch(p) => {
                let s = p.summary();
                println!("[{:>4}ms] TEMP: {:?} amp={} gain={} vol={} leds={:?} fx={:?}", t0.elapsed().as_millis(), s.name, s.amp, s.gain, s.volume, s.leds, s.fx_names);
                break;
            }
            other => println!("[{:>4}ms] {:?}", t0.elapsed().as_millis(), other),
        }
    }
    d.send(Cmd::Shutdown);
    std::thread::sleep(Duration::from_millis(100));
}
