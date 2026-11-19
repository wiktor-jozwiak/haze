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
    /// capture from the default input device (false: DI player only)
    pub input: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { buffer: 128, rate: 48000, input: true }
    }
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
        let out_dev = host.default_output_device().ok_or("brak urządzenia wyjściowego audio")?;
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

        // input → ring (mono). Prefilled with one buffer of silence so the output never starves.
        let cap = (buffer as usize * 8).max(4096);
        let (mut ring_tx, ring_rx) = rtrb::RingBuffer::<f32>::new(cap);
        for _ in 0..buffer {
            let _ = ring_tx.push(0.0);
        }
        let mut in_name = None;
        let in_stream = if cfg.input {
            match host.default_input_device() {
                Some(dev) => {
                    in_name = dev.name().ok();
                    let sup = dev.default_input_config().map_err(|e| e.to_string())?;
                    let ch = sup.channels();
                    let sc = stream_cfg(ch);
                    let s = match sup.sample_format() {
                        cpal::SampleFormat::F32 => build_in::<f32>(&dev, &sc, ring_tx),
                        cpal::SampleFormat::I16 => build_in::<i16>(&dev, &sc, ring_tx),
                        cpal::SampleFormat::I32 => build_in::<i32>(&dev, &sc, ring_tx),
                        f => return Err(format!("nieobsługiwany format wejścia {f:?}")),
                    }?;
                    s.play().map_err(|e| e.to_string())?;
                    Some(s)
                }
                None => None,
            }
        } else {
            None
        };

        let sc = stream_cfg(out_ch as u16);
        let out = match out_sup.sample_format() {
            cpal::SampleFormat::F32 => build_out::<f32>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I16 => build_out::<i16>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            cpal::SampleFormat::I32 => build_out::<i32>(&out_dev, &sc, chain, ring_rx, out_ch, buffer as usize, meters.clone()),
            f => return Err(format!("nieobsługiwany format wyjścia {f:?}")),
        }?;
        out.play().map_err(|e| e.to_string())?;

        let lat_samples = buffer as f32 * if in_stream.is_some() { 3.0 } else { 1.0 } + crate::conv::B as f32;
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
