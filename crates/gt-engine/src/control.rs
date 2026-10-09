//! Parameter control on the audio thread: where a [`ParamId`] lives in the engine, and the
//! modulators that move parameters (ARCHITECTURE.md §7.4).
//!
//! Every render quantum (32 frames, the control rate) the processor evaluates the automation
//! lanes of the song and the modulators of the [`ModPlan`] and writes the results to their
//! destinations. Gains (channel and strip volume, pan, sends) ramp linearly to the new value
//! across the quantum, so they follow automation sample by sample between control points;
//! everything else takes the value once per quantum and smooths it the way its knob does.

use gt_core::modulation::{LfoRate, ModSourceKind, ModulatorId};
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
        strip: u8,
        /// Which control.
        param: StripParam,
    },
    /// An effect parameter.
    Effect {
        /// Strip index.
        strip: u8,
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
                strip: u(strip)?,
                param,
            },
            ParamId::Effect {
                strip,
                slot: fx,
                index,
                ..
            } => Self::Effect {
                strip: u(strip)?,
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
        self.out = match *source {
            ModSourceKind::Lfo { shape, rate, phase } => {
                let p = match (rate, clock.beats) {
                    (LfoRate::Sync(_), Some(beats)) => {
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
                        self.phase = (self.phase + hz * clock.dt).rem_euclid(1024.0);
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
                let k = 1.0 - (-(clock.dt * 1000.0) / f64::from(ms.max(0.01))).exp();
                self.env += (x - self.env) * k as f32;
                if self.env < 1e-6 {
                    self.env = 0.0;
                }
                self.env
            }
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
}

/// All modulators of one parameter.
#[derive(Debug, Clone)]
pub struct ModTarget {
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
#[derive(Debug, Clone, Default)]
pub struct ModPlan {
    /// One entry per modulated parameter.
    pub targets: Vec<ModTarget>,
}

impl ModPlan {
    /// Groups the enabled modulators of `project` whose targets exist.
    pub fn compile(project: &Project) -> Self {
        let mut targets: Vec<(ParamId, ModTarget)> = Vec::new();
        for m in project.modulators.iter().filter(|m| m.enabled) {
            let Some(dest) = ParamDest::resolve(&m.target, project) else {
                continue;
            };
            let entry = ModEntry {
                id: m.id,
                source: m.source,
                amount: m.amount.clamp(-1.0, 1.0),
                state: ModState::default(),
            };
            match targets.iter_mut().find(|(id, _)| *id == m.target) {
                Some((_, t)) => t.mods.push(entry),
                None => targets.push((
                    m.target,
                    ModTarget {
                        dest,
                        info: *m.target.info(),
                        base: m.target.normalized(project),
                        mods: vec![entry],
                        lane: None,
                    },
                )),
            }
        }
        Self {
            targets: targets.into_iter().map(|(_, t)| t).collect(),
        }
    }

    /// Takes over the run-time state of modulators that were already running, so editing a
    /// modulator does not restart its LFO. Real-time safe (no allocation).
    pub(crate) fn inherit(&mut self, old: &ModPlan) {
        for t in &mut self.targets {
            for m in &mut t.mods {
                if let Some(o) = old
                    .targets
                    .iter()
                    .flat_map(|t| &t.mods)
                    .find(|o| o.id == m.id)
                {
                    m.state = o.state;
                }
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
    use gt_core::{ModSourceKind, SynthParam};

    fn clock(beats: Option<f64>) -> ModClock {
        ModClock {
            dt: 32.0 / 48_000.0,
            bpm: 120.0,
            beats,
        }
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
}
