//! Pure DSP building blocks for GloomTunes Studio.
//!
//! Nothing in this crate allocates after construction, performs I/O, locks or panics on valid
//! input. Every type is safe to use on the audio thread once it has been created.

#![forbid(unsafe_code)]

mod gain;
mod osc;
mod ramp;

pub use gain::{db_to_gain, gain_to_db};
pub use osc::SineOsc;
pub use ramp::LinearRamp;
