//! haze-dsp — standalone guitar amp + multi-FX engine.
//!
//! IN → GATE → [COMP · EQ · MOD pre] → DRIVE → AMP → CAB/IR → [COMP · EQ post] → MOD →
//! DELAY → DELAY 2 → REVERB → LIMITER → OUT
//!
//! Real-time rules: the audio callback never allocates, never locks, never frees. Parameters
//! are atomics (`params`), large objects travel through SPSC rings (`chain::Cmd` / `Trash`).

pub mod blocks;
pub mod chain;
pub mod conv;
pub mod di;
pub mod engine;
pub mod filters;
pub mod fx;
pub mod fxunit;
pub mod pcfx;
pub mod params;
pub mod practice;
pub mod tonestack;
pub mod tuner;

pub use chain::{render_offline, write_wav, Chain, Cmd};
pub use engine::{list_devices, Config, DeviceInfo, Engine, Info};
pub use params::{EqKey, Meters, Params, P};
pub use tuner::{Pitch, Tuner};
pub use practice::{LoopAction, LoopState};
