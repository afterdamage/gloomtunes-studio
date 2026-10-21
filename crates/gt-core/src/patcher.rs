//! The modular patcher (Prompt 34, ARCHITECTURE.md D154): a graph of modules wired with free
//! audio, MIDI and modulation routing, played as an instrument (`Instrument::Patcher`) or
//! used as an effect (`EffectKind::Patcher`).
//!
//! Modules are built-in signal blocks (oscillators, filters, envelopes, LFOs, maths), any
//! built-in effect, Gloom Synth, or a plugin. A wire joins an output to an input of the same
//! kind, or a modulation output to any parameter of any module (plugins included), scaled by
//! its amount. Wires that close a loop read the previous block, so any graph is valid.
//!
//! Signals: audio is stereo; modulation is one value per block (unbounded; envelopes and
//! gates 0 to 1, LFOs -1 to 1, the voice pitch in octaves from middle C); MIDI is notes.
//! Modules after [`ModuleKind::Voices`] run once per voice (up to [`PATCH_VOICES`]); effects,
//! synths, plugins and the output mix the voices.

use crate::param::{ParamCurve, ParamInfo, ParamUnit};
use crate::plugin::{PluginKind, PluginRef};
use crate::{EffectKind, SynthPatch};

/// Most modules in a patch.
pub const MAX_NODES: usize = 48;
/// Most wires in a patch.
pub const MAX_WIRES: usize = 128;
/// Voices of a patch's per-voice modules.
pub const PATCH_VOICES: usize = 8;

/// A module's id inside its patch (stable across edits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NodeId(pub u16);

/// The kind of a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    /// Stereo audio.
    Audio,
    /// A modulation value.
    Mod,
    /// Notes.
    Midi,
}

/// What a module is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    /// The notes the channel plays.
    NoteIn,
    /// The audio coming into the effect (silent in an instrument).
    AudioIn,
    /// What the patch outputs.
    Output,
    /// Turns notes into per-voice pitch, gate and velocity.
    Voices,
    /// Band-limited oscillator.
    Oscillator,
    /// White noise.
    Noise,
    /// State-variable filter.
    Filter,
    /// ADSR envelope.
    Envelope,
    /// Low-frequency oscillator.
    Lfo,
    /// Gain and pan, with a modulation input.
    Amp,
    /// Four audio inputs with levels.
    Mixer,
    /// Arithmetic on two modulation signals.
    Math,
    /// Shifts notes.
    Transpose,
    /// A built-in effect.
    Effect(EffectKind),
    /// Gloom Synth with its own patch.
    Synth,
    /// A plugin (instrument or effect).
    Plugin,
}

const fn p(
    key: &'static str,
    name: &'static str,
    (min, max, default): (f32, f32, f32),
    curve: ParamCurve,
    unit: ParamUnit,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        min,
        max,
        default,
        curve,
        unit,
        choices: &[],
    }
}

const fn c(
    key: &'static str,
    name: &'static str,
    choices: &'static [&'static str],
    default: f32,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        min: 0.0,
        max: (choices.len() - 1) as f32,
        default,
        curve: ParamCurve::Stepped,
        unit: ParamUnit::Choice,
        choices,
    }
}

/// Oscillator waves, by value.
pub const OSC_WAVES: &[&str] = &["Sine", "Triangle", "Saw", "Square"];
/// Filter responses, by value.
pub const PATCH_FILTERS: &[&str] = &["Low-pass", "High-pass", "Band-pass", "Notch"];
/// LFO shapes, by value.
pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Saw", "Square", "Random"];
/// Maths operations, by value.
pub const MATH_OPS: &[&str] = &["A + B", "A × B", "Min", "Max", "Lag"];

const OUTPUT: &[ParamInfo] = &[p(
    "level",
    "Level",
    (0.0, 2.0, 1.0),
    ParamCurve::Power(2.0),
    ParamUnit::Gain,
)];
const VOICES: &[ParamInfo] = &[
    p(
        "voices",
        "Voices",
        (1.0, PATCH_VOICES as f32, PATCH_VOICES as f32),
        ParamCurve::Stepped,
        ParamUnit::Count,
    ),
    p(
        "glide",
        "Glide",
        (0.0, 2000.0, 0.0),
        ParamCurve::Power(3.0),
        ParamUnit::Ms,
    ),
];
const OSCILLATOR: &[ParamInfo] = &[
    c("wave", "Wave", OSC_WAVES, 2.0),
    p(
        "octave",
        "Octave",
        (-3.0, 3.0, 0.0),
        ParamCurve::Stepped,
        ParamUnit::Count,
    ),
    p(
        "tune",
        "Tune",
        (-12.0, 12.0, 0.0),
        ParamCurve::Linear,
        ParamUnit::Semitones,
    ),
    p(
        "fine",
        "Fine",
        (-100.0, 100.0, 0.0),
        ParamCurve::Linear,
        ParamUnit::Cents,
    ),
    p(
        "width",
        "Width",
        (0.05, 0.95, 0.5),
        ParamCurve::Linear,
        ParamUnit::Percent,
    ),
    p(
        "level",
        "Level",
        (0.0, 1.0, 0.5),
        ParamCurve::Linear,
        ParamUnit::Percent,
    ),
];
const NOISE: &[ParamInfo] = &[p(
    "level",
    "Level",
    (0.0, 1.0, 0.3),
    ParamCurve::Linear,
    ParamUnit::Percent,
)];
const FILTER: &[ParamInfo] = &[
    c("mode", "Mode", PATCH_FILTERS, 0.0),
    p(
        "cutoff",
        "Cutoff",
        (20.0, 20_000.0, 2000.0),
        ParamCurve::Exp,
        ParamUnit::Hz,
    ),
    p(
        "res",
        "Resonance",
        (0.0, 1.0, 0.2),
        ParamCurve::Linear,
        ParamUnit::Percent,
    ),
    p(
        "keytrack",
        "Key track",
        (0.0, 1.0, 0.5),
        ParamCurve::Linear,
        ParamUnit::Percent,
    ),
];
const ENVELOPE: &[ParamInfo] = &[
    p(
        "attack",
        "Attack",
        (0.0, 5000.0, 5.0),
        ParamCurve::Power(3.0),
        ParamUnit::Ms,
    ),
    p(
        "decay",
        "Decay",
        (0.0, 5000.0, 300.0),
        ParamCurve::Power(3.0),
        ParamUnit::Ms,
    ),
    p(
        "sustain",
        "Sustain",
        (0.0, 1.0, 0.6),
        ParamCurve::Linear,
        ParamUnit::Percent,
    ),
    p(
        "release",
        "Release",
        (0.0, 8000.0, 300.0),
        ParamCurve::Power(3.0),
        ParamUnit::Ms,
    ),
];
const LFO: &[ParamInfo] = &[
    c("shape", "Shape", LFO_SHAPES, 0.0),
    p(
        "rate",
        "Rate",
        (0.01, 40.0, 2.0),
        ParamCurve::Exp,
        ParamUnit::Hz,
    ),
];
const AMP: &[ParamInfo] = &[
    p(
        "level",
        "Level",
        (0.0, 2.0, 1.0),
        ParamCurve::Power(2.0),
        ParamUnit::Gain,
    ),
    p(
        "pan",
        "Pan",
        (-1.0, 1.0, 0.0),
        ParamCurve::Linear,
        ParamUnit::Pan,
    ),
];
const MIXER: &[ParamInfo] = &[
    p(
        "in1",
        "In 1",
        (0.0, 2.0, 1.0),
        ParamCurve::Power(2.0),
        ParamUnit::Gain,
    ),
    p(
        "in2",
        "In 2",
        (0.0, 2.0, 1.0),
        ParamCurve::Power(2.0),
        ParamUnit::Gain,
    ),
    p(
        "in3",
        "In 3",
        (0.0, 2.0, 1.0),
        ParamCurve::Power(2.0),
        ParamUnit::Gain,
    ),
    p(
        "in4",
        "In 4",
        (0.0, 2.0, 1.0),
        ParamCurve::Power(2.0),
        ParamUnit::Gain,
    ),
];
const MATH: &[ParamInfo] = &[
    c("op", "Operation", MATH_OPS, 0.0),
    p(
        "scale",
        "Scale",
        (-4.0, 4.0, 1.0),
        ParamCurve::Linear,
        ParamUnit::Plain,
    ),
    p(
        "offset",
        "Offset",
        (-1.0, 1.0, 0.0),
        ParamCurve::Linear,
        ParamUnit::Plain,
    ),
    p(
        "lag",
        "Lag",
        (0.0, 2000.0, 100.0),
        ParamCurve::Power(3.0),
        ParamUnit::Ms,
    ),
];
const TRANSPOSE: &[ParamInfo] = &[p(
    "semis",
    "Semitones",
    (-24.0, 24.0, 0.0),
    ParamCurve::Stepped,
    ParamUnit::Semitones,
)];

use PortKind::{Audio, Midi, Mod};

impl ModuleKind {
    /// The modules the "add" menu offers (effects and plugins are added separately).
    pub const BUILT_IN: [ModuleKind; 11] = [
        Self::Voices,
        Self::Oscillator,
        Self::Noise,
        Self::Filter,
        Self::Envelope,
        Self::Lfo,
        Self::Amp,
        Self::Mixer,
        Self::Math,
        Self::Transpose,
        Self::Synth,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::NoteIn => "Notes in",
            Self::AudioIn => "Audio in",
            Self::Output => "Output",
            Self::Voices => "Voices",
            Self::Oscillator => "Oscillator",
            Self::Noise => "Noise",
            Self::Filter => "Filter",
            Self::Envelope => "Envelope",
            Self::Lfo => "LFO",
            Self::Amp => "Amp",
            Self::Mixer => "Mixer",
            Self::Math => "Maths",
            Self::Transpose => "Transpose",
            Self::Effect(k) => k.name(),
            Self::Synth => "Gloom Synth",
            Self::Plugin => "Plugin",
        }
    }

    /// Project-file key.
    pub fn key(self) -> String {
        match self {
            Self::NoteIn => "note_in".into(),
            Self::AudioIn => "audio_in".into(),
            Self::Output => "output".into(),
            Self::Voices => "voices".into(),
            Self::Oscillator => "osc".into(),
            Self::Noise => "noise".into(),
            Self::Filter => "filter".into(),
            Self::Envelope => "env".into(),
            Self::Lfo => "lfo".into(),
            Self::Amp => "amp".into(),
            Self::Mixer => "mixer".into(),
            Self::Math => "math".into(),
            Self::Transpose => "transpose".into(),
            Self::Effect(k) => format!("fx.{}", k.key()),
            Self::Synth => "synth".into(),
            Self::Plugin => "plugin".into(),
        }
    }

    /// The kind for a key.
    pub fn from_key(key: &str) -> Option<Self> {
        if let Some(fx) = key.strip_prefix("fx.") {
            return EffectKind::ALL
                .into_iter()
                .find(|k| k.key() == fx && *k != EffectKind::Plugin && *k != EffectKind::Patcher)
                .map(Self::Effect);
        }
        [Self::NoteIn, Self::AudioIn, Self::Output, Self::Plugin]
            .into_iter()
            .chain(Self::BUILT_IN)
            .find(|k| k.key() == key)
    }

    /// The parameters of a built-in module or effect (plugins and synths: none here).
    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            Self::Output => OUTPUT,
            Self::Voices => VOICES,
            Self::Oscillator => OSCILLATOR,
            Self::Noise => NOISE,
            Self::Filter => FILTER,
            Self::Envelope => ENVELOPE,
            Self::Lfo => LFO,
            Self::Amp => AMP,
            Self::Mixer => MIXER,
            Self::Math => MATH,
            Self::Transpose => TRANSPOSE,
            Self::Effect(k) => k.params(),
            Self::NoteIn | Self::AudioIn | Self::Synth | Self::Plugin => &[],
        }
    }

    /// Input ports, in order.
    pub fn inputs(self) -> &'static [(&'static str, PortKind)] {
        match self {
            Self::NoteIn | Self::AudioIn | Self::Noise | Self::Lfo => &[],
            Self::Output => &[("In", Audio)],
            Self::Voices | Self::Transpose | Self::Synth => &[("Notes", Midi)],
            Self::Oscillator => &[("Pitch", Mod)],
            Self::Filter => &[("In", Audio), ("Pitch", Mod)],
            Self::Envelope => &[("Gate", Mod)],
            Self::Amp => &[("In", Audio), ("Gain", Mod)],
            Self::Mixer => &[("1", Audio), ("2", Audio), ("3", Audio), ("4", Audio)],
            Self::Math => &[("A", Mod), ("B", Mod)],
            Self::Effect(_) => &[("In", Audio)],
            Self::Plugin => &[("In", Audio), ("Notes", Midi)],
        }
    }

    /// Output ports, in order.
    pub fn outputs(self) -> &'static [(&'static str, PortKind)] {
        match self {
            Self::NoteIn | Self::Transpose => &[("Notes", Midi)],
            Self::Output => &[],
            Self::Voices => &[("Pitch", Mod), ("Gate", Mod), ("Velocity", Mod)],
            Self::Envelope | Self::Lfo | Self::Math => &[("Out", Mod)],
            Self::Plugin => &[("Out", Audio), ("Notes", Midi)],
            _ => &[("Out", Audio)],
        }
    }

    /// True for modules that run once per voice when fed by one that does.
    pub fn per_voice(self) -> bool {
        matches!(
            self,
            Self::Voices
                | Self::Oscillator
                | Self::Noise
                | Self::Filter
                | Self::Envelope
                | Self::Lfo
                | Self::Amp
                | Self::Mixer
                | Self::Math
        )
    }

    /// True for the modules every patch has exactly one of (or one each).
    pub fn is_fixed(self) -> bool {
        matches!(self, Self::NoteIn | Self::AudioIn | Self::Output)
    }
}

/// One module of a patch.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Its id.
    pub id: NodeId,
    /// What it is.
    pub kind: ModuleKind,
    /// Parameter values in the order of `kind.params()`.
    pub params: Vec<f32>,
    /// Position on the canvas, in points.
    pub pos: [f32; 2],
    /// The sound of a [`ModuleKind::Synth`].
    pub synth: Option<Box<SynthPatch>>,
    /// The plugin of a [`ModuleKind::Plugin`].
    pub plugin: Option<Box<PluginRef>>,
}

impl Node {
    /// A module at its defaults.
    pub fn new(id: NodeId, kind: ModuleKind, pos: [f32; 2]) -> Self {
        Self {
            id,
            kind,
            params: kind.params().iter().map(|p| p.default).collect(),
            pos,
            synth: (kind == ModuleKind::Synth).then(Box::default),
            plugin: None,
        }
    }

    /// Display name (a plugin's own name).
    pub fn name(&self) -> &str {
        match &self.plugin {
            Some(p) => &p.name,
            None => self.kind.name(),
        }
    }

    /// Number of parameters a modulation wire can target.
    pub fn param_count(&self) -> usize {
        match &self.plugin {
            Some(p) => p.values.len(),
            None => self.params.len(),
        }
    }

    /// Name of parameter `i`.
    pub fn param_name(&self, i: usize) -> &str {
        match &self.plugin {
            Some(p) => p.params.get(i).map_or("?", |p| p.name.as_str()),
            None => self.kind.params().get(i).map_or("?", |p| p.name),
        }
    }

    /// Parameter `i`'s value: plain for built-ins, normalized for plugins.
    pub fn param(&self, i: usize) -> f32 {
        match &self.plugin {
            Some(p) => p.values.get(i).copied().unwrap_or(0.0),
            None => self.params.get(i).copied().unwrap_or(0.0),
        }
    }

    /// The input ports; a plugin effect takes no notes, a plugin instrument no audio.
    pub fn inputs(&self) -> &'static [(&'static str, PortKind)] {
        match (self.kind, self.plugin.as_ref().map(|p| p.kind)) {
            (ModuleKind::Plugin, Some(PluginKind::Effect)) => &[("In", Audio)],
            (ModuleKind::Plugin, _) => &[("Notes", Midi)],
            (k, _) => k.inputs(),
        }
    }

    /// The output ports; only a plugin that sends MIDI has a notes output.
    pub fn outputs(&self) -> &'static [(&'static str, PortKind)] {
        match (
            self.kind,
            self.plugin.as_ref().is_some_and(|p| p.sends_midi),
        ) {
            (ModuleKind::Plugin, false) => &[("Out", Audio)],
            (k, _) => k.outputs(),
        }
    }
}

/// Where a wire goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireEnd {
    /// An input port.
    Input(u8),
    /// A parameter, moved by a modulation output.
    Param(u16),
}

/// A connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wire {
    /// Source module.
    pub from: NodeId,
    /// Its output port.
    pub port: u8,
    /// Target module.
    pub to: NodeId,
    /// Its input or parameter.
    pub end: WireEnd,
    /// Scale: for an input, a gain; for a parameter, how far a modulation of 1 moves it, as a
    /// fraction of its knob travel (-1 to 1).
    pub amount: f32,
}

/// Why a wire was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// No such module or port.
    Missing,
    /// The ports carry different signals.
    Kinds,
    /// Only modulation outputs move parameters.
    NotMod,
    /// The same wire exists.
    Duplicate,
    /// The patch has [`MAX_WIRES`].
    Full,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "no such port",
            Self::Kinds => "the ports carry different signals",
            Self::NotMod => "only modulation moves a parameter",
            Self::Duplicate => "already connected",
            Self::Full => "too many wires",
        })
    }
}

/// A patch: modules and wires.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    /// The modules, in the order they were added.
    pub nodes: Vec<Node>,
    /// The wires.
    pub wires: Vec<Wire>,
}

impl Default for Patch {
    fn default() -> Self {
        Self::instrument()
    }
}

impl Patch {
    /// A playable starting point: notes → voices → oscillator → filter → amp (envelope) → out.
    pub fn instrument() -> Self {
        let mut p = Self {
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let notes = p.add(ModuleKind::NoteIn, [20.0, 40.0]).expect("room");
        p.add(ModuleKind::AudioIn, [20.0, 260.0]).expect("room");
        let out = p.add(ModuleKind::Output, [820.0, 120.0]).expect("room");
        let voices = p.add(ModuleKind::Voices, [170.0, 40.0]).expect("room");
        let osc = p.add(ModuleKind::Oscillator, [340.0, 20.0]).expect("room");
        let filter = p.add(ModuleKind::Filter, [500.0, 20.0]).expect("room");
        let env = p.add(ModuleKind::Envelope, [340.0, 200.0]).expect("room");
        let amp = p.add(ModuleKind::Amp, [660.0, 100.0]).expect("room");
        for (from, port, to, end) in [
            (notes, 0, voices, WireEnd::Input(0)),
            (voices, 0, osc, WireEnd::Input(0)),
            (voices, 0, filter, WireEnd::Input(1)),
            (voices, 1, env, WireEnd::Input(0)),
            (osc, 0, filter, WireEnd::Input(0)),
            (filter, 0, amp, WireEnd::Input(0)),
            (env, 0, amp, WireEnd::Input(1)),
            (amp, 0, out, WireEnd::Input(0)),
        ] {
            p.connect(from, port, to, end, 1.0).expect("valid");
        }
        p
    }

    /// An effect starting point: audio in → out.
    pub fn effect() -> Self {
        let mut p = Self {
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        p.add(ModuleKind::NoteIn, [20.0, 260.0]).expect("room");
        let input = p.add(ModuleKind::AudioIn, [20.0, 60.0]).expect("room");
        let out = p.add(ModuleKind::Output, [600.0, 60.0]).expect("room");
        p.connect(input, 0, out, WireEnd::Input(0), 1.0)
            .expect("valid");
        p
    }

    /// The module with an id.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Mutable module with an id.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Adds a module; `None` when the patch is full (or a fixed module already exists).
    pub fn add(&mut self, kind: ModuleKind, pos: [f32; 2]) -> Option<NodeId> {
        if self.nodes.len() >= MAX_NODES
            || (kind.is_fixed() && self.nodes.iter().any(|n| n.kind == kind))
        {
            return None;
        }
        let id = NodeId(self.nodes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0));
        self.nodes.push(Node::new(id, kind, pos));
        Some(id)
    }

    /// Adds a plugin module.
    pub fn add_plugin(&mut self, plugin: PluginRef, pos: [f32; 2]) -> Option<NodeId> {
        let id = self.add(ModuleKind::Plugin, pos)?;
        self.node_mut(id).expect("added").plugin = Some(Box::new(plugin));
        Some(id)
    }

    /// Removes a module and its wires (the fixed ones stay). False if nothing was removed.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let Some(i) = self.nodes.iter().position(|n| n.id == id) else {
            return false;
        };
        if self.nodes[i].kind.is_fixed() {
            return false;
        }
        self.nodes.remove(i);
        self.wires.retain(|w| w.from != id && w.to != id);
        true
    }

    /// The kind of a module's output port.
    pub fn output_kind(&self, id: NodeId, port: u8) -> Option<PortKind> {
        self.node(id)?
            .outputs()
            .get(usize::from(port))
            .map(|(_, k)| *k)
    }

    /// Checks a wire against the patch.
    pub fn check(&self, w: &Wire) -> Result<(), WireError> {
        let from = self.output_kind(w.from, w.port).ok_or(WireError::Missing)?;
        let to = self.node(w.to).ok_or(WireError::Missing)?;
        match w.end {
            WireEnd::Input(i) => {
                let (_, kind) = to.inputs().get(usize::from(i)).ok_or(WireError::Missing)?;
                if *kind != from {
                    return Err(WireError::Kinds);
                }
            }
            WireEnd::Param(i) => {
                if usize::from(i) >= to.param_count() {
                    return Err(WireError::Missing);
                }
                if from != PortKind::Mod {
                    return Err(WireError::NotMod);
                }
            }
        }
        Ok(())
    }

    /// Connects an output to an input or parameter.
    pub fn connect(
        &mut self,
        from: NodeId,
        port: u8,
        to: NodeId,
        end: WireEnd,
        amount: f32,
    ) -> Result<usize, WireError> {
        let w = Wire {
            from,
            port,
            to,
            end,
            amount: amount.clamp(-4.0, 4.0),
        };
        self.check(&w)?;
        if self
            .wires
            .iter()
            .any(|x| (x.from, x.port, x.to, x.end) == (from, port, to, end))
        {
            return Err(WireError::Duplicate);
        }
        if self.wires.len() >= MAX_WIRES {
            return Err(WireError::Full);
        }
        self.wires.push(w);
        Ok(self.wires.len() - 1)
    }

    /// The plugins in the patch, with their module ids.
    pub fn plugins(&self) -> impl Iterator<Item = (NodeId, &PluginRef)> {
        self.nodes
            .iter()
            .filter_map(|n| Some((n.id, n.plugin.as_deref()?)))
    }

    /// Mutable plugin of a module.
    pub fn plugin_mut(&mut self, id: NodeId) -> Option<&mut PluginRef> {
        self.node_mut(id)?.plugin.as_deref_mut()
    }

    /// Which modules run once per voice: those fed (through per-voice modules) by
    /// [`ModuleKind::Voices`]. Indexed like `nodes`.
    pub fn per_voice(&self) -> Vec<bool> {
        let mut poly: Vec<bool> = self
            .nodes
            .iter()
            .map(|n| n.kind == ModuleKind::Voices)
            .collect();
        let index = |id: NodeId| self.nodes.iter().position(|n| n.id == id);
        loop {
            let mut changed = false;
            for w in &self.wires {
                let (Some(a), Some(b)) = (index(w.from), index(w.to)) else {
                    continue;
                };
                if matches!(w.end, WireEnd::Input(_))
                    && poly[a]
                    && !poly[b]
                    && self.nodes[b].kind.per_voice()
                {
                    poly[b] = true;
                    changed = true;
                }
            }
            if !changed {
                return poly;
            }
        }
    }

    /// The modules in processing order (indices into `nodes`): each after the modules that
    /// feed it, except along loops, which are cut where they close.
    pub fn order(&self) -> Vec<usize> {
        let n = self.nodes.len();
        let index = |id: NodeId| self.nodes.iter().position(|x| x.id == id);
        let edges: Vec<(usize, usize)> = self
            .wires
            .iter()
            .filter_map(|w| Some((index(w.from)?, index(w.to)?)))
            .filter(|(a, b)| a != b)
            .collect();
        let mut done = vec![false; n];
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            // Ready: every feeder done. With none ready (a loop), take the first left.
            let next = (0..n)
                .find(|&i| !done[i] && edges.iter().all(|&(a, b)| b != i || done[a] || a == i))
                .or_else(|| (0..n).find(|&i| !done[i]))
                .expect("some left");
            done[next] = true;
            out.push(next);
        }
        out
    }

    /// Drops wires that no longer fit, repairs values, and adds missing fixed modules.
    pub fn sanitize(&mut self) {
        self.nodes.truncate(MAX_NODES);
        let mut seen = std::collections::BTreeSet::new();
        self.nodes.retain(|n| seen.insert(n.id));
        for kind in [ModuleKind::NoteIn, ModuleKind::AudioIn, ModuleKind::Output] {
            let mut first = true;
            self.nodes.retain(|n| {
                if n.kind != kind {
                    return true;
                }
                std::mem::replace(&mut first, false)
            });
            if first {
                let id = NodeId(self.nodes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0));
                self.nodes.push(Node::new(id, kind, [20.0, 20.0]));
            }
        }
        for n in &mut self.nodes {
            let infos = n.kind.params();
            n.params.resize(infos.len(), 0.0);
            for (v, info) in n.params.iter_mut().zip(infos) {
                *v = if v.is_finite() {
                    info.clamp(*v)
                } else {
                    info.default
                };
            }
            if !n.pos.iter().all(|x| x.is_finite()) {
                n.pos = [0.0, 0.0];
            }
            if n.kind == ModuleKind::Synth && n.synth.is_none() {
                n.synth = Some(Box::default());
            }
            if let Some(s) = &mut n.synth {
                s.sanitize();
            }
            if let Some(p) = &mut n.plugin {
                p.sanitize();
            }
        }
        let wires = std::mem::take(&mut self.wires);
        for w in wires {
            let amount = if w.amount.is_finite() { w.amount } else { 0.0 };
            let _ = self.connect(w.from, w.port, w.to, w.end, amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starting_patches_are_valid_and_ordered() {
        let p = Patch::instrument();
        assert_eq!(p.wires.len(), 8);
        let order = p.order();
        let at = |k: ModuleKind| {
            let i = p.nodes.iter().position(|n| n.kind == k).unwrap();
            order.iter().position(|&x| x == i).unwrap()
        };
        assert!(at(ModuleKind::Voices) < at(ModuleKind::Oscillator));
        assert!(at(ModuleKind::Filter) < at(ModuleKind::Amp));
        assert!(at(ModuleKind::Envelope) < at(ModuleKind::Amp));
        assert!(at(ModuleKind::Amp) < at(ModuleKind::Output));
        let poly = p.per_voice();
        let is = |k: ModuleKind| poly[p.nodes.iter().position(|n| n.kind == k).unwrap()];
        assert!(is(ModuleKind::Oscillator) && is(ModuleKind::Amp));
        assert!(!is(ModuleKind::Output) && !is(ModuleKind::NoteIn));
        assert_eq!(Patch::effect().wires.len(), 1);
    }

    #[test]
    fn wires_are_checked() {
        let mut p = Patch::effect();
        let input = p.nodes[1].id;
        let lfo = p.add(ModuleKind::Lfo, [0.0, 0.0]).unwrap();
        let fx = p
            .add(ModuleKind::Effect(EffectKind::Delay), [0.0, 0.0])
            .unwrap();
        assert_eq!(
            p.connect(lfo, 0, fx, WireEnd::Input(0), 1.0),
            Err(WireError::Kinds)
        );
        assert_eq!(
            p.connect(input, 0, fx, WireEnd::Param(0), 1.0),
            Err(WireError::NotMod)
        );
        assert!(p.connect(lfo, 0, fx, WireEnd::Param(1), 0.5).is_ok());
        assert_eq!(
            p.connect(lfo, 0, fx, WireEnd::Param(1), 0.2),
            Err(WireError::Duplicate)
        );
        assert_eq!(
            p.connect(lfo, 0, fx, WireEnd::Param(999), 0.2),
            Err(WireError::Missing)
        );
        // The fixed modules stay; others go with their wires.
        assert!(!p.remove(input));
        assert!(p.remove(lfo));
        assert_eq!(p.wires.len(), 1);
        assert!(p.add(ModuleKind::Output, [0.0, 0.0]).is_none());
    }

    #[test]
    fn loops_still_order_every_module() {
        let mut p = Patch::effect();
        let a = p.add(ModuleKind::Math, [0.0, 0.0]).unwrap();
        let b = p.add(ModuleKind::Math, [0.0, 0.0]).unwrap();
        p.connect(a, 0, b, WireEnd::Input(0), 1.0).unwrap();
        p.connect(b, 0, a, WireEnd::Input(0), 1.0).unwrap();
        p.connect(a, 0, a, WireEnd::Param(1), 1.0).unwrap();
        let order = p.order();
        assert_eq!(order.len(), p.nodes.len());
        let mut sorted = order.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), p.nodes.len());
    }

    #[test]
    fn sanitize_repairs_a_damaged_patch() {
        let mut p = Patch::instrument();
        p.nodes.retain(|n| n.kind != ModuleKind::Output);
        p.nodes[3].params[1] = f32::NAN;
        p.nodes[3].params.push(5.0);
        p.wires.push(Wire {
            from: NodeId(99),
            port: 0,
            to: NodeId(0),
            end: WireEnd::Input(0),
            amount: 1.0,
        });
        p.sanitize();
        assert_eq!(
            p.nodes
                .iter()
                .filter(|n| n.kind == ModuleKind::Output)
                .count(),
            1
        );
        assert_eq!(p.nodes[3].params.len(), p.nodes[3].kind.params().len());
        assert!(p.nodes[3].params.iter().all(|v| v.is_finite()));
        // The wire into the old output and the dangling one are gone.
        assert_eq!(p.wires.len(), 7);
        for k in ModuleKind::BUILT_IN
            .into_iter()
            .chain([ModuleKind::Effect(EffectKind::Reverb), ModuleKind::Plugin])
        {
            assert_eq!(ModuleKind::from_key(&k.key()), Some(k));
        }
    }
}
