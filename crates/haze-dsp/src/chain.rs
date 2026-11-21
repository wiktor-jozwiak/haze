//! The full signal chain. `Chain::process` runs on the audio thread: no allocation, no locks.
//! Large objects (IRs, DI loops) arrive as boxes through a wait-free SPSC ring (`rtrb`) and
//! the replaced ones go back through a second ring, so they are freed on the UI thread.

use crate::blocks::{Amp, Drive, Gate, HumFilter, Limiter, Modulation, Trim, TRIM_MAKEUP};
use crate::fx::{Comp, Echo, EqSlot, Verb};
use crate::fxunit::FxUnit;
use crate::pcfx::{PcFx, PcVals};
use crate::conv::{builtin_cab, Convolver, IrSpectrum, CABS};
use crate::filters::{db, Smooth};
use crate::params::{EqKey, Meters, Params, EQ_SLOTS, FX_SLOTS, P};
use crate::practice::{Drums, Kit, LoopAction, LoopState, Looper};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Post-amp signal into the guitar-level domain and back: the DI riff sits at −18 LUFS at
/// the input and around +4 LUFS after amp and cab whatever the voicing (measured).
const POST_IN: f32 = 0.1;
const POST_OUT: f32 = 10.0;

/// The drum bus at fader 0 dB sits about 3 dB under the DI riff through a clean amp at the
/// output. Measured at the default output gain: guitar −16.6 dBFS RMS, the bare rock kit
/// −34.2 dBFS, so the kit alone could never get within 17 dB of the guitar.
pub const DRUM_MAKEUP: f32 = 5.4;

pub enum Cmd {
    /// Replace the user IR (slot 3) or a built-in slot.
    SetIr(usize, Box<IrSpectrum>),
    /// Replace the DI loop (built-in riff or a loaded .wav).
    SetDi(Box<Vec<f32>>),
    /// Restart the DI loop from the top.
    RewindDi,
    /// Memory for the looper: interleaved [base L, base R, layer L, layer R] frames.
    SetLoopBuffer(Box<[f32]>),
    Loop(LoopAction),
    /// Drum machine back to bar start (on start / tempo tap).
    DrumsRestart,
}

/// Objects the audio thread hands back to be dropped elsewhere.
pub enum Trash {
    Ir(Box<IrSpectrum>),
    Di(Box<Vec<f32>>),
    Loop(Box<[f32]>),
}

pub struct Chain {
    pub fs: f32,
    params: Arc<Params>,
    meters: Arc<Meters>,
    cmds: rtrb::Consumer<Cmd>,
    trash: rtrb::Producer<Trash>,
    tuner: Option<rtrb::Producer<f32>>,
    hum: HumFilter,
    gate: Gate,
    drive: Drive,
    amp: Amp,
    /// one convolver per IR slot, so switching cabinets never rebuilds spectra
    cabs: [Convolver; 4],
    comp: Comp,
    eqs: [EqSlot; EQ_SLOTS],
    /// the MOD and FX slots' wah / pitch / slicer / ... types
    units: [FxUnit; FX_SLOTS],
    /// PC-only effects after the patch's chain
    pc: PcFx,
    modu: Modulation,
    delay: Echo,
    delay2: Echo,
    reverb: Verb,
    limiter: Limiter,
    looper: Looper,
    drums: Drums,
    drums_were_on: bool,
    /// looper request held for the next bar line (sync) or the end of the count-in
    loop_wait: Option<LoopAction>,
    count_in: bool,
    /// bars played at the current speed-trainer tempo
    trainer_bars: u32,
    di: Box<Vec<f32>>,
    di_pos: usize,
    in_gain: Smooth,
    trim: Trim,
    out_gain: Smooth,
    /// practice mixer faders: monitored guitar, drum bus, looper playback
    gtr_fader: Smooth,
    drum_fader: Smooth,
    loop_fader: Smooth,
    master: Smooth,
    amp_gain: Smooth,
    /// start-up fade-in (0..1): filters, cab and amp settle without a click on engine start
    fade: f32,
    fade_step: f32,
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
        // smoothers start at the current settings: starting at 0 dB and gliding down to the
        // real output gain blasted the first 20 ms at full level on every engine start
        let (in_g, out_g) = (db(params.get(P::InputGainDb)), db(params.get(P::OutGainDb)));
        let (master, amp_gain) = (params.get(P::AmpMaster), params.get(P::AmpGain));
        let faders = (params.get(P::GuitarLevel), params.get(P::DrumLevel), params.get(P::LoopLevel));
        let chain = Chain {
            fs,
            params,
            meters,
            cmds: cmd_rx,
            trash: trash_tx,
            tuner,
            hum: HumFilter::new(fs),
            gate: Gate::new(fs),
            drive: Drive::new(fs),
            amp: Amp::new(fs),
            cabs: [cab(0), cab(1), cab(2), cab(2)],
            comp: Comp::new(fs),
            eqs: std::array::from_fn(|_| EqSlot::new(fs)),
            units: std::array::from_fn(|_| FxUnit::new(fs)),
            pc: PcFx::new(fs),
            modu: Modulation::new(fs),
            delay: Echo::new(fs),
            delay2: Echo::new(fs),
            reverb: Verb::new(fs),
            limiter: Limiter::new(fs),
            looper: Looper::new(),
            drums: Drums::new(fs),
            drums_were_on: false,
            loop_wait: None,
            count_in: false,
            trainer_bars: 0,
            di: Box::new(crate::di::builtin_riff(fs)),
            di_pos: 0,
            in_gain: Smooth::new(in_g, 20.0, fs),
            trim: Trim::new(fs),
            out_gain: Smooth::new(out_g, 20.0, fs),
            gtr_fader: Smooth::new(faders.0, 20.0, fs),
            drum_fader: Smooth::new(faders.1, 20.0, fs),
            loop_fader: Smooth::new(faders.2, 20.0, fs),
            master: Smooth::new(master, 20.0, fs),
            amp_gain: Smooth::new(amp_gain, 20.0, fs),
            fade: 0.0,
            fade_step: 1.0 / (0.03 * fs),
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
                Cmd::SetLoopBuffer(b) => {
                    let old = self.looper.set_buffer(b);
                    if let Err(rtrb::PushError::Full(t)) = self.trash.push(Trash::Loop(old)) {
                        std::mem::forget(t);
                    }
                }
                Cmd::Loop(a) => self.loop_request(a),
                Cmd::DrumsRestart => self.drums.reset(),
            }
        }
    }

    /// Looper button: immediate, or held for the bar line (drums + sync) / a count-in bar.
    fn loop_request(&mut self, a: LoopAction) {
        let p = &*self.params;
        let st = self.looper.state;
        if self.loop_wait.is_some() {
            // a second press while waiting cancels; CLEAR / STOP always cancel
            self.loop_wait = None;
            self.count_in = false;
            if matches!(a, LoopAction::Clear | LoopAction::Stop) {
                self.looper.action(a);
            }
            return;
        }
        let starts = a == LoopAction::Cycle && st == LoopState::Empty;
        let closes = st == LoopState::Recording && matches!(a, LoopAction::Cycle | LoopAction::Play | LoopAction::Stop);
        if p.on(P::DrumOn) && p.on(P::LoopSync) && (starts || closes) {
            self.loop_wait = Some(a);
        } else if !p.on(P::DrumOn) && p.on(P::CountIn) && starts {
            self.drums.reset();
            self.count_in = true;
            self.loop_wait = Some(a);
        } else {
            self.looper.action(a);
        }
    }

    /// `input`: mono interface input (ignored when the DI player is the source).
    /// Outputs are overwritten. All three slices have the same length.
    pub fn process(&mut self, input: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        self.poll_cmds();
        let p = &*self.params;
        let use_di = p.on(P::Source) && !self.di.is_empty();
        let in_g = db(p.get(P::InputGainDb));
        let auto_trim = p.on(P::InputAuto);
        let out_g = if p.on(P::Mute) { 0.0 } else { db(p.get(P::OutGainDb)) };
        // the DI loop is clean: the hum filter is for the interface input only
        let hum_hz = if use_di { 0.0 } else { p.get(P::HumHz) };
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
        let comp_on = p.on(P::CompOn);
        let comp_pre = p.on(P::CompPre);
        if comp_on {
            self.comp.set(
                p.get(P::CompThreshDb),
                p.get(P::CompRatio),
                p.get(P::CompAttackMs),
                p.get(P::CompReleaseMs),
                p.get(P::CompMakeupDb),
                p.get(P::CompToneDb),
            );
        }
        // EQ slots: on / pre flags as bit masks, coefficients refreshed only on change
        let (mut eq_pre, mut eq_post) = (0u8, 0u8);
        for (i, e) in self.eqs.iter_mut().enumerate() {
            if p.get_i(Params::eq(i, EqKey::On)) < 0.5 {
                continue;
            }
            let v: [f32; 11] = std::array::from_fn(|k| p.get_i(Params::eq(i, EqKey::V(k))));
            e.set(p.get_i(Params::eq(i, EqKey::Kind)) as i32, &v);
            if p.get_i(Params::eq(i, EqKey::Pre)) >= 0.5 {
                eq_pre |= 1 << i;
            } else {
                eq_post |= 1 << i;
            }
        }
        // effect units: kind and values per slot, 0 = off
        let mut unit_kind = [0i32; FX_SLOTS];
        let mut unit_pre = [false; FX_SLOTS];
        let mut unit_v = [[0f32; 10]; FX_SLOTS];
        for s in 0..FX_SLOTS {
            if p.get_i(Params::fx(s, EqKey::On)) >= 0.5 {
                unit_kind[s] = p.get_i(Params::fx(s, EqKey::Kind)) as i32;
                unit_pre[s] = p.get_i(Params::fx(s, EqKey::Pre)) >= 0.5;
                unit_v[s] = std::array::from_fn(|k| p.get_i(Params::fx(s, EqKey::V(k))));
            }
        }
        let midi = p.get(P::PitchMidi);
        let pcv = PcVals::read(p);
        self.pc.prepare(&pcv, p);
        let mod_on = p.on(P::ModOn);
        let mod_pre = p.on(P::ModPre);
        let (mt, mr, md, mm) = (p.get(P::ModType) as i32, p.get(P::ModRate), p.get(P::ModDepth), p.get(P::ModMix));
        let delay_on = p.on(P::DelayOn);
        self.delay.set(p.get(P::DelayType) as i32, p.get(P::DelayHiCut));
        let d1 = (p.get(P::DelayMs), p.get(P::DelayFeedback), p.get(P::DelayMix), p.get(P::DelayDirect), p.get(P::DelayModRate), p.get(P::DelayModDepth));
        let delay2_on = p.on(P::Delay2On);
        self.delay2.set(p.get(P::Delay2Type) as i32, p.get(P::Delay2HiCut));
        let d2 = (p.get(P::Delay2Ms), p.get(P::Delay2Feedback), p.get(P::Delay2Mix), p.get(P::Delay2Direct), p.get(P::Delay2ModRate), p.get(P::Delay2ModDepth));
        let rev_on = p.on(P::ReverbOn);
        if rev_on {
            self.reverb.set(p.get(P::ReverbType) as i32, p.get(P::ReverbTime), p.get(P::ReverbDensity), p.get(P::ReverbLowCut), p.get(P::ReverbHighCut));
        }
        let (rpre, rwet, rdir) = (p.get(P::ReverbPreMs), p.get(P::ReverbMix), p.get(P::ReverbDirect));
        let lim_on = p.on(P::Limiter);
        let drums_on = p.on(P::DrumOn);
        if drums_on && !self.drums_were_on {
            self.drums.reset();
        }
        self.drums_were_on = drums_on;
        let (bpm, pat, dlev, llev) = (p.get(P::Bpm), p.get(P::DrumPattern) as usize, p.get(P::DrumLevel), p.get(P::LoopLevel));
        let sig = (p.get(P::TimeSig) as usize).min(2);
        let glev = p.get(P::GuitarLevel);
        self.drums.set_kit(&Kit {
            tune: p.get(P::DrumTune),
            decay: p.get(P::DrumDecay),
            human: p.get(P::DrumHuman),
            fills: p.get(P::DrumFills).max(0.0) as u32,
            swing: p.get(P::DrumSwing),
            levels: std::array::from_fn(|k| p.get_i(P::DrumKickLv as usize + k)),
        });
        if pat >= crate::practice::USER {
            self.drums.set_user(std::array::from_fn(|r| p.get_raw(P::DrumUser0, r)), p.get(P::DrumUserLen) as usize);
        }
        let trainer = drums_on && p.on(P::TrainerOn);
        let (t_step, t_bars, t_target) = (p.get(P::TrainerStep), p.get(P::TrainerBars).max(1.0) as u32, p.get(P::TrainerTarget));
        if !trainer {
            self.trainer_bars = 0;
        }
        // the clock stops with the drums; a sync request then has no bar line to wait for
        if !drums_on && !self.count_in && self.loop_wait.is_some() {
            if let Some(a) = self.loop_wait.take() {
                self.looper.action(a);
            }
        }

        let mut in_peak = 0f32;
        let mut out_peak = 0f32;
        let (mut g_peak, mut d_peak, mut l_peak) = (0f32, 0f32, 0f32);
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
            // before the tuner too: hum drags the pitch estimate
            x = self.hum.tick(x, hum_hz);
            // after the hum notch: hum must not count as playing. The DI loop is already at
            // the voicing level
            if auto_trim && !use_di {
                x = self.trim.tick(x);
            }
            if let Some(t) = self.tuner.as_mut() {
                let _ = t.push(x);
            }
            if gate_on {
                x = self.gate.tick(x, gate_t);
            }
            if auto_trim && !use_di {
                x *= TRIM_MAKEUP;
            }
            if comp_on && comp_pre {
                x = self.comp.tick(x);
            }
            for (i, e) in self.eqs.iter_mut().enumerate() {
                if eq_pre & (1 << i) != 0 {
                    x = e.tick(x);
                }
            }
            for (s, u) in self.units.iter_mut().enumerate() {
                if unit_kind[s] > 0 && unit_pre[s] {
                    x = u.tick(x, unit_kind[s], &unit_v[s], midi);
                }
            }
            if mod_on && mod_pre {
                // ahead of the mono amp: one side of the stereo modulation
                x = self.modu.tick(x, mt, mr, md, mm).0;
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
            // after the amp the signal runs ~20 dB above the guitar level the compressor and
            // the envelope-driven units (touch wah, slow gear, humanizer) are calibrated for:
            // they work on it scaled down into that range and hand it back
            if comp_on && !comp_pre {
                x = self.comp.tick(x * POST_IN) * POST_OUT;
            }
            for (i, e) in self.eqs.iter_mut().enumerate() {
                if eq_post & (1 << i) != 0 {
                    x = e.tick(x);
                }
            }
            for (s, u) in self.units.iter_mut().enumerate() {
                if unit_kind[s] > 0 && !unit_pre[s] {
                    x = u.tick(x * POST_IN, unit_kind[s], &unit_v[s], midi) * POST_OUT;
                }
            }
            x = self.pc.mono(x, &pcv, midi);
            let (mut l, mut r) = if mod_on && !mod_pre { self.modu.tick(x, mt, mr, md, mm) } else { (x, x) };
            if delay_on {
                (l, r) = self.delay.tick(l, r, d1.0, d1.1, d1.2, d1.3, d1.4, d1.5);
            }
            if delay2_on {
                (l, r) = self.delay2.tick(l, r, d2.0, d2.1, d2.2, d2.3, d2.4, d2.5);
            }
            if rev_on {
                (l, r) = self.reverb.tick(l, r, rpre, rwet, rdir);
            }
            (l, r) = self.pc.stereo(l, r, &pcv);
            // a feedback block that went non-finite would otherwise ring NaN/inf for ever:
            // wipe the recirculating state and drop this sample
            if !(l.is_finite() && r.is_finite()) {
                self.reverb.reset();
                self.delay.reset();
                self.delay2.reset();
                self.units.iter_mut().for_each(FxUnit::reset);
                self.pc.reset();
                (l, r) = (0.0, 0.0);
            }
            // drums first: a looper action held for the bar line lands on its first sample
            let (mut dl, mut dr) = (0.0, 0.0);
            if drums_on || self.count_in {
                (dl, dr) = self.drums.tick(bpm, if self.count_in { 0 } else { pat }, sig);
                if self.drums.bar_started() {
                    self.pc.bar();
                    // count-in: the first bar line opens the count bar, the second ends it
                    if !self.count_in || self.drums.bar >= 2 {
                        self.count_in = false;
                        if let Some(a) = self.loop_wait.take() {
                            self.looper.action(a);
                        }
                    }
                    if trainer && self.drums.bar > 1 {
                        self.trainer_bars += 1;
                        if self.trainer_bars >= t_bars {
                            self.trainer_bars = 0;
                            let now = p.get(P::Bpm);
                            if now < t_target {
                                p.set(P::Bpm, (now + t_step).min(t_target));
                            }
                        }
                    }
                }
            }
            // looper records the processed guitar (not the drums), plays back alongside it
            // (pre-fader: the guitar fader is a monitor level, the loop keeps what was played)
            let (pl, pr) = self.looper.tick(l, r);
            let og = self.out_gain.next(out_g) * self.fade;
            self.fade = (self.fade + self.fade_step).min(1.0);
            let gg = self.gtr_fader.next(glev) * og;
            let dg = self.drum_fader.next(dlev) * DRUM_MAKEUP * og;
            let lg = self.loop_fader.next(llev) * og;
            let (gl, gr, ll, lr, xl, xr) = (l * gg, r * gg, pl * lg, pr * lg, dl * dg, dr * dg);
            g_peak = g_peak.max(gl.abs()).max(gr.abs());
            l_peak = l_peak.max(ll.abs()).max(lr.abs());
            d_peak = d_peak.max(xl.abs()).max(xr.abs());
            l = gl + ll + xl;
            r = gr + lr + xr;
            if lim_on {
                (l, r) = self.limiter.tick(l, r);
            }
            out_peak = out_peak.max(l.abs()).max(r.abs());
            out_l[i] = l.clamp(-1.0, 1.0);
            out_r[i] = r.clamp(-1.0, 1.0);
        }
        self.meters.loop_state.store(self.looper.state as u32, Ordering::Relaxed);
        self.meters.loop_pos.store(self.looper.pos as u32, Ordering::Relaxed);
        self.meters.loop_len.store(self.looper.len as u32, Ordering::Relaxed);
        self.meters.drum_step.store(self.drums.step as u32, Ordering::Relaxed);
        let wait = if self.count_in { 2 } else if self.loop_wait.is_some() { 1 } else { 0 };
        self.meters.loop_wait.store(wait, Ordering::Relaxed);
        Meters::raise(&self.meters.in_peak, in_peak);
        Meters::raise(&self.meters.gtr_peak, g_peak);
        Meters::raise(&self.meters.drum_peak, d_peak);
        Meters::raise(&self.meters.loop_peak, l_peak);
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
