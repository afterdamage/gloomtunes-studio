//! Compiled, real-time-friendly views of the project document.
//!
//! The UI thread compiles the document into these flat structures and sends them boxed to the
//! audio thread, which only reads them. Replacements come back through the garbage queue.

use std::sync::Arc;

use gt_core::playlist::Curve;
use gt_core::synth::{
    ModDest as DocDest, ModSlot as DocModSlot, ModSource as DocSource, SynthParam as P,
};
use gt_core::{
    Adsr, AutoPoint, Channel, ClipKind, Instrument, LoopMode, ParamId, ParamInfo, Pattern, Project,
    SampleData, SampleSource, SynthPatch, STEP_TICKS,
};

use crate::control::ParamDest;
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

/// An audio clip as the engine plays it. Times are song seconds (from tick 0 through the
/// tempo map), so the audio keeps its own speed across tempo changes.
#[derive(Debug, Clone)]
pub struct AudioPlay {
    /// Song time of the clip's left edge.
    pub start_s: f64,
    /// Song time of the clip's right edge.
    pub end_s: f64,
    /// Song time at which the sample's first frame would play (left edge minus the slip
    /// offset); may be before `start_s`.
    pub origin_s: f64,
    /// The audio.
    pub sample: Arc<SampleData>,
    /// Linear gain.
    pub gain: f32,
    /// Mixer strip it plays into.
    pub route: u8,
    /// Fade in at the left edge (the clip starts inside the audio, so it would click).
    pub fade_in: bool,
    /// Fade out at the right edge (the clip ends before the audio does).
    pub fade_out: bool,
}

/// One automated parameter over the whole timeline: absolute (tick, normalized value) points
/// shaped by their curves, holding the first and last values outside them.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoLane {
    /// Where the parameter lives.
    pub dest: ParamDest,
    /// Range and taper, to turn normalized values into plain ones.
    pub info: ParamInfo,
    /// Points sorted by tick.
    pub points: Vec<AutoPoint>,
    /// Set by the engine when a modulator of the same parameter adds to this lane (the lane
    /// is then applied with the modulation instead of on its own).
    pub(crate) modulated: bool,
}

/// What the engine plays, flattened from the document.
///
/// Pattern mode: one pattern's note events, repeating every `length` ticks from tick 0. Song
/// mode: every clip of the playlist laid out on the timeline once, with audio clips and
/// automation lanes.
#[derive(Debug, Clone, Default)]
pub struct SongSnapshot {
    /// Pattern length (pattern mode) or where the last clip ends (song mode). 0: no notes.
    pub length: i64,
    /// True in pattern mode: events repeat every `length` ticks.
    pub repeat: bool,
    /// Events sorted by [`SongEvent::order`]; ticks within the pattern (pattern mode) or on
    /// the timeline (song mode).
    pub events: Vec<SongEvent>,
    /// Audio clips sorted by start (song mode).
    pub audio: Vec<AudioPlay>,
    /// Automation lanes (song mode).
    pub automation: Vec<AutoLane>,
}

impl SongSnapshot {
    /// Compiles the current pattern of `project` (pattern mode). Slots are channel indices.
    /// Swing delays notes that start on an odd 1/16 step by `swing * STEP_TICKS / 2`. Note-offs
    /// past the pattern end wrap around to the next repetition. The pattern length fits all
    /// notes (`Pattern::length_ticks`); notes with a negative start are skipped.
    pub fn compile(project: &Project) -> Self {
        let pattern = project.current_pattern();
        let length = pattern.length_ticks();
        let swing = swing_ticks(project);
        let mut events = Vec::new();
        for (slot, ch) in project.channels.iter().enumerate() {
            for n in pattern.channel_notes(ch.id) {
                if n.start < 0 || n.start >= length {
                    continue;
                }
                let on = n.start + swing_of(n.start, swing);
                let off = on + n.length.max(1);
                push_note(
                    &mut events,
                    slot,
                    n.key,
                    n.velocity,
                    on.rem_euclid(length),
                    off.rem_euclid(length),
                );
            }
        }
        events.sort_by_key(SongEvent::order);
        Self {
            length,
            repeat: true,
            events,
            ..Self::default()
        }
    }

    /// Compiles the playlist (song mode). Pattern clips repeat their pattern for the clip's
    /// length and cut notes at the clip's end; muted clips and clips on silent tracks are left
    /// out. Audio clips whose sample `samples` cannot supply yet are skipped. Automation clips
    /// on the same target merge into one lane; where clips overlap, the later-starting one wins.
    pub fn compile_song(
        project: &Project,
        samples: impl Fn(&SampleSource) -> Option<Arc<SampleData>>,
    ) -> Self {
        let pl = &project.playlist;
        let swing = swing_ticks(project);
        let tempo = &project.tempo;
        let mut events = Vec::new();
        let mut audio: Vec<AudioPlay> = Vec::new();
        let mut lanes: Vec<(ParamId, Vec<Span<'_>>)> = Vec::new();
        for clip in &pl.clips {
            if clip.muted || !pl.is_track_audible(clip.track) {
                continue;
            }
            match &clip.kind {
                ClipKind::Pattern(id) => {
                    if let Some(pat) = project.pattern(*id) {
                        expand_pattern(project, pat, clip, swing, &mut events);
                    }
                }
                ClipKind::Audio { source, gain } => {
                    let Some(sample) = samples(source) else {
                        continue;
                    };
                    let route = pl
                        .track(clip.track)
                        .map_or(0, |t| t.insert.min(gt_core::STRIPS - 1));
                    let sec = |t: i64| tempo.tick_to_seconds(t as f64);
                    let (start_s, end_s, origin_s) = (
                        sec(clip.start),
                        sec(clip.end()),
                        sec(clip.start - clip.offset),
                    );
                    let fade_out = end_s < origin_s + sample.seconds();
                    audio.push(AudioPlay {
                        start_s,
                        end_s,
                        origin_s,
                        fade_in: clip.offset > 0,
                        fade_out,
                        sample,
                        gain: gain.clamp(0.0, 4.0),
                        route: route as u8,
                    });
                }
                ClipKind::Automation(a) => {
                    if !a.target.is_valid(project) || a.points.is_empty() {
                        continue;
                    }
                    let span = (clip.start, clip.end(), clip.offset, a.points.as_slice());
                    match lanes.iter_mut().find(|(t, _)| *t == a.target) {
                        Some((_, v)) => v.push(span),
                        None => lanes.push((a.target, vec![span])),
                    }
                }
            }
        }
        events.sort_by_key(SongEvent::order);
        audio.sort_by(|a: &AudioPlay, b| a.start_s.total_cmp(&b.start_s));
        let automation = lanes
            .into_iter()
            .filter_map(|(target, spans)| {
                Some(AutoLane {
                    dest: ParamDest::resolve(&target, project)?,
                    info: *target.info(),
                    points: lane_points(spans),
                    modulated: false,
                })
            })
            .collect();
        Self {
            length: pl.song_end(),
            repeat: false,
            events,
            audio,
            automation,
        }
    }

    /// Events with `lo <= tick <= hi`.
    pub fn events_in(&self, lo: i64, hi: i64) -> &[SongEvent] {
        let a = self.events.partition_point(|e| e.tick < lo);
        let b = self.events.partition_point(|e| e.tick <= hi);
        &self.events[a..b.max(a)]
    }
}

/// An automation clip on the timeline: start, end, source offset and its points.
type Span<'a> = (i64, i64, i64, &'a [AutoPoint]);

fn swing_ticks(project: &Project) -> i64 {
    (f64::from(project.swing.clamp(0.0, 1.0)) * STEP_TICKS as f64 / 2.0).round() as i64
}

/// Swing delay of a note starting at `start` (odd 1/16 steps only).
fn swing_of(start: i64, swing: i64) -> i64 {
    if start.rem_euclid(2 * STEP_TICKS) == STEP_TICKS {
        swing
    } else {
        0
    }
}

fn push_note(events: &mut Vec<SongEvent>, slot: usize, key: u8, velocity: f32, on: i64, off: i64) {
    events.push(SongEvent {
        tick: on,
        slot: slot as u16,
        key,
        kind: NoteKind::On {
            velocity: velocity.clamp(0.0, 1.0),
        },
    });
    events.push(SongEvent {
        tick: off,
        slot: slot as u16,
        key,
        kind: NoteKind::Off,
    });
}

/// Adds the notes a pattern clip plays: the pattern repeats from source position 0 every
/// pattern length; the clip shows source `[offset, offset + length)`.
fn expand_pattern(
    project: &Project,
    pat: &Pattern,
    clip: &gt_core::Clip,
    swing: i64,
    events: &mut Vec<SongEvent>,
) {
    let len = pat.length_ticks();
    if len <= 0 {
        return;
    }
    let (src_lo, src_hi) = (clip.offset, clip.offset + clip.length);
    let first = src_lo.div_euclid(len) - 1; // swing can push a note into the next repetition
    let last = (src_hi - 1).div_euclid(len);
    for (slot, ch) in project.channels.iter().enumerate() {
        for n in pat.channel_notes(ch.id) {
            if n.start < 0 || n.start >= len {
                continue;
            }
            let src_on = n.start + swing_of(n.start, swing);
            for k in first..=last {
                let src = k * len + src_on;
                if src < src_lo || src >= src_hi {
                    continue;
                }
                let on = clip.start + (src - src_lo);
                let off = (on + n.length.max(1)).min(clip.end());
                push_note(events, slot, n.key, n.velocity, on, off);
            }
        }
    }
}

/// Segments a clip cut inside a curved segment is resampled into (straight pieces).
const CUT_PIECES: usize = 16;

/// Lays clip spans `(start, end, offset, points)` out on the timeline. Spans are taken in start
/// order and each is cut where the next begins. Inside a span the clip's points keep their
/// curves; a curved segment that the cut splits is resampled into straight pieces, since a
/// piece of an S-curve is not an S-curve. Between spans the last value holds.
fn lane_points(mut spans: Vec<Span<'_>>) -> Vec<AutoPoint> {
    spans.sort_by_key(|s| s.0);
    let mut out = Vec::new();
    for (i, &(start, end, offset, points)) in spans.iter().enumerate() {
        let end = spans.get(i + 1).map_or(end, |n| end.min(n.0));
        if end <= start {
            continue;
        }
        let (lo, hi) = (offset, offset + (end - start));
        let to_tick = |x: i64| start + x - offset;
        let value = |x: f64| gt_core::playlist::value_at(points, x);
        let mut x = lo;
        while x < hi {
            // The segment containing x: from points[k - 1] to points[k].
            let k = points.partition_point(|p| p.at <= x);
            // Several points on one tick make a step: keep all but the last (which starts the
            // segment below), so the value just before x stays what the clip drew.
            let first = points.partition_point(|p| p.at < x);
            for p in points.get(first..k.saturating_sub(1)).unwrap_or(&[]) {
                out.push(AutoPoint {
                    at: to_tick(x),
                    value: p.value,
                    curve: p.curve,
                });
            }
            let seg_end = points.get(k).map_or(hi, |p| p.at.min(hi));
            let curve = match k {
                0 => Curve::Hold,
                k if k == points.len() => Curve::Hold,
                k => points[k - 1].curve,
            };
            let whole = k > 0 && points[k - 1].at == x && points.get(k).is_some_and(|p| p.at <= hi);
            if whole || curve.cuts_cleanly() {
                out.push(AutoPoint {
                    at: to_tick(x),
                    value: value(x as f64),
                    curve,
                });
            } else {
                let span = (seg_end - x) as f64;
                for j in 0..CUT_PIECES {
                    let at = x as f64 + span * j as f64 / CUT_PIECES as f64;
                    out.push(AutoPoint {
                        at: to_tick(at.round() as i64),
                        value: value(at),
                        curve: Curve::Linear,
                    });
                }
            }
            x = seg_end;
        }
        out.push(AutoPoint {
            at: end,
            value: value(hi as f64),
            curve: Curve::Hold,
        });
    }
    // Resampling can round two points onto one tick; keep the later.
    out.dedup_by(|b, a| {
        if a.at == b.at && a.curve == Curve::Linear && b.curve == Curve::Linear {
            *a = *b;
            true
        } else {
            false
        }
    });
    out
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
    /// The synth's knobs and modulation matrix (used when `kind` is `Synth`).
    pub patch: PatchValues,
    /// Linear gain (the channel's volume).
    pub gain: f32,
    /// Muted, or silenced by another channel's solo: the channel plays at gain 0.
    pub silenced: bool,
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
    /// Mixer strip the channel plays into.
    pub route: u8,
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            kind: InstrumentKind::Sampler,
            patch: PatchValues::default(),
            gain: 0.0,
            silenced: false,
            pan: 0.0,
            pitch: 0.0,
            start: 0.0,
            end: 1.0,
            looped: false,
            adsr: Adsr::default(),
            route: 0,
        }
    }
}

impl ChannelParams {
    /// Engine parameters for a document channel. `silenced` is the effective mute (mute, or
    /// another channel is soloed).
    pub fn from_channel(ch: &Channel, silenced: bool) -> Self {
        let mut p = Self {
            gain: ch.volume.clamp(0.0, Channel::MAX_VOLUME),
            silenced,
            pan: ch.pan.clamp(-1.0, 1.0),
            route: ch.insert.min(gt_core::STRIPS - 1) as u8,
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
                p.patch = PatchValues {
                    values: patch.values,
                    mods: patch.mods,
                };
            }
        }
        p
    }
}

/// A Gloom Synth patch without its name: plain values by `SynthParam` index and the
/// modulation matrix. Copyable, so the engine can keep one per channel and apply automation
/// to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatchValues {
    /// Values in plain units, indexed by `SynthParam`.
    pub values: [f32; P::COUNT],
    /// Modulation matrix.
    pub mods: [DocModSlot; gt_core::synth::MOD_SLOTS],
}

impl Default for PatchValues {
    fn default() -> Self {
        let p = SynthPatch::default();
        Self {
            values: p.values,
            mods: p.mods,
        }
    }
}

impl PatchValues {
    /// The synth's typed settings.
    pub fn settings(&self) -> SynthSettings {
        synth_settings_of(&self.values, &self.mods)
    }
}

/// Converts a document patch (values by parameter index) to the synth's typed settings.
pub fn synth_settings(patch: &SynthPatch) -> SynthSettings {
    synth_settings_of(&patch.values, &patch.mods)
}

/// [`synth_settings`] from the values and matrix alone. Real-time safe.
fn synth_settings_of(
    values: &[f32; P::COUNT],
    doc_mods: &[DocModSlot; gt_core::synth::MOD_SLOTS],
) -> SynthSettings {
    let v = |p: P| p.info().clamp(values[p as usize]);
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
    for (m, d) in mods.iter_mut().zip(doc_mods) {
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
        assert_eq!(bass.patch.settings().sub_level, 0.5);
    }

    #[test]
    fn silenced_channels_keep_their_volume_and_say_so() {
        let p = Project::demo();
        let a = ChannelParams::from_channel(&p.channels[0], true);
        assert!(a.silenced);
        assert_eq!(a.gain, 0.63);
        assert!(!ChannelParams::from_channel(&p.channels[0], false).silenced);
    }

    fn song_project() -> (Project, gt_core::ChannelId, gt_core::PatternId) {
        let mut p = Project::empty();
        let c = p.add_channel("c", None).unwrap();
        for step in [0, 4, 8, 12] {
            p.current_pattern_mut().toggle_step(c, step);
        }
        let id = p.current_pattern;
        (p, c, id)
    }

    #[test]
    fn pattern_clips_repeat_cut_and_slip() {
        let (mut p, _, id) = song_project();
        let t = p.playlist.tracks[0].id;
        // 2.5 bars of a 1-bar pattern from bar 2, slipped by one beat.
        let clip = p.playlist.add_clip(t, 3840, 9600, ClipKind::Pattern(id));
        p.playlist.clip_mut(clip).unwrap().offset = 960;
        let s = SongSnapshot::compile_song(&p, |_| None);
        assert!(!s.repeat);
        assert_eq!(s.length, 3840 + 9600);
        let on: Vec<i64> = ons(&s).iter().map(|e| e.0).collect();
        // Source beats 1, 2, 3 of the first repetition, then 0..3, then 0..1 (cut at 2.5 bars).
        let want: Vec<i64> = (0..10).map(|k| 3840 + k * 960).collect();
        assert_eq!(on, want);
        // The last note's off is cut at the clip end; none passes it.
        assert!(s.events.iter().all(|e| e.tick <= 3840 + 9600));
        assert!(s.events.windows(2).all(|w| w[0].order() <= w[1].order()));
    }

    #[test]
    fn muted_clips_and_silent_tracks_do_not_play() {
        let (mut p, _, id) = song_project();
        let (a, b) = (p.playlist.tracks[0].id, p.playlist.tracks[1].id);
        let x = p.playlist.add_clip(a, 0, 3840, ClipKind::Pattern(id));
        p.playlist.add_clip(b, 3840, 3840, ClipKind::Pattern(id));
        let count = |p: &Project| ons(&SongSnapshot::compile_song(p, |_| None)).len();
        assert_eq!(count(&p), 8);
        p.playlist.clip_mut(x).unwrap().muted = true;
        assert_eq!(count(&p), 4);
        p.playlist.clip_mut(x).unwrap().muted = false;
        p.playlist.tracks[1].solo = true;
        assert_eq!(count(&p), 4);
        p.playlist.tracks[1].mute = true;
        assert_eq!(count(&p), 0);
    }

    #[test]
    fn audio_clips_follow_the_tempo_map() {
        let mut p = Project::empty();
        // 120 BPM for a half note, then 60 BPM.
        p.tempo = gt_core::TempoMap::from_points_lossy(&[
            gt_core::TempoPoint {
                at: gt_core::Tick(0),
                bpm: 120.0,
            },
            gt_core::TempoPoint {
                at: gt_core::Tick(1920),
                bpm: 60.0,
            },
        ]);
        p.playlist.tracks[2].insert = 7;
        let t = p.playlist.tracks[2].id;
        let src = SampleSource::BuiltIn(gt_core::BuiltInSample::Kick);
        let c = p.playlist.add_clip(
            t,
            3840,
            960,
            ClipKind::Audio {
                source: src.clone(),
                gain: 0.5,
            },
        );
        p.playlist.clip_mut(c).unwrap().offset = 960;
        let data = Arc::new(SampleData::mono(48_000, vec![0.0; 144_000]));
        assert!(SongSnapshot::compile_song(&p, |_| None).audio.is_empty());
        let s = SongSnapshot::compile_song(&p, |x| (x == &src).then(|| Arc::clone(&data)));
        let a = &s.audio[0];
        // Tick 3840 = 1 s + 1920 ticks at 1 s per beat = 3 s; the clip lasts one beat (1 s).
        assert_eq!((a.start_s, a.end_s), (3.0, 4.0));
        assert_eq!(a.origin_s, 2.0, "slipped by one beat at 60 BPM");
        assert_eq!((a.route, a.gain), (7, 0.5));
        assert!(a.fade_in && a.fade_out, "cut on both sides of 3 s of audio");
    }

    #[test]
    fn automation_lanes_merge_clips_and_last_start_wins() {
        let mut p = Project::demo();
        let t = p.playlist.tracks[6].id;
        let target = ParamId::strip_pan(2);
        let auto = |v0, v1| {
            ClipKind::Automation(gt_core::Automation {
                target,
                points: vec![AutoPoint::new(0, v0), AutoPoint::new(1000, v1)],
            })
        };
        p.playlist.add_clip(t, 0, 1000, auto(0.0, 1.0));
        p.playlist.add_clip(t, 500, 1000, auto(0.2, 0.2));
        let s = SongSnapshot::compile_song(&p, |_| None);
        let lane = s
            .automation
            .iter()
            .find(|l| {
                l.dest
                    == ParamDest::Strip {
                        strip: 2,
                        param: gt_core::StripParam::Pan,
                    }
            })
            .expect("lane");
        let v = |t: f64| gt_core::playlist::value_at(&lane.points, t);
        assert_eq!(v(0.0), 0.0);
        assert_eq!(v(250.0), 0.25);
        assert_eq!(v(499.0), 0.499);
        assert_eq!(v(500.0), 0.2, "the later clip takes over");
        assert_eq!(v(5000.0), 0.2, "holds after the last clip");
        assert_eq!(lane.info.from_normalized(0.25), -0.5);
        // The demo's delay swell and bass filter are there too.
        assert_eq!(s.automation.len(), 3);
    }

    #[test]
    fn lanes_keep_curves_and_resample_cut_ones() {
        let mut p = Project::empty();
        let t = p.playlist.tracks[0].id;
        let target = ParamId::strip_pan(3);
        let pts = vec![
            AutoPoint {
                at: 0,
                value: 0.0,
                curve: Curve::Smooth,
            },
            AutoPoint {
                at: 1000,
                value: 1.0,
                curve: Curve::Bezier(0.6),
            },
            AutoPoint::new(2000, 0.0),
        ];
        let c = p.playlist.add_clip(
            t,
            5000,
            2000,
            ClipKind::Automation(gt_core::Automation {
                target,
                points: pts.clone(),
            }),
        );
        // An uncut clip keeps the points and curves as they are.
        let s = SongSnapshot::compile_song(&p, |_| None);
        assert_eq!(
            s.automation[0].points[..2]
                .iter()
                .map(|q| (q.at - 5000, q.value, q.curve))
                .collect::<Vec<_>>(),
            pts[..2]
                .iter()
                .map(|q| (q.at, q.value, q.curve))
                .collect::<Vec<_>>()
        );
        // Slipped 300 ticks: the clip starts inside the S-curve and ends inside the Bézier.
        p.playlist.clip_mut(c).unwrap().offset = 300;
        p.playlist.clip_mut(c).unwrap().length = 1600;
        let s = SongSnapshot::compile_song(&p, |_| None);
        let lane = &s.automation[0];
        let want = |tick: i64| gt_core::playlist::value_at(&pts, (tick - 5000 + 300) as f64);
        let got = |tick: i64| gt_core::playlist::value_at(&lane.points, tick as f64);
        for tick in (5000..6600).step_by(37) {
            assert!(
                (got(tick) - want(tick)).abs() < 0.01,
                "{tick}: {} vs {}",
                got(tick),
                want(tick)
            );
        }
        // Both segments are cut, so both are resampled, but the peak point survives exactly;
        // the value holds after the clip.
        assert!(lane.points.iter().any(|q| q.at == 5700 && q.value == 1.0));
        assert!((got(9000) - want(6600)).abs() < 1e-6);
        assert!(lane.points.windows(2).all(|w| w[0].at <= w[1].at));
    }
}
