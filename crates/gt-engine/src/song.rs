//! Compiled, real-time-friendly views of the project document.
//!
//! The UI thread compiles the document into these flat structures and sends them boxed to the
//! audio thread, which only reads them. Replacements come back through the garbage queue.

use gt_core::synth::{ModDest as DocDest, ModSource as DocSource, SynthParam as P};
use gt_core::{Adsr, Channel, Instrument, LoopMode, Project, SynthPatch, STEP_TICKS};
use gt_dsp::synth::{
    EnvSettings, LfoSettings, ModDest, ModSlot, ModSource, OscSettings, SynthSettings, MOD_SLOTS,
};
use gt_dsp::{LfoWave, Wave};

/// What a pattern event does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoteKind {
    /// Start a note with this velocity (0 to 1).
    On {
        /// Velocity.
        velocity: f32,
    },
    /// Release the note.
    Off,
}

/// One note-on or note-off at a tick inside the pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongEvent {
    /// Tick from the pattern start, in `0..length`.
    pub tick: i64,
    /// Channel slot (index in the channel rack).
    pub slot: u16,
    /// MIDI key.
    pub key: u8,
    /// On or off.
    pub kind: NoteKind,
}

impl SongEvent {
    /// Sort key: by tick, and note-offs before note-ons on the same tick, so a note that ends
    /// exactly where the next one starts does not cut the new one.
    fn order(&self) -> (i64, u8) {
        let k = match self.kind {
            NoteKind::Off => 0,
            NoteKind::On { .. } => 1,
        };
        (self.tick, k)
    }
}

/// The playing pattern, flattened: sorted note events that repeat every `length` ticks from
/// tick 0 of the timeline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SongSnapshot {
    /// Pattern length in ticks (0: nothing plays).
    pub length: i64,
    /// Events sorted by [`SongEvent::order`].
    pub events: Vec<SongEvent>,
}

impl SongSnapshot {
    /// Compiles the current pattern of `project`. Slots are channel indices. Swing delays notes
    /// that start on an odd 1/16 step by `swing * STEP_TICKS / 2`. Note-offs past the pattern
    /// end wrap around to the next repetition. The pattern length fits all notes
    /// (`Pattern::length_ticks`); notes with a negative start are skipped.
    pub fn compile(project: &Project) -> Self {
        let pattern = project.current_pattern();
        let length = pattern.length_ticks();
        let swing_ticks =
            (f64::from(project.swing.clamp(0.0, 1.0)) * STEP_TICKS as f64 / 2.0).round() as i64;
        let mut events = Vec::new();
        for (slot, ch) in project.channels.iter().enumerate() {
            for n in pattern.channel_notes(ch.id) {
                if n.start < 0 || n.start >= length {
                    continue;
                }
                let swung = n.start.rem_euclid(2 * STEP_TICKS) == STEP_TICKS;
                let on = n.start + if swung { swing_ticks } else { 0 };
                let off = on + n.length.max(1);
                events.push(SongEvent {
                    tick: on.rem_euclid(length),
                    slot: slot as u16,
                    key: n.key,
                    kind: NoteKind::On {
                        velocity: n.velocity.clamp(0.0, 1.0),
                    },
                });
                events.push(SongEvent {
                    tick: off.rem_euclid(length),
                    slot: slot as u16,
                    key: n.key,
                    kind: NoteKind::Off,
                });
            }
        }
        events.sort_by_key(SongEvent::order);
        Self { length, events }
    }

    /// Events with `lo <= tick <= hi`.
    pub fn events_in(&self, lo: i64, hi: i64) -> &[SongEvent] {
        let a = self.events.partition_point(|e| e.tick < lo);
        let b = self.events.partition_point(|e| e.tick <= hi);
        &self.events[a..b.max(a)]
    }
}

/// Which instrument a channel slot plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstrumentKind {
    /// The sampler (uses the sample and the sampler fields).
    #[default]
    Sampler,
    /// Gloom Synth (uses `synth`).
    Synth,
}

/// Per-channel settings as the engine uses them. Sent boxed, see `EngineCommand`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelParams {
    /// Instrument.
    pub kind: InstrumentKind,
    /// Synth settings (used when `kind` is `Synth`).
    pub synth: SynthSettings,
    /// Linear gain; 0 when muted or silenced by another channel's solo.
    pub gain: f32,
    /// Pan, -1 to 1.
    pub pan: f32,
    /// Transposition in semitones.
    pub pitch: f32,
    /// Start point, fraction of the sample.
    pub start: f32,
    /// End point, fraction of the sample.
    pub end: f32,
    /// Loop instead of one-shot.
    pub looped: bool,
    /// Envelope.
    pub adsr: Adsr,
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            kind: InstrumentKind::Sampler,
            synth: SynthSettings::default(),
            gain: 0.0,
            pan: 0.0,
            pitch: 0.0,
            start: 0.0,
            end: 1.0,
            looped: false,
            adsr: Adsr::default(),
        }
    }
}

impl ChannelParams {
    /// Engine parameters for a document channel. `silenced` is the effective mute (mute, or
    /// another channel is soloed).
    pub fn from_channel(ch: &Channel, silenced: bool) -> Self {
        let mut p = Self {
            gain: if silenced {
                0.0
            } else {
                ch.volume.clamp(0.0, Channel::MAX_VOLUME)
            },
            pan: ch.pan.clamp(-1.0, 1.0),
            ..Self::default()
        };
        match &ch.instrument {
            Instrument::Sampler(s) => {
                p.pitch = s.pitch;
                p.start = s.start;
                p.end = s.end;
                p.looped = s.loop_mode == LoopMode::Loop;
                p.adsr = s.adsr;
            }
            Instrument::Synth(patch) => {
                p.kind = InstrumentKind::Synth;
                p.synth = synth_settings(patch);
            }
        }
        p
    }
}

/// Converts a document patch (values by parameter index) to the synth's typed settings.
pub fn synth_settings(patch: &SynthPatch) -> SynthSettings {
    let v = |p: P| p.info().clamp(patch.get(p));
    let wave = |x: f32| match x as u8 {
        0 => Wave::Sine,
        1 => Wave::Triangle,
        3 => Wave::Square,
        _ => Wave::Saw,
    };
    let lfo_wave = |x: f32| match x as u8 {
        1 => LfoWave::Triangle,
        2 => LfoWave::Saw,
        3 => LfoWave::Square,
        4 => LfoWave::SampleHold,
        _ => LfoWave::Sine,
    };
    let osc = |w, o, s, f, l, pw| OscSettings {
        wave: wave(v(w)),
        octave: v(o),
        semitones: v(s),
        cents: v(f),
        level: v(l),
        pulse_width: v(pw),
    };
    let env = |a, d, s, r| EnvSettings {
        attack_ms: v(a),
        decay_ms: v(d),
        sustain: v(s),
        release_ms: v(r),
    };
    let mut mods = [ModSlot::default(); MOD_SLOTS];
    for (m, d) in mods.iter_mut().zip(&patch.mods) {
        *m = ModSlot {
            source: match d.source {
                DocSource::Off => ModSource::Off,
                DocSource::Lfo1 => ModSource::Lfo1,
                DocSource::Lfo2 => ModSource::Lfo2,
                DocSource::ModEnv => ModSource::ModEnv,
                DocSource::AmpEnv => ModSource::AmpEnv,
                DocSource::Velocity => ModSource::Velocity,
                DocSource::Note => ModSource::Note,
                DocSource::Random => ModSource::Random,
            },
            dest: match d.dest {
                DocDest::Off => ModDest::Off,
                DocDest::Pitch => ModDest::Pitch,
                DocDest::Osc1Pitch => ModDest::Osc1Pitch,
                DocDest::Osc2Pitch => ModDest::Osc2Pitch,
                DocDest::Osc1Pw => ModDest::Osc1Pw,
                DocDest::Osc2Pw => ModDest::Osc2Pw,
                DocDest::Osc1Level => ModDest::Osc1Level,
                DocDest::Osc2Level => ModDest::Osc2Level,
                DocDest::NoiseLevel => ModDest::NoiseLevel,
                DocDest::Cutoff => ModDest::Cutoff,
                DocDest::Resonance => ModDest::Resonance,
                DocDest::Pan => ModDest::Pan,
                DocDest::Amp => ModDest::Amp,
                DocDest::Lfo1Rate => ModDest::Lfo1Rate,
                DocDest::Lfo2Rate => ModDest::Lfo2Rate,
                DocDest::UnisonDetune => ModDest::UnisonDetune,
            },
            amount: d.amount.clamp(-1.0, 1.0),
        };
    }
    SynthSettings {
        osc: [
            osc(
                P::Osc1Wave,
                P::Osc1Octave,
                P::Osc1Semi,
                P::Osc1Fine,
                P::Osc1Level,
                P::Osc1Pw,
            ),
            osc(
                P::Osc2Wave,
                P::Osc2Octave,
                P::Osc2Semi,
                P::Osc2Fine,
                P::Osc2Level,
                P::Osc2Pw,
            ),
        ],
        sub_level: v(P::SubLevel),
        noise_level: v(P::NoiseLevel),
        unison: v(P::UnisonVoices) as u8,
        unison_detune_cents: v(P::UnisonDetune),
        unison_spread: v(P::UnisonSpread),
        cutoff_hz: v(P::Cutoff),
        resonance: v(P::Resonance),
        filter_env_octaves: v(P::FilterEnv),
        key_track: v(P::KeyTrack),
        drive: v(P::Drive),
        amp_env: env(P::AmpAttack, P::AmpDecay, P::AmpSustain, P::AmpRelease),
        mod_env: env(P::ModAttack, P::ModDecay, P::ModSustain, P::ModRelease),
        lfo: [
            LfoSettings {
                wave: lfo_wave(v(P::Lfo1Wave)),
                rate_hz: v(P::Lfo1Rate),
            },
            LfoSettings {
                wave: lfo_wave(v(P::Lfo2Wave)),
                rate_hz: v(P::Lfo2Rate),
            },
        ],
        glide_ms: v(P::Glide),
        velocity_sensitivity: v(P::VelocitySens),
        volume: v(P::Volume),
        mods,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gt_core::{Project, PPQ};

    fn ons(s: &SongSnapshot) -> Vec<(i64, u16)> {
        s.events
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::On { .. }))
            .map(|e| (e.tick, e.slot))
            .collect()
    }

    #[test]
    fn demo_pattern_compiles_to_sorted_events() {
        let p = Project::demo();
        let s = SongSnapshot::compile(&p);
        assert_eq!(s.length, 4 * PPQ);
        assert!(s.events.windows(2).all(|w| w[0].order() <= w[1].order()));
        let kicks: Vec<_> = ons(&s).into_iter().filter(|&(_, slot)| slot == 0).collect();
        assert_eq!(kicks, vec![(0, 0), (960, 0), (1920, 0), (2880, 0)]);
        // Every on has an off.
        let offs = s.events.iter().filter(|e| e.kind == NoteKind::Off).count();
        assert_eq!(offs * 2, s.events.len());
    }

    #[test]
    fn swing_delays_odd_steps_only() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        for step in 0..4 {
            p.current_pattern_mut().toggle_step(c, step);
        }
        p.swing = 1.0;
        let s = SongSnapshot::compile(&p);
        assert_eq!(
            ons(&s).iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![0, 240 + 120, 480, 720 + 120]
        );
        p.swing = 0.5;
        let s = SongSnapshot::compile(&p);
        assert_eq!(ons(&s)[1].0, 240 + 60);
    }

    #[test]
    fn offs_wrap_and_order_before_ons() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        p.current_pattern_mut().toggle_step(c, 15);
        p.current_pattern_mut().toggle_step(c, 0);
        let s = SongSnapshot::compile(&p);
        // Step 15's off lands on tick 0 of the next repetition, before step 0's on.
        assert_eq!(s.events[0].tick, 0);
        assert_eq!(s.events[0].kind, NoteKind::Off);
        assert!(matches!(s.events[1].kind, NoteKind::On { .. }));
    }

    #[test]
    fn pattern_length_covers_notes_past_the_grid() {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        let pat = p.current_pattern_mut();
        pat.steps = 32;
        pat.toggle_step(c, 20);
        pat.toggle_step(c, 2);
        pat.steps = 16;
        let s = SongSnapshot::compile(&p);
        assert_eq!(s.length, 32 * STEP_TICKS);
        assert_eq!(ons(&s), vec![(480, 0), (20 * 240, 0)]);
    }

    #[test]
    fn events_in_is_inclusive() {
        let s = SongSnapshot::compile(&Project::demo());
        let e = s.events_in(960, 960);
        assert!(e.iter().all(|e| e.tick == 960));
        assert!(e.iter().any(|e| e.slot == 0));
        assert!(s.events_in(5, 4).is_empty());
    }

    #[test]
    fn default_patch_maps_to_the_synth_defaults() {
        // The document defaults and the DSP defaults describe the same sound.
        assert_eq!(
            synth_settings(&SynthPatch::default()),
            SynthSettings::default()
        );
        let p = Project::demo();
        let bass = ChannelParams::from_channel(&p.channels[4], false);
        assert_eq!(bass.kind, InstrumentKind::Synth);
        assert_eq!(bass.synth.sub_level, 0.5);
    }

    #[test]
    fn silenced_channels_have_zero_gain() {
        let p = Project::demo();
        assert_eq!(ChannelParams::from_channel(&p.channels[0], true).gain, 0.0);
        assert_eq!(
            ChannelParams::from_channel(&p.channels[0], false).gain,
            0.63
        );
    }
}
