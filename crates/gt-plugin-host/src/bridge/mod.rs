//! Out-of-process plugins (Prompt 33, D150): a plugin the user marks "run in its own process"
//! is loaded by a bridge child (`gloomtunes --plugin-bridge`), so a crash takes down only that
//! process. The app keeps the instance as a [`BridgeInstance`] behind the same `Instance`
//! trait, and the engine drives a [`BridgeProcessor`] behind the same `PluginProcessor`.
//!
//! * Control (loading, parameters, state, activation, editors) goes over the child's stdin and
//!   stdout as JSON lines; the child's own messages (parameter changes, restarts, editor
//!   edits) come back the same way.
//! * Audio and events go through a shared memory file with a lock-free handshake: the engine
//!   writes a block's events, context and input, bumps the request number and waits (spinning,
//!   then yielding) for the child to set the done number; the child waits for a new request
//!   (a futex on Linux, polling elsewhere), processes, writes the output back. A late or dead
//!   child fails the block, so the engine bypasses the plugin as it does any failing one.
//! * The app notices a dead child, starts a new one with the last state and restarts the
//!   instance (three times at most).

mod child;
mod host;
mod shm;

pub use child::bridge_child_main;
pub(crate) use host::{BridgeCommand, BridgeInstance};

use serde::{Deserialize, Serialize};

use crate::catalog::PluginInfo;

/// The switch that makes the app a bridge child.
pub const BRIDGE_SWITCH: &str = "--plugin-bridge";

/// Lines of the protocol start with this, so anything a plugin prints is skipped.
const PREFIX: &str = "GTB1 ";

/// A parameter description (`gt_core::PluginParamInfo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParamDto {
    id: u32,
    name: String,
    default: f32,
    steps: u32,
    automatable: bool,
    hidden: bool,
}

impl From<&gt_core::PluginParamInfo> for ParamDto {
    fn from(p: &gt_core::PluginParamInfo) -> Self {
        Self {
            id: p.id,
            name: p.name.clone(),
            default: p.default,
            steps: p.steps,
            automatable: p.automatable,
            hidden: p.hidden,
        }
    }
}

impl From<ParamDto> for gt_core::PluginParamInfo {
    fn from(p: ParamDto) -> Self {
        Self {
            id: p.id,
            name: p.name,
            default: p.default,
            steps: p.steps,
            automatable: p.automatable,
            hidden: p.hidden,
        }
    }
}

/// From the app to the child. Requests with an id get one [`Reply`] with that id.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Request {
    Load(PluginInfo),
    ReadParams,
    ValueText(u32, f32),
    SaveState,
    LoadState(Vec<u8>),
    SetOffline(bool),
    Activate {
        rate: u32,
        shm: std::path::PathBuf,
    },
    Deactivate,
    OpenEditor(String),
    CloseEditor,
    Quit,
    /// No reply.
    HostChanged(u32, f32),
    /// No reply.
    PluginReported(u32, f32),
}

impl Request {
    fn replies(&self) -> bool {
        !matches!(self, Self::HostChanged(..) | Self::PluginReported(..))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Reply {
    Done,
    Failed(String),
    Loaded { has_editor: bool },
    Params(Vec<ParamDto>, Vec<f32>),
    Text(Option<String>),
    State(Option<Vec<u8>>),
    Activated { latency: usize },
}

/// From the child to the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Message {
    Reply(u64, Reply),
    /// The processor changed a parameter (index, normalized value).
    Param(u32, f32),
    /// The editor changed a parameter that has not reached the processor (VST3).
    Edit(u32, f32),
    /// Restart, latency changed, parameters to read again, state changed.
    Signal(SignalKind),
    EditorOpen(bool),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum SignalKind {
    Restart,
    Latency,
    RescanParams,
    Dirty,
}

fn encode<T: Serialize>(id: Option<u64>, msg: &T) -> String {
    let json = serde_json::to_string(&(id, msg)).unwrap_or_default();
    format!("{PREFIX}{json}\n")
}

/// Reads a protocol line; anything printed in front of the prefix on the same line (a
/// plugin's output without a newline) is skipped.
fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Option<(Option<u64>, T)> {
    let at = line.find(PREFIX)?;
    serde_json::from_str(line[at + PREFIX.len()..].trim_end()).ok()
}

#[cfg(test)]
mod tests;
