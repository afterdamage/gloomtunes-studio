//! Third-party plugins as document data (Step 11).
//!
//! A [`PluginRef`] names a plugin (format and id), keeps its saved state as an opaque blob and
//! mirrors its parameters: their descriptions as the plugin gave them, and their current values
//! normalized to 0..1 of each parameter's range. The mirror is what automation, modulators,
//! MIDI learn and undo work on, exactly like the built-in instruments' settings; the plugin host
//! keeps the running plugin in step with it. The state blob is authoritative when a plugin is
//! loaded; the mirror is refreshed from the plugin then.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Most parameters mirrored per plugin. Larger plugins have the rest left out of automation.
pub const MAX_PLUGIN_PARAMS: usize = 4096;

/// Plugin formats. Only CLAP is hosted; VST3 is designed but not built (ARCHITECTURE.md §2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginFormat {
    /// CLAP.
    Clap,
}

impl PluginFormat {
    /// Stable key for files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Clap => "clap",
        }
    }

    /// The format with a file key.
    pub fn from_key(key: &str) -> Option<Self> {
        (key == "clap").then_some(Self::Clap)
    }
}

/// What a plugin does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    /// Plays notes: goes in the channel rack.
    Instrument,
    /// Processes audio: goes in a mixer effect slot.
    Effect,
}

/// One parameter as the plugin described it.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginParamInfo {
    /// The plugin's own stable id for it.
    pub id: u32,
    /// Display name, with the plugin's module path in front when it has one ("Osc 1 / Pitch").
    pub name: String,
    /// Default value, normalized.
    pub default: f32,
    /// Number of steps between minimum and maximum for stepped parameters, 0 for continuous.
    pub steps: u32,
    /// The plugin allows the host to automate it.
    pub automatable: bool,
    /// The plugin asks hosts not to show it.
    pub hidden: bool,
}

impl PluginParamInfo {
    /// Snaps a normalized value onto the parameter's steps (stepped parameters) and into 0..1.
    pub fn snap(&self, v: f32) -> f32 {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            self.default
        };
        if self.steps == 0 {
            v
        } else {
            let n = self.steps as f32;
            (v * n).round() / n
        }
    }
}

/// Identity of one loaded plugin during a session, linking the document to the running plugin.
/// Not saved: a loaded project gets fresh ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginInstanceId(pub u64);

impl PluginInstanceId {
    /// A new id, never handed out before in this process.
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A plugin used by a channel or an effect slot.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRef {
    /// Plugin format.
    pub format: PluginFormat,
    /// The plugin's id, e.g. "org.surge-synth-team.surge-xt".
    pub id: String,
    /// Display name.
    pub name: String,
    /// Vendor.
    pub vendor: String,
    /// Instrument or effect.
    pub kind: PluginKind,
    /// The plugin file it was last loaded from (a hint: plugins are found by id).
    pub path: PathBuf,
    /// The plugin's own saved state, as last loaded or saved. Empty: its defaults.
    pub state: Arc<[u8]>,
    /// Parameter descriptions, in the order the plugin listed them.
    pub params: Arc<[PluginParamInfo]>,
    /// Current values, normalized, in the order of `params`.
    pub values: Vec<f32>,
    /// The running plugin this document entry belongs to.
    pub instance: PluginInstanceId,
}

impl PluginRef {
    /// A plugin with no state and no parameters yet (the host fills them in when it loads).
    pub fn new(id: &str, name: &str, vendor: &str, kind: PluginKind, path: PathBuf) -> Self {
        Self {
            format: PluginFormat::Clap,
            id: id.to_owned(),
            name: name.to_owned(),
            vendor: vendor.to_owned(),
            kind,
            path,
            state: Arc::from(Vec::new()),
            params: Arc::from(Vec::new()),
            values: Vec::new(),
            instance: PluginInstanceId::fresh(),
        }
    }

    /// Position of parameter `id` in `params`.
    pub fn param_index(&self, id: u32) -> Option<usize> {
        self.params.iter().position(|p| p.id == id)
    }

    /// Normalized value of parameter `id`.
    pub fn value(&self, id: u32) -> Option<f32> {
        self.values.get(self.param_index(id)?).copied()
    }

    /// Sets parameter `id` (normalized, snapped to its steps). False if the plugin has no such
    /// parameter.
    pub fn set_value(&mut self, id: u32, v: f32) -> bool {
        let Some(i) = self.param_index(id) else {
            return false;
        };
        self.values[i] = self.params[i].snap(v);
        true
    }

    /// A short hash of the plugin id, so a parameter address stays tied to the plugin it was
    /// made for when another plugin takes its place (FNV-1a, 32-bit).
    pub fn id_hash(&self) -> u32 {
        hash_id(&self.id)
    }

    /// Fixes the value count and ranges and caps the parameter list.
    pub fn sanitize(&mut self) {
        if self.params.len() > MAX_PLUGIN_PARAMS {
            self.params = Arc::from(&self.params[..MAX_PLUGIN_PARAMS]);
        }
        self.values.resize(self.params.len(), 0.0);
        for (v, p) in self.values.iter_mut().zip(self.params.iter()) {
            *v = p.snap(*v);
        }
    }
}

/// FNV-1a over the bytes of a plugin id.
pub fn hash_id(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// Where a plugin sits in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluginOwner {
    /// The instrument of a channel.
    Channel(crate::ChannelId),
    /// A mixer effect slot.
    Effect {
        /// Strip index.
        strip: usize,
        /// Slot index.
        slot: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u32, steps: u32) -> PluginParamInfo {
        PluginParamInfo {
            id,
            name: format!("P{id}"),
            default: 0.25,
            steps,
            automatable: true,
            hidden: false,
        }
    }

    #[test]
    fn values_snap_and_sanitize() {
        let mut p = PluginRef::new("a.b", "AB", "V", PluginKind::Effect, PathBuf::new());
        p.params = Arc::from(vec![info(7, 0), info(9, 4)]);
        p.values = vec![2.0];
        p.sanitize();
        assert_eq!(p.values, vec![1.0, 0.0]);
        assert!(p.set_value(9, 0.4));
        assert_eq!(p.value(9), Some(0.5), "four steps: 0, 0.25, 0.5, 0.75, 1");
        assert!(!p.set_value(8, 0.4));
        assert_eq!(info(1, 0).snap(f32::NAN), 0.25);
        assert_ne!(PluginInstanceId::fresh(), PluginInstanceId::fresh());
        assert_eq!(hash_id(""), 0x811c_9dc5);
        assert_ne!(hash_id("a.b"), hash_id("a.c"));
    }
}
