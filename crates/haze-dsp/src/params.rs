//! Parameter store shared between the UI and the audio thread: one `AtomicU32` (f32 bits)
//! per parameter. Writes from the UI are relaxed stores, reads in the callback are relaxed
//! loads; no locks, no allocation.

use std::sync::atomic::{AtomicU32, Ordering};

macro_rules! params {
    ($($(#[$m:meta])* $name:ident = $def:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        pub enum P { $($(#[$m])* $name),* }
        const DEFAULTS: &[f32] = &[$($def),*];
    };
}

params! {
    // source: 0 = interface input, 1 = built-in DI loop / loaded .wav
    Source = 0.0,
    InputGainDb = 0.0,
    /// level the interface input to the guitar level the amp models are voiced for (peaks
    /// about −9 dBFS): a quiet interface otherwise leaves clean tones far too soft
    InputAuto = 1.0,
    /// mains hum notch on the interface input: 50 / 60 Hz, 0 = off
    HumHz = 50.0,
    GateOn = 1.0,
    GateThreshDb = -62.0,
    DriveOn = 0.0,
    /// `blocks::DRIVE_MODELS`: 0 T-Scream, 1 Klon, 2 Blues Driver, 3 RAT, 4 Muff, 5 Metal Zone,
    /// 6 clean boost, 7 mid boost, 8 treble boost
    DriveModel = 0.0,
    DriveAmt = 0.5,
    DriveTone = 0.5,
    DriveLevel = 0.5,
    /// 0 Acoustic, 1 Clean, 2 Pushed, 3 Crunch, 4 Lead, 5 Brown, 6 Modern Metal
    AmpVoicing = 1.0,
    AmpGain = 0.5,
    AmpBass = 0.5,
    AmpMid = 0.5,
    AmpTreble = 0.5,
    AmpPresence = 0.5,
    AmpResonance = 0.5,
    AmpMaster = 0.5,
    AmpSag = 0.3,
    CabOn = 1.0,
    /// 0 1x12, 1 2x12, 2 4x12, 3 user .wav
    CabIr = 2.0,
    /// compressor / limiter (the patch's COMP or LIMITER effect); levels in dBFS
    CompOn = 0.0,
    /// 1 = before the amp (MOD/FX slot ahead of the amp in the chain pattern)
    CompPre = 1.0,
    CompThreshDb = -24.0,
    CompRatio = 4.0,
    CompAttackMs = 10.0,
    CompReleaseMs = 150.0,
    CompMakeupDb = 0.0,
    /// high-shelf tilt of the compressed signal, dB
    CompToneDb = 0.0,
    ModOn = 0.0,
    /// modulation before the amp (mono) instead of after the cab (stereo)
    ModPre = 0.0,
    /// 0 Chorus, 1 Flanger, 2 Phaser, 3 Tremolo, 4 Rotary
    ModType = 0.0,
    ModRate = 0.8,
    ModDepth = 0.5,
    ModMix = 0.5,
    // DELAY and DELAY 2, in series. Types are the Katana's codes (`fx::DELAY_TYPES`); levels
    // are linear with the Katana's 100 = unity, so EFFECT LEVEL 55 is a wet gain of 0.55.
    DelayOn = 0.0,
    DelayType = 0.0,
    DelayMs = 380.0,
    /// 0..1 of the Katana's FEEDBACK range (1 ≈ endless repeats)
    DelayFeedback = 0.35,
    DelayMix = 0.3,
    DelayDirect = 1.0,
    /// feedback-path high cut in Hz, 0 = flat
    DelayHiCut = 0.0,
    /// MODULATE type: LFO rate in Hz and depth 0..1
    DelayModRate = 1.0,
    DelayModDepth = 0.5,
    Delay2On = 0.0,
    Delay2Type = 0.0,
    Delay2Ms = 500.0,
    Delay2Feedback = 0.3,
    Delay2Mix = 0.3,
    Delay2Direct = 1.0,
    Delay2HiCut = 0.0,
    Delay2ModRate = 1.0,
    Delay2ModDepth = 0.5,
    ReverbOn = 0.0,
    /// Katana code (`fx::REVERB_TYPES`): 1 Room, 3 Hall, 4 Plate, 5 Spring, 6 Modulate
    ReverbType = 4.0,
    /// RT60 in seconds (the Katana's REVERB TIME 1..100 is 0.1..10 s)
    ReverbTime = 2.0,
    ReverbPreMs = 20.0,
    /// cut filters on the reverb sound, Hz, 0 = flat
    ReverbLowCut = 0.0,
    ReverbHighCut = 0.0,
    /// 0..1: input diffusion (the Katana's DENSITY 0..10)
    ReverbDensity = 0.8,
    ReverbMix = 0.25,
    ReverbDirect = 1.0,
    Limiter = 1.0,
    // headroom: the amp models put out −7 dBFS RMS with peaks past full scale; at −6 dB the
    // limiter flattened every pick attack and powered monitors farted. −18 dB leaves the
    // limiter as a safety net only (≈ −19 dBFS RMS); loudness is the speakers' knob.
    OutGainDb = -18.0,
    /// silence the output (tuner MUTE), smoothed like any gain change
    Mute = 0.0,
    /// the guitar's note as the tuner hears it (fractional MIDI), −1 = no pitch; fed by the
    /// UI at 15 Hz for the pitch-aware effects (harmonist, intelligent ring mod)
    PitchMidi = -1.0,
    // PC-only effects (`pcfx`), after the patch's own chain. Rhythmic ones follow `Bpm`.
    /// harmonizer in key: voices in scale steps (V2 = 0: off), levels 0..1
    PcHarmOn = 0.0,
    PcHarmKey = 4.0,
    PcHarmMinor = 1.0,
    PcHarmV1 = 2.0,
    PcHarmV2 = 0.0,
    PcHarmMix = 0.6,
    /// rhythm: 0 multi-tap delay, 1 slicer, 2 tremolo; division index into `pcfx::DIVS`;
    /// amount = feedback (delay), duty (slicer) or depth (tremolo)
    PcRhyOn = 0.0,
    PcRhyMode = 0.0,
    PcRhyDiv = 2.0,
    PcRhyMix = 0.5,
    PcRhyAmt = 0.5,
    /// shimmer: a hall whose tail is pitched up and fed back; decay in s, pitch in semitones
    PcShimOn = 0.0,
    PcShimMix = 0.4,
    PcShimDecay = 6.0,
    PcShimAmt = 0.5,
    PcShimPitch = 12.0,
    /// freeze: on = hold the last moment as an endless pad; texture = grain length 0..1
    PcFrzOn = 0.0,
    PcFrzMix = 0.7,
    PcFrzTexture = 0.5,
    /// lo-fi: bit depth, sample rate Hz, wow / hiss 0..1, wet mix
    PcLofiOn = 0.0,
    PcLofiBits = 8.0,
    PcLofiRate = 11025.0,
    PcLofiWow = 0.4,
    PcLofiHiss = 0.3,
    PcLofiMix = 1.0,
    /// tape stop: on = the tape winds down over `PcStopTime` s and stays stopped
    PcStopOn = 0.0,
    PcStopTime = 1.0,
    // practice suite
    Bpm = 100.0,
    DrumOn = 0.0,
    /// index into `practice::PATTERNS` (0 Metronome, 1 Rock, 2 Pop, 3 Funk, 4 Shuffle,
    /// 5 Half-time, 6 Metal, 7 6/8)
    DrumPattern = 0.0,
    DrumLevel = 0.5,
    LoopLevel = 0.8,
    /// metronome time signature: 0 4/4, 1 3/4, 2 6/8
    TimeSig = 0.0,
    /// drums off: one bar of clicks before the looper starts recording
    CountIn = 1.0,
    /// drums on: looper record start / loop close wait for the next bar line
    LoopSync = 1.0,
    /// speed trainer: +TrainerStep BPM every TrainerBars bars, up to TrainerTarget
    TrainerOn = 0.0,
    TrainerStep = 5.0,
    TrainerBars = 4.0,
    // practice mixer: linear gains of the monitored guitar and the kit's mix groups
    // (`practice::KIT_GROUPS`); the drum bus is `DrumLevel`, the looper `LoopLevel`
    GuitarLevel = 1.0,
    DrumKickLv = 1.0,
    DrumSnareLv = 1.0,
    DrumHatLv = 1.0,
    DrumCymLv = 1.0,
    DrumTomLv = 1.0,
    DrumClickLv = 1.0,
    /// kit feel (`practice::Kit`): tune in semitones, decay multiplier, humanise 0..1,
    /// fills 0 off / 1 every 8 / 2 every 4 / 3 every 2 bars, extra swing 0..1
    DrumTune = 0.0,
    DrumDecay = 1.0,
    DrumHuman = 0.3,
    DrumFills = 2.0,
    DrumSwing = 0.0,
    /// user groove (`DrumPattern` = `practice::USER`): 12 or 16 steps, and one packed row per
    /// `practice::USER_ROWS` voice, 2 bits per step. Rows are raw bits: `get_raw` / `set_raw`.
    DrumUserLen = 16.0,
    DrumUser0 = 0.0,
    DrumUser1 = 0.0,
    DrumUser2 = 0.0,
    DrumUser3 = 0.0,
    DrumUser4 = 0.0,
    DrumUser5 = 0.0,
    DrumUser6 = 0.0,
    DrumUser7 = 0.0,
    TrainerTarget = 160.0,
}

const NAMED: usize = P::TrainerTarget as usize + 1;
const _: () = assert!(DEFAULTS.len() == NAMED, "TrainerTarget must stay the last named parameter");

/// EQ slots after the named parameters, addressed through `Params::eq`: 0 = EQ, 1 = EQ 2,
/// 2 = a PARAMETRIC / GRAPHIC EQ in the MOD or FX slot. Each holds `EqKey` values.
pub const EQ_SLOTS: usize = 3;
/// Values of one EQ slot: on, pre-amp, kind, then 11 kind-specific values (`EqKey::V`).
pub const EQ_VALS: usize = 14;
/// Effect units after the EQ slots, addressed through `Params::fx`: 0 = the MOD slot, 1 = FX.
/// Each holds On, Pre, Kind (`fxunit::FX_KINDS`) and 10 kind-specific values (`EqKey::V`).
pub const FX_SLOTS: usize = 2;
pub const FX_VALS: usize = 13;
pub const COUNT: usize = NAMED + EQ_SLOTS * EQ_VALS + FX_SLOTS * FX_VALS;

/// One value of an EQ slot.
///
/// Parametric (kind 0): V(0) low cut Hz (0 = flat), V(1) low gain dB, V(2..=4) low-mid
/// freq / Q / gain, V(5..=7) high-mid freq / Q / gain, V(8) high gain dB, V(9) high cut Hz
/// (0 = flat), V(10) level dB. Graphic (kind 1): V(0..=9) the GE-10 bands in dB, V(10) level.
#[derive(Clone, Copy, Debug)]
pub enum EqKey {
    On,
    Pre,
    Kind,
    V(usize),
}

pub struct Params {
    v: [AtomicU32; COUNT],
}

impl Default for Params {
    fn default() -> Self {
        Params { v: std::array::from_fn(|i| AtomicU32::new(DEFAULTS.get(i).copied().unwrap_or(0.0).to_bits())) }
    }
}

impl Params {
    #[inline]
    pub fn get(&self, p: P) -> f32 {
        f32::from_bits(self.v[p as usize].load(Ordering::Relaxed))
    }
    #[inline]
    pub fn on(&self, p: P) -> bool {
        self.get(p) >= 0.5
    }
    pub fn set(&self, p: P, x: f32) {
        self.v[p as usize].store(x.to_bits(), Ordering::Relaxed);
    }
    /// Index of an EQ slot value, for `get_i` / `set_i`.
    pub fn eq(slot: usize, k: EqKey) -> usize {
        let o = match k {
            EqKey::On => 0,
            EqKey::Pre => 1,
            EqKey::Kind => 2,
            EqKey::V(i) => 3 + i.min(EQ_VALS - 4),
        };
        NAMED + slot.min(EQ_SLOTS - 1) * EQ_VALS + o
    }
    /// Index of an effect unit value (same keys as an EQ slot), for `get_i` / `set_i`.
    pub fn fx(slot: usize, k: EqKey) -> usize {
        let o = match k {
            EqKey::On => 0,
            EqKey::Pre => 1,
            EqKey::Kind => 2,
            EqKey::V(i) => 3 + i.min(FX_VALS - 4),
        };
        NAMED + EQ_SLOTS * EQ_VALS + slot.min(FX_SLOTS - 1) * FX_VALS + o
    }
    /// Raw 32 bits of a parameter that holds a bit field (the user groove rows).
    #[inline]
    pub fn get_raw(&self, p: P, offset: usize) -> u32 {
        self.v[p as usize + offset].load(Ordering::Relaxed)
    }
    pub fn set_raw(&self, p: P, offset: usize, bits: u32) {
        self.v[p as usize + offset].store(bits, Ordering::Relaxed);
    }
    #[inline]
    pub fn get_i(&self, i: usize) -> f32 {
        f32::from_bits(self.v[i].load(Ordering::Relaxed))
    }
    pub fn set_i(&self, i: usize, x: f32) {
        self.v[i].store(x.to_bits(), Ordering::Relaxed);
    }
}

/// Values the audio thread publishes for the UI (meters).
#[derive(Default)]
pub struct Meters {
    pub in_peak: AtomicU32,
    pub out_peak: AtomicU32,
    pub in_clip: AtomicU32,
    pub out_clip: AtomicU32,
    /// callbacks that took longer than their buffer duration
    pub xruns: AtomicU32,
    /// input ring ran dry (output re-buffers: one short gap) / dropped surplus (clock drift)
    pub ring_underruns: AtomicU32,
    pub ring_skips: AtomicU32,
    /// errors reported by the capture stream (overruns, device trouble)
    pub in_errors: AtomicU32,
    /// input channel the guitar was found on (0 = first)
    pub in_channel: AtomicU32,
    /// DSP load of the last callback, 0..1 of the buffer duration
    pub load: AtomicU32,
    /// looper: `practice::LoopState` as u32, play position and length in frames
    pub loop_state: AtomicU32,
    pub loop_pos: AtomicU32,
    pub loop_len: AtomicU32,
    /// current 16th of the drum bar (0..15)
    pub drum_step: AtomicU32,
    /// looper request on hold: 0 none, 1 waiting for the bar line, 2 counting in
    pub loop_wait: AtomicU32,
    /// practice mixer peaks at the output (after the faders and the output gain), f32 bits;
    /// read resets them like `in_peak`
    pub gtr_peak: AtomicU32,
    pub drum_peak: AtomicU32,
    pub loop_peak: AtomicU32,
    /// scheduling the audio threads got: bit 0 output, bit 1 capture is SCHED_FIFO; bit 2 /
    /// bit 3 the same thread fell back to nice −11 (0 = nothing yet, or plain timesharing)
    pub rt: AtomicU32,
}

impl Meters {
    /// Peak since the last read (read resets it).
    pub fn take_peak(a: &AtomicU32) -> f32 {
        f32::from_bits(a.swap(0, Ordering::Relaxed))
    }
    #[inline]
    pub fn raise(a: &AtomicU32, x: f32) {
        let cur = f32::from_bits(a.load(Ordering::Relaxed));
        if x > cur {
            a.store(x.to_bits(), Ordering::Relaxed);
        }
    }
}
