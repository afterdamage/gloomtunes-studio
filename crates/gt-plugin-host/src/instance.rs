//! One loaded plugin instance on the main thread: creating it, reading its parameters, saving
//! and restoring its state, and activating it to get a processor for the engine.

use std::ffi::CString;
use std::sync::Arc;

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::PluginGui;
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::render::{PluginRender, RenderMode};
use clack_extensions::state::PluginState;
use clack_host::prelude::*;
use gt_core::{PluginKind, PluginParamInfo, MAX_PLUGIN_PARAMS};
use gt_engine::RENDER_QUANTUM;

use crate::catalog::PluginInfo;
use crate::host::{GtHost, MainThread, Shared, Signals};
use crate::processor::{ClapProcessor, Layout, ParamMap, ParamOut, OUT_QUEUE};

/// The host's name as plugins see it.
pub(crate) fn host_info() -> HostInfo {
    HostInfo::new(
        "GloomTunes Studio",
        "Vasil Vasilev",
        "https://github.com/afterdamage/gloomtunes-studio",
        env!("CARGO_PKG_VERSION"),
    )
    .expect("no NUL bytes in the host info")
}

/// A created plugin instance with its extensions.
pub(crate) struct LoadedPlugin {
    pub(crate) instance: PluginInstance<GtHost>,
    pub(crate) signals: Arc<Signals>,
    pub(crate) info: PluginInfo,
    params_ext: Option<PluginParams>,
    state_ext: Option<PluginState>,
    pub(crate) gui_ext: Option<PluginGui>,
    latency_ext: Option<PluginLatency>,
    render_ext: Option<PluginRender>,
    audio_ports_ext: Option<PluginAudioPorts>,
    note_ports_ext: Option<PluginNotePorts>,
    /// Parameter mapping, in the plugin's order (shared with the processor).
    pub(crate) params: Arc<[ParamMap]>,
}

impl LoadedPlugin {
    /// Creates an instance of `info` from a loaded plugin file.
    pub(crate) fn create(
        entry: &PluginEntry,
        info: &PluginInfo,
        signals: Arc<Signals>,
    ) -> Result<Self, String> {
        let id = CString::new(info.id.as_str()).map_err(|_| "bad plugin id".to_owned())?;
        let name = info.name.clone();
        let sig = Arc::clone(&signals);
        let mut instance = PluginInstance::<GtHost>::new(
            move |_| Shared::new(sig, &name),
            |shared| MainThread::new(shared),
            entry,
            &id,
            &host_info(),
        )
        .map_err(|e| format!("cannot create {}: {e}", info.name))?;
        let h = instance.plugin_handle();
        Ok(Self {
            params_ext: h.get_extension(),
            state_ext: h.get_extension(),
            gui_ext: h.get_extension(),
            latency_ext: h.get_extension(),
            render_ext: h.get_extension(),
            audio_ports_ext: h.get_extension(),
            note_ports_ext: h.get_extension(),
            instance,
            signals,
            info: info.clone(),
            params: Arc::from(Vec::new()),
        })
    }

    /// Reads the parameter list (descriptions for the document and the mapping for the
    /// processor) and the current values, normalized.
    pub(crate) fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>) {
        let Some(ext) = self.params_ext else {
            self.params = Arc::from(Vec::new());
            return (Vec::new(), Vec::new());
        };
        let h = self.instance.plugin_handle();
        let count = (ext.count(&h) as usize).min(MAX_PLUGIN_PARAMS);
        let mut infos = Vec::with_capacity(count);
        let mut maps = Vec::with_capacity(count);
        let mut values = Vec::with_capacity(count);
        let mut buf = ParamInfoBuffer::new();
        for i in 0..count as u32 {
            let Some(p) = ext.get_info(&h, i, &mut buf) else {
                continue;
            };
            let (min, max) = if p.min_value.is_finite() && p.max_value.is_finite() {
                (p.min_value, p.max_value.max(p.min_value))
            } else {
                (0.0, 1.0)
            };
            let stepped = p.flags.contains(ParamInfoFlags::IS_STEPPED);
            let map = ParamMap {
                id: p.id.get(),
                min,
                max,
                stepped,
            };
            let name = String::from_utf8_lossy(trim_nul(p.name)).into_owned();
            let module = String::from_utf8_lossy(trim_nul(p.module)).into_owned();
            let name = if module.is_empty() {
                name
            } else {
                format!("{} / {name}", module.trim_matches('/'))
            };
            let steps = if stepped {
                (max - min).round().clamp(0.0, 1e6) as u32
            } else {
                0
            };
            infos.push(PluginParamInfo {
                id: map.id,
                name,
                default: map.to_norm(p.default_value),
                steps,
                automatable: p.flags.contains(ParamInfoFlags::IS_AUTOMATABLE)
                    && !p.flags.contains(ParamInfoFlags::IS_READONLY),
                hidden: p.flags.contains(ParamInfoFlags::IS_HIDDEN),
            });
            values.push(
                ext.get_value(&h, p.id)
                    .map_or(infos[infos.len() - 1].default, |v| map.to_norm(v)),
            );
            maps.push(map);
        }
        self.params = Arc::from(maps);
        (infos, values)
    }

    /// The plugin's state, if it can save one.
    pub(crate) fn save_state(&mut self) -> Option<Vec<u8>> {
        let ext = self.state_ext?;
        let mut out = Vec::new();
        ext.save(&self.instance.plugin_handle(), &mut out).ok()?;
        Some(out)
    }

    /// Restores a saved state.
    pub(crate) fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        let Some(ext) = self.state_ext else {
            return Err(format!("{} cannot restore a state", self.info.name));
        };
        let mut reader = state;
        ext.load(&self.instance.plugin_handle(), &mut reader)
            .map_err(|_| format!("{} rejected its saved state", self.info.name))
    }

    /// Switches between real-time and offline rendering (export).
    pub(crate) fn set_offline(&mut self, offline: bool) {
        if let Some(ext) = self.render_ext {
            let mode = if offline {
                RenderMode::Offline
            } else {
                RenderMode::Realtime
            };
            let _ = ext.set(&self.instance.plugin_handle(), mode);
        }
    }

    fn layout(&mut self) -> Layout {
        let h = self.instance.plugin_handle();
        let mut layout = Layout::default();
        if let Some(ext) = self.audio_ports_ext {
            let mut buf = AudioPortInfoBuffer::new();
            for is_input in [true, false] {
                let mut main = None;
                let mut counts = Vec::new();
                for i in 0..ext.count(&h, is_input).min(64) {
                    let info = ext.get(&h, i, is_input, &mut buf);
                    let channels = info
                        .as_ref()
                        .map_or(0, |p| p.channel_count.min(32) as usize);
                    let is_main = info.as_ref().is_some_and(|p| {
                        p.flags
                            .contains(clack_extensions::audio_ports::AudioPortFlags::IS_MAIN)
                    });
                    if main.is_none() && (is_main || i == 0) && channels > 0 {
                        main = Some(i as usize);
                    }
                    counts.push(channels);
                }
                if is_input {
                    layout.inputs = counts;
                    layout.main_in = main;
                } else {
                    layout.outputs = counts;
                    layout.main_out = main;
                }
            }
        }
        if let Some(ext) = self.note_ports_ext {
            let mut buf = NotePortInfoBuffer::new();
            if ext.count(&h, true) > 0 {
                if let Some(p) = ext.get(&h, 0, true, &mut buf) {
                    let clap = p.supported_dialects.contains(NoteDialects::CLAP);
                    let midi = p.supported_dialects.contains(NoteDialects::MIDI);
                    if clap || midi {
                        layout.note_port = Some((0, !clap));
                    }
                }
            }
        }
        layout
    }

    /// Activates the plugin for `sample_rate` and blocks of up to [`RENDER_QUANTUM`] frames.
    /// Returns the processor for the engine and the queue of the plugin's own parameter
    /// changes.
    pub(crate) fn activate(
        &mut self,
        sample_rate: u32,
    ) -> Result<(ClapProcessor, rtrb::Consumer<ParamOut>), String> {
        let layout = self.layout();
        let audio = self
            .instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: f64::from(sample_rate),
                    min_frames_count: 1,
                    max_frames_count: RENDER_QUANTUM as u32,
                },
            )
            .map_err(|e| format!("cannot start {}: {e}", self.info.name))?;
        let latency = self
            .latency_ext
            .map_or(0, |ext| ext.get(&self.instance.plugin_handle()) as usize)
            .min(1 << 20);
        let (tx, rx) = rtrb::RingBuffer::new(OUT_QUEUE);
        let effect = self.info.kind() == PluginKind::Effect;
        let processor =
            ClapProcessor::new(audio, Arc::clone(&self.params), layout, effect, latency, tx);
        Ok((processor, rx))
    }

    /// Deactivates, once the engine has handed the processor back (false if it has not yet).
    pub(crate) fn try_deactivate(&mut self) -> bool {
        !self.instance.is_active() || self.instance.try_deactivate().is_ok()
    }
}

fn trim_nul(b: &[u8]) -> &[u8] {
    b.split(|&c| c == 0).next().unwrap_or(b)
}
