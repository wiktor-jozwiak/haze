//! Parameter store shared between the UI and the audio thread: one `AtomicU32` (f32 bits)
//! per parameter. Writes from the UI are relaxed stores, reads in the callback are relaxed
//! loads; no locks, no allocation.

use std::sync::atomic::{AtomicU32, Ordering};

macro_rules! params {
    ($($(#[$m:meta])* $name:ident = $def:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(usize)]
        pub enum P { $($(#[$m])* $name),* , EqBand0, EqBand1, EqBand2, EqBand3, EqBand4, EqBand5, EqBand6, EqBand7, EqBand8, EqBand9 }
        const DEFAULTS: &[f32] = &[$($def),* , 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    };
}

params! {
    // source: 0 = interface input, 1 = built-in DI loop / loaded .wav
    Source = 0.0,
    InputGainDb = 0.0,
    GateOn = 1.0,
    GateThreshDb = -62.0,
    DriveOn = 0.0,
    /// 0 T-Scream, 1 Klon, 2 Blues Driver, 3 RAT, 4 Muff, 5 Metal Zone
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
    EqOn = 0.0,
    ModOn = 0.0,
    /// 0 Chorus, 1 Flanger, 2 Phaser, 3 Tremolo, 4 Rotary
    ModType = 0.0,
    ModRate = 0.8,
    ModDepth = 0.5,
    ModMix = 0.5,
    DelayOn = 0.0,
    DelayMs = 380.0,
    DelayFeedback = 0.35,
    DelayMix = 0.3,
    DelayPingPong = 1.0,
    ReverbOn = 0.0,
    /// 0 Plate, 1 Hall, 2 Spring, 3 Shimmer
    ReverbType = 0.0,
    ReverbDecay = 0.5,
    ReverbMix = 0.25,
    Limiter = 1.0,
    // headroom: first start plays the DI loop through speakers
    OutGainDb = -6.0,
}

pub const COUNT: usize = P::EqBand9 as usize + 1;
/// ISO centre frequencies of the 10-band EQ.
pub const EQ_FREQS: [f32; 10] = [31.0, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];

pub struct Params {
    v: [AtomicU32; COUNT],
}

impl Default for Params {
    fn default() -> Self {
        Params { v: std::array::from_fn(|i| AtomicU32::new(DEFAULTS[i].to_bits())) }
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
    pub fn eq_band(i: usize) -> P {
        // SAFETY-free mapping: bands are the last ten variants in order
        [P::EqBand0, P::EqBand1, P::EqBand2, P::EqBand3, P::EqBand4, P::EqBand5, P::EqBand6, P::EqBand7, P::EqBand8, P::EqBand9][i]
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
    /// DSP load of the last callback, 0..1 of the buffer duration
    pub load: AtomicU32,
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
