//! The modular patcher on the audio thread (Prompt 34, ARCHITECTURE.md D154).
//!
//! A [`PatchDsp`] is built from a [`Patch`] on the UI thread: modules in processing order,
//! each with its state, output buffers and resolved input wires, all allocated up front. Each
//! quantum every module runs once: per-voice modules once per sounding voice lane, the rest
//! once with the voices mixed. Modulation is one value per quantum (an amp interpolates its
//! gain across it); notes act at the start of the quantum they fall in.

// Lanes, channels and frames index several parallel buffers at once.
#![allow(clippy::needless_range_loop)]

use gt_core::patcher::{ModuleKind, NodeId, Patch, WireEnd, PATCH_VOICES};
use gt_core::{ParamInfo, PluginInstanceId, PluginKind};
use gt_dsp::fx::{Effect, FxContext};
use gt_dsp::{blep_sample, Adsr, AdsrParams, GloomSynth, Phase, Wave};

use crate::plugin::{PluginContext, PluginTable};
use crate::{Telemetry, RENDER_QUANTUM};

const Q: usize = RENDER_QUANTUM;
const V: usize = PATCH_VOICES;
/// Notes per MIDI buffer and quantum; more are dropped.
const MAX_NOTES: usize = 128;
/// Middle C, the pitch of an oscillator with nothing on its pitch input.
const MIDDLE_C_HZ: f32 = 261.625_56;
/// Key of a "release everything" event.
const ALL_OFF: u8 = 255;

type Stereo = [[f32; Q]; 2];

/// One note event: a velocity of 0 is a note off.
#[derive(Debug, Clone, Copy, Default)]
struct NoteEv {
    offset: u32,
    key: u8,
    velocity: f32,
}

/// The notes of one quantum.
#[derive(Clone, Copy)]
struct Notes {
    len: usize,
    ev: [NoteEv; MAX_NOTES],
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            len: 0,
            ev: [NoteEv::default(); MAX_NOTES],
        }
    }
}

impl Notes {
    fn push(&mut self, e: NoteEv) {
        if self.len < MAX_NOTES {
            self.ev[self.len] = e;
            self.len += 1;
        }
    }

    fn as_slice(&self) -> &[NoteEv] {
        &self.ev[..self.len]
    }
}

/// An output's buffer.
#[derive(Debug, Clone, Copy)]
enum Buf {
    Audio(usize),
    Mod(usize),
    Midi(usize),
}

/// A resolved wire into a module.
#[derive(Debug, Clone, Copy)]
struct Feed {
    buf: Buf,
    /// The source runs per voice.
    poly: bool,
    end: WireEnd,
    amount: f32,
}

/// Each module's own state.
enum State {
    None,
    Voices(Box<VoicesState>),
    Osc([Phase; V]),
    Noise([u32; V]),
    /// Two integrator states per channel and lane.
    Filter([[[f32; 2]; 2]; V]),
    Env {
        adsr: [Adsr; V],
        gate: [bool; V],
    },
    Lfo {
        phase: f32,
        held: f32,
        rng: u32,
    },
    /// The previous quantum's gain per lane.
    Amp([f32; V]),
    /// The lag's state per lane.
    Math([f32; V]),
    /// The shifted key sent for each held input key.
    Transpose([u8; 128]),
    Effect(Box<dyn Effect>),
    Synth {
        synth: Box<GloomSynth>,
        age: u64,
    },
    Plugin {
        instance: PluginInstanceId,
        kind: PluginKind,
    },
}

#[derive(Default)]
struct VoicesState {
    key: [u8; V],
    pitch: [f32; V],
    target: [f32; V],
    gate: [bool; V],
    velocity: [f32; V],
    age: [u64; V],
    /// A note waiting one quantum for its stolen lane's gate to fall.
    pending: [Option<(u8, f32)>; V],
    clock: u64,
}

struct NodeDsp {
    id: NodeId,
    kind: ModuleKind,
    poly: bool,
    infos: &'static [ParamInfo],
    /// The document's values (plain; normalized for plugins).
    base: Vec<f32>,
    /// Values with modulation, per lane.
    values: Vec<[f32; V]>,
    /// The values last handed to an effect or plugin.
    sent: Vec<f32>,
    feeds: Vec<Feed>,
    outs: [Option<Buf>; 3],
    state: State,
}

/// New values for a patch whose structure did not change: each module's parameter values
/// and each wire's amount, in the order of the running [`PatchDsp`].
#[derive(Debug, Clone, PartialEq)]
pub struct PatchUpdate {
    values: Vec<Vec<f32>>,
    amounts: Vec<f32>,
}

impl PatchUpdate {
    /// The values of `patch`, for a [`PatchDsp`] built from a patch of the same structure.
    pub fn of(patch: &Patch) -> Self {
        let order = patch.order();
        let values = order
            .iter()
            .map(|&i| {
                let n = &patch.nodes[i];
                match &n.plugin {
                    Some(p) => p.values.clone(),
                    None => n.params.clone(),
                }
            })
            .collect();
        let amounts = order
            .iter()
            .flat_map(|&i| {
                let id = patch.nodes[i].id;
                patch
                    .wires
                    .iter()
                    .filter(move |w| w.to == id && patch.node(w.from).is_some())
                    .map(|w| w.amount)
            })
            .collect();
        Self { values, amounts }
    }
}

/// True when two patches build the same [`PatchDsp`] apart from values (so a
/// [`PatchUpdate`] carries the difference).
pub fn same_structure(a: &Patch, b: &Patch) -> bool {
    a.nodes.len() == b.nodes.len()
        && a.wires.len() == b.wires.len()
        && a.nodes.iter().zip(&b.nodes).all(|(x, y)| {
            x.id == y.id
                && x.kind == y.kind
                && x.synth == y.synth
                && x.plugin
                    .as_ref()
                    .map(|p| (p.instance, p.values.len(), p.kind, p.sends_midi))
                    == y.plugin
                        .as_ref()
                        .map(|p| (p.instance, p.values.len(), p.kind, p.sends_midi))
        })
        && a.wires
            .iter()
            .zip(&b.wires)
            .all(|(x, y)| (x.from, x.port, x.to, x.end) == (y.from, y.port, y.to, y.end))
}

/// Which patch a [`PatchUpdate`] is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchTarget {
    /// A patcher channel's, by slot.
    Channel(u16),
    /// A patcher effect's.
    Effect {
        /// Strip index.
        strip: u16,
        /// Slot index.
        slot: u8,
    },
}

/// A patch ready to run. Built (allocating) on the UI thread with [`PatchDsp::build`].
pub struct PatchDsp {
    sr: f32,
    nodes: Vec<NodeDsp>,
    audio: Vec<[Stereo; V]>,
    mods: Vec<[f32; V]>,
    midi: Vec<Notes>,
    /// Notes the channel plays, for the next quantum.
    notes_in: Notes,
    /// Audio coming in (an effect's input).
    audio_in: Stereo,
    /// Gathered input audio of the module running.
    scratch: [Stereo; V],
    /// Lanes that have played a note (the rest stay silent and are skipped).
    lanes: usize,
    /// The lane of the newest note, read by modules without voices.
    newest: usize,
    plugin_notes: Notes,
}

impl std::fmt::Debug for PatchDsp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PatchDsp({} modules)", self.nodes.len())
    }
}

fn xorshift(s: &mut u32) -> f32 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *s = x;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Equal-power pan gains.
fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (
        theta.cos() * std::f32::consts::SQRT_2,
        theta.sin() * std::f32::consts::SQRT_2,
    )
}

impl PatchDsp {
    /// Builds a patch at `sample_rate`. Allocates: call on the UI thread.
    pub fn build(patch: &Patch, sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let order = patch.order();
        let poly = patch.per_voice();
        let (mut audio, mut mods, mut midi) = (0, 0, 0);
        // Output buffers by (node index, port).
        let mut bufs: Vec<[Option<Buf>; 3]> = vec![[None; 3]; patch.nodes.len()];
        for (i, n) in patch.nodes.iter().enumerate() {
            for (k, (_, kind)) in n.outputs().iter().enumerate().take(3) {
                bufs[i][k] = Some(match kind {
                    gt_core::patcher::PortKind::Audio => {
                        audio += 1;
                        Buf::Audio(audio - 1)
                    }
                    gt_core::patcher::PortKind::Mod => {
                        mods += 1;
                        Buf::Mod(mods - 1)
                    }
                    gt_core::patcher::PortKind::Midi => {
                        midi += 1;
                        Buf::Midi(midi - 1)
                    }
                });
            }
        }
        let index = |id: NodeId| patch.nodes.iter().position(|n| n.id == id);
        let nodes = order
            .iter()
            .map(|&i| {
                let n = &patch.nodes[i];
                let feeds = patch
                    .wires
                    .iter()
                    .filter(|w| w.to == n.id)
                    .filter_map(|w| {
                        let src = index(w.from)?;
                        Some(Feed {
                            buf: bufs[src][usize::from(w.port).min(2)]?,
                            poly: poly[src],
                            end: w.end,
                            amount: w.amount,
                        })
                    })
                    .collect();
                let base = match &n.plugin {
                    Some(p) => p.values.clone(),
                    None => n.params.clone(),
                };
                let state = match n.kind {
                    ModuleKind::Voices => State::Voices(Box::default()),
                    ModuleKind::Oscillator => State::Osc([Phase::default(); V]),
                    ModuleKind::Noise => {
                        State::Noise(std::array::from_fn(|k| 0x9E37_79B9 ^ (k as u32 * 7919 + 1)))
                    }
                    ModuleKind::Filter => State::Filter([[[0.0; 2]; 2]; V]),
                    ModuleKind::Envelope => State::Env {
                        adsr: [Adsr::default(); V],
                        gate: [false; V],
                    },
                    ModuleKind::Lfo => State::Lfo {
                        phase: 0.0,
                        held: 0.0,
                        rng: 0x1234_5678 ^ u32::from(n.id.0),
                    },
                    ModuleKind::Amp => State::Amp([0.0; V]),
                    ModuleKind::Math => State::Math([0.0; V]),
                    ModuleKind::Transpose => State::Transpose([ALL_OFF; 128]),
                    ModuleKind::Effect(kind) => {
                        let mut slot = gt_core::EffectSlot::new(kind);
                        slot.params.clone_from(&n.params);
                        State::Effect(crate::create_effect(&slot, sr).0)
                    }
                    ModuleKind::Synth => {
                        let patch = n.synth.as_deref().cloned().unwrap_or_default();
                        let mut synth = Box::new(GloomSynth::new(sr));
                        synth.set_tables(crate::tables::synth_tables(&patch, |_| None));
                        synth.set_settings(&crate::song::synth_settings(&patch));
                        State::Synth { synth, age: 0 }
                    }
                    ModuleKind::Plugin => match &n.plugin {
                        Some(p) => State::Plugin {
                            instance: p.instance,
                            kind: p.kind,
                        },
                        None => State::None,
                    },
                    ModuleKind::NoteIn
                    | ModuleKind::AudioIn
                    | ModuleKind::Output
                    | ModuleKind::Mixer => State::None,
                };
                NodeDsp {
                    id: n.id,
                    kind: n.kind,
                    poly: poly[i],
                    infos: n.kind.params(),
                    values: base.iter().map(|&v| [v; V]).collect(),
                    sent: vec![f32::NAN; base.len()],
                    base,
                    feeds,
                    outs: bufs[i],
                    state,
                }
            })
            .collect();
        Self {
            sr,
            nodes,
            audio: vec![[[[0.0; Q]; 2]; V]; audio],
            mods: vec![[0.0; V]; mods],
            midi: vec![Notes::default(); midi],
            notes_in: Notes::default(),
            audio_in: [[0.0; Q]; 2],
            scratch: [[[0.0; Q]; 2]; V],
            lanes: 0,
            newest: 0,
            plugin_notes: Notes::default(),
        }
    }

    /// Takes over the running state (voices, phases, envelopes, effect tails) of the modules
    /// `old` shares with this patch. Swaps only: real-time safe.
    pub fn inherit(&mut self, old: &mut PatchDsp) {
        self.lanes = old.lanes;
        self.newest = old.newest;
        for n in &mut self.nodes {
            let Some(o) = old
                .nodes
                .iter_mut()
                .find(|o| o.id == n.id && o.kind == n.kind)
            else {
                continue;
            };
            match (&mut n.state, &mut o.state) {
                (State::Synth { .. }, _) | (State::Plugin { .. }, _) => {}
                (a, b) => std::mem::swap(a, b),
            }
            // An effect taken over gets this patch's values on its next quantum.
            n.sent.fill(f32::NAN);
        }
    }

    /// Replaces the values (same structure); hands the old ones back in `v`.
    pub fn set_values(&mut self, v: &mut PatchUpdate) {
        for (n, vals) in self.nodes.iter_mut().zip(v.values.iter_mut()) {
            if n.base.len() == vals.len() {
                std::mem::swap(&mut n.base, vals);
            }
        }
        let mut amounts = v.amounts.iter();
        for n in &mut self.nodes {
            for f in &mut n.feeds {
                if let Some(&a) = amounts.next() {
                    f.amount = a;
                }
            }
        }
    }

    /// Queues a note for the next quantum.
    pub fn note_on(&mut self, offset: u32, key: u8, velocity: f32) {
        self.notes_in.push(NoteEv {
            offset,
            key,
            velocity: velocity.max(1e-3),
        });
    }

    /// Queues a note off.
    pub fn note_off(&mut self, offset: u32, key: u8) {
        self.notes_in.push(NoteEv {
            offset,
            key,
            velocity: 0.0,
        });
    }

    /// Releases every note.
    pub fn release_all(&mut self, offset: u32) {
        self.notes_in.push(NoteEv {
            offset,
            key: ALL_OFF,
            velocity: 0.0,
        });
    }

    /// Runs one quantum: `l` and `r` hold the input (silence for an instrument) and get the
    /// output. Plugin modules run from `plugins`; without them they pass audio through.
    pub(crate) fn run(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        fx: &FxContext,
        pctx: &PluginContext,
        mut plugins: Option<(&mut PluginTable, &Telemetry)>,
    ) {
        let n = l.len().min(r.len()).min(Q);
        self.audio_in[0][..n].copy_from_slice(&l[..n]);
        self.audio_in[1][..n].copy_from_slice(&r[..n]);
        self.audio_in[0][n..].fill(0.0);
        self.audio_in[1][n..].fill(0.0);
        l[..n].fill(0.0);
        r[..n].fill(0.0);
        let sr = self.sr;
        for k in 0..self.nodes.len() {
            self.update_values(k);
            let node = &self.nodes[k];
            let lanes = if node.poly { self.lanes.max(1) } else { 1 };
            match node.kind {
                ModuleKind::NoteIn => {
                    if let Some(Buf::Midi(b)) = node.outs[0] {
                        self.midi[b] = self.notes_in;
                    }
                }
                ModuleKind::AudioIn => {
                    if let Some(Buf::Audio(b)) = node.outs[0] {
                        self.audio[b][0] = self.audio_in;
                    }
                }
                ModuleKind::Output => {
                    self.gather_audio(k, 0, false);
                    let g = self.nodes[k].values[0][0];
                    for i in 0..n {
                        l[i] = self.scratch[0][0][i] * g;
                        r[i] = self.scratch[0][1][i] * g;
                    }
                }
                ModuleKind::Voices => self.run_voices(k),
                ModuleKind::Oscillator => {
                    let pitch = self.gather_mod(k, 0, 0.0);
                    let node = &mut self.nodes[k];
                    let Some(Buf::Audio(b)) = node.outs[0] else {
                        continue;
                    };
                    let State::Osc(phase) = &mut node.state else {
                        continue;
                    };
                    for lane in 0..lanes {
                        let v = &node.values;
                        let wave = match v[0][lane] as usize {
                            0 => Wave::Sine,
                            1 => Wave::Triangle,
                            2 => Wave::Saw,
                            _ => Wave::Square,
                        };
                        let oct =
                            pitch[lane] + v[1][lane] + v[2][lane] / 12.0 + v[3][lane] / 1200.0;
                        let dt = (MIDDLE_C_HZ * oct.clamp(-10.0, 7.0).exp2() / sr).min(0.49);
                        let (pw, level) = (v[4][lane], v[5][lane]);
                        let out = &mut self.audio[b][lane];
                        for i in 0..Q {
                            let t = phase[lane].advance(dt);
                            out[0][i] = blep_sample(wave, t, dt, pw) * level;
                        }
                        out[1] = out[0];
                    }
                }
                ModuleKind::Noise => {
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Audio(b)), State::Noise(rng)) = (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for lane in 0..lanes {
                        let level = node.values[0][lane];
                        let out = &mut self.audio[b][lane];
                        for i in 0..Q {
                            out[0][i] = xorshift(&mut rng[lane]) * level;
                            out[1][i] = xorshift(&mut rng[lane]) * level;
                        }
                    }
                }
                ModuleKind::Filter => {
                    self.gather_audio(k, 0, node.poly);
                    let pitch = self.gather_mod(k, 1, 0.0);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Audio(b)), State::Filter(ic)) = (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for lane in 0..lanes {
                        let v = &node.values;
                        let mode = v[0][lane] as usize;
                        let fc = (v[1][lane]
                            * (pitch[lane] * v[3][lane]).clamp(-10.0, 10.0).exp2())
                        .clamp(20.0, 0.45 * sr);
                        let g = (std::f32::consts::PI * fc / sr).tan();
                        let kq = 2.0 - 1.96 * v[2][lane].clamp(0.0, 1.0);
                        let a1 = 1.0 / (1.0 + g * (g + kq));
                        let (a2, a3) = (g * a1, g * g * a1);
                        for ch in 0..2 {
                            let [s1, s2] = &mut ic[lane][ch];
                            let x = &self.scratch[lane][ch];
                            let out = &mut self.audio[b][lane][ch];
                            for i in 0..Q {
                                let v0 = x[i];
                                let v3 = v0 - *s2;
                                let v1 = a1 * *s1 + a2 * v3;
                                let v2 = *s2 + a2 * *s1 + a3 * v3;
                                *s1 = 2.0 * v1 - *s1;
                                *s2 = 2.0 * v2 - *s2;
                                out[i] = match mode {
                                    0 => v2,
                                    1 => v0 - kq * v1 - v2,
                                    2 => v1,
                                    _ => v0 - kq * v1,
                                };
                            }
                        }
                    }
                }
                ModuleKind::Envelope => {
                    let gate = self.gather_mod(k, 0, 0.0);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Mod(b)), State::Env { adsr, gate: held }) =
                        (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    let rate = sr / Q as f32;
                    for lane in 0..lanes {
                        let v = &node.values;
                        let p =
                            AdsrParams::new(rate, v[0][lane], v[1][lane], v[2][lane], v[3][lane]);
                        let on = gate[lane] > 0.5;
                        if on && !held[lane] {
                            adsr[lane].retrigger(p);
                        } else if !on && held[lane] {
                            adsr[lane].set_params(p);
                            adsr[lane].release();
                        } else {
                            adsr[lane].set_params(p);
                        }
                        held[lane] = on;
                        self.mods[b][lane] = adsr[lane].next_level();
                    }
                }
                ModuleKind::Lfo => {
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Mod(b)), State::Lfo { phase, held, rng }) =
                        (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    let (shape, rate) = (node.values[0][0] as usize, node.values[1][0]);
                    let t = *phase;
                    let v = match shape {
                        0 => (t * std::f32::consts::TAU).sin(),
                        1 => 1.0 - 4.0 * (t - 0.5).abs(),
                        2 => 2.0 * t - 1.0,
                        3 => {
                            if t < 0.5 {
                                1.0
                            } else {
                                -1.0
                            }
                        }
                        _ => *held,
                    };
                    let next = t + rate.max(0.0) * Q as f32 / sr;
                    if next >= 1.0 {
                        *held = xorshift(rng);
                    }
                    *phase = next.fract();
                    self.mods[b] = [v; V];
                }
                ModuleKind::Amp => {
                    let has_gain = node.feeds.iter().any(|f| f.end == WireEnd::Input(1));
                    self.gather_audio(k, 0, node.poly);
                    let gain = self.gather_mod(k, 1, 0.0);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Audio(b)), State::Amp(prev)) = (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for lane in 0..lanes {
                        let g = node.values[0][lane] * if has_gain { gain[lane] } else { 1.0 };
                        let (pl, pr) = pan_gains(node.values[1][lane]);
                        let from = prev[lane];
                        let step = (g - from) / Q as f32;
                        let x = &self.scratch[lane];
                        let out = &mut self.audio[b][lane];
                        for i in 0..Q {
                            let gi = from + step * (i + 1) as f32;
                            out[0][i] = x[0][i] * gi * pl;
                            out[1][i] = x[1][i] * gi * pr;
                        }
                        prev[lane] = g;
                    }
                }
                ModuleKind::Mixer => {
                    let poly = node.poly;
                    let Some(Buf::Audio(b)) = node.outs[0] else {
                        continue;
                    };
                    for lane in 0..lanes {
                        self.audio[b][lane] = [[0.0; Q]; 2];
                    }
                    for input in 0..4u8 {
                        self.gather_audio(k, input, poly);
                        for lane in 0..lanes {
                            let g = self.nodes[k].values[usize::from(input)][lane];
                            for ch in 0..2 {
                                for i in 0..Q {
                                    self.audio[b][lane][ch][i] += self.scratch[lane][ch][i] * g;
                                }
                            }
                        }
                    }
                }
                ModuleKind::Math => {
                    let op = node.values[0][0] as usize;
                    let a = self.gather_mod(k, 0, 0.0);
                    let b_default = if op == 1 { 1.0 } else { 0.0 };
                    let has_b = node.feeds.iter().any(|f| f.end == WireEnd::Input(1));
                    let bv = self.gather_mod(k, 1, b_default);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Mod(out)), State::Math(lag)) = (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for lane in 0..lanes {
                        let v = &node.values;
                        let b = if has_b { bv[lane] } else { b_default };
                        let x = match op {
                            0 => a[lane] + b,
                            1 => a[lane] * b,
                            2 if has_b => a[lane].min(b),
                            3 if has_b => a[lane].max(b),
                            4 => {
                                let frames = v[3][lane] * 0.001 * sr / Q as f32;
                                let c = if frames < 1.0 { 1.0 } else { 1.0 / frames };
                                lag[lane] += (a[lane] - lag[lane]) * c;
                                lag[lane]
                            }
                            _ => a[lane],
                        };
                        let y = x * v[1][lane] + v[2][lane];
                        self.mods[out][lane] = if y.is_finite() { y } else { 0.0 };
                    }
                }
                ModuleKind::Transpose => {
                    let notes = self.gather_notes(k);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Midi(b)), State::Transpose(held)) =
                        (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    let semis = node.values[0][0] as i16;
                    let mut out = Notes::default();
                    for e in notes.as_slice() {
                        if e.key == ALL_OFF {
                            *held = [ALL_OFF; 128];
                            out.push(*e);
                        } else if e.velocity > 0.0 {
                            let key = (i16::from(e.key) + semis).clamp(0, 127) as u8;
                            held[usize::from(e.key & 0x7F)] = key;
                            out.push(NoteEv { key, ..*e });
                        } else {
                            let k = &mut held[usize::from(e.key & 0x7F)];
                            if *k != ALL_OFF {
                                out.push(NoteEv { key: *k, ..*e });
                                *k = ALL_OFF;
                            }
                        }
                    }
                    self.midi[b] = out;
                }
                ModuleKind::Effect(_) => {
                    self.gather_audio(k, 0, false);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Audio(b)), State::Effect(fx_box)) =
                        (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for (i, (v, s)) in node.values.iter().zip(node.sent.iter_mut()).enumerate() {
                        if v[0] != *s {
                            fx_box.set_param(i, v[0]);
                            *s = v[0];
                        }
                    }
                    let out = &mut self.audio[b][0];
                    *out = self.scratch[0];
                    let [ol, or] = out;
                    fx_box.process(ol, or, fx);
                }
                ModuleKind::Synth => {
                    let notes = self.gather_notes(k);
                    let node = &mut self.nodes[k];
                    let (Some(Buf::Audio(b)), State::Synth { synth, age }) =
                        (node.outs[0], &mut node.state)
                    else {
                        continue;
                    };
                    for e in notes.as_slice() {
                        if e.key == ALL_OFF {
                            synth.release_all();
                        } else if e.velocity > 0.0 {
                            *age += 1;
                            synth.note_on(e.key, e.velocity, *age);
                        } else {
                            synth.note_off(e.key);
                        }
                    }
                    let [ol, or] = &mut self.audio[b][0];
                    ol.fill(0.0);
                    or.fill(0.0);
                    if synth.is_active() {
                        synth.render(ol, or);
                    }
                }
                ModuleKind::Plugin => {
                    let notes = self.gather_notes(k);
                    self.gather_audio(k, 0, false);
                    let node = &mut self.nodes[k];
                    let State::Plugin { instance, kind } = node.state else {
                        continue;
                    };
                    let Some(Buf::Audio(b)) = node.outs[0] else {
                        continue;
                    };
                    let out = &mut self.audio[b][0];
                    *out = if kind == PluginKind::Effect {
                        self.scratch[0]
                    } else {
                        [[0.0; Q]; 2]
                    };
                    let Some((table, telemetry)) = plugins.as_mut() else {
                        continue;
                    };
                    let index = table.find(instance);
                    if let Some(i) = index {
                        for (p, (v, s)) in node.values.iter().zip(node.sent.iter_mut()).enumerate()
                        {
                            // Only modulated values: the plugin host sends the document's.
                            if v[0] != *s && v[0] != node.base[p] {
                                table.set_param_at(i, p as u32, v[0]);
                                *s = v[0];
                            }
                        }
                    }
                    let [ol, or] = out;
                    if kind == PluginKind::Effect {
                        table.run_effect(index, ol, or, pctx, telemetry);
                    } else {
                        for e in notes.as_slice() {
                            if e.key == ALL_OFF {
                                table.release_all(index, e.offset);
                            } else if e.velocity > 0.0 {
                                table.note_on(index, e.offset, e.key, e.velocity);
                            } else {
                                table.note_off(index, e.offset, e.key);
                            }
                        }
                        table.run_instrument(index, ol, or, pctx, telemetry);
                    }
                    if let Some(Buf::Midi(m)) = node.outs[1] {
                        let sent = &mut self.plugin_notes;
                        sent.len = 0;
                        table.midi_out(index, &mut |offset, msg| {
                            let (status, key, vel) = (msg[0] & 0xF0, msg[1] & 0x7F, msg[2]);
                            if status == 0x90 && vel > 0 {
                                sent.push(NoteEv {
                                    offset,
                                    key,
                                    velocity: f32::from(vel) / 127.0,
                                });
                            } else if status == 0x80 || status == 0x90 {
                                sent.push(NoteEv {
                                    offset,
                                    key,
                                    velocity: 0.0,
                                });
                            }
                        });
                        self.midi[m] = self.plugin_notes;
                    }
                }
            }
        }
        self.notes_in.len = 0;
    }

    /// Applies modulation to module `k`'s values for this quantum.
    fn update_values(&mut self, k: usize) {
        let lanes = if self.nodes[k].poly {
            self.lanes.max(1)
        } else {
            1
        };
        let newest = self.newest;
        let node = &mut self.nodes[k];
        let plugin = matches!(node.state, State::Plugin { .. });
        for (p, (vals, &base)) in node.values.iter_mut().zip(&node.base).enumerate() {
            let mut moved = false;
            let info = node.infos.get(p);
            let start = match (plugin, info) {
                (true, _) => base,
                (false, Some(info)) => info.to_normalized(base),
                (false, None) => base,
            };
            let mut t = [start; V];
            for f in &node.feeds {
                if f.end != WireEnd::Param(p as u16) {
                    continue;
                }
                let Buf::Mod(b) = f.buf else {
                    continue;
                };
                moved = true;
                for (lane, t) in t.iter_mut().enumerate().take(lanes) {
                    let src = if !f.poly {
                        0
                    } else if node.poly {
                        lane
                    } else {
                        newest
                    };
                    *t += f.amount * self.mods[b][src];
                }
            }
            for lane in 0..lanes {
                vals[lane] = if !moved {
                    base
                } else if plugin {
                    t[lane].clamp(0.0, 1.0)
                } else if let Some(info) = info {
                    info.from_normalized(t[lane].clamp(0.0, 1.0))
                } else {
                    base
                };
            }
        }
    }

    /// Sums the modulation wires into input `port` of module `k`, per lane.
    fn gather_mod(&self, k: usize, port: u8, unconnected: f32) -> [f32; V] {
        let node = &self.nodes[k];
        let mut out = [0.0; V];
        let mut any = false;
        for f in &node.feeds {
            let (WireEnd::Input(p), Buf::Mod(b)) = (f.end, f.buf) else {
                continue;
            };
            if p != port {
                continue;
            }
            any = true;
            for (lane, o) in out.iter_mut().enumerate() {
                let src = if !f.poly {
                    0
                } else if node.poly {
                    lane
                } else {
                    self.newest
                };
                *o += f.amount * self.mods[b][src];
            }
        }
        if any {
            out
        } else {
            [unconnected; V]
        }
    }

    /// Sums the audio wires into input `port` of module `k` into `scratch`: per lane for a
    /// per-voice module, mixed into lane 0 otherwise.
    fn gather_audio(&mut self, k: usize, port: u8, poly: bool) {
        let lanes = if poly { self.lanes.max(1) } else { 1 };
        let used = self.lanes.max(1);
        for s in &mut self.scratch[..lanes] {
            *s = [[0.0; Q]; 2];
        }
        let node = &self.nodes[k];
        for f in &node.feeds {
            let (WireEnd::Input(p), Buf::Audio(b)) = (f.end, f.buf) else {
                continue;
            };
            if p != port {
                continue;
            }
            let src = &self.audio[b];
            match (f.poly, poly) {
                (true, true) => {
                    for lane in 0..lanes {
                        add(&mut self.scratch[lane], &src[lane], f.amount);
                    }
                }
                (true, false) => {
                    for s in &src[..used] {
                        add(&mut self.scratch[0], s, f.amount);
                    }
                }
                (false, _) => {
                    for lane in 0..lanes {
                        add(&mut self.scratch[lane], &src[0], f.amount);
                    }
                }
            }
        }
    }

    /// The notes on module `k`'s MIDI inputs, in wire order.
    fn gather_notes(&self, k: usize) -> Notes {
        let mut out = Notes::default();
        for f in &self.nodes[k].feeds {
            if let Buf::Midi(b) = f.buf {
                for e in self.midi[b].as_slice() {
                    out.push(*e);
                }
            }
        }
        out
    }

    fn run_voices(&mut self, k: usize) {
        let notes = self.gather_notes(k);
        let sr = self.sr;
        let node = &mut self.nodes[k];
        let count = (node.values[0][0] as usize).clamp(1, V);
        let glide_ms = node.values[1][0];
        let State::Voices(s) = &mut node.state else {
            return;
        };
        // Notes held back for a stolen lane start now.
        for lane in 0..V {
            if let Some((key, velocity)) = s.pending[lane].take() {
                start(s, lane, key, velocity, glide_ms);
            }
        }
        for e in notes.as_slice() {
            if e.key == ALL_OFF {
                s.gate = [false; V];
                s.pending = [None; V];
            } else if e.velocity > 0.0 {
                // A free lane (the longest released), else the oldest note's.
                let lane = (0..count)
                    .filter(|&i| !s.gate[i] && s.pending[i].is_none())
                    .min_by_key(|&i| s.age[i])
                    .unwrap_or_else(|| (0..count).min_by_key(|&i| s.age[i]).unwrap_or(0));
                s.clock += 1;
                s.age[lane] = s.clock;
                if s.gate[lane] {
                    // Its gate falls for a quantum so envelopes start again.
                    s.gate[lane] = false;
                    s.pending[lane] = Some((e.key, e.velocity));
                } else {
                    start(s, lane, e.key, e.velocity, glide_ms);
                }
                self.newest = lane;
                self.lanes = self.lanes.max(lane + 1);
            } else {
                for lane in 0..V {
                    if s.key[lane] == e.key && s.gate[lane] {
                        s.gate[lane] = false;
                    }
                    if s.pending[lane].is_some_and(|(key, _)| key == e.key) {
                        s.pending[lane] = None;
                    }
                }
            }
        }
        let c = if glide_ms <= 0.0 {
            1.0
        } else {
            1.0 - (-(Q as f32) / (glide_ms * 0.001 * sr)).exp()
        };
        for lane in 0..V {
            s.pitch[lane] += (s.target[lane] - s.pitch[lane]) * c;
        }
        let outs = node.outs;
        let values = [
            s.pitch,
            s.gate.map(|g| if g { 1.0 } else { 0.0 }),
            s.velocity,
        ];
        for (o, v) in outs.iter().zip(values) {
            if let Some(Buf::Mod(b)) = o {
                self.mods[*b] = v;
            }
        }

        fn start(s: &mut VoicesState, lane: usize, key: u8, velocity: f32, glide_ms: f32) {
            s.key[lane] = key;
            s.target[lane] = (f32::from(key) - 60.0) / 12.0;
            if glide_ms <= 0.0 || s.age[lane] == 0 {
                s.pitch[lane] = s.target[lane];
            }
            s.gate[lane] = true;
            s.velocity[lane] = velocity;
        }
    }
}

fn add(dst: &mut Stereo, src: &Stereo, g: f32) {
    for ch in 0..2 {
        for (d, s) in dst[ch].iter_mut().zip(&src[ch]) {
            *d += s * g;
        }
    }
}

/// As a mixer effect (`EffectKind::Patcher`): the mixer runs it with the plugin table
/// through [`Effect::as_any_mut`]; anywhere else plugin modules pass their audio through.
impl Effect for PatchDsp {
    fn param_count(&self) -> usize {
        0
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn reset(&mut self) {
        for n in &mut self.nodes {
            if let State::Effect(fx) = &mut n.state {
                fx.reset();
            }
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        self.run(l, r, ctx, &PluginContext::default(), None);
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::patcher::{ModuleKind as M, WireEnd as E};

    const SR: f32 = 48_000.0;

    fn ctx() -> FxContext<'static> {
        FxContext {
            bpm: 120.0,
            sidechain: None,
            beats: None,
        }
    }

    /// Runs `quanta` quanta, returning the left output.
    fn run(dsp: &mut PatchDsp, quanta: usize, input: f32) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..quanta {
            let (mut l, mut r) = ([input; Q], [input; Q]);
            dsp.run(&mut l, &mut r, &ctx(), &PluginContext::default(), None);
            out.extend_from_slice(&l);
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Strength of `hz` in `x`.
    fn tone(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let w = std::f32::consts::TAU * hz * i as f32 / SR;
            re += v * w.cos();
            im += v * w.sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f32
    }

    #[test]
    fn the_starting_instrument_plays_its_notes_with_envelopes() {
        let mut dsp = PatchDsp::build(&Patch::instrument(), SR);
        assert!(rms(&run(&mut dsp, 10, 0.0)) == 0.0, "silent before a note");
        dsp.note_on(0, 69, 0.8);
        dsp.note_on(0, 76, 0.8);
        let x = run(&mut dsp, 300, 0.0);
        let held = &x[Q * 100..];
        assert!(held.iter().all(|v| v.is_finite() && v.abs() < 4.0));
        // Both voices: A4 and E5.
        assert!(tone(held, 440.0) > 0.02, "{}", tone(held, 440.0));
        assert!(tone(held, 659.255) > 0.02, "{}", tone(held, 659.255));
        dsp.note_off(0, 69);
        dsp.note_off(0, 76);
        let tail = run(&mut dsp, 1000, 0.0);
        assert!(rms(&tail[tail.len() - 4800..]) < 1e-3, "released");
    }

    #[test]
    fn an_lfo_on_a_parameter_moves_it_and_the_effect_patch_passes_audio() {
        let mut p = Patch::effect();
        let mut dsp = PatchDsp::build(&p, SR);
        let x = run(&mut dsp, 4, 0.25);
        assert!(x.iter().all(|v| (*v - 0.25).abs() < 1e-6));
        // Audio in → amp → out, with an LFO on the amp's level.
        let input = p.nodes[1].id;
        let out = p.nodes[2].id;
        p.wires.clear();
        let amp = p.add(M::Amp, [0.0, 0.0]).unwrap();
        let lfo = p.add(M::Lfo, [0.0, 0.0]).unwrap();
        p.node_mut(lfo).unwrap().params[1] = 5.0;
        p.connect(input, 0, amp, E::Input(0), 1.0).unwrap();
        p.connect(amp, 0, out, E::Input(0), 1.0).unwrap();
        p.connect(lfo, 0, amp, E::Param(0), -0.5).unwrap();
        let mut dsp = PatchDsp::build(&p, SR);
        let x = run(&mut dsp, 1500, 0.25);
        let (lo, hi) = x[Q * 10..]
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(hi - lo > 0.1, "{lo}..{hi}");
        // New values without a rebuild: the LFO off, the amp at half.
        p.wires.pop();
        assert!(!same_structure(&p, &Patch::instrument()));
        let mut p2 = p.clone();
        p2.node_mut(amp).unwrap().params[0] = 0.5;
        let mut d2 = PatchDsp::build(&p, SR);
        let mut v = PatchUpdate::of(&p2);
        d2.set_values(&mut v);
        let x = run(&mut d2, 20, 0.25);
        assert!((x[Q * 19] - 0.125).abs() < 1e-3, "{}", x[Q * 19]);
    }

    #[test]
    fn a_feedback_loop_runs_and_stays_finite() {
        let mut p = Patch::effect();
        let (input, out) = (p.nodes[1].id, p.nodes[2].id);
        let a = p.add(M::Mixer, [0.0, 0.0]).unwrap();
        let d = p
            .add(M::Effect(gt_core::EffectKind::Delay), [0.0, 0.0])
            .unwrap();
        p.connect(input, 0, a, E::Input(0), 1.0).unwrap();
        p.connect(a, 0, d, E::Input(0), 1.0).unwrap();
        p.connect(d, 0, a, E::Input(1), 0.7).unwrap();
        p.connect(a, 0, out, E::Input(0), 1.0).unwrap();
        let mut dsp = PatchDsp::build(&p, SR);
        let x = run(&mut dsp, 2000, 0.1);
        assert!(x.iter().all(|v| v.is_finite() && v.abs() < 50.0));
    }

    #[test]
    fn a_synth_module_and_transpose_follow_the_notes() {
        let mut p = Patch::instrument();
        let notes = p.nodes[0].id;
        let out = p.nodes[2].id;
        p.wires.retain(|w| w.to != out);
        let t = p.add(M::Transpose, [0.0, 0.0]).unwrap();
        p.node_mut(t).unwrap().params[0] = 12.0;
        let s = p.add(M::Synth, [0.0, 0.0]).unwrap();
        p.connect(notes, 0, t, E::Input(0), 1.0).unwrap();
        p.connect(t, 0, s, E::Input(0), 1.0).unwrap();
        p.connect(s, 0, out, E::Input(0), 1.0).unwrap();
        let mut dsp = PatchDsp::build(&p, SR);
        dsp.note_on(0, 57, 0.9);
        let x = run(&mut dsp, 200, 0.0);
        let held = &x[Q * 50..];
        // Gloom Synth's default saw an octave up: 440 Hz, not 220.
        assert!(tone(held, 440.0) > 3.0 * tone(held, 220.0));
        dsp.note_off(0, 57);
        let tail = run(&mut dsp, 1500, 0.0);
        assert!(
            rms(&tail[tail.len() - 4800..]) < 1e-3,
            "note off reached the synth"
        );
    }

    #[test]
    fn rebuilding_keeps_the_voices_running() {
        let p = Patch::instrument();
        let mut a = PatchDsp::build(&p, SR);
        a.note_on(0, 60, 1.0);
        run(&mut a, 100, 0.0);
        let mut b = PatchDsp::build(&p, SR);
        b.inherit(&mut a);
        let x = run(&mut b, 10, 0.0);
        assert!(rms(&x) > 0.01, "the held note goes on");
    }

    #[test]
    fn plugin_modules_run_from_the_table_with_modulated_parameters() {
        use crate::plugin::tests::fake;
        use gt_core::{PluginKind as K, PluginRef};
        let t = Telemetry::default();
        let mut table = PluginTable::new();
        let id = PluginInstanceId(77);
        table.set(2, Some(fake(id, 1.0)), &t);
        let mut p = Patch::effect();
        let (input, out) = (p.nodes[1].id, p.nodes[2].id);
        p.wires.clear();
        let mut pl = PluginRef::new("fake", "Fake", "GT", K::Effect, Default::default());
        pl.instance = id;
        pl.values = vec![1.0, 0.0];
        let node = p.add_plugin(pl, [0.0, 0.0]).unwrap();
        p.connect(input, 0, node, E::Input(0), 1.0).unwrap();
        p.connect(node, 0, out, E::Input(0), 1.0).unwrap();
        // A maths module at -0.75 on the gain: 1 - 0.75.
        let m = p.add(M::Math, [0.0, 0.0]).unwrap();
        p.node_mut(m).unwrap().params[2] = -0.75;
        p.connect(m, 0, node, E::Param(0), 1.0).unwrap();
        let mut dsp = PatchDsp::build(&p, SR);
        let mut last = 0.0;
        for _ in 0..3 {
            let (mut l, mut r) = ([1.0; Q], [1.0; Q]);
            dsp.run(
                &mut l,
                &mut r,
                &ctx(),
                &PluginContext::default(),
                Some((&mut table, &t)),
            );
            last = l[0];
        }
        assert!((last - 0.25).abs() < 1e-5, "{last}");
        // Without the table it passes through.
        let x = run(&mut dsp, 1, 1.0);
        assert_eq!(x[0], 1.0);
    }
}
