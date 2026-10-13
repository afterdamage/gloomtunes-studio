//! Factory presets of GT Test Tone, offered through CLAP preset discovery (Prompt 18): one
//! provider that lists two presets stored inside the plugin.

use std::ffi::CStr;

use clack_extensions::preset_discovery::prelude::*;
use clack_plugin::prelude::*;

/// Id of the preset provider.
pub const PROVIDER_ID: &str = "studio.gloomtunes.test-presets";

/// The factory presets: name, load key, volume, wave.
pub const PRESETS: [(&CStr, &CStr, f32, u32); 2] =
    [(c"Soft Sine", c"soft", 0.25, 0), (c"Buzz", c"buzz", 0.8, 1)];

/// The preset discovery factory.
pub struct PresetFactory {
    pub(crate) descriptor: ProviderDescriptor,
}

impl PresetDiscoveryFactoryImpl for PresetFactory {
    fn provider_count(&self) -> u32 {
        1
    }

    fn provider_descriptor(&self, index: u32) -> Option<&ProviderDescriptor> {
        (index == 0).then_some(&self.descriptor)
    }

    fn create_provider<'a>(
        &'a self,
        indexer: IndexerInfo<'a>,
        provider_id: &CStr,
    ) -> Option<ProviderInstance<'a>> {
        if provider_id != self.descriptor.id()? {
            return None;
        }
        Some(ProviderInstance::new(
            indexer,
            &self.descriptor,
            |mut indexer| {
                indexer.declare_location(LocationInfo {
                    name: c"GT Test Tone factory",
                    flags: Flags::IS_FACTORY_CONTENT,
                    location: Location::Plugin,
                })?;
                Ok(Provider)
            },
        ))
    }
}

struct Provider;

impl ProviderImpl<'_> for Provider {
    fn get_metadata(
        &mut self,
        location: Location,
        receiver: &mut MetadataReceiver,
    ) -> Result<(), PluginError> {
        if location != Location::Plugin {
            return Err(PluginError::Message("no presets there"));
        }
        for (name, key, _, _) in PRESETS {
            receiver
                .begin_preset(Some(name), Some(key))?
                .add_plugin_id(UniversalPluginId::clap(c"studio.gloomtunes.test-tone"))
                .set_flags(Flags::IS_FACTORY_CONTENT);
        }
        Ok(())
    }
}
