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
}

impl Default for Config {
    fn default() -> Self {
        Config { buffer: 128, rate: 48000, input: None, output: None }
    }
}

/// One selectable device: cpal name plus a human label from /proc/asound/cards.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub label: String,
    /// looks like a microphone / webcam: feedback risk when used as the guitar input
    pub mic: bool,
}

fn card_labels() -> Vec<(String, String)> {
    // " 0 [CODEC          ]: USB-Audio - USB AUDIO  CODEC"
    let txt = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    txt.lines()
        .filter_map(|l| {
            let (a, b) = l.split_once("]:")?;
            let id = a.split_once('[')?.1.trim().to_string();
            let name = b.split_once(" - ").map_or(b, |x| x.1).split_whitespace().collect::<Vec<_>>().join(" ");
            Some((id, name))
        })
        .collect()
}

/// One entry per sound card (`sysdefault:CARD=…`, which converts format/rate) plus the
/// system default. Outputs skip HDMI-only cards' noise by listing them like the rest.
pub fn list_devices(input: bool) -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let labels = card_labels();
    let devs: Vec<String> = match if input { host.input_devices() } else { host.output_devices() } {
        Ok(it) => it.filter_map(|d| d.name().ok()).collect(),
        Err(_) => vec![],
    };
    let mut out = vec![];
    if !input && devs.iter().any(|d| d == "default") {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe (PipeWire)".into(), mic: false });
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
        let label = labels.iter().find(|(id, _)| id == card).map_or(card.to_string(), |x| x.1.clone());
        let low = label.to_lowercase();
        let mic = input && (low.contains("camera") || low.contains("webcam") || low.contains("mic") || low.contains("hd-audio"));
        out.push(DeviceInfo { id: d.clone(), label, mic });
    }
    if input && devs.iter().any(|d| d == "default") {
        out.push(DeviceInfo { id: "default".into(), label: "systemowe (zwykle mikrofon!)".into(), mic: true });
    }
    out
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
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

        // input → ring (mono). Direct ALSA cards (USB interfaces) often refuse small fixed
        // buffers, so the input tries growing sizes; the ring is prefilled with one input
        // buffer of silence so the output never starves.
        let mut in_name = None;
        let mut in_buf = 0u32;
        let mut ring_rx = rtrb::RingBuffer::<f32>::new(1).1;
        let in_stream = if let Some(want) = cfg.input.as_deref() {
            let dev = find_device(&host, want, true).ok_or(format!("input {want} not found"))?;
            let sup = dev.default_input_config().map_err(|e| e.to_string())?;
            let mut last_err = String::new();
            let mut got = None;
            for size in [buffer, 256, 512, 1024].into_iter().filter(|s| *s >= buffer) {
                let cap = (size as usize * 8).max(4096);
                let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(cap);
                for _ in 0..size.max(buffer) {
                    let _ = tx.push(0.0);
                }
                let sc = stream_cfg(sup.channels());
                let sc = cpal::StreamConfig { buffer_size: cpal::BufferSize::Fixed(size), ..sc };
                let s = match sup.sample_format() {
                    cpal::SampleFormat::F32 => build_in::<f32>(&dev, &sc, tx),
                    cpal::SampleFormat::I16 => build_in::<i16>(&dev, &sc, tx),
                    cpal::SampleFormat::I32 => build_in::<i32>(&dev, &sc, tx),
                    f => return Err(format!("unsupported input format {f:?}")),
                };
                match s.and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
                    Ok(s) => {
                        got = Some(s);
                        ring_rx = rx;
                        in_buf = size;
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

        let sc = stream_cfg(out_ch as u16);
        let out = match out_sup.sample_format() {
            cpal::SampleFormat::F32 => build_out::<f32>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I16 => build_out::<i16>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I32 => build_out::<i32>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            f => return Err(format!("unsupported output format {f:?}")),
        }?;
        out.play().map_err(|e| e.to_string())?;

        let lat_samples = buffer as f32 + if in_stream.is_some() { 2.0 * in_buf as f32 } else { 0.0 } + crate::conv::B as f32;
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

fn build_in<T: SizedSample>(dev: &cpal::Device, sc: &cpal::StreamConfig, mut ring: rtrb::Producer<f32>) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let ch = sc.channels as usize;
    dev.build_input_stream(
        sc,
        move |data: &[T], _| {
            // first channel = guitar input (Hi-Z on most interfaces)
            for frame in data.chunks(ch) {
                let _ = ring.push(frame[0].to_sample::<f32>());
            }
        },
        |e| eprintln!("haze-dsp input: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

fn build_out<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    sc: &cpal::StreamConfig,
    mut chain: Chain,
    mut ring: rtrb::Consumer<f32>,
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
    dev.build_output_stream(
        sc,
        move |data: &mut [T], _| {
            let t0 = Instant::now();
            let frames = data.len() / ch;
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(max);
                for x in inp[..n].iter_mut() {
                    *x = ring.pop().unwrap_or(0.0);
                }
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
