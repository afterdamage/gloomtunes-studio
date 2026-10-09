//! GT Test Gain: a stereo gain effect that can misbehave on request.

use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use clack_extensions::audio_ports::*;
use clack_extensions::params::*;
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};

use crate::AtomicF32;

/// Gain, 0 to 2 (plain value).
pub const PARAM_GAIN: u32 = 1;
/// Misbehave: 0 off, 1 output NaN, 2 report an error.
pub const PARAM_MISBEHAVE: u32 = 2;

/// The plugin type.
pub struct GainPlugin;

impl Plugin for GainPlugin {
    type AudioProcessor<'a> = GainProcessor<'a>;
    type Shared<'a> = GainShared;
    type MainThread<'a> = GainMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&GainShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>();
        #[cfg(target_os = "linux")]
        builder
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::timer::PluginTimer>();
    }
}

/// State shared by all of the plugin's threads.
pub struct GainShared {
    pub(crate) gain: AtomicF32,
    misbehave: AtomicU32,
    /// The editor changed the gain; the next block tells the host.
    pub(crate) edited: AtomicBool,
}

impl GainShared {
    pub(crate) fn new() -> Self {
        Self {
            gain: AtomicF32::new(1.0),
            misbehave: AtomicU32::new(0),
            edited: AtomicBool::new(false),
        }
    }

    fn handle(&self, event: &UnknownEvent) {
        if let Some(CoreEventSpace::ParamValue(e)) = event.as_core_event() {
            match e.param_id().map(ClapId::get) {
                Some(PARAM_GAIN) => self.gain.set((e.value() as f32).clamp(0.0, 2.0)),
                Some(PARAM_MISBEHAVE) => self
                    .misbehave
                    .store(e.value().round().clamp(0.0, 2.0) as u32, Ordering::Relaxed),
                _ => {}
            }
        }
    }
}

impl PluginShared<'_> for GainShared {}

/// Main-thread side.
pub struct GainMainThread<'a> {
    pub(crate) shared: &'a GainShared,
    #[cfg(target_os = "linux")]
    pub(crate) host: HostMainThreadHandle<'a>,
    #[cfg(target_os = "linux")]
    pub(crate) editor: std::cell::RefCell<Option<crate::gui::Editor>>,
}

impl<'a> GainMainThread<'a> {
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub(crate) fn new(host: HostMainThreadHandle<'a>, shared: &'a GainShared) -> Self {
        Self {
            shared,
            #[cfg(target_os = "linux")]
            host,
            #[cfg(target_os = "linux")]
            editor: std::cell::RefCell::new(None),
        }
    }
}

impl<'a> PluginMainThread<'a, GainShared> for GainMainThread<'a> {}

/// Audio-thread side.
pub struct GainProcessor<'a> {
    shared: &'a GainShared,
}

impl<'a> PluginAudioProcessor<'a, GainShared, GainMainThread<'a>> for GainProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &GainMainThread,
        shared: &'a GainShared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        if self.shared.edited.swap(false, Ordering::Relaxed) {
            let gain = f64::from(self.shared.gain.get());
            let _ = events.output.try_push(ParamValueEvent::new(
                0,
                ClapId::new(PARAM_GAIN),
                Pckn::match_all(),
                gain,
            ));
        }
        let mut pair = audio
            .port_pair(0)
            .ok_or(PluginError::Message("no audio port"))?;
        let mut channels = pair
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 audio"))?;
        let mut bufs = [None, None];
        for (pair, buf) in channels.iter_mut().zip(&mut bufs) {
            *buf = match pair {
                ChannelPair::InPlace(b) => Some(b),
                ChannelPair::InputOutput(i, o) => {
                    o.copy_from_slice(i);
                    Some(o)
                }
                ChannelPair::InputOnly(_) | ChannelPair::OutputOnly(_) => None,
            };
        }
        for batch in events.input.batch() {
            for e in batch.events() {
                self.shared.handle(e);
            }
            let range = batch.sample_bounds();
            let misbehave = self.shared.misbehave.load(Ordering::Relaxed);
            if misbehave == 2 {
                return Err(PluginError::Message("asked to fail"));
            }
            let gain = if misbehave == 1 {
                f32::NAN
            } else {
                self.shared.gain.get()
            };
            for buf in bufs.iter_mut().flatten() {
                for x in &mut buf[range] {
                    *x *= gain;
                }
            }
        }
        Ok(ProcessStatus::ContinueIfNotQuiet)
    }
}

impl PluginAudioProcessorParams for GainProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for e in input {
            self.shared.handle(e);
        }
    }
}

impl PluginAudioPortsImpl for GainMainThread<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, _is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: Some(ClapId::new(0)),
            });
        }
    }
}

impl PluginMainThreadParams for GainMainThread<'_> {
    fn count(&self) -> u32 {
        2
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        let flags = ParamInfoFlags::IS_AUTOMATABLE;
        match index {
            0 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_GAIN),
                flags,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: 0.0,
                max_value: 2.0,
                default_value: 1.0,
            }),
            1 => info.set(&ParamInfo {
                id: ClapId::new(PARAM_MISBEHAVE),
                flags: flags | ParamInfoFlags::IS_STEPPED | ParamInfoFlags::IS_ENUM,
                cookie: Default::default(),
                name: b"Misbehave",
                module: b"Test",
                min_value: 0.0,
                max_value: 2.0,
                default_value: 0.0,
            }),
            _ => {}
        }
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        match id.get() {
            PARAM_GAIN => Some(f64::from(self.shared.gain.get())),
            PARAM_MISBEHAVE => Some(f64::from(self.shared.misbehave.load(Ordering::Relaxed))),
            _ => None,
        }
    }

    fn value_to_text(
        &self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        match id.get() {
            PARAM_GAIN => write!(writer, "{:.1} dB", 20.0 * value.max(1e-5).log10()),
            PARAM_MISBEHAVE => {
                let names = ["Off", "NaN", "Error"];
                write!(writer, "{}", names[(value.round() as usize).min(2)])
            }
            _ => Err(std::fmt::Error),
        }
    }

    fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
        None
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        for e in input {
            self.shared.handle(e);
        }
    }
}

impl PluginStateImpl for GainMainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(b"GTG1")?;
        output.write_all(&self.shared.gain.get().to_le_bytes())?;
        output.write_all(&self.shared.misbehave.load(Ordering::Relaxed).to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = [0; 12];
        input.read_exact(&mut buf)?;
        if &buf[..4] != b"GTG1" {
            return Err(PluginError::Message("not a GT Test Gain state"));
        }
        let gain = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let mode = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        self.shared.gain.set(gain.clamp(0.0, 2.0));
        self.shared.misbehave.store(mode.min(2), Ordering::Relaxed);
        Ok(())
    }
}
