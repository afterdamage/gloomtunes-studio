//! Top-level views (panels).

mod audio_panel;
mod transport_bar;

pub use audio_panel::{audio_panel, AudioAction, AudioPanelModel, BUFFER_SIZES};
pub use transport_bar::{transport_bar, PlayState, TransportAction, TransportModel};
