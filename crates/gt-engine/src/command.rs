//! Messages between the UI thread and the audio thread (ARCHITECTURE.md §4).

use std::sync::Arc;

use gt_core::{SampleData, TempoMap, Tick, TimeSigMap};

use crate::control::ModPlan;
use crate::mixer::{EffectBox, MixerParams};
use crate::plugin::PluginBox;
use crate::song::{ChannelParams, SongSnapshot};

/// Loop region in ticks. Playback wraps from `end` back to `start` while `enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRegion {
    /// First tick inside the loop.
    pub start: Tick,
    /// First tick after the loop.
    pub end: Tick,
    /// Whether playback wraps.
    pub enabled: bool,
}

impl Default for LoopRegion {
    fn default() -> Self {
        Self {
            start: Tick(0),
            end: Tick(4 * gt_core::PPQ * 4),
            enabled: false,
        }
    }
}

/// UI → engine. Applied at the start of the next render quantum.
///
/// Every variant is small (checked at compile time below); large payloads travel boxed, with the
/// allocation done on the UI thread and the old value returned through the garbage queue.
#[derive(Debug)]
pub enum EngineCommand {
    /// Start playing from the current position (or resume from pause).
    Play,
    /// Stop and hold the current position.
    Pause,
    /// Stop and return to where playback last started; when already stopped, return to bar 1.
    Stop,
    /// Move the playhead.
    Locate(Tick),
    /// Set the loop region. Regions shorter than a 1/16 note are treated as disabled.
    SetLoop(LoopRegion),
    /// Replace the tempo map. The playhead keeps its musical position.
    SetTempoMap(Box<TempoMap>),
    /// Replace the time signatures (used for beats, bars and the metronome).
    SetSignatures(Box<TimeSigMap>),
    /// Metronome clicks while playing.
    SetMetronome(bool),
    /// The 440 Hz device test tone from Step 1.
    SetTestTone(bool),
    /// Fade the whole output to silence (before closing the device). Telemetry `silent` turns
    /// true once done. `FadeIn` reverses it.
    FadeOut,
    /// Undo `FadeOut`.
    FadeIn,
    /// Replace what plays (a pattern, or the arrangement). Held notes are released and
    /// automation values are dropped (the new song's lanes set them again while playing).
    SetSong(Box<SongSnapshot>),
    /// Replace the song without releasing playing notes or resetting automation, for edits
    /// that only add notes (live recording). Notes the old song started still end, because
    /// the new song has the same note-offs.
    UpdateSong(Box<SongSnapshot>),
    /// Replace the modulators. Running LFOs and followers keep their state (by modulator id).
    SetModulation(Box<ModPlan>),
    /// Replace a channel's settings. Gain and pan glide; the rest applies to new notes.
    SetChannelParams {
        /// Channel slot (rack index), below `MAX_CHANNELS`.
        slot: u16,
        /// New settings (the box comes back as garbage).
        params: Box<ChannelParams>,
    },
    /// Choose the channel whose output feeds the oscilloscope buffer in [`crate::Telemetry`]
    /// (none: the buffer is left alone).
    SetScopeChannel(Option<u16>),
    /// Replace a channel's sample. Its playing voices stop.
    SetChannelSample {
        /// Channel slot.
        slot: u16,
        /// The new sample, or none for silence. The old one comes back as garbage.
        sample: Option<Arc<SampleData>>,
    },
    /// Start a note on a channel now (auditioning from the UI).
    NoteOn {
        /// Channel slot.
        slot: u16,
        /// MIDI key.
        key: u8,
        /// Velocity, 0 to 1.
        velocity: f32,
    },
    /// Release a note started with `NoteOn`.
    NoteOff {
        /// Channel slot.
        slot: u16,
        /// MIDI key.
        key: u8,
    },
    /// Play a sample once through the preview voice (sample browser), or stop it with `None`.
    PreviewSample(Option<Arc<SampleData>>),
    /// Replace the mixer settings (faders, pans, routing, processing order, effect bypass).
    /// Levels glide; effects keep their state.
    SetMixer(Box<MixerParams>),
    /// Put an effect into a mixer slot, or empty it. The previous effect comes back as garbage.
    SetEffect {
        /// Strip index.
        strip: u8,
        /// Slot index, below `FX_SLOTS`.
        slot: u8,
        /// The new effect, built with [`crate::create_effect`].
        effect: Option<EffectBox>,
    },
    /// Change one effect parameter (plain value; the effect smooths it).
    SetEffectParam {
        /// Strip index.
        strip: u8,
        /// Slot index.
        slot: u8,
        /// Parameter index in the effect's table.
        index: u8,
        /// New value.
        value: f32,
    },
    /// The channel slot that live notes ([`crate::LiveNote`]) play, or none to mute them.
    /// Notes already held finish on the slot they started on.
    SetLiveChannel(Option<u16>),
    /// Put a plugin into the plugin table (or empty an entry). Channels and effect slots that
    /// name its instance start using it. The previous one comes back as garbage, stopped.
    SetPlugin {
        /// Table entry, below [`crate::MAX_PLUGINS`].
        index: u8,
        /// The plugin.
        plugin: Option<Box<PluginBox>>,
    },
    /// Change one plugin parameter (normalized; the plugin gets it at the next block).
    SetPluginParam {
        /// Table entry.
        index: u8,
        /// Position in the plugin's parameter list.
        param: u32,
        /// Normalized value.
        value: f32,
    },
    /// Run a plugin that was bypassed after a failure again (it is reset first).
    RetryPlugin(u8),
    /// Count `bars` bars of metronome clicks at the playhead's tempo and time signature, then
    /// start playing. Play, Pause and Stop cancel a count-in. Ignored while playing.
    CountIn {
        /// Bars to count; 0 plays at once.
        bars: u8,
    },
}

impl EngineCommand {
    /// True if applying this command hands something back through the garbage queue, so the
    /// engine must only take it when the queue has room.
    pub(crate) fn returns_garbage(&self) -> bool {
        matches!(
            self,
            Self::SetTempoMap(_)
                | Self::SetSignatures(_)
                | Self::SetSong(_)
                | Self::UpdateSong(_)
                | Self::SetModulation(_)
                | Self::SetChannelParams { .. }
                | Self::SetChannelSample { .. }
                | Self::PreviewSample(_)
                | Self::SetMixer(_)
                | Self::SetEffect { .. }
                | Self::SetPlugin { .. }
        )
    }
}

// Keep commands cheap to copy through the queue.
const _: () = assert!(core::mem::size_of::<EngineCommand>() <= 32);

/// Transport state as seen by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum TransportState {
    /// Not playing; position is where Stop put it.
    #[default]
    Stopped = 0,
    /// Playing.
    Playing = 1,
    /// Not playing; position held.
    Paused = 2,
}

impl TransportState {
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Playing,
            2 => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// Engine → UI notifications. Plain `Copy` data, so the audio thread never frees anything when
/// the queue is full and an event has to be dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    /// The transport changed state or jumped.
    TransportChanged {
        /// New state.
        state: TransportState,
        /// Position in ticks at the change.
        position: Tick,
    },
    /// A live note (key down or up) arrived while the transport was playing, for recording.
    LiveNote {
        /// MIDI key.
        key: u8,
        /// Velocity from 0 to 1; 0 for a key going up.
        velocity: f32,
        /// Song position where the engine played it (the start of its quantum), in ticks.
        tick: f64,
    },
}

/// Things the engine is done with, handed back so they are freed on the UI thread.
#[derive(Debug)]
pub enum Garbage {
    /// A replaced tempo map.
    TempoMap(Box<TempoMap>),
    /// Replaced time signatures.
    Signatures(Box<TimeSigMap>),
    /// A replaced song.
    Song(Box<SongSnapshot>),
    /// A replaced (or empty) modulation plan.
    Modulation(Box<ModPlan>),
    /// A channel-settings box, already copied into the channel.
    Params(Box<ChannelParams>),
    /// A sample no longer used by a channel or the preview voice.
    Sample(Arc<SampleData>),
    /// A mixer-settings box, already copied into the mixer.
    Mixer(Box<MixerParams>),
    /// An effect taken out of a slot.
    Effect(EffectBox),
    /// A plugin taken out of the plugin table (already stopped).
    Plugin(Box<PluginBox>),
}
