//! Two small CLAP plugins for testing GloomTunes Studio's plugin host (Step 11).
//!
//! * **GT Test Gain** (`studio.gloomtunes.test-gain`): a stereo gain effect with a "Misbehave"
//!   switch that makes it output NaN or report an error, so the host's bypass can be tested.
//!   On Linux it has a small X11 editor (click to set the gain) driven by a host timer.
//! * **GT Test Tone** (`studio.gloomtunes.test-tone`): an 8-voice sine/square instrument. It
//!   reports the last key played through a read-only parameter, which exercises the plugin to
//!   host parameter path.
//!
//! Both save their state. The crate builds as a `cdylib` (the `.clap` file: copy
//! `libgt_test_plugin.so` or `gt_test_plugin.dll` to a CLAP folder under a `.clap` name) and as
//! an `rlib`, so the host's tests can load it in process. It is not shipped with the app.

use std::ffi::CStr;
use std::sync::atomic::{AtomicU32, Ordering};

use clack_plugin::entry::prelude::*;
use clack_plugin::prelude::*;

mod gain;
#[cfg(target_os = "linux")]
mod gui;
mod tone;

pub use gain::GainPlugin;
pub use tone::TonePlugin;

/// Id of the gain effect.
pub const GAIN_ID: &str = "studio.gloomtunes.test-gain";
/// Id of the tone instrument.
pub const TONE_ID: &str = "studio.gloomtunes.test-tone";

/// An `f32` shared between threads.
pub(crate) struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub(crate) fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub(crate) fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub(crate) fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// The plugin file's entry point: one factory listing both plugins.
pub struct TestEntry {
    factory: PluginFactoryWrapper<TestFactory>,
}

impl Entry for TestEntry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        use clack_plugin::plugin::features::*;
        Ok(Self {
            factory: PluginFactoryWrapper::new(TestFactory {
                gain: PluginDescriptor::new(GAIN_ID, "GT Test Gain")
                    .with_vendor("GloomTunes")
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .with_features([AUDIO_EFFECT, STEREO]),
                tone: PluginDescriptor::new(TONE_ID, "GT Test Tone")
                    .with_vendor("GloomTunes")
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .with_features([INSTRUMENT, SYNTHESIZER, STEREO]),
            }),
        })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

struct TestFactory {
    gain: PluginDescriptor,
    tone: PluginDescriptor,
}

impl PluginFactoryImpl for TestFactory {
    fn plugin_count(&self) -> u32 {
        2
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.gain),
            1 => Some(&self.tone),
            _ => None,
        }
    }

    fn create_plugin<'a>(
        &'a self,
        host_info: HostInfo<'a>,
        plugin_id: &CStr,
    ) -> Option<PluginInstance<'a>> {
        if plugin_id == self.gain.id()? {
            Some(PluginInstance::new::<GainPlugin>(
                host_info,
                &self.gain,
                |_| Ok(gain::GainShared::new()),
                |host, shared| Ok(gain::GainMainThread::new(host, shared)),
            ))
        } else if plugin_id == self.tone.id()? {
            Some(PluginInstance::new::<TonePlugin>(
                host_info,
                &self.tone,
                |_| Ok(tone::ToneShared::new()),
                |_, shared| Ok(tone::ToneMainThread { shared }),
            ))
        } else {
            None
        }
    }
}

clack_export_entry!(TestEntry);
