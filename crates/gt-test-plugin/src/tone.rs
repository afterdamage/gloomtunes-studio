//! GT Test Tone: an 8-voice sine or square instrument.

use std::ffi::CStr;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::*;
use clack_extensions::note_ports::*;
use clack_extensions::params::*;
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};

use crate::AtomicF32;

/// Volume, 0 to 1.
pub const PARAM_VOLUME: u32 = 1;
/// Waveform: 0 sine, 1 square.
pub const PARAM_WAVE: u32 = 2;
/// Last key played, 0 to 127 (read-only: the plugin sets it).
pub const PARAM_LAST_KEY: u32 = 3;

const VOICES: usize = 8;
/// Release time constant, in seconds.
const RELEASE: f32 = 0.02;

/// The plugin type.
pub struct TonePlugin;

impl Plugin for TonePlugin {
    type AudioProcessor<'a> = ToneProcessor<'a>;
    type Shared<'a> = ToneShared;
    type MainThread<'a> = ToneMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&ToneShared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>();
    }
}

/// State shared by all of the plugin's threads.
pub struct ToneShared {
    volume: AtomicF32,
    wave: AtomicU32,
    last_key: AtomicU32,
}

impl ToneShared {
    pub(crate) fn new() -> Self {
        Self {
            volume: AtomicF32::new(0.5),
            wave: AtomicU32::new(0),
            last_key: AtomicU32::new(60),
        }
    }

    fn set_param(&self, id: u32, value: f64) {
        match id {
            PARAM_VOLUME => self.volume.set((value as f32).clamp(0.0, 1.0)),
            PARAM_WAVE => self
                .wave
                .store(value.round().clamp(0.0, 1.0) as u32, Ordering::Relaxed),
            _ => {}
        }
    }
}

impl PluginShared<'_> for ToneShared {}

/// Main-thread side.
pub struct ToneMainThread<'a> {
    pub(crate) shared: &'a ToneShared,
}

impl<'a> PluginMainThread<'a, ToneShared> for ToneMainThread<'a> {}

#[derive(Clone, Copy)]
struct Voice {
    key: Option<u8>,
    phase: f32,
    step: f32,
    level: f32,
    gate: bool,
    velocity: f32,
}

/// Audio-thread side.
pub struct ToneProcessor<'a> {
    shared: &'a ToneShared,
    voices: [Voice; VOICES],
    release: f32,
    sample_rate: f32,
}

impl<'a> PluginAudioProcessor<'a, ToneShared, ToneMainThread<'a>> for ToneProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &ToneMainThread,
        shared: &'a ToneShared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sample_rate = config.sample_rate as f32;
        Ok(Self {
            shared,
            voices: [Voice {
                key: None,
                phase: 0.0,
                step: 0.0,
                level: 0.0,
                gate: false,
                velocity: 0.0,
            }; VOICES],
            release: (-1.0 / (RELEASE * sample_rate)).exp(),
            sample_rate,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut port = audio
            .output_port(0)
            .ok_or(PluginError::Message("no output port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("expected f32 output"))?;
        let Some(out) = channels.channel_mut(0) else {
            return Err(PluginError::Message("no output channel"));
        };
        out.fill(0.0);
        for batch in events.input.batch() {
            for e in batch.events() {
                match e.as_core_event() {
                    Some(CoreEventSpace::NoteOn(n)) => {
                        if let Some(key) = n.pckn().key.into_specific() {
                            let key = key.min(127) as u8;
                            self.note_on(key, n.velocity() as f32);
                            self.shared
                                .last_key
                                .store(u32::from(key), Ordering::Relaxed);
                            let _ = events.output.try_push(ParamValueEvent::new(
                                n.header().time(),
                                ClapId::new(PARAM_LAST_KEY),
                                Pckn::match_all(),
                                f64::from(key),
                            ));
                        }
                    }
                    Some(CoreEventSpace::NoteOff(n)) => {
                        let key = n.pckn().key.into_specific();
                        for v in &mut self.voices {
                            if key.is_none() || v.key.map(u16::from) == key {
                                v.gate = false;
                            }
                        }
                    }
                    Some(CoreEventSpace::NoteChoke(_)) => self.stop_all(),
                    Some(CoreEventSpace::ParamValue(p)) => {
                        if let Some(id) = p.param_id() {
                            self.shared.set_param(id.get(), p.value());
                        }
                    }
                    _ => {}
                }
            }
            self.render(&mut out[batch.sample_bounds()]);
        }
        if channels.channel_count() > 1 {
            let (first, rest) = channels.split_at_mut(1);
            if let Some(first) = first.channel(0) {
                for ch in rest {
                    ch.copy_from_slice(first);
                }
            }
        }
        if self.voices.iter().any(|v| v.key.is_some()) {
            Ok(ProcessStatus::Continue)
        } else {
            Ok(ProcessStatus::Sleep)
        }
    }

    fn reset(&mut self) {
        self.stop_all();
    }

    fn stop_processing(&mut self) {
        self.stop_all();
    }
}

impl ToneProcessor<'_> {
    fn note_on(&mut self, key: u8, velocity: f32) {
        let i = self
            .voices
            .iter()
            .position(|v| v.key.is_none())
            .unwrap_or_else(|| {
                // Steal the quietest voice.
                let mut best = 0;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.level < self.voices[best].level {
                        best = i;
                    }
                }
                best
            });
        let freq = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
        self.voices[i] = Voice {
            key: Some(key),
            phase: 0.0,
            step: freq / self.sample_rate,
            level: 1.0,
            gate: true,
            velocity: velocity.clamp(0.0, 1.0),
        };
    }

    fn stop_all(&mut self) {
        for v in &mut self.voices {
            v.key = None;
            v.gate = false;
        }
    }

    fn render(&mut self, out: &mut [f32]) {
        let volume = self.shared.volume.get();
        let square = self.shared.wave.load(Ordering::Relaxed) == 1;
        for v in self.voices.iter_mut().filter(|v| v.key.is_some()) {
            for x in out.iter_mut() {
                let s = if square {
                    if v.phase < 0.5 {
                        0.5
                    } else {
                        -0.5
                    }
                } else {
                    (v.phase * std::f32::consts::TAU).sin()
                };
                *x += s * v.level * v.velocity * volume * 0.5;
                v.phase = (v.phase + v.step).fract();
                if !v.gate {
                    v.level *= self.release;
                }
            }
            if !v.gate && v.level < 1e-4 {
                v.key = None;
            }
        }
    }
}

impl PluginAudioProcessorParams for ToneProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for e in input {
            if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event() {
                if let Some(id) = p.param_id() {
                    self.shared.set_param(id.get(), p.value());
                }
            }
        }
    }
}

impl PluginAudioPortsImpl for ToneMainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input {
            0
        } else {
            1
        }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if !is_input && index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"main",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for ToneMainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input {
            1
        } else {
            0
        }
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(1),
                name: b"main",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects: NoteDialects::CLAP,
            });
        }
    }
}

impl PluginMainThreadParams for ToneMainThread<'_> {
    fn count(&self) -> u32 {
        3
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        let auto = ParamInfoFlags::IS_AUTOMATABLE;
        let (id, flags, name, max, default): (u32, _, &[u8], f64, f64) = match index {
            0 => (PARAM_VOLUME, auto, b"Volume", 1.0, 0.5),
            1 => (
                PARAM_WAVE,
                auto | ParamInfoFlags::IS_STEPPED | ParamInfoFlags::IS_ENUM,
                b"Wave",
                1.0,
                0.0,
            ),
            2 => (
                PARAM_LAST_KEY,
                ParamInfoFlags::IS_READONLY | ParamInfoFlags::IS_STEPPED,
                b"Last key",
                127.0,
                60.0,
            ),
            _ => return,
        };
        info.set(&ParamInfo {
            id: ClapId::new(id),
            flags,
            cookie: Default::default(),
            name,
            module: b"",
            min_value: 0.0,
            max_value: max,
            default_value: default,
        });
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        match id.get() {
            PARAM_VOLUME => Some(f64::from(self.shared.volume.get())),
            PARAM_WAVE => Some(f64::from(self.shared.wave.load(Ordering::Relaxed))),
            PARAM_LAST_KEY => Some(f64::from(self.shared.last_key.load(Ordering::Relaxed))),
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
            PARAM_VOLUME => write!(writer, "{:.0} %", value * 100.0),
            PARAM_WAVE => write!(writer, "{}", if value >= 0.5 { "Square" } else { "Sine" }),
            PARAM_LAST_KEY => write!(writer, "{value:.0}"),
            _ => Err(std::fmt::Error),
        }
    }

    fn text_to_value(&self, _id: ClapId, _text: &CStr) -> Option<f64> {
        None
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        for e in input {
            if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event() {
                if let Some(id) = p.param_id() {
                    self.shared.set_param(id.get(), p.value());
                }
            }
        }
    }
}

impl PluginStateImpl for ToneMainThread<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(b"GTT1")?;
        output.write_all(&self.shared.volume.get().to_le_bytes())?;
        output.write_all(&self.shared.wave.load(Ordering::Relaxed).to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut buf = [0; 12];
        input.read_exact(&mut buf)?;
        if &buf[..4] != b"GTT1" {
            return Err(PluginError::Message("not a GT Test Tone state"));
        }
        let volume = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let wave = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        self.shared.set_param(PARAM_VOLUME, f64::from(volume));
        self.shared.set_param(PARAM_WAVE, f64::from(wave));
        Ok(())
    }
}
