//! Pure DSP building blocks for GloomTunes Studio.
//!
//! Nothing in this crate allocates after construction, performs I/O, locks or panics on valid
//! input. Every type is safe to use on the audio thread once it has been created. The drum
//! generators in [`drums`] allocate and are meant for loader threads.

#![forbid(unsafe_code)]

mod adsr;
mod click;
pub mod drums;
mod gain;
mod osc;
mod ramp;
mod sampler;

pub use adsr::{Adsr, AdsrParams};
pub use click::Click;
pub use gain::{db_to_gain, gain_to_db};
pub use osc::SineOsc;
pub use ramp::LinearRamp;
pub use sampler::{SamplerVoice, VoiceRegion};
