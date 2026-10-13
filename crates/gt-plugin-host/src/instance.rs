//! One loaded CLAP plugin instance on the main thread: creating it, reading its parameters,
//! saving and restoring its state, activating it to get a processor for the engine, its
//! main-thread work and its editor.

use std::ffi::CString;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiConfiguration, GuiSize, PluginGui};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::preset_discovery::prelude::{Location, PluginPresetLoad};
use clack_extensions::render::{PluginRender, RenderMode};
use clack_extensions::state::PluginState;
use clack_host::prelude::*;
use gt_core::{PluginKind, PluginParamInfo, MAX_EXTRA_OUTPUTS, MAX_PLUGIN_PARAMS};
use gt_engine::RENDER_QUANTUM;

use crate::backend::{Activated, Instance};
use crate::catalog::PluginInfo;
use crate::host::{GtHost, MainThread, Shared, Signals};
use crate::presets::{Preset, Source};
use crate::processor::{ClapProcessor, Layout, ParamMap, OUT_QUEUE};
use crate::window::HostWindow;

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

/// An open plugin editor.
struct Editor {
    gui: PluginGui,
    /// Our window, for an embedded editor (none for a floating one).
    window: Option<HostWindow>,
    resizable: bool,
}

/// A created plugin instance with its extensions.
pub(crate) struct LoadedPlugin {
    pub(crate) instance: PluginInstance<GtHost>,
    /// The plugin file (for its preset providers).
    entry: PluginEntry,
    pub(crate) signals: Arc<Signals>,
    pub(crate) info: PluginInfo,
    params_ext: Option<PluginParams>,
    state_ext: Option<PluginState>,
    gui_ext: Option<PluginGui>,
    latency_ext: Option<PluginLatency>,
    render_ext: Option<PluginRender>,
    audio_ports_ext: Option<PluginAudioPorts>,
    note_ports_ext: Option<PluginNotePorts>,
    /// Parameter mapping, in the plugin's order (shared with the processor).
    pub(crate) params: Arc<[ParamMap]>,
    editor: Option<Editor>,
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
            entry: entry.clone(),
            signals,
            info: info.clone(),
            params: Arc::from(Vec::new()),
            editor: None,
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
                let mut names = Vec::new();
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
                    if !is_input && channels > 0 {
                        let name = info.as_ref().map_or_else(String::new, |p| {
                            String::from_utf8_lossy(trim_nul(p.name)).trim().to_owned()
                        });
                        names.push((i as usize, name));
                    }
                    counts.push(channels);
                }
                if is_input {
                    layout.inputs = counts;
                    layout.main_in = main;
                } else {
                    layout.outputs = counts;
                    layout.main_out = main;
                    for (i, name) in names.drain(..) {
                        if Some(i) != main && layout.extra_outs.len() < MAX_EXTRA_OUTPUTS {
                            let name = if name.is_empty() {
                                format!("Out {}", layout.extra_outs.len() + 2)
                            } else {
                                name
                            };
                            layout.extra_outs.push(i);
                            layout.extra_names.push(name);
                        }
                    }
                }
            }
        }
        if let Some(ext) = self.note_ports_ext {
            layout.note_out = ext.count(&h, false) > 0;
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
    pub(crate) fn activate_clap(
        &mut self,
        sample_rate: u32,
    ) -> Result<(ClapProcessor, rtrb::Consumer<crate::processor::ParamOut>), String> {
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

impl Instance for LoadedPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    fn read_params(&mut self) -> (Vec<PluginParamInfo>, Vec<f32>) {
        LoadedPlugin::read_params(self)
    }

    fn outputs(&mut self) -> (Vec<String>, bool) {
        let layout = self.layout();
        (layout.extra_names, layout.note_out)
    }

    fn value_text(&mut self, index: usize, value: f32) -> Option<String> {
        let ext = self.params_ext?;
        let map = *self.params.get(index)?;
        let id = ClapId::new(map.id);
        let mut buf = [0u8; 256];
        let text = ext
            .value_to_text(
                &self.instance.plugin_handle(),
                id,
                map.to_plain(value),
                &mut buf,
            )
            .ok()?;
        let text = String::from_utf8_lossy(trim_nul(text)).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    fn factory_presets(&mut self) -> Vec<Preset> {
        crate::presets::clap_factory(&self.entry, &self.info.id)
    }

    fn load_factory(&mut self, source: &Source) -> Result<(), String> {
        let Source::Clap { file, key } = source else {
            return Err("not a CLAP preset".to_owned());
        };
        let h = self.instance.plugin_handle();
        let ext: PluginPresetLoad = h
            .get_extension()
            .ok_or_else(|| format!("{} cannot load presets", self.info.name))?;
        let location = match file {
            Some(path) => Location::File { path },
            None => Location::Plugin,
        };
        ext.load_from_location(&h, location, key.as_deref())
            .map_err(|_| format!("{} could not load the preset", self.info.name))
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        LoadedPlugin::save_state(self)
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        LoadedPlugin::load_state(self, state)
    }

    fn set_offline(&mut self, offline: bool) {
        LoadedPlugin::set_offline(self, offline);
    }

    fn activate(&mut self, sample_rate: u32) -> Result<Activated, String> {
        let (p, rx) = self.activate_clap(sample_rate)?;
        Ok((Box::new(p), rx))
    }

    fn try_deactivate(&mut self) -> bool {
        LoadedPlugin::try_deactivate(self)
    }

    fn main_thread_work(&mut self, now: Instant) {
        if Signals::take(&self.signals.callback) {
            self.instance.call_on_main_thread_callback();
        }
        let inst = &mut self.instance;
        let (ext, due) = inst.access_handler(|h| h.due_timers(now));
        if let Some(ext) = ext {
            for id in due {
                ext.on_timer(&inst.plugin_handle(), id);
            }
        }
        #[cfg(unix)]
        {
            let (ext, fds) = inst.access_handler(|h| (h.fd_ext.get(), h.fds.borrow().clone()));
            if let Some(ext) = ext {
                for (fd, flags) in ready_fds(&fds) {
                    ext.on_fd(&inst.plugin_handle(), fd, flags);
                }
            }
        }
    }

    fn next_frame(&self, now: Instant) -> Option<Duration> {
        let (timer, fds) = self.instance.access_handler(|h| {
            #[cfg(unix)]
            let fds = !h.fds.borrow().is_empty();
            #[cfg(not(unix))]
            let fds = false;
            (h.next_timer(now), fds)
        });
        if fds || self.editor.is_some() {
            Some(timer.map_or(FRAME, |w| w.min(FRAME)))
        } else {
            timer
        }
    }

    fn has_editor(&self) -> bool {
        self.gui_ext.is_some()
    }

    fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        if let Some(ed) = self.editor.as_mut() {
            if let Some(w) = ed.window.as_mut() {
                w.raise();
            }
            return Ok(());
        }
        let gui = self
            .gui_ext
            .ok_or_else(|| format!("{} has no editor", self.info.name))?;
        let api = HostWindow::api().ok_or("plugin editors are not supported here")?;
        let embedded = GuiConfiguration {
            api_type: api,
            is_floating: false,
        };
        let floating = GuiConfiguration {
            api_type: api,
            is_floating: true,
        };
        self.editor = Some(open_editor(self, gui, embedded, floating, title)?);
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(ed) = self.editor.take() {
            ed.gui.destroy(&self.instance.plugin_handle());
            drop(ed.window);
        }
    }

    fn poll_editor(&mut self) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let h = self.instance.plugin_handle();
        let mut close = Signals::take(&self.signals.gui_closed);
        if let Some((width, height)) = self.signals.take_resize() {
            if let Some(w) = ed.window.as_mut() {
                w.resize(width, height, ed.resizable);
                let _ = ed.gui.set_size(&h, GuiSize { width, height });
            }
        }
        if let Some(w) = ed.window.as_mut() {
            let ev = w.poll();
            close |= ev.close;
            if let Some((width, height)) = ev.resized {
                let want = GuiSize { width, height };
                if ed.resizable {
                    let size = ed.gui.adjust_size(&h, want).unwrap_or(want);
                    let _ = ed.gui.set_size(&h, size);
                    if size != want {
                        w.resize(size.width, size.height, true);
                    }
                } else if let Some(size) = ed.gui.get_size(&h) {
                    if size != want {
                        w.resize(size.width, size.height, false);
                    }
                }
            }
        }
        if close {
            self.close_editor();
        }
    }
}

/// Frame interval while an editor or file descriptors need polling.
pub(crate) const FRAME: Duration = Duration::from_millis(16);

fn open_editor(
    plugin: &mut LoadedPlugin,
    gui: PluginGui,
    embedded: GuiConfiguration,
    floating: GuiConfiguration,
    title: &str,
) -> Result<Editor, String> {
    let h = plugin.instance.plugin_handle();
    let name = plugin.info.name.clone();
    if gui.is_api_supported(&h, embedded) {
        gui.create(&h, embedded)
            .map_err(|e| format!("{name} could not open its editor: {e}"))?;
        let _ = gui.set_scale(&h, 1.0);
        let size = gui.get_size(&h).unwrap_or(GuiSize {
            width: 640,
            height: 480,
        });
        let resizable = gui.can_resize(&h);
        let window = match HostWindow::open(title, size.width, size.height, resizable) {
            Ok(w) => w,
            Err(e) => {
                gui.destroy(&h);
                return Err(format!("cannot open a window for {name}: {e}"));
            }
        };
        // SAFETY: the window lives in the editor record and is dropped only after
        // `gui.destroy` (see `close_editor`).
        #[allow(unsafe_code)]
        let parented = unsafe { gui.set_parent(&h, window.clap_window()) };
        if let Err(e) = parented {
            gui.destroy(&h);
            return Err(format!("{name} could not open its editor: {e}"));
        }
        let _ = gui.show(&h);
        Ok(Editor {
            gui,
            window: Some(window),
            resizable,
        })
    } else if gui.is_api_supported(&h, floating) {
        gui.create(&h, floating)
            .map_err(|e| format!("{name} could not open its editor: {e}"))?;
        if let Ok(t) = std::ffi::CString::new(title) {
            gui.suggest_title(&h, &t);
        }
        let _ = gui.show(&h);
        Ok(Editor {
            gui,
            window: None,
            resizable: false,
        })
    } else {
        Err(format!("{name}'s editor cannot run on this system"))
    }
}

/// File descriptors that are ready, with what they are ready for (a zero-timeout poll).
#[cfg(unix)]
fn ready_fds(
    fds: &[(std::os::fd::RawFd, clack_extensions::posix_fd::FdFlags)],
) -> Vec<(std::os::fd::RawFd, clack_extensions::posix_fd::FdFlags)> {
    use clack_extensions::posix_fd::FdFlags;
    let want: Vec<(std::os::fd::RawFd, bool, bool)> = fds
        .iter()
        .map(|&(fd, f)| (fd, f.contains(FdFlags::READ), f.contains(FdFlags::WRITE)))
        .collect();
    crate::host::poll_fds(&want)
        .into_iter()
        .map(|(fd, read, write, error)| {
            let mut f = FdFlags::empty();
            if read {
                f |= FdFlags::READ;
            }
            if write {
                f |= FdFlags::WRITE;
            }
            if error {
                f |= FdFlags::ERROR;
            }
            (fd, f)
        })
        .collect()
}

fn trim_nul(b: &[u8]) -> &[u8] {
    b.split(|&c| c == 0).next().unwrap_or(b)
}
