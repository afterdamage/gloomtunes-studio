//! Top-level views (panels).

mod analyzer;
mod audio_editor;
mod audio_panel;
mod browser;
mod channel_rack;
mod channel_settings;
mod drum_pads;
mod input_panel;
mod midi_panel;
mod mixer;
mod modulators;
mod performance;
mod piano_roll;
mod player_panel;
mod playlist;
mod plugins;
mod sampler_panel;
mod settings;
mod synth_panel;
mod transport_bar;
mod workflow;

pub use analyzer::{analyzer_panel, AnalyzerState};
pub use audio_editor::{audio_editor, AudioEditor, AudioEditorAction};
pub use audio_panel::{
    audio_panel, cpu_meter, AudioAction, AudioPanelModel, CpuLoad, BUFFER_SIZES, CPU_WARN,
};
pub use browser::{
    browser, parse_tags, BrowserAction, BrowserEntry, BrowserModel, BrowserRow, BrowserTab, RowItem,
};
pub use channel_rack::{channel_rack, rack_groups, RackAction, RackState, RackView};
pub use channel_settings::{channel_settings, layer_panel, ChannelSettingsOut};
pub use drum_pads::{drum_pads, DrumPadOut, DrumPadState};
pub use input_panel::{
    input_menu, input_panel, offered_inputs, InputAction, InputPanelModel, PairMode,
};
pub use midi_panel::{
    midi_panel, AutoMode, LoopPasses, MidiAction, MidiPanelModel, MidiPortRow, MidiRouteRow,
    RecordMode,
};
pub use mixer::{mixer_view, MixerRequest, MixerState, MixerView, StripMeter, STRIP_ICON_GLYPHS};
pub use modulators::{modulators_panel, ModulatorsState};
pub use performance::{grid as performance_rows, performance_grid, PerformanceView};
pub use piano_roll::{
    group_color, key_name, piano_roll, stretch_notes, GhostChannel, Lane, LaneDraw, NoteProp,
    PianoRollAction, PianoRollState, PianoRollView, ScaleKind, Snap, Tool,
};
pub use player_panel::{player_panel, PlayerPanelOut, PlayerPanelView};
pub use playlist::{
    add_automation, playlist, AudioLookup, BounceKind, PlaylistAction, PlaylistSnap, PlaylistState,
    PlaylistTool, PlaylistView,
};
pub use plugins::{
    plugin_browser, plugin_controls, PluginAction, PluginBrowserAction, PluginBrowserState,
    PluginBrowserView, PluginEntry, PluginPanelView, PluginState,
};
pub use sampler_panel::{sampler_panel, SamplerPanelOut, SamplerPanelView};
pub use settings::{
    first_run, perf_panel, privacy_panel, settings_tabs, shortcut_editor, theme_editor,
    FirstRunAction, FirstRunModel, FirstRunState, PerfAction, PerfModel, SettingsPage,
    ShortcutEditorState, ThemeEdit,
};
pub use synth_panel::{scope_trigger, synth_panel, SynthPanelAction, SynthPanelView};
pub use transport_bar::{transport_bar, PlayState, TapTempo, TransportAction, TransportModel};
pub use workflow::{
    help_panel, history_panel, markdown, project_info_panel, shortcut_page, HelpState, HELP_PAGES,
};
