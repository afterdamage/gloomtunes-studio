//! Top-level views (panels).

mod audio_panel;
mod browser;
mod channel_rack;
mod mixer;
mod piano_roll;
mod sampler_panel;
mod synth_panel;
mod transport_bar;

pub use audio_panel::{audio_panel, AudioAction, AudioPanelModel, BUFFER_SIZES};
pub use browser::{browser, BrowserAction, BrowserEntry, BrowserModel};
pub use channel_rack::{channel_rack, RackAction, RackState, RackView};
pub use mixer::{mixer_view, MixerState, MixerView, StripMeter};
pub use piano_roll::{
    key_name, piano_roll, PianoRollAction, PianoRollState, PianoRollView, ScaleKind, Snap, Tool,
};
pub use sampler_panel::{sampler_panel, SamplerPanelView};
pub use synth_panel::{scope_trigger, synth_panel, SynthPanelAction, SynthPanelView};
pub use transport_bar::{transport_bar, PlayState, TransportAction, TransportModel};
