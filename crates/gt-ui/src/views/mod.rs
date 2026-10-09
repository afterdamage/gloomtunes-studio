//! Top-level views (panels).

mod audio_panel;
mod browser;
mod channel_rack;
mod midi_panel;
mod mixer;
mod modulators;
mod piano_roll;
mod playlist;
mod plugins;
mod sampler_panel;
mod settings;
mod synth_panel;
mod transport_bar;

pub use audio_panel::{
    audio_panel, cpu_meter, AudioAction, AudioPanelModel, CpuLoad, BUFFER_SIZES, CPU_WARN,
};
pub use browser::{browser, BrowserAction, BrowserEntry, BrowserModel};
pub use channel_rack::{channel_rack, RackAction, RackState, RackView};
pub use midi_panel::{midi_panel, MidiAction, MidiPanelModel, MidiPortRow};
pub use mixer::{mixer_view, MixerState, MixerView, StripMeter};
pub use modulators::{modulators_panel, ModulatorsState};
pub use piano_roll::{
    key_name, piano_roll, PianoRollAction, PianoRollState, PianoRollView, ScaleKind, Snap, Tool,
};
pub use playlist::{
    add_automation, playlist, AudioLookup, PlaylistAction, PlaylistSnap, PlaylistState,
    PlaylistTool, PlaylistView,
};
pub use plugins::{
    plugin_browser, plugin_controls, PluginAction, PluginBrowserAction, PluginBrowserState,
    PluginBrowserView, PluginEntry, PluginPanelView, PluginState,
};
pub use sampler_panel::{sampler_panel, SamplerPanelView};
pub use settings::{
    first_run, perf_panel, privacy_panel, settings_tabs, shortcut_editor, theme_editor,
    FirstRunAction, FirstRunModel, FirstRunState, PerfAction, PerfModel, SettingsPage,
    ShortcutEditorState, ThemeEdit,
};
pub use synth_panel::{scope_trigger, synth_panel, SynthPanelAction, SynthPanelView};
pub use transport_bar::{transport_bar, PlayState, TransportAction, TransportModel};
