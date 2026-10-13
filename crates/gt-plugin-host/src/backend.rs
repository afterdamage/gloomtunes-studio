//! What the plugin host needs from a loaded plugin, whatever its format (CLAP or VST3, D113).
//!
//! The manager keeps instances as `Box<dyn Instance>` and plugin files as [`Entry`], so
//! loading, parameter sync, restarts, editors, state and export work the same for both
//! formats.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clack_host::prelude::PluginEntry;
use gt_core::{PluginFormat, PluginParamInfo};
use gt_engine::PluginProcessor;

use crate::catalog::PluginInfo;
use crate::host::Signals;
use crate::presets::{Preset, Source};
use crate::processor::ParamOut;

/// A loaded plugin file.
pub(crate) enum Entry {
    Clap(PluginEntry),
    Vst3(Rc<crate::vst3::Module>),
}

impl Entry {
    /// Loads a plugin file (runs its initialization code).
    pub(crate) fn load(path: &Path, format: PluginFormat) -> Result<Self, String> {
        match format {
            PluginFormat::Clap => {
                // SAFETY: loading runs the plugin file's initialization code; the caller
                // guards the call so a crash quarantines the file.
                #[allow(unsafe_code)]
                let entry = unsafe { PluginEntry::load(path) };
                entry.map(Self::Clap).map_err(|e| e.to_string())
            }
            PluginFormat::Vst3 => crate::vst3::Module::load(path).map(|m| Self::Vst3(Rc::new(m))),
        }
    }

    /// Creates an instance of `info`.
    pub(crate) fn create(
        &self,
        info: &PluginInfo,
        signals: Arc<Signals>,
    ) -> Result<Box<dyn Instance>, String> {
        match self {
            Self::Clap(e) => Ok(Box::new(crate::instance::LoadedPlugin::create(
                e, info, signals,
            )?)),
            Self::Vst3(m) => Ok(Box::new(crate::vst3::Vst3Plugin::create(m, info, signals)?)),
        }
    }
}

/// The processor an activation gives, with the queue of parameter changes the plugin makes
/// while processing.
pub(crate) type Activated = (Box<dyn PluginProcessor>, rtrb::Consumer<ParamOut>);

/// A plugin instance on the main thread.
pub(crate) trait Instance {
    fn info(&self) -> &PluginInfo;
    fn signals(&self) -> &Arc<Signals>;
    /// Reads the parameter list (descriptions and the mapping for the processor) and the
    /// current values, normalized.
    fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>);
    /// Names of the stereo outputs after the main one, and whether the plugin sends MIDI.
    fn outputs(&mut self) -> (Vec<String>, bool) {
        (Vec::new(), false)
    }
    /// Parameter `index` at `value` (normalized) as the plugin writes it, with its unit
    /// ("-6.0 dB", "Saw"); None if the plugin does not say.
    fn value_text(&mut self, _index: usize, _value: f32) -> Option<String> {
        None
    }
    /// The plugin's state, if it can save one.
    fn save_state(&mut self) -> Option<Vec<u8>>;
    /// Restores a saved state.
    fn load_state(&mut self, state: &[u8]) -> Result<(), String>;
    /// Switches between real-time and offline rendering (export).
    fn set_offline(&mut self, offline: bool);
    /// Activates for `sample_rate` and blocks of up to `RENDER_QUANTUM` frames.
    fn activate(&mut self, sample_rate: u32) -> Result<Activated, String>;
    /// Deactivates, once the engine has handed the processor back (false if it has not yet).
    fn try_deactivate(&mut self) -> bool;
    /// Runs main-thread requests: callbacks, due timers, ready file descriptors.
    fn main_thread_work(&mut self, now: Instant);
    /// How soon main-thread work wants another frame.
    fn next_frame(&self, now: Instant) -> Option<Duration>;
    /// The host changed parameter `index` to `value` (normalized): plugins whose editor is
    /// separate from their processor (VST3) hear about it here; CLAP plugins get it with the
    /// next block.
    fn host_changed(&mut self, _index: usize, _value: f32) {}
    /// The processor reported parameter `index` at `value` (VST3 forwards it to its editor).
    fn plugin_reported(&mut self, _index: usize, _value: f32) {}
    /// The plugin's own (factory) presets.
    fn factory_presets(&mut self) -> Vec<Preset> {
        Vec::new()
    }
    /// Loads one of them (the host reads the parameters again afterwards).
    fn load_factory(&mut self, _source: &Source) -> Result<(), String> {
        Err(format!("{} cannot load presets", self.info().name))
    }
    /// Parameter changes made in the plugin's editor that have not reached its processor yet
    /// (VST3's `performEdit`): index and normalized value.
    fn take_edits(&mut self) -> Vec<(usize, f32)> {
        Vec::new()
    }

    // Editors.
    fn has_editor(&self) -> bool;
    fn editor_open(&self) -> bool;
    /// Opens the editor, or raises it when open.
    fn open_editor(&mut self, title: &str) -> Result<(), String>;
    fn close_editor(&mut self);
    /// Handles editor window events and requests (resizes, close).
    fn poll_editor(&mut self);
}
