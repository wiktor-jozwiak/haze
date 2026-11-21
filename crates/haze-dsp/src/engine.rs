//! Real-time I/O on cpal. Input callback → mono → SPSC ring → output callback runs the chain.
//! Both callbacks only touch preallocated buffers and atomics.

use crate::chain::{Chain, Cmd, Trash};
use crate::params::{Meters, Params};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Config {
    pub buffer: u32,
    pub rate: u32,
    /// capture device (cpal name, e.g. `sysdefault:CARD=CODEC`); None = DI player only.
    /// Never the system default on purpose: that is usually a microphone, and a mic in front
    /// of the speakers through an amp sim howls.
    pub input: Option<String>,
    /// playback device; None = system default
    pub output: Option<String>,
    /// capture channel carrying the guitar (0-based); None = follow the louder one. Must be
    /// set when the other input holds a microphone: typing or talking would win the auto
    /// pick, go through the amp to the speakers and back into the mic (feedback squeal).
    pub channel: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Config { buffer: 128, rate: 48000, input: None, output: None, channel: None }
    }
}

/// One selectable device: cpal name plus a human label from /proc/asound/cards.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub label: String,
    /// looks like a microphone / webcam: feedback risk when used as the guitar input
    pub mic: bool,
    /// opened by another program (or by haze's own running engine): cpal could not probe it
    pub busy: bool,
}

/// (card number, card id, human name) per line of /proc/asound/cards.
#[cfg(target_os = "linux")]
fn card_labels() -> Vec<(u32, String, String)> {
    // " 0 [CODEC          ]: USB-Audio - USB AUDIO  CODEC"
    let txt = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    txt.lines()
        .filter_map(|l| {
            let (a, b) = l.split_once("]:")?;
            let (num, id) = a.split_once('[')?;
            let num = num.trim().parse().ok()?;
            let name = b.split_once(" - ").map_or(b, |x| x.1).split_whitespace().collect::<Vec<_>>().join(" ");
            Some((num, id.trim().to_string(), name))
        })
        .collect()
}

fn is_mic(label: &str) -> bool {
    let low = label.to_lowercase();
    low.contains("camera") || low.contains("webcam") || low.contains("mic") || low.contains("hd-audio")
}

/// One entry per sound card (`sysdefault:CARD=…`, which converts format/rate) plus the
/// system default. Outputs skip HDMI-only cards' noise by listing them like the rest.
#[cfg(target_os = "linux")]
pub fn list_devices(input: bool) -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let labels = card_labels();
    let devs: Vec<String> = match if input { host.input_devices() } else { host.output_devices() } {
        Ok(it) => it.filter_map(|d| d.name().ok()).collect(),
        Err(_) => vec![],
    };
    let mut out = vec![];
    if !input && devs.iter().any(|d| d == "default") {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe (PipeWire)".into(), mic: false, busy: false });
    }
    // per card: `front:` takes small fixed buffers on USB interfaces, `sysdefault:` often
    // refuses them (EINVAL on set_buffer_size); fall back to it only when there is no front
    let mut cards: Vec<(String, String)> = vec![];
    for d in &devs {
        let card = d.strip_prefix("front:CARD=").map(|c| c.split(',').next().unwrap_or(c)).or_else(|| d.strip_prefix("sysdefault:CARD="));
        let Some(card) = card else { continue };
        match cards.iter_mut().find(|(c, _)| c == card) {
            Some(e) if d.starts_with("front:") => e.1 = d.clone(),
            Some(_) => {}
            None => cards.push((card.to_string(), d.clone())),
        }
    }
    for (card, d) in &cards {
        let card = card.as_str();
        let label = labels.iter().find(|(_, id, _)| id == card).map_or(card.to_string(), |x| x.2.clone());
        let mic = input && is_mic(&label);
        out.push(DeviceInfo { id: d.clone(), label, mic, busy: false });
    }
    // cpal lists only devices it can open right now, so a capture card held by someone else
    // (PipeWire recording from it, or haze's own running engine) vanished from the picker.
    // List it anyway from /proc/asound: picking it restarts the engine, which frees ours.
    if input {
        for (num, id, label) in &labels {
            let has_capture = std::path::Path::new(&format!("/proc/asound/card{num}/pcm0c")).exists();
            if has_capture && !cards.iter().any(|(c, _)| c == id) {
                out.push(DeviceInfo { id: format!("front:CARD={id},DEV=0"), label: label.clone(), mic: is_mic(label), busy: true });
            }
        }
    }
    if input && devs.iter().any(|d| d == "default") {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe (zwykle mikrofon!)".into(), mic: true, busy: false });
    }
    out
}

/// Windows (WASAPI) and others: every endpoint by its friendly name, e.g.
/// `Microphone (Realtek(R) Audio)` or `IN (KATANA)`, plus the system default. WASAPI shared
/// mode resamples (see vendor/cpal/HAZE-PATCH.md), so any rate the engine asks for opens.
#[cfg(not(target_os = "linux"))]
pub fn list_devices(input: bool) -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let devs: Vec<String> = match if input { host.input_devices() } else { host.output_devices() } {
        Ok(it) => it.filter_map(|d| d.name().ok()).collect(),
        Err(_) => vec![],
    };
    let mut out = vec![];
    if !input {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe".into(), mic: false, busy: false });
    }
    for d in devs {
        out.push(DeviceInfo { mic: input && is_mic(&d), label: d.clone(), id: d, busy: false });
    }
    if input {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe (zwykle mikrofon!)".into(), mic: true, busy: false });
    }
    out
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    // ALSA has a real PCM called "default"; WASAPI has no such name, only a default endpoint
    #[cfg(not(target_os = "linux"))]
    if name == "default" {
        return if input { host.default_input_device() } else { host.default_output_device() };
    }
    let mut it: Box<dyn Iterator<Item = cpal::Device>> = if input { Box::new(host.input_devices().ok()?) } else { Box::new(host.output_devices().ok()?) };
    it.find(|d| d.name().is_ok_and(|n| n == name))
}

#[derive(Clone, Debug, Default)]
pub struct Info {
    pub host: String,
    pub input: Option<String>,
    pub output: String,
    pub rate: u32,
    pub buffer: u32,
    /// input buffer + ring prefill + output buffer + convolver partition, in ms
    pub latency_ms: f32,
}

pub struct Engine {
    _in: Option<cpal::Stream>,
    _out: cpal::Stream,
    cmds: rtrb::Producer<Cmd>,
    trash: rtrb::Consumer<Trash>,
    pub tuner: rtrb::Consumer<f32>,
    pub meters: Arc<Meters>,
    pub info: Info,
}

impl Engine {
    pub fn start(params: Arc<Params>, cfg: &Config) -> Result<Engine, String> {
        let host = cpal::default_host();
        let out_dev = match cfg.output.as_deref() {
            Some(n) => find_device(&host, n, false).ok_or(format!("output {n} not found"))?,
            None => host.default_output_device().ok_or("no audio output device")?,
        };
        let out_name = out_dev.name().unwrap_or_else(|_| "default".into());
        let rate = cfg.rate;
        let buffer = cfg.buffer;
        let out_sup = out_dev.default_output_config().map_err(|e| e.to_string())?;
        let out_ch = out_sup.channels() as usize;
        let stream_cfg = |ch: u16| cpal::StreamConfig {
            channels: ch,
            sample_rate: cpal::SampleRate(rate),
            buffer_size: cpal::BufferSize::Fixed(buffer),
        };

        let meters = Arc::new(Meters::default());
        let (tuner_tx, tuner_rx) = rtrb::RingBuffer::new(rate as usize / 2);
        let (chain, h) = Chain::new(rate as f32, params, meters.clone(), Some(tuner_tx));

        // input → ring (mono). On ALSA, cpal's `Fixed(v)` means period v/4 and buffer v, and a
        // capture opened straight on the card (no PipeWire in between) overruns as soon as its
        // thread is late by more than that buffer: at 128 frames (2.7 ms) any desktop hiccup
        // dropped input and the speakers crackled. The capture buffer therefore never goes
        // below 512 frames (period 128, ~10 ms of slack); latency grows by the period only.
        // Cards that refuse a size get the next one.
        let mut in_name = None;
        let mut in_period = 0u32;
        let mut ring_rx = rtrb::RingBuffer::<f32>::new(1).1;
        let in_stream = if let Some(want) = cfg.input.as_deref() {
            let dev = find_device(&host, want, true).ok_or(format!("input {want} not found"))?;
            let sup = dev.default_input_config().map_err(|e| e.to_string())?;
            let mut last_err = String::new();
            let mut got = None;
            let first = (buffer * 4).clamp(512, 2048);
            for size in [first, 1024, 2048, 4096].into_iter().filter(|s| *s >= first) {
                let cap = (size as usize * 8).max(8192);
                let (tx, rx) = rtrb::RingBuffer::<f32>::new(cap);
                let sc = stream_cfg(sup.channels());
                let sc = cpal::StreamConfig { buffer_size: cpal::BufferSize::Fixed(size), ..sc };
                let m = meters.clone();
                let s = match sup.sample_format() {
                    cpal::SampleFormat::F32 => build_in::<f32>(&dev, &sc, tx, m, cfg.channel),
                    cpal::SampleFormat::I16 => build_in::<i16>(&dev, &sc, tx, m, cfg.channel),
                    cpal::SampleFormat::I32 => build_in::<i32>(&dev, &sc, tx, m, cfg.channel),
                    f => return Err(format!("unsupported input format {f:?}")),
                };
                match s.and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
                    Ok(s) => {
                        got = Some(s);
                        ring_rx = rx;
                        // ALSA: period = buffer / 4. WASAPI hands over whole device periods
                        // (~10 ms) up to the buffer size per callback.
                        in_period = if cfg!(windows) { size } else { size / 4 };
                        break;
                    }
                    Err(e) => last_err = e,
                }
            }
            in_name = Some(want.to_string());
            Some(got.ok_or(last_err)?)
        } else {
            None
        };

        // ring fill the output keeps in reserve: two input periods absorb the burst pattern
        // (input delivers a period at a time, output pulls its own quantum) plus jitter
        let target = (in_period as usize * 2).max(128);
        let sc = stream_cfg(out_ch as u16);
        let jb = Jitter { ring: ring_rx, target, primed: false, live: in_stream.is_some() };
        let out = match out_sup.sample_format() {
            cpal::SampleFormat::F32 => build_out::<f32>(&out_dev, &sc, chain, jb, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I16 => build_out::<i16>(&out_dev, &sc, chain, jb, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I32 => build_out::<i32>(&out_dev, &sc, chain, jb, out_ch, buffer as usize, meters.clone()),
            f => return Err(format!("unsupported output format {f:?}")),
        }?;
        out.play().map_err(|e| e.to_string())?;

        let lat_samples = buffer as f32 + if in_stream.is_some() { (in_period as usize + target) as f32 } else { 0.0 } + crate::conv::B as f32;
        Ok(Engine {
            _in: in_stream,
            _out: out,
            cmds: h.cmds,
            trash: h.trash,
            tuner: tuner_rx,
            meters,
            info: Info {
                host: host.id().name().to_string(),
                input: in_name,
                output: out_name,
                rate,
                buffer,
                latency_ms: lat_samples / rate as f32 * 1000.0,
            },
        })
    }

    pub fn send(&mut self, c: Cmd) -> bool {
        self.collect_trash();
        self.cmds.push(c).is_ok()
    }

    /// Free objects the audio thread replaced. Call regularly from the UI thread.
    pub fn collect_trash(&mut self) {
        while self.trash.pop().is_ok() {}
    }

    pub fn fs(&self) -> f32 {
        self.info.rate as f32
    }

    pub fn load(&self) -> f32 {
        f32::from_bits(self.meters.load.load(Ordering::Relaxed))
    }
}

/// Real-time priority for the calling audio thread: SCHED_FIFO 70 (under PipeWire's 88, so
/// its graph still runs first), else nice −11. Without it the threads keep whatever niceness
/// the launcher had: started from a nice-16 desktop shell next to a render using every core,
/// the capture thread missed its 10 ms buffer about once a second, every overrun restarted
/// the PCM and the dropout came out of the speakers as a crackle on every note.
/// Returns 1 for FIFO, 4 for the nice fallback, 0 when neither was allowed. Once per thread,
/// from its first callback: a syscall, no allocation.
fn promote_audio_thread() -> u32 {
    #[cfg(target_os = "linux")]
    unsafe {
        let param = libc::sched_param { sched_priority: 70 };
        // reset on fork: a child process never inherits real-time scheduling
        if libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0 {
            return 1;
        }
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        if libc::setpriority(libc::PRIO_PROCESS, tid, -11) == 0 {
            return 4;
        }
    }
    // cpal's WASAPI threads already run at THREAD_PRIORITY_TIME_CRITICAL
    u32::from(cfg!(windows))
}

fn build_in<T: SizedSample>(
    dev: &cpal::Device,
    sc: &cpal::StreamConfig,
    mut ring: rtrb::Producer<f32>,
    meters: Arc<Meters>,
    fixed: Option<usize>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let ch = sc.channels as usize;
    let mut pick = ChannelPick::new(sc.sample_rate.0 as f32);
    let err_meters = meters.clone();
    let mut promoted = false;
    dev.build_input_stream(
        sc,
        move |data: &[T], _| {
            if !promoted {
                promoted = true;
                meters.rt.fetch_or(promote_audio_thread() << 1, Ordering::Relaxed);
            }
            // the guitar may sit on either input: a fixed channel, or follow the one being played
            let c = match fixed {
                Some(c) => c.min(ch - 1),
                None => pick.update(data.chunks(ch).map(|f| (f[0].to_sample::<f32>(), f.get(1).map_or(0.0, |x| x.to_sample::<f32>())))),
            };
            meters.in_channel.store(c as u32, Ordering::Relaxed);
            for frame in data.chunks(ch) {
                let _ = ring.push(frame[c.min(ch - 1)].to_sample::<f32>());
            }
        },
        move |e| {
            err_meters.in_errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("haze-dsp input: {e}");
        },
        None,
    )
    .map_err(|e| e.to_string())
}

/// Which input channel carries the guitar. Interfaces differ (input 1 or 2, a front switch
/// that moves it), and the other channel often holds only hum or crosstalk. Per channel the
/// score is how far a 0.5 s block peak rises above 1.25× the quietest block of the last ~4 s,
/// in absolute terms, best block of those ~4 s: steady hum scores ≈ 0, a played string scores its own level, and
/// crosstalk only a small fraction of the channel it leaks from (a ratio test picked the
/// crosstalk: tiny, but far above a silent floor). Switches when the other channel scores
/// 4× more and peaks above −70 dBFS; never per sample, so no zipper clicks. Comparing the
/// best of ~4 s (not the last block) keeps the pauses between notes from handing the choice
/// to the other channel.
pub struct ChannelPick {
    cur: usize,
    peak: [f32; 2],
    blocks: [[f32; 8]; 2],
    scores: [[f32; 8]; 2],
    slot: usize,
    count: u32,
    block_len: u32,
}

impl ChannelPick {
    pub fn new(fs: f32) -> Self {
        ChannelPick { cur: 0, peak: [0.0; 2], blocks: [[f32::MAX; 8]; 2], scores: [[0.0; 8]; 2], slot: 0, count: 0, block_len: (0.5 * fs) as u32 }
    }

    /// Feed one callback's frames (first two channels); returns the channel to use.
    pub fn update(&mut self, frames: impl Iterator<Item = (f32, f32)>) -> usize {
        for (a, b) in frames {
            self.peak[0] = self.peak[0].max(a.abs());
            self.peak[1] = self.peak[1].max(b.abs());
            self.count += 1;
            if self.count < self.block_len {
                continue;
            }
            self.count = 0;
            let mut score = [0f32; 2];
            for c in 0..2 {
                self.blocks[c][self.slot] = self.peak[c];
                let floor = self.blocks[c].iter().fold(f32::MAX, |m, x| m.min(*x));
                self.scores[c][self.slot] = (self.peak[c] - 1.25 * floor).max(0.0);
                score[c] = self.scores[c].iter().fold(0.0, |m, x| m.max(*x));
            }
            let other = 1 - self.cur;
            if self.peak[other] > 3e-4 && score[other] > score[self.cur] * 4.0 {
                self.cur = other;
            }
            self.slot = (self.slot + 1) % 8;
            self.peak = [0.0; 2];
        }
        self.cur
    }
}

/// Input ring seen from the output callback: a jitter buffer. It starts (and restarts after
/// running dry) only once `target` samples wait, so a late input costs one short gap
/// instead of zeros sprinkled into every following block; surplus from the input's clock
/// running fast is dropped back to the target.
struct Jitter {
    ring: rtrb::Consumer<f32>,
    target: usize,
    primed: bool,
    /// false: no capture stream, the chain gets silence (DI player)
    live: bool,
}

impl Jitter {
    /// Fill `out` with input samples, or silence while (re)buffering.
    fn read(&mut self, out: &mut [f32], meters: &Meters) {
        let n = out.len();
        let avail = self.ring.slots();
        if !self.live || (!self.primed && avail < self.target + n) {
            out.fill(0.0);
            return;
        }
        if avail < n {
            // ran dry: emit silence and wait for a full reserve again
            self.primed = false;
            meters.ring_underruns.fetch_add(1, Ordering::Relaxed);
            out.fill(0.0);
            return;
        }
        self.primed = true;
        // the reserve may hold up to 4x target before the surplus is dropped (one click
        // every few minutes on a fast input clock, never during normal jitter)
        let surplus = avail.saturating_sub(n + self.target * 4);
        if surplus > 0 {
            let drop = avail - n - self.target;
            if let Ok(c) = self.ring.read_chunk(drop) {
                c.commit_all();
            }
            meters.ring_skips.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(c) = self.ring.read_chunk(n) {
            let (a, b) = c.as_slices();
            out[..a.len()].copy_from_slice(a);
            out[a.len()..].copy_from_slice(b);
            c.commit_all();
        }
    }
}

fn build_out<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    sc: &cpal::StreamConfig,
    mut chain: Chain,
    mut ring: Jitter,
    ch: usize,
    buffer: usize,
    meters: Arc<Meters>,
) -> Result<cpal::Stream, String> {
    // scratch sized for the largest callback we accept; bigger ones are processed in slices
    let max = buffer.max(1024) * 4;
    let mut inp = vec![0f32; max];
    let mut l = vec![0f32; max];
    let mut r = vec![0f32; max];
    let fs = chain.fs;
    let mut promoted = false;
    dev.build_output_stream(
        sc,
        move |data: &mut [T], _| {
            if !promoted {
                promoted = true;
                meters.rt.fetch_or(promote_audio_thread(), Ordering::Relaxed);
            }
            let t0 = Instant::now();
            let frames = data.len() / ch;
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(max);
                ring.read(&mut inp[..n], &meters);
                chain.process(&inp[..n], &mut l[..n], &mut r[..n]);
                for (i, f) in data[done * ch..(done + n) * ch].chunks_mut(ch).enumerate() {
                    f[0] = T::from_sample(l[i]);
                    if ch > 1 {
                        f[1] = T::from_sample(r[i]);
                    }
                    for s in f.iter_mut().skip(2) {
                        *s = T::from_sample(0.0f32);
                    }
                }
                done += n;
            }
            let load = t0.elapsed().as_secs_f32() / (frames as f32 / fs);
            meters.load.store(load.to_bits(), Ordering::Relaxed);
            if load > 1.0 {
                meters.xruns.fetch_add(1, Ordering::Relaxed);
            }
        },
        |e| eprintln!("haze-dsp output: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}
