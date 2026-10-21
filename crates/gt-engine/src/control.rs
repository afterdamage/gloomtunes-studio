//! Parameter control on the audio thread: where a [`ParamId`] lives in the engine, and the
//! modulators that move parameters (ARCHITECTURE.md §7.4).
//!
//! Every render quantum (32 frames, the control rate) the processor evaluates the automation
//! lanes of the song and the modulators of the [`ModPlan`] and writes the results to their
//! destinations. Gains (channel and strip volume, pan, sends) ramp linearly to the new value
//! across the quantum, so they follow automation sample by sample between control points;
//! everything else takes the value once per quantum and smooths it the way its knob does.

use gt_core::formula::{FormulaVars, Program, MAX_INPUTS};
use gt_core::modulation::{LfoRate, MidiControl, ModField, ModSourceKind, ModulatorId};
use gt_core::params::{ChannelParam, StripParam};
use gt_core::{ParamId, ParamInfo, Project, PPQ, STRIPS};
use gt_dsp::Noise;

/// Where a parameter lives in the engine. Built from a [`ParamId`] by [`ParamDest::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamDest {
    /// A channel slot's volume, pan or sampler setting.
    Channel {
        /// Channel slot (rack index).
        slot: u16,
        /// Which setting.
        param: ChannelParam,
    },
    /// A Gloom Synth knob of a channel slot.
    Synth {
        /// Channel slot.
        slot: u16,
        /// `SynthParam` index.
        index: u8,
    },
    /// The amount of a Gloom Synth modulation matrix row.
    SynthMod {
        /// Channel slot.
        slot: u16,
        /// Matrix row.
        row: u8,
    },
    /// A mixer strip's fader, balance or send.
    Strip {
        /// Strip index.
        strip: u16,
        /// Which control.
        param: StripParam,
    },
    /// An effect parameter.
    Effect {
        /// Strip index.
        strip: u16,
        /// Slot index.
        slot: u8,
        /// Parameter index.
        index: u8,
    },
    /// A plugin parameter (normalized value).
    Plugin {
        /// The plugin instance.
        instance: gt_core::PluginInstanceId,
        /// Position in the plugin's parameter list.
        index: u32,
    },
}

impl ParamDest {
    /// The engine address of `id` in `project`, or `None` if the target does not exist.
    pub fn resolve(id: &ParamId, project: &Project) -> Option<Self> {
        if !id.is_valid(project) {
            return None;
        }
        let slot = |c| project.channel_index(c).and_then(|i| u16::try_from(i).ok());
        let u = |x: usize| u8::try_from(x).ok();
        Some(match *id {
            // Tempo automation is part of the tempo map, not a lane; modulator fields are
            // moved inside the modulation plan.
            ParamId::Tempo | ParamId::Modulator { .. } => return None,
            ParamId::Channel { channel, param } => Self::Channel {
                slot: slot(channel)?,
                param,
            },
            ParamId::Synth { channel, param } => Self::Synth {
                slot: slot(channel)?,
                index: u(param as usize)?,
            },
            ParamId::SynthMod { channel, slot: row } => Self::SynthMod {
                slot: slot(channel)?,
                row: u(row)?,
            },
            ParamId::Strip { strip, param } => Self::Strip {
                strip: u16::try_from(strip).ok()?,
                param,
            },
            ParamId::Effect {
                strip,
                slot: fx,
                index,
                ..
            } => Self::Effect {
                strip: u16::try_from(strip).ok()?,
                slot: u(fx)?,
                index: u(index)?,
            },
            ParamId::Plugin { id: param, .. } => {
                let p = id.plugin_ref(project)?;
                Self::Plugin {
                    instance: p.instance,
                    index: u32::try_from(p.param_index(param)?).ok()?,
                }
            }
        })
    }

    /// True for the gain-like controls that ramp per sample across a quantum.
    pub fn ramps(&self) -> bool {
        matches!(
            self,
            Self::Channel {
                param: ChannelParam::Volume | ChannelParam::Pan,
                ..
            } | Self::Strip { .. }
        )
    }
}

/// Run-time state of one modulator. Kept across plan changes by modulator id.
#[derive(Debug, Clone, Copy)]
pub struct ModState {
    /// LFO phase in cycles (free-running LFOs; synced ones derive it from the song position).
    phase: f64,
    /// Whole cycle the S&H level belongs to.
    cycle: i64,
    /// Current S&H level.
    held: f32,
    noise: Noise,
    /// Envelope follower level, 0..1.
    env: f32,
    /// Seconds run, and quarter notes run while the song is stopped (formulas).
    t: f64,
    free_beats: f64,
    /// Last output, for tests and meters.
    out: f32,
}

impl Default for ModState {
    fn default() -> Self {
        Self {
            phase: 0.0,
            cycle: i64::MIN,
            held: 0.0,
            noise: Noise::new(0x1234_5678),
            env: 0.0,
            t: 0.0,
            free_beats: 0.0,
            out: 0.0,
        }
    }
}

/// What a modulator needs to know about the quantum it runs in.
#[derive(Debug, Clone, Copy)]
pub struct ModClock {
    /// Length of a quantum in seconds.
    pub dt: f64,
    /// Current tempo.
    pub bpm: f64,
    /// Song position in quarter notes at the end of the quantum while playing.
    pub beats: Option<f64>,
    /// Mod wheel and aftertouch of the target's channel, 0 to 1 (MIDI sources).
    pub mod_wheel: f32,
    pub aftertouch: f32,
}

impl ModState {
    /// Advances by one quantum and returns the source value: -1..1 for an LFO, 0..1 for a
    /// follower. `level` reads a strip's last peak level (followers only).
    pub fn step(
        &mut self,
        source: &ModSourceKind,
        clock: &ModClock,
        level: impl Fn(usize) -> f32,
    ) -> f32 {
        self.step_scaled(source, clock, 0.0, level)
    }

    /// As [`Self::step`], with the source's speed moved by `rate_oct` octaves (another
    /// modulator on its rate). A synced LFO whose rate is moved runs free from its phase.
    /// Formulas are evaluated by [`ModEntry::step`]; here they hold their last output.
    pub fn step_scaled(
        &mut self,
        source: &ModSourceKind,
        clock: &ModClock,
        rate_oct: f32,
        level: impl Fn(usize) -> f32,
    ) -> f32 {
        let speed = f64::from(rate_oct.clamp(-8.0, 8.0)).exp2();
        let moved = rate_oct.abs() > 1e-6;
        self.out = match *source {
            ModSourceKind::Lfo { shape, rate, phase } => {
                let p = match (rate, clock.beats) {
                    (LfoRate::Sync(_), Some(beats)) if !moved => {
                        let cpb = rate.cycles_per_beat().unwrap_or(1.0);
                        self.phase = beats * cpb;
                        self.phase
                    }
                    _ => {
                        let hz = match rate {
                            LfoRate::Hz(hz) => f64::from(hz),
                            LfoRate::Sync(_) => {
                                rate.cycles_per_beat().unwrap_or(1.0) * clock.bpm / 60.0
                            }
                        };
                        // Keep the phase small so f64 keeps its precision for hours.
                        self.phase = (self.phase + hz * speed * clock.dt).rem_euclid(1024.0);
                        self.phase
                    }
                } + f64::from(phase);
                let cycle = p.floor() as i64;
                if cycle != self.cycle {
                    self.cycle = cycle;
                    self.held = self.noise.next_value();
                }
                shape.value(p, self.held)
            }
            ModSourceKind::Follower {
                strip,
                attack_ms,
                release_ms,
                gain,
            } => {
                let x = (level(strip.min(STRIPS - 1)) * gain).clamp(0.0, 1.0);
                let ms = if x > self.env { attack_ms } else { release_ms };
                let ms = f64::from(ms.max(0.01)) / speed;
                let k = 1.0 - (-(clock.dt * 1000.0) / ms).exp();
                self.env += (x - self.env) * k as f32;
                if self.env < 1e-6 {
                    self.env = 0.0;
                }
                self.env
            }
            ModSourceKind::Midi { control } => {
                let x = match control {
                    MidiControl::ModWheel => clock.mod_wheel,
                    MidiControl::Aftertouch => clock.aftertouch,
                };
                // 7-bit steps would zipper: glide over about 5 ms.
                let k = 1.0 - (-(clock.dt * 1000.0) / 5.0).exp();
                self.env += (x.clamp(0.0, 1.0) - self.env) * k as f32;
                self.env
            }
            ModSourceKind::Formula => self.out,
        };
        self.out
    }

    /// The value of the last `step`.
    pub fn output(&self) -> f32 {
        self.out
    }
}

/// One modulator as the engine runs it.
#[derive(Debug, Clone, Copy)]
pub struct ModEntry {
    /// The document modulator (for keeping state across plans).
    pub id: ModulatorId,
    /// Source settings.
    pub source: ModSourceKind,
    /// Depth, -1..1 of the normalized range.
    pub amount: f32,
    /// Run-time state.
    pub state: ModState,
    /// The compiled formula (formula sources).
    pub program: Program,
    /// Where the formula's inputs are in [`ModPlan::outputs`] (`u16::MAX`: not running).
    inputs: [u16; MAX_INPUTS],
    /// This entry's place in [`ModPlan::outputs`].
    flat: u16,
    /// What other modulators add to the depth (plain) and the speed (octaves) this quantum.
    amount_mod: f32,
    rate_mod: f32,
}

impl ModEntry {
    fn new(id: ModulatorId, source: ModSourceKind, amount: f32, program: Program) -> Self {
        Self {
            id,
            source,
            amount: amount.clamp(-1.0, 1.0),
            state: ModState::default(),
            program,
            inputs: [u16::MAX; MAX_INPUTS],
            flat: 0,
            amount_mod: 0.0,
            rate_mod: 0.0,
        }
    }

    /// The depth this quantum: its own plus what modulators on it add.
    #[inline]
    pub fn depth(&self) -> f32 {
        (self.amount + self.amount_mod).clamp(-1.0, 1.0)
    }

    /// Advances by one quantum and returns the source value. `outputs` are every modulator's
    /// outputs of the previous quantum (formula inputs).
    pub fn step(&mut self, clock: &ModClock, outputs: &[f32], level: impl Fn(usize) -> f32) -> f32 {
        if self.source != ModSourceKind::Formula {
            return self
                .state
                .step_scaled(&self.source, clock, self.rate_mod, level);
        }
        let st = &mut self.state;
        st.t += clock.dt;
        st.free_beats = (st.free_beats + clock.dt * clock.bpm / 60.0).rem_euclid(1_048_576.0);
        let mut ins = [0.0_f32; MAX_INPUTS];
        for (v, &k) in ins.iter_mut().zip(&self.inputs) {
            *v = outputs.get(usize::from(k)).copied().unwrap_or(0.0);
        }
        let vars = FormulaVars {
            t: st.t as f32,
            beat: clock.beats.unwrap_or(st.free_beats) as f32,
            bpm: clock.bpm as f32,
            wheel: clock.mod_wheel,
            touch: clock.aftertouch,
        };
        st.out = self.program.eval(&vars, &ins);
        st.out
    }
}

/// One modulator moving another's depth or speed.
#[derive(Debug, Clone, Copy)]
struct ModLink {
    from: u16,
    to: u16,
    field: ModField,
}

/// All modulators of one parameter.
#[derive(Debug, Clone)]
pub struct ModTarget {
    /// The parameter (for the UI's live values).
    pub target: ParamId,
    /// Where the parameter lives.
    pub dest: ParamDest,
    /// Range and taper, to turn normalized values into plain ones.
    pub info: ParamInfo,
    /// The document value (normalized), used when no automation lane drives the parameter.
    pub base: f32,
    /// The modulators.
    pub mods: Vec<ModEntry>,
    /// Index of the song's automation lane for the same destination, linked by the engine.
    pub(crate) lane: Option<usize>,
}

/// The modulators of a project, grouped by target. Sent boxed with
/// `EngineCommand::SetModulation`.
///
/// Modulators of other modulators (D151) run in `chain`: before the targets each quantum,
/// every link adds its source's output of the previous quantum to its destination's depth or
/// speed, so modulators can move each other in any order, cycles included, one quantum late.
#[derive(Debug, Clone, Default)]
pub struct ModPlan {
    /// One entry per modulated parameter.
    pub targets: Vec<ModTarget>,
    /// Modulators whose target is another modulator.
    pub chain: Vec<ModEntry>,
    links: Vec<ModLink>,
    /// Every modulator's last output: the chain's first, then the targets' in order.
    pub outputs: Vec<f32>,
    /// Where each flat index past the chain lives: (target, modulator).
    places: Vec<(u16, u16)>,
}

impl ModPlan {
    /// Groups the enabled modulators of `project` whose targets exist. A formula that does
    /// not compile outputs 0.
    pub fn compile(project: &Project) -> Self {
        let mut targets: Vec<(ParamId, ModTarget)> = Vec::new();
        let mut chain = Vec::new();
        let enabled = |id: ModulatorId| project.modulators.iter().any(|m| m.id == id && m.enabled);
        for m in project.modulators.iter().filter(|m| m.enabled) {
            let program = if m.source == ModSourceKind::Formula {
                Program::compile(project.formula(m.id)).unwrap_or_default()
            } else {
                Program::default()
            };
            let entry = ModEntry::new(m.id, m.source, m.amount, program);
            if let ParamId::Modulator { id, .. } = m.target {
                if m.target.is_valid(project) && enabled(id) && id != m.id {
                    chain.push(entry);
                }
                continue;
            }
            let Some(dest) = ParamDest::resolve(&m.target, project) else {
                continue;
            };
            match targets.iter_mut().find(|(id, _)| *id == m.target) {
                Some((_, t)) => t.mods.push(entry),
                None => targets.push((
                    m.target,
                    ModTarget {
                        target: m.target,
                        dest,
                        info: *m.target.info(),
                        base: m.target.normalized(project),
                        mods: vec![entry],
                        lane: None,
                    },
                )),
            }
        }
        let mut plan = Self {
            targets: targets.into_iter().map(|(_, t)| t).collect(),
            chain,
            ..Self::default()
        };
        // Flat indices, then links and formula inputs by modulator id.
        let mut ids: Vec<ModulatorId> = Vec::new();
        for (k, e) in plan.chain.iter_mut().enumerate() {
            e.flat = k as u16;
            ids.push(e.id);
        }
        for (t, target) in plan.targets.iter_mut().enumerate() {
            for (m, e) in target.mods.iter_mut().enumerate() {
                e.flat = ids.len() as u16;
                ids.push(e.id);
                plan.places.push((t as u16, m as u16));
            }
        }
        let flat_of = |id: ModulatorId| ids.iter().position(|&x| x == id).map(|k| k as u16);
        for m in project.modulators.iter().filter(|m| m.enabled) {
            if let ParamId::Modulator { id, field } = m.target {
                if let (Some(from), Some(to)) = (flat_of(m.id), flat_of(id)) {
                    if from != to {
                        plan.links.push(ModLink { from, to, field });
                    }
                }
            }
        }
        plan.outputs = vec![0.0; ids.len()];
        let fix = |e: &mut ModEntry| {
            for (k, id) in e.program.inputs().iter().enumerate() {
                e.inputs[k] = flat_of(*id).unwrap_or(u16::MAX);
            }
        };
        plan.chain.iter_mut().for_each(fix);
        plan.targets
            .iter_mut()
            .flat_map(|t| t.mods.iter_mut())
            .for_each(fix);
        plan
    }

    fn entry_mut(&mut self, flat: u16) -> Option<&mut ModEntry> {
        let k = usize::from(flat);
        if k < self.chain.len() {
            return self.chain.get_mut(k);
        }
        let (t, m) = *self.places.get(k - self.chain.len())?;
        self.targets
            .get_mut(usize::from(t))?
            .mods
            .get_mut(usize::from(m))
    }

    /// Starts a quantum: records every output of the last one, applies the links to depths
    /// and speeds, then runs the chain. Real-time safe.
    pub fn run_chain(&mut self, clock: &ModClock, level: impl Fn(usize) -> f32) {
        let chain = self.chain.iter_mut();
        let mains = self.targets.iter_mut().flat_map(|t| t.mods.iter_mut());
        for e in chain.chain(mains) {
            if let Some(o) = self.outputs.get_mut(usize::from(e.flat)) {
                *o = e.state.output();
            }
            e.amount_mod = 0.0;
            e.rate_mod = 0.0;
        }
        for i in 0..self.links.len() {
            let l = self.links[i];
            let Some(&src) = self.outputs.get(usize::from(l.from)) else {
                continue;
            };
            // The source's depth is in normalized units of the field's range: 2 for a depth
            // (-1..1), 8 octaves for a speed.
            let Some(depth) = self.entry_mut(l.from).map(|e| e.depth()) else {
                continue;
            };
            let Some(to) = self.entry_mut(l.to) else {
                continue;
            };
            match l.field {
                ModField::Amount => to.amount_mod += 2.0 * depth * src,
                ModField::Rate => to.rate_mod += 8.0 * depth * src,
            }
        }
        let outputs = &self.outputs;
        for e in &mut self.chain {
            e.step(clock, outputs, &level);
        }
    }

    /// Takes over the run-time state of modulators that were already running, so editing a
    /// modulator does not restart its LFO. Real-time safe (no allocation).
    pub(crate) fn inherit(&mut self, old: &ModPlan) {
        let olds = || {
            old.chain
                .iter()
                .chain(old.targets.iter().flat_map(|t| &t.mods))
        };
        let news = self
            .chain
            .iter_mut()
            .chain(self.targets.iter_mut().flat_map(|t| t.mods.iter_mut()));
        for m in news {
            if let Some(o) = olds().find(|o| o.id == m.id) {
                m.state = o.state;
            }
        }
    }
}

/// Song position in quarter notes for a tick.
pub(crate) fn beats_of(tick: f64) -> f64 {
    tick / PPQ as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::modulation::LfoShape;
    use gt_core::{ModField, ModSourceKind, SynthParam};

    fn clock(beats: Option<f64>) -> ModClock {
        ModClock {
            dt: 32.0 / 48_000.0,
            bpm: 120.0,
            beats,
            mod_wheel: 0.0,
            aftertouch: 0.0,
        }
    }

    #[test]
    fn midi_sources_glide_to_the_channels_controller() {
        let mut s = ModState::default();
        let src = ModSourceKind::Midi {
            control: MidiControl::Aftertouch,
        };
        let c = ModClock {
            aftertouch: 0.8,
            mod_wheel: 0.1,
            ..clock(None)
        };
        let first = s.step(&src, &c, |_| 0.0);
        assert!(first > 0.0 && first < 0.8, "glides, does not jump: {first}");
        let mut v = 0.0;
        for _ in 0..200 {
            v = s.step(&src, &c, |_| 0.0);
        }
        assert!((v - 0.8).abs() < 1e-3);
    }

    #[test]
    fn synced_lfo_follows_the_song_position() {
        let src = ModSourceKind::Lfo {
            shape: LfoShape::SawUp,
            rate: LfoRate::Sync(5), // 1/4
            phase: 0.0,
        };
        let mut s = ModState::default();
        assert_eq!(s.step(&src, &clock(Some(2.0)), |_| 0.0), -1.0);
        assert!((s.step(&src, &clock(Some(2.5)), |_| 0.0)).abs() < 1e-6);
        // Stopped: runs free at the tempo, one cycle per beat = 2 Hz at 120 BPM.
        let mut s = ModState::default();
        let c = clock(None);
        let n = (0.25 / c.dt).round() as usize; // a quarter second = half a cycle
        let mut v = 0.0;
        for _ in 0..n {
            v = s.step(&src, &c, |_| 0.0);
        }
        assert!(v.abs() < 0.01, "{v}");
    }

    #[test]
    fn free_lfo_and_sample_hold() {
        let sine = ModSourceKind::Lfo {
            shape: LfoShape::Sine,
            rate: LfoRate::Hz(1.5),
            phase: 0.25,
        };
        let mut s = ModState::default();
        let first = s.step(&sine, &clock(Some(0.0)), |_| 0.0);
        assert!(first > 0.99, "phase offset a quarter cycle: {first}");
        let sh = ModSourceKind::Lfo {
            shape: LfoShape::SampleHold,
            rate: LfoRate::Hz(10.0),
            phase: 0.0,
        };
        let mut s = ModState::default();
        let mut levels = Vec::new();
        for _ in 0..1500 {
            let v = s.step(&sh, &clock(None), |_| 0.0);
            if levels.last() != Some(&v) {
                levels.push(v);
            }
        }
        // 1 s at 10 Hz: about 10 distinct levels, all in range.
        assert!((9..=12).contains(&levels.len()), "{}", levels.len());
        assert!(levels.iter().all(|v| (-1.0..=1.0).contains(v)));
    }

    #[test]
    fn follower_rises_fast_and_falls_slowly() {
        let f = ModSourceKind::Follower {
            strip: 3,
            attack_ms: 5.0,
            release_ms: 200.0,
            gain: 2.0,
        };
        let mut s = ModState::default();
        let c = clock(None);
        let quanta = |ms: f64| (ms / 1000.0 / c.dt).round() as usize;
        let mut v = 0.0;
        for _ in 0..quanta(25.0) {
            v = s.step(&f, &c, |strip| if strip == 3 { 0.4 } else { 0.0 });
        }
        assert!(
            (v - 0.8).abs() < 0.01,
            "level 0.4 x gain 2 after 5 time constants: {v}"
        );
        for _ in 0..quanta(200.0) {
            v = s.step(&f, &c, |_| 0.0);
        }
        assert!(
            (v - 0.8 * (-1.0_f32).exp()).abs() < 0.02,
            "one release time constant: {v}"
        );
    }

    #[test]
    fn plans_group_by_target_and_skip_missing_ones() {
        let mut p = Project::demo();
        let bass = p.channels[4].id;
        let cutoff = ParamId::Synth {
            channel: bass,
            param: SynthParam::Cutoff,
        };
        p.add_modulator(cutoff, ModSourceKind::default_lfo());
        p.add_modulator(cutoff, ModSourceKind::default_follower(1));
        let off = p
            .add_modulator(ParamId::strip_pan(5), ModSourceKind::default_lfo())
            .unwrap();
        p.modulator_mut(off).unwrap().enabled = false;
        p.add_modulator(
            ParamId::Effect {
                strip: 9,
                slot: 9,
                kind: gt_core::EffectKind::Chorus,
                index: 0,
            },
            ModSourceKind::default_lfo(),
        );
        let plan = ModPlan::compile(&p);
        // The demo's two, plus the cutoff with both of its modulators.
        assert_eq!(plan.targets.len(), 3);
        let t = plan
            .targets
            .iter()
            .find(|t| {
                t.dest
                    == ParamDest::Synth {
                        slot: 4,
                        index: SynthParam::Cutoff as u8,
                    }
            })
            .unwrap();
        assert_eq!(t.mods.len(), 2);
        assert!((t.base - cutoff.normalized(&p)).abs() < 1e-6);
        // State carries over by id.
        let mut old = plan.clone();
        old.targets[0].mods[0].state.env = 0.7;
        let mut new = ModPlan::compile(&p);
        new.inherit(&old);
        assert_eq!(new.targets[0].mods[0].state.env, 0.7);
    }

    fn chained_project() -> (Project, ParamId, gt_core::ModulatorId) {
        let mut p = Project::demo();
        let bass = p.channels[4].id;
        let cutoff = ParamId::Synth {
            channel: bass,
            param: SynthParam::Cutoff,
        };
        let main = p
            .add_modulator(
                cutoff,
                ModSourceKind::Lfo {
                    shape: LfoShape::Square,
                    rate: LfoRate::Hz(0.01),
                    phase: 0.0,
                },
            )
            .unwrap();
        (p, cutoff, main)
    }

    fn plan_target(plan: &mut ModPlan, target: ParamId) -> &mut ModTarget {
        plan.targets
            .iter_mut()
            .find(|t| t.target == target)
            .unwrap()
    }

    #[test]
    fn a_modulator_moves_another_modulators_depth() {
        let (mut p, cutoff, main) = chained_project();
        p.modulator_mut(main).unwrap().amount = 0.25;
        // A formula holding 1: the depth goes up by 2 x 0.25 (the depth field spans -1..1).
        let f = p
            .add_modulator(
                ParamId::Modulator {
                    id: main,
                    field: ModField::Amount,
                },
                ModSourceKind::Formula,
            )
            .unwrap();
        p.formulas.insert(f, "1".to_owned());
        p.modulator_mut(f).unwrap().amount = 0.25;
        let mut plan = ModPlan::compile(&p);
        assert_eq!(plan.chain.len(), 1);
        let c = clock(None);
        for _ in 0..3 {
            plan.run_chain(&c, |_| 0.0);
            let outputs = plan.outputs.clone();
            for m in &mut plan_target(&mut plan, cutoff).mods {
                m.step(&c, &outputs, |_| 0.0);
            }
        }
        let m = &plan_target(&mut plan, cutoff).mods[0];
        assert!((m.depth() - 0.75).abs() < 1e-6, "{}", m.depth());
        // Disabled, it stops; removing the target removes it too.
        p.modulator_mut(f).unwrap().enabled = false;
        assert!(ModPlan::compile(&p).chain.is_empty());
        p.remove_modulator(main);
        assert!(p.modulators.iter().all(|m| m.id != f));
        assert!(!p.formulas.contains_key(&f));
    }

    #[test]
    fn a_modulator_speeds_up_another_lfo_and_formulas_read_modulators() {
        let (mut p, cutoff, main) = chained_project();
        let src = ModSourceKind::Lfo {
            shape: LfoShape::SawUp,
            rate: LfoRate::Hz(1.0),
            phase: 0.0,
        };
        p.modulator_mut(main).unwrap().source = src;
        // +1 octave of rate: a formula of 1 at depth 1/8 (the rate field spans 8 octaves).
        let f = p
            .add_modulator(
                ParamId::Modulator {
                    id: main,
                    field: ModField::Rate,
                },
                ModSourceKind::Formula,
            )
            .unwrap();
        p.formulas.insert(f, "1".to_owned());
        p.modulator_mut(f).unwrap().amount = 0.125;
        // A second formula reads the LFO (m<id>) and inverts it.
        let g = p
            .add_modulator(ParamId::strip_pan(3), ModSourceKind::Formula)
            .unwrap();
        p.formulas.insert(g, format!("-m{}", main.0));
        let mut plan = ModPlan::compile(&p);
        let c = clock(None);
        // A quarter second: half a cycle at 2 Hz, so the saw is back near 0.
        let n = (0.25 / c.dt).round() as usize;
        let (mut lfo, mut inv) = (0.0, 0.0);
        for _ in 0..n {
            plan.run_chain(&c, |_| 0.0);
            let outputs = plan.outputs.clone();
            for t in &mut plan.targets {
                for m in &mut t.mods {
                    let v = m.step(&c, &outputs, |_| 0.0);
                    if m.id == main {
                        lfo = v;
                    } else if m.id == g {
                        inv = v;
                    }
                }
            }
        }
        assert!(lfo.abs() < 0.02, "{lfo}");
        // One quantum late.
        assert!((inv + lfo).abs() < 0.02, "{inv} {lfo}");
        let _ = plan_target(&mut plan, cutoff);
    }
}
