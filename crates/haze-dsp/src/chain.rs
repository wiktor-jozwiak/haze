//! The full signal chain. `Chain::process` runs on the audio thread: no allocation, no locks.
//! Large objects (IRs, DI loops) arrive as boxes through a wait-free SPSC ring (`rtrb`) and
//! the replaced ones go back through a second ring, so they are freed on the UI thread.

use crate::blocks::{Amp, Delay, Drive, Eq10, Gate, Limiter, Modulation, Reverb};
use crate::conv::{builtin_cab, Convolver, IrSpectrum, CABS};
use crate::filters::{db, Smooth};
use crate::params::{Meters, Params, P};
use std::sync::Arc;

pub enum Cmd {
    /// Replace the user IR (slot 3) or a built-in slot.
    SetIr(usize, Box<IrSpectrum>),
    /// Replace the DI loop (built-in riff or a loaded .wav).
    SetDi(Box<Vec<f32>>),
    /// Restart the DI loop from the top.
    RewindDi,
}

/// Objects the audio thread hands back to be dropped elsewhere.
pub enum Trash {
    Ir(Box<IrSpectrum>),
    Di(Box<Vec<f32>>),
}

pub struct Chain {
    pub fs: f32,
    params: Arc<Params>,
    meters: Arc<Meters>,
    cmds: rtrb::Consumer<Cmd>,
    trash: rtrb::Producer<Trash>,
    tuner: Option<rtrb::Producer<f32>>,
    gate: Gate,
    drive: Drive,
    amp: Amp,
    /// one convolver per IR slot, so switching cabinets never rebuilds spectra
    cabs: [Convolver; 4],
    eq: Eq10,
    modu: Modulation,
    delay: Delay,
    reverb: Reverb,
    limiter: Limiter,
    di: Box<Vec<f32>>,
    di_pos: usize,
    in_gain: Smooth,
    out_gain: Smooth,
    master: Smooth,
    amp_gain: Smooth,
}

pub struct ChainHandles {
    pub cmds: rtrb::Producer<Cmd>,
    pub trash: rtrb::Consumer<Trash>,
}

impl Chain {
    /// Builds everything the callback will ever touch (IR spectra, delay lines, DI loop).
    pub fn new(fs: f32, params: Arc<Params>, meters: Arc<Meters>, tuner: Option<rtrb::Producer<f32>>) -> (Chain, ChainHandles) {
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(16);
        let (trash_tx, trash_rx) = rtrb::RingBuffer::new(16);
        let cab = |i: usize| Convolver::new(Box::new(IrSpectrum::new(&builtin_cab(i.min(2), fs), CABS[i.min(2)])));
        let chain = Chain {
            fs,
            params,
            meters,
            cmds: cmd_rx,
            trash: trash_tx,
            tuner,
            gate: Gate::new(fs),
            drive: Drive::new(fs),
            amp: Amp::new(fs),
            cabs: [cab(0), cab(1), cab(2), cab(2)],
            eq: Eq10::new(fs),
            modu: Modulation::new(fs),
            delay: Delay::new(fs),
            reverb: Reverb::new(fs),
            limiter: Limiter::new(fs),
            di: Box::new(crate::di::builtin_riff(fs)),
            di_pos: 0,
            in_gain: Smooth::new(1.0, 20.0, fs),
            out_gain: Smooth::new(1.0, 20.0, fs),
            master: Smooth::new(0.5, 20.0, fs),
            amp_gain: Smooth::new(0.5, 20.0, fs),
        };
        (chain, ChainHandles { cmds: cmd_tx, trash: trash_rx })
    }

    fn poll_cmds(&mut self) {
        while let Ok(c) = self.cmds.pop() {
            match c {
                Cmd::SetIr(slot, ir) => {
                    let old = self.cabs[slot.min(3)].swap_ir(ir);
                    // ring full: keep it rather than free here (leaks one IR at worst)
                    if let Err(rtrb::PushError::Full(t)) = self.trash.push(Trash::Ir(old)) {
                        std::mem::forget(t);
                    }
                }
                Cmd::SetDi(d) => {
                    let old = std::mem::replace(&mut self.di, d);
                    self.di_pos = 0;
                    if let Err(rtrb::PushError::Full(t)) = self.trash.push(Trash::Di(old)) {
                        std::mem::forget(t);
                    }
                }
                Cmd::RewindDi => self.di_pos = 0,
            }
        }
    }

    /// `input`: mono interface input (ignored when the DI player is the source).
    /// Outputs are overwritten. All three slices have the same length.
    pub fn process(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        self.poll_cmds();
        let p = &*self.params;
        let use_di = p.on(P::Source) && !self.di.is_empty();
        let in_g = db(p.get(P::InputGainDb));
        let out_g = db(p.get(P::OutGainDb));
        let gate_on = p.on(P::GateOn);
        let gate_t = p.get(P::GateThreshDb);
        let drive_on = p.on(P::DriveOn);
        let dm = p.get(P::DriveModel) as i32;
        let (da, dt, dl) = (p.get(P::DriveAmt), p.get(P::DriveTone), p.get(P::DriveLevel));
        let voicing = p.get(P::AmpVoicing) as i32;
        let (gain, bass, mid, treble) = (p.get(P::AmpGain), p.get(P::AmpBass), p.get(P::AmpMid), p.get(P::AmpTreble));
        let (pres, res, master, sag) = (p.get(P::AmpPresence), p.get(P::AmpResonance), p.get(P::AmpMaster), p.get(P::AmpSag));
        let cab_on = p.on(P::CabOn);
        let cab = (p.get(P::CabIr) as usize).min(3);
        let eq_on = p.on(P::EqOn);
        if eq_on {
            for i in 0..10 {
                self.eq.set(i, p.get(Params::eq_band(i)).clamp(-15.0, 15.0));
            }
        }
        let mod_on = p.on(P::ModOn);
        let (mt, mr, md, mm) = (p.get(P::ModType) as i32, p.get(P::ModRate), p.get(P::ModDepth), p.get(P::ModMix));
        let delay_on = p.on(P::DelayOn);
        let (dms, dfb, dmix, ping) = (p.get(P::DelayMs), p.get(P::DelayFeedback), p.get(P::DelayMix), p.on(P::DelayPingPong));
        let rev_on = p.on(P::ReverbOn);
        let (rt, rd, rm) = (p.get(P::ReverbType) as i32, p.get(P::ReverbDecay), p.get(P::ReverbMix));
        let lim_on = p.on(P::Limiter);

        let mut in_peak = 0f32;
        let mut out_peak = 0f32;
        for i in 0..out_l.len() {
            let src = if use_di {
                let s = self.di[self.di_pos];
                self.di_pos += 1;
                if self.di_pos >= self.di.len() {
                    self.di_pos = 0;
                }
                s
            } else {
                input.get(i).copied().unwrap_or(0.0)
            };
            let mut x = src * self.in_gain.next(in_g);
            in_peak = in_peak.max(x.abs());
            if let Some(t) = self.tuner.as_mut() {
                let _ = t.push(x);
            }
            if gate_on {
                x = self.gate.tick(x, gate_t);
            }
            if drive_on {
                x = self.drive.tick(x, dm, da, dt, dl);
            }
            let ag = self.amp_gain.next(gain);
            let mg = self.master.next(master);
            x = self.amp.tick(x, voicing, ag, bass, mid, treble, pres, res, mg, sag);
            if cab_on {
                x = self.cabs[cab].tick(x);
            }
            if eq_on {
                x = self.eq.tick(x);
            }
            let (mut l, mut r) = if mod_on { self.modu.tick(x, mt, mr, md, mm) } else { (x, x) };
            if delay_on {
                (l, r) = self.delay.tick(l, r, dms, dfb, dmix, ping);
            }
            if rev_on {
                (l, r) = self.reverb.tick(l, r, rt, rd, rm);
            }
            let og = self.out_gain.next(out_g);
            l *= og;
            r *= og;
            if lim_on {
                (l, r) = self.limiter.tick(l, r);
            }
            out_peak = out_peak.max(l.abs()).max(r.abs());
            out_l[i] = l.clamp(-1.0, 1.0);
            out_r[i] = r.clamp(-1.0, 1.0);
        }
        Meters::raise(&self.meters.in_peak, in_peak);
        Meters::raise(&self.meters.out_peak, out_peak);
        if in_peak >= 0.99 {
            self.meters.in_clip.store(1, std::sync::atomic::Ordering::Relaxed);
        }
        if out_peak >= 1.0 {
            self.meters.out_clip.store(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Render `seconds` of the chain offline (DI source forced on): interleaved stereo.
pub fn render_offline(params: Arc<Params>, fs: f32, seconds: f32) -> Vec<f32> {
    params.set(P::Source, 1.0);
    let (mut chain, _h) = Chain::new(fs, params, Arc::new(Meters::default()), None);
    let n = (seconds * fs) as usize;
    let mut out = Vec::with_capacity(n * 2);
    let block = 256;
    let silence = vec![0f32; block];
    let mut l = vec![0f32; block];
    let mut r = vec![0f32; block];
    let mut done = 0;
    while done < n {
        let k = block.min(n - done);
        chain.process(&silence[..k], &mut l[..k], &mut r[..k]);
        for i in 0..k {
            out.push(l[i]);
            out.push(r[i]);
        }
        done += k;
    }
    out
}

pub fn write_wav(path: &std::path::Path, fs: u32, interleaved: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 2, sample_rate: fs, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in interleaved {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}
