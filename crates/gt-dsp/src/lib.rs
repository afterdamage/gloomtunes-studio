//! Pure DSP building blocks for GloomTunes Studio.
//!
//! Nothing in this crate allocates after construction, performs I/O, locks or panics on valid
//! input. Every type is safe to use on the audio thread once it has been created. The drum
//! generators in [`drums`] allocate and are meant for loader threads.

#![forbid(unsafe_code)]

mod adsr;
pub mod analysis;
pub mod audio_edit;
pub mod audio_to_midi;
mod blep;
mod click;
pub mod denoise;
pub mod drums;
pub mod fx;
mod gain;
pub mod interp;
mod ladder;
pub mod latency;
mod lfo;
mod note;
pub mod onsets;
mod osc;
mod ramp;
mod sampler;
pub mod slicer;
pub mod stretch;
pub mod synth;

pub use adsr::{Adsr, AdsrParams};
pub use blep::{blep_sample, Phase, Wave};
pub use click::Click;
pub use fx::{Effect, FxContext};
pub use gain::{db_to_gain, gain_to_db};
pub use ladder::{ladder_g, ladder_k, ladder_response, Ladder};
pub use lfo::{Lfo, LfoWave, Noise};
pub use note::{Glide, NoteExpr};
pub use osc::SineOsc;
pub use ramp::LinearRamp;
pub use sampler::{LfoDest, SamplerVoice, VoiceLfo, VoiceRegion, VoiceSettings};
pub use synth::{Expression, GloomSynth, SynthSettings};
