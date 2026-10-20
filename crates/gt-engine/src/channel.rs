//! Channel slots: preallocated voice pools that play a sample or Gloom Synth.

use std::sync::Arc;

use gt_core::params::ChannelParam;
use gt_core::synth::{SynthParam, MOD_SLOTS};
use gt_core::SampleData;
use gt_dsp::synth::PORTA_DEFAULT_S;
use gt_dsp::{
    AdsrParams, Expression, Glide, GloomSynth, LinearRamp, NoteExpr, SamplerVoice, VoiceRegion,
    VoiceSettings,
};

use crate::song::{ChannelParams, InstrumentKind, PatchValues};
use crate::zones::{ZoneMap, MAX_AUX};
use crate::RENDER_QUANTUM;

/// Zone index of a voice playing the channel's single sample (no key map).
const MAIN_ZONE: u16 = u16::MAX;

/// Voices per channel. When all are busy, the oldest is stolen.
pub const VOICES_PER_CHANNEL: usize = 16;
/// Gain/pan smoothing time in seconds.
const GAIN_RAMP_S: f32 = 0.01;

/// Values set by automation or modulation. `None` means "use the document's".
#[derive(Debug, Clone, Copy)]
struct Controls {
    channel: [Option<f32>; ChannelParam::ALL.len()],
    synth: [Option<f32>; SynthParam::COUNT],
    mods: [Option<f32>; MOD_SLOTS],
    /// The synth's settings must be rebuilt before the next render.
    synth_dirty: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            channel: [None; ChannelParam::ALL.len()],
            synth: [None; SynthParam::COUNT],
            mods: [None; MOD_SLOTS],
            synth_dirty: false,
        }
    }
}

/// One channel of the rack on the audio thread. Everything is allocated at engine creation:
/// each slot holds both a sampler voice pool and a Gloom Synth, and plays whichever its
/// parameters select.
pub(crate) struct ChannelSlot {
    synth: Box<GloomSynth>,
    sr: f32,
    /// The document's settings.
    params: ChannelParams,
    /// Automation and modulation on top of `params`.
    ctl: Controls,
    env: AdsrParams,
    sample: Option<Arc<SampleData>>,
    /// Multi-sample key map (Prompt 23); when set it replaces `sample`.
    zones: Option<Arc<ZoneMap>>,
    /// Round-robin turn.
    turn: u32,
    /// How voices read and shape the sample (from the params, with automation).
    voice_set: VoiceSettings,
    voices: [SamplerVoice; VOICES_PER_CHANNEL],
    /// Separate outputs of the key map's zones (drum pads), one quantum each.
    aux: Box<[[[f32; RENDER_QUANTUM]; 2]; MAX_AUX]>,
    gain_l: LinearRamp,
    gain_r: LinearRamp,
    ramp_frames: u32,
    /// Plugin table entry running this channel's instrument plugin.
    pub(crate) plugin: Option<u8>,
    /// Pitch bend (-1 to 1), mod wheel and aftertouch from live input (Prompt 16).
    bend: f32,
    expr: Expression,
    /// Sustain pedal down, and the sampler voices it holds after their key was let go.
    sustain: bool,
    sustained: [bool; VOICES_PER_CHANNEL],
    /// Read step of the last sampler note, where portamento starts from.
    last_step: Option<f64>,
}

/// Mod X below the middle closes a per-voice low-pass down to this frequency at 0.
const TONE_MIN_HZ: f32 = 200.0;
/// Mod Y above the middle starts the sample up to this far into its region.
const START_OFFSET_MAX: f64 = 0.5;

/// One-pole coefficient for mod X: open from the middle up, closing to [`TONE_MIN_HZ`].
fn tone_coef(sr: f32, e: &NoteExpr) -> f32 {
    let x = e.x();
    if x >= 0.0 {
        return 1.0;
    }
    // From 20 kHz at the middle down to 200 Hz at 0, evenly in octaves.
    let hz = TONE_MIN_HZ * 100.0_f32.powf(1.0 + x);
    1.0 - (-std::f32::consts::TAU * hz / sr.max(1.0)).exp()
}

/// Equal-power pan law: -3 dB per side at centre, so a sound keeps its loudness as it moves.
fn pan_gains(gain: f32, pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (gain * theta.cos(), gain * theta.sin())
}

impl ChannelSlot {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let params = ChannelParams::default();
        // Interpolation tables are built here, not on the audio thread.
        SamplerVoice::warm_up();
        Self {
            synth: Box::new(GloomSynth::new(sample_rate)),
            sr: sample_rate,
            params,
            ctl: Controls::default(),
            env: Self::env_for(sample_rate, &params),
            sample: None,
            zones: None,
            turn: 0,
            voice_set: VoiceSettings::default(),
            voices: [SamplerVoice::new(sample_rate); VOICES_PER_CHANNEL],
            aux: Box::new([[[0.0; RENDER_QUANTUM]; 2]; MAX_AUX]),
            gain_l: LinearRamp::new(0.0),
            gain_r: LinearRamp::new(0.0),
            ramp_frames: (sample_rate * GAIN_RAMP_S) as u32,
            plugin: None,
            bend: 0.0,
            expr: Expression::default(),
            sustain: false,
            sustained: [false; VOICES_PER_CHANNEL],
            last_step: None,
        }
    }

    /// The instrument plugin instance this channel plays, if it is a plugin channel.
    pub(crate) fn plugin_instance(&self) -> Option<gt_core::PluginInstanceId> {
        (self.params.kind == InstrumentKind::Plugin)
            .then_some(self.params.plugin)
            .flatten()
    }

    /// The channel's settings.
    pub(crate) fn params(&self) -> &ChannelParams {
        &self.params
    }

    /// True for a plugin channel (its notes and audio go through the plugin table).
    pub(crate) fn is_plugin(&self) -> bool {
        self.params.kind == InstrumentKind::Plugin
    }

    fn env_for(sr: f32, p: &ChannelParams) -> AdsrParams {
        let a = p.adsr;
        AdsrParams::new(sr, a.attack_ms, a.decay_ms, a.sustain, a.release_ms)
    }

    /// The document settings with automation and modulation applied.
    fn effective(&self) -> ChannelParams {
        let mut p = self.params;
        let c = |k: ChannelParam| self.ctl.channel[k as usize];
        p.gain = c(ChannelParam::Volume).unwrap_or(p.gain);
        p.pan = c(ChannelParam::Pan).unwrap_or(p.pan);
        p.pitch = c(ChannelParam::Pitch).unwrap_or(p.pitch);
        p.start = c(ChannelParam::Start).unwrap_or(p.start);
        p.end = c(ChannelParam::End).unwrap_or(p.end).max(p.start);
        p.adsr.attack_ms = c(ChannelParam::Attack).unwrap_or(p.adsr.attack_ms);
        p.adsr.decay_ms = c(ChannelParam::Decay).unwrap_or(p.adsr.decay_ms);
        p.adsr.sustain = c(ChannelParam::Sustain).unwrap_or(p.adsr.sustain);
        p.adsr.release_ms = c(ChannelParam::Release).unwrap_or(p.adsr.release_ms);
        p.sampler.cutoff = c(ChannelParam::Cutoff).unwrap_or(p.sampler.cutoff);
        p.sampler.resonance = c(ChannelParam::Resonance).unwrap_or(p.sampler.resonance);
        p
    }

    /// Voice settings for the effective parameters.
    fn voice_settings(sr: f32, p: &ChannelParams) -> VoiceSettings {
        let s = &p.sampler;
        let e = s.filter_env;
        VoiceSettings {
            sinc: s.sinc,
            reverse: s.reverse,
            filter: s.filter,
            cutoff: s.cutoff,
            // Resonance 0..1 to Q 0.5..15, evenly in ratio.
            q: 0.5 * 30.0_f32.powf(s.resonance.clamp(0.0, 1.0)),
            env_octaves: s.env_amount,
            filter_env: AdsrParams::new(sr, e.attack_ms, e.decay_ms, e.sustain, e.release_ms),
            lfos: s.lfos,
        }
    }

    /// The synth patch with automation and modulation applied.
    fn effective_patch(&self) -> PatchValues {
        let mut v = self.params.patch;
        for (x, c) in v.values.iter_mut().zip(&self.ctl.synth) {
            if let Some(c) = c {
                *x = *c;
            }
        }
        for (m, c) in v.mods.iter_mut().zip(&self.ctl.mods) {
            if let Some(c) = c {
                m.amount = *c;
            }
        }
        v
    }

    /// Points the gain ramps at the effective gain and pan over `frames`.
    fn retarget(&mut self, frames: u32) {
        let p = self.effective();
        let gain = if p.silenced { 0.0 } else { p.gain };
        let (l, r) = pan_gains(gain, p.pan);
        self.gain_l.set_target(l, frames);
        self.gain_r.set_target(r, frames);
    }

    /// Applies new settings. Gain and pan glide over 10 ms; the rest applies to new notes.
    pub(crate) fn set_params(&mut self, p: ChannelParams) {
        if p.kind != self.params.kind {
            // The other instrument's voices would never get their note-offs.
            self.kill_all();
        }
        self.params = p;
        if self.bend != 0.0 {
            self.set_bend(self.bend);
        }
        if p.kind == InstrumentKind::Synth {
            self.synth.set_settings(&self.effective_patch().settings());
            self.ctl.synth_dirty = false;
        }
        self.env = Self::env_for(self.sr, &self.effective());
        self.voice_set = Self::voice_settings(self.sr, &self.effective());
        self.retarget(self.ramp_frames);
    }

    /// Like `set_params`, but jumps straight to the new gain (for a channel that was idle).
    pub(crate) fn set_params_now(&mut self, p: ChannelParams) {
        self.set_params(p);
        self.gain_l = LinearRamp::new(self.gain_l.target());
        self.gain_r = LinearRamp::new(self.gain_r.target());
    }

    /// Sets a channel or sampler parameter from automation or modulation. Volume and pan ramp
    /// to the new value over `frames`; pitch retunes sounding voices; the rest applies to new
    /// notes.
    pub(crate) fn control(&mut self, param: ChannelParam, value: f32, frames: u32) {
        let old = self.effective();
        let slot = &mut self.ctl.channel[param as usize];
        if *slot == Some(value) {
            return;
        }
        *slot = Some(value);
        match param {
            ChannelParam::Volume | ChannelParam::Pan => self.retarget(frames),
            ChannelParam::Pitch => {
                let ratio = f64::from(((value - old.pitch) / 12.0).exp2());
                for v in self.voices.iter_mut().filter(|v| v.is_active()) {
                    v.scale_step(ratio);
                }
            }
            ChannelParam::Attack
            | ChannelParam::Decay
            | ChannelParam::Sustain
            | ChannelParam::Release => {
                self.env = Self::env_for(self.sr, &self.effective());
            }
            ChannelParam::Start | ChannelParam::End => {}
            ChannelParam::Cutoff | ChannelParam::Resonance => {
                self.voice_set = Self::voice_settings(self.sr, &self.effective());
            }
        }
    }

    /// Sets a Gloom Synth knob (plain value, `SynthParam` index) from automation or
    /// modulation. Takes effect at the next `flush_controls`.
    pub(crate) fn control_synth(&mut self, index: usize, value: f32) {
        if let Some(slot) = self.ctl.synth.get_mut(index) {
            if *slot != Some(value) {
                *slot = Some(value);
                self.ctl.synth_dirty = true;
            }
        }
    }

    /// Sets the amount of a Gloom Synth modulation matrix row.
    pub(crate) fn control_synth_mod(&mut self, row: usize, value: f32) {
        if let Some(slot) = self.ctl.mods.get_mut(row) {
            if *slot != Some(value) {
                *slot = Some(value);
                self.ctl.synth_dirty = true;
            }
        }
    }

    /// Hands changed synth controls to the synth (once per quantum, however many changed).
    pub(crate) fn flush_controls(&mut self) {
        if self.ctl.synth_dirty {
            self.ctl.synth_dirty = false;
            if self.params.kind == InstrumentKind::Synth {
                self.synth.set_settings(&self.effective_patch().settings());
            }
        }
    }

    /// Drops every automation and modulation value; the document's settings apply again.
    pub(crate) fn clear_controls(&mut self) {
        let had = self.ctl.channel.iter().any(Option::is_some)
            || self.ctl.synth.iter().any(Option::is_some)
            || self.ctl.mods.iter().any(Option::is_some);
        if !had {
            return;
        }
        let pitch = self.effective().pitch;
        self.ctl = Controls::default();
        let ratio = f64::from(((self.params.pitch - pitch) / 12.0).exp2());
        for v in self.voices.iter_mut().filter(|v| v.is_active()) {
            v.scale_step(ratio);
        }
        let p = self.params;
        self.set_params(p);
    }

    /// Swaps the sample, returning the old one (for the garbage queue). Playing voices stop:
    /// they were reading the old data.
    pub(crate) fn set_sample(&mut self, s: Option<Arc<SampleData>>) -> Option<Arc<SampleData>> {
        self.kill_all();
        std::mem::replace(&mut self.sample, s)
    }

    /// Swaps the key map, returning the old one (for the garbage queue). Playing voices stop.
    /// Replaces the synth's wavetables; returns the old ones.
    pub(crate) fn set_tables(
        &mut self,
        t: Option<Arc<gt_dsp::synth::wavetable::SynthTables>>,
    ) -> Option<Arc<gt_dsp::synth::wavetable::SynthTables>> {
        self.synth.set_tables(t)
    }

    pub(crate) fn set_zones(&mut self, z: Option<Arc<ZoneMap>>) -> Option<Arc<ZoneMap>> {
        self.kill_all();
        std::mem::replace(&mut self.zones, z)
    }

    /// A free voice, or the oldest one when the pool or the voice limit is full.
    fn free_voice(&self) -> usize {
        let limit = usize::from(
            self.params
                .sampler
                .voices
                .clamp(1, VOICES_PER_CHANNEL as u8),
        );
        let active = self.voices.iter().filter(|v| v.is_active()).count();
        let free = (active < limit)
            .then(|| self.voices.iter().position(|v| !v.is_active()))
            .flatten();
        free.unwrap_or_else(|| {
            // Steal the oldest sounding voice. A hard cut, acceptable for now (ROADMAP Step 3
            // limits).
            let mut oldest: Option<usize> = None;
            for (i, v) in self.voices.iter().enumerate() {
                if v.is_active() && oldest.is_none_or(|o| v.age < self.voices[o].age) {
                    oldest = Some(i);
                }
            }
            oldest.unwrap_or(0)
        })
    }

    /// Starts a note. `age` orders voices for stealing.
    pub(crate) fn note_on(&mut self, key: u8, velocity: f32, age: u64) {
        self.note_on_expr(key, velocity, age, NoteExpr::default());
    }

    /// Starts a note with per-note values (Prompt 21). A slide note starts no voice: the
    /// newest held voice glides to its key instead.
    pub(crate) fn note_on_expr(&mut self, key: u8, velocity: f32, age: u64, e: NoteExpr) {
        if self.params.kind == InstrumentKind::Plugin {
            return;
        }
        if self.params.kind == InstrumentKind::Synth {
            self.synth.note_on_expr(key, velocity, age, e);
            return;
        }
        let p = self.effective();
        let sp = p.sampler;
        // The zones this note plays: from the key map, or the single sample over its range.
        let mut picked = [0u16; VOICES_PER_CHANNEL];
        let n = match &self.zones {
            Some(m) => {
                let n = m.pick(key, velocity, sp.round_robin, self.turn, &mut picked);
                self.turn = self.turn.wrapping_add(1);
                n
            }
            None if self.sample.as_ref().is_some_and(|s| s.frames() > 0)
                && (sp.key_lo..=sp.key_hi).contains(&key) =>
            {
                picked[0] = MAIN_ZONE;
                1
            }
            None => 0,
        };
        if n == 0 {
            return;
        }
        let glide_frames = (sp.glide_ms.max(0.0) * 0.001 * self.sr) as u32;
        let mut first = true;
        for &zone in &picked[..n] {
            let (root, tune, zgain, rate, frames, zpan, zenv, choke, part) = match &self.zones {
                Some(m) if zone != MAIN_ZONE => {
                    let z = &m.zones[usize::from(zone)];
                    let env = z.env.map(|a| {
                        AdsrParams::new(self.sr, a.attack_ms, a.decay_ms, a.sustain, a.release_ms)
                    });
                    let (rate, frames) = (z.data.sample_rate, z.data.frames());
                    let part = z.region.unwrap_or((p.start, p.end));
                    (
                        z.root, z.tune, z.gain, rate, frames, z.pan, env, z.choke, part,
                    )
                }
                _ => match &self.sample {
                    Some(s) => (
                        sp.root_key,
                        0.0,
                        1.0,
                        s.sample_rate,
                        s.frames(),
                        0.0,
                        None,
                        0,
                        (p.start, p.end),
                    ),
                    None => continue,
                },
            };
            if choke > 0 {
                if let Some(m) = &self.zones {
                    for v in self.voices.iter_mut().filter(|v| v.is_active()) {
                        if m.zones
                            .get(usize::from(v.zone))
                            .is_some_and(|z| z.choke == choke)
                        {
                            v.choke();
                        }
                    }
                }
            }
            if frames == 0 {
                continue;
            }
            let semis = p.pitch + self.expr.bend_semis + f32::from(key) - f32::from(root)
                + tune
                + e.fine / 100.0;
            let step = f64::from((semis / 12.0).exp2()) * f64::from(rate) / f64::from(self.sr);
            if first {
                first = false;
                if let Glide::Slide { seconds } = e.glide {
                    let newest = self
                        .voices
                        .iter_mut()
                        .filter(|v| v.is_held())
                        .max_by_key(|v| v.age);
                    if let Some(v) = newest {
                        v.glide_to(step, (seconds.max(0.0) * self.sr) as u32);
                        self.last_step = Some(step);
                        return;
                    }
                }
                if sp.mono {
                    let held = self
                        .voices
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.is_held())
                        .max_by_key(|(_, v)| v.age)
                        .map(|(i, _)| i);
                    if let (true, Some(i)) = (sp.legato, held) {
                        // Legato: the sounding voice glides to the new key.
                        self.voices[i].legato_to(key, step, glide_frames.max(1));
                        self.sustained[i] = false;
                        self.last_step = Some(step);
                        return;
                    }
                    for v in &mut self.voices {
                        if v.is_active() {
                            v.release_any();
                        }
                    }
                }
            }
            let region = VoiceRegion::from_fractions(frames, part.0, part.1, p.looped);
            let v = velocity.clamp(0.0, 1.0);
            // Velocity to amplitude: squared, a common curve that spreads soft hits more evenly
            // in loudness than a linear map.
            let gain = v * v * zgain;
            let idx = self.free_voice();
            let from = match (e.glide, self.last_step) {
                (Glide::Porta, Some(last)) => last,
                (_, Some(last)) if sp.mono && glide_frames > 0 => last,
                _ => step,
            };
            let voice = &mut self.voices[idx];
            voice.trigger(key, gain, from, region, zenv.unwrap_or(self.env), age);
            voice.zone = zone;
            if self.voice_set != VoiceSettings::default() {
                voice.set_settings(self.voice_set);
            }
            let pan = if zpan == 0.0 {
                e.pan
            } else {
                (e.pan + zpan).clamp(-1.0, 1.0)
            };
            voice.set_note_values(pan, tone_coef(self.sr, &e), e.release_scale());
            if from != step {
                let frames = if sp.mono && glide_frames > 0 && e.glide != Glide::Porta {
                    glide_frames
                } else {
                    (PORTA_DEFAULT_S * self.sr) as u32
                };
                voice.glide_to(step, frames);
            }
            let y = e.y();
            if y > 0.0 {
                voice.seek(
                    region.start + f64::from(y) * START_OFFSET_MAX * (region.end - region.start),
                );
            }
            self.sustained[idx] = false;
            self.last_step = Some(step);
        }
    }

    /// Starts a note that has been under way for `into_s` seconds (playback started or
    /// jumped into it). The synth starts the note fresh while it is `held`; the sampler starts
    /// its voice that far into the sample, if the sample is still sounding there. Slides and
    /// portamento are not chased: the note starts at its own pitch.
    pub(crate) fn chase(
        &mut self,
        key: u8,
        velocity: f32,
        into_s: f32,
        held: bool,
        age: u64,
        e: NoteExpr,
    ) {
        let e = NoteExpr {
            glide: Glide::None,
            ..e
        };
        if self.params.kind == InstrumentKind::Synth {
            if held {
                self.synth.note_on_expr(key, velocity, age, e);
            }
            return;
        }
        if self.params.kind == InstrumentKind::Plugin {
            return;
        }
        let p = self.effective();
        // A looped voice stops at its note-off, so it only sounds while held.
        if p.looped && !held {
            return;
        }
        self.note_on_expr(key, velocity, age, e);
        let into = f64::from(into_s.max(0.0)) * f64::from(self.sr);
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.is_active() && v.age == age)
        {
            let region = v.region();
            let offset = f64::from(e.y().max(0.0)) * START_OFFSET_MAX * (region.end - region.start);
            let mut pos = region.start + offset + into * v.step();
            if pos >= region.end {
                if !p.looped {
                    v.kill();
                    continue;
                }
                let span = (region.end - region.start).max(1.0);
                pos = region.start + (pos - region.start) % span;
            }
            v.seek(pos);
        }
    }

    /// Releases held voices playing `key` (one-shot voices ignore it). With the sustain
    /// pedal down they keep sounding until it comes up.
    pub(crate) fn note_off(&mut self, key: u8) {
        self.synth.note_off(key);
        for (v, s) in self.voices.iter_mut().zip(&mut self.sustained) {
            if v.is_held() && !*s && v.key() == key {
                if self.sustain {
                    *s = true;
                } else {
                    v.release();
                }
            }
        }
    }

    /// Releases every voice.
    pub(crate) fn release_all(&mut self) {
        self.synth.release_all();
        for v in &mut self.voices {
            v.release();
        }
        self.sustained = [false; VOICES_PER_CHANNEL];
    }

    /// Cuts every voice with a short fade: another channel of its choke group played.
    pub(crate) fn choke(&mut self) {
        self.synth.choke_all();
        for v in &mut self.voices {
            if v.is_active() {
                v.choke();
            }
        }
        self.sustained = [false; VOICES_PER_CHANNEL];
    }

    /// Silences every voice at once.
    pub(crate) fn kill_all(&mut self) {
        self.synth.kill_all();
        for v in &mut self.voices {
            v.kill();
        }
        self.sustained = [false; VOICES_PER_CHANNEL];
    }

    /// Sustain pedal down or up. Up releases the voices whose keys were let go meanwhile.
    pub(crate) fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        self.synth.set_sustain(down);
        if !down {
            for (v, s) in self.voices.iter_mut().zip(&mut self.sustained) {
                if std::mem::take(s) {
                    v.release();
                }
            }
        }
    }

    /// Pitch bend, -1 to 1, times the channel's bend range. Sounding voices follow.
    pub(crate) fn set_bend(&mut self, bend: f32) {
        let bend = bend.clamp(-1.0, 1.0);
        let old = self.expr.bend_semis;
        self.bend = bend;
        self.expr.bend_semis = bend * self.params.bend_range;
        let ratio = f64::from(((self.expr.bend_semis - old) / 12.0).exp2());
        if ratio != 1.0 {
            for v in self.voices.iter_mut().filter(|v| v.is_active()) {
                v.scale_step(ratio);
            }
        }
        self.synth.set_expression(self.expr);
    }

    /// Mod wheel, 0 to 1.
    pub(crate) fn set_mod_wheel(&mut self, v: f32) {
        self.expr.mod_wheel = v.clamp(0.0, 1.0);
        self.synth.set_expression(self.expr);
    }

    /// Channel aftertouch, 0 to 1.
    pub(crate) fn set_pressure(&mut self, v: f32) {
        self.expr.pressure = v.clamp(0.0, 1.0);
        self.synth.set_expression(self.expr);
    }

    /// Polyphonic aftertouch on `key`, 0 to 1 (the synth's voices on that key).
    pub(crate) fn set_key_pressure(&mut self, key: u8, v: f32) {
        self.synth.set_key_pressure(key, v);
    }

    /// An MPE expression on `key` (the synth's voices on that key).
    pub(crate) fn set_note_expression(&mut self, key: u8, kind: crate::NoteExprKind, v: f32) {
        match kind {
            crate::NoteExprKind::Tuning => self.synth.set_key_tuning(key, v),
            crate::NoteExprKind::Pressure => self.synth.set_key_pressure(key, v),
            crate::NoteExprKind::Timbre => self.synth.set_key_timbre(key, v),
        }
    }

    /// The live expression: bend in semitones, mod wheel and aftertouch.
    pub(crate) fn expression(&self) -> Expression {
        self.expr
    }

    /// Mixer strip this channel plays into.
    pub(crate) fn route(&self) -> u16 {
        self.params.route
    }

    /// Inserts of a multi-output plugin's extra outputs.
    pub(crate) fn extra_routes(&self) -> &[Option<u16>; gt_core::MAX_EXTRA_OUTPUTS] {
        &self.params.extra_routes
    }

    /// The slot that plays the plugin's MIDI output.
    pub(crate) fn midi_out(&self) -> Option<u16> {
        self.params.midi_out
    }

    /// Gain of a plugin's extra outputs: the channel volume with automation, no pan.
    pub(crate) fn extra_gain(&self) -> f32 {
        if self.params.silenced {
            0.0
        } else {
            self.ctl.channel[ChannelParam::Volume as usize].unwrap_or(self.params.gain)
        }
    }

    /// True if any voice is sounding.
    pub(crate) fn is_active(&self) -> bool {
        self.voices.iter().any(SamplerVoice::is_active) || self.synth.is_active()
    }

    /// Adds the voices (dry, before channel gain) into the two buffers; voices of zones with
    /// a separate output too.
    pub(crate) fn render_voices(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.render_voices_at(out_l, out_r, None);
    }

    /// The mixer inserts of the separate outputs (drum pads), in aux order.
    pub(crate) fn aux_routes(&self) -> &[u16] {
        self.zones.as_ref().map_or(&[], |m| m.routes.as_slice())
    }

    /// Hands each separate output's quantum (with the channel's volume) to `f` with its
    /// insert, then clears it.
    pub(crate) fn drain_aux(&mut self, mut f: impl FnMut(u16, &[f32], &[f32], f32)) {
        let gain = self.extra_gain();
        let Some(m) = &self.zones else {
            return;
        };
        for (k, &route) in m.routes.iter().enumerate().take(MAX_AUX) {
            let [l, r] = &mut self.aux[k];
            f(route, l, r, gain);
            l.fill(0.0);
            r.fill(0.0);
        }
    }

    /// Like [`Self::render_voices`]; with `aux_at`, the frames start at that offset in the
    /// quantum and voices of zones with a separate output go to their aux buffers instead.
    pub(crate) fn render_voices_at(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        aux_at: Option<usize>,
    ) {
        if self.synth.is_active() {
            self.synth.render(out_l, out_r);
        }
        for i in 0..VOICES_PER_CHANNEL {
            if !self.voices[i].is_active() {
                continue;
            }
            let zone = self.voices[i].zone;
            let (data, aux) = match (&self.zones, zone) {
                (Some(m), z) if z != MAIN_ZONE => match m.zones.get(usize::from(z)) {
                    Some(z) => (Some(&z.data), z.aux),
                    None => (None, 0),
                },
                _ => (self.sample.as_ref(), 0),
            };
            let Some(d) = data else {
                self.voices[i].kill();
                continue;
            };
            let n = out_l.len();
            match aux_at {
                Some(at) if aux > 0 && at + n <= RENDER_QUANTUM => {
                    let [l, r] = &mut self.aux[usize::from(aux - 1) % MAX_AUX];
                    self.voices[i].render(
                        d.left(),
                        d.right(),
                        &mut l[at..at + n],
                        &mut r[at..at + n],
                    );
                }
                _ => self.voices[i].render(d.left(), d.right(), out_l, out_r),
            }
        }
    }

    /// Applies the smoothed channel gain and pan to `dry`, adds the result into `mix`, and
    /// returns the channel's peak level.
    pub(crate) fn mix_into(&mut self, dry: (&[f32], &[f32]), mix: (&mut [f32], &mut [f32])) -> f32 {
        let mut peak = 0.0_f32;
        for (((&dl, &dr), ml), mr) in dry
            .0
            .iter()
            .zip(dry.1)
            .zip(mix.0.iter_mut())
            .zip(mix.1.iter_mut())
        {
            let l = dl * self.gain_l.next_value();
            let r = dr * self.gain_r.next_value();
            *ml += l;
            *mr += r;
            peak = peak.max(l.abs()).max(r.abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_law_is_equal_power() {
        for pan in [-1.0, -0.3, 0.0, 0.5, 1.0] {
            let (l, r) = pan_gains(1.0, pan);
            assert!((l * l + r * r - 1.0).abs() < 1e-6, "{pan}");
        }
        let (l, r) = pan_gains(1.0, 0.0);
        assert!((l - r).abs() < 1e-7 && (l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let (l, r) = pan_gains(1.0, -1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
    }

    #[test]
    fn oldest_voice_is_stolen() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_sample(Some(Arc::new(SampleData::mono(48_000, vec![0.5; 48_000]))));
        for age in 0..VOICES_PER_CHANNEL as u64 + 3 {
            ch.note_on(60, 1.0, age);
        }
        let mut ages: Vec<_> = ch.voices.iter().map(|v| v.age).collect();
        ages.sort_unstable();
        assert_eq!(ages, (3..VOICES_PER_CHANNEL as u64 + 3).collect::<Vec<_>>());
    }

    #[test]
    fn synth_channels_play_without_a_sample() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_params_now(ChannelParams {
            kind: InstrumentKind::Synth,
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 1);
        let (mut l, mut r) = (vec![0.0; 2048], vec![0.0; 2048]);
        ch.render_voices(&mut l, &mut r);
        assert!(l.iter().any(|x| x.abs() > 0.01));
        // Switching back to the sampler silences the synth's voices.
        ch.set_params(ChannelParams::default());
        assert!(!ch.is_active());
    }

    #[test]
    fn pitch_and_rate_set_the_read_step() {
        // A 24 kHz sample on a 48 kHz engine, transposed up an octave, reads 1 frame per frame.
        let mut ch = ChannelSlot::new(48_000.0);
        let data: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        ch.set_sample(Some(Arc::new(SampleData::mono(24_000, data.clone()))));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            pitch: 12.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 0);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        ch.render_voices(&mut l, &mut r);
        assert_eq!(l[50], data[50]);
    }

    #[test]
    fn automated_pitch_retunes_sounding_voices() {
        let mut ch = ChannelSlot::new(48_000.0);
        let data: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        ch.set_sample(Some(Arc::new(SampleData::mono(48_000, data))));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.note_on(60, 1.0, 0);
        let slope = |ch: &mut ChannelSlot| {
            let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
            ch.render_voices(&mut l, &mut r);
            (l[40] - l[20]) / 20.0 * 48_000.0
        };
        assert!((slope(&mut ch) - 1.0).abs() < 1e-3);
        ch.control(ChannelParam::Pitch, 12.0, 32);
        assert!(
            (slope(&mut ch) - 2.0).abs() < 1e-3,
            "an octave up reads twice as fast"
        );
        ch.clear_controls();
        assert!(
            (slope(&mut ch) - 1.0).abs() < 1e-3,
            "back to the document's pitch"
        );
    }

    #[test]
    fn synth_controls_apply_once_per_flush_and_clear() {
        let mut ch = ChannelSlot::new(48_000.0);
        let mut p = ChannelParams {
            kind: InstrumentKind::Synth,
            gain: 1.0,
            ..ChannelParams::default()
        };
        p.patch.values[SynthParam::Cutoff as usize] = 1000.0;
        ch.set_params_now(p);
        ch.control_synth(SynthParam::Cutoff as usize, 300.0);
        assert_eq!(
            ch.synth.settings().cutoff_hz,
            1000.0,
            "not before the flush"
        );
        ch.flush_controls();
        assert_eq!(ch.synth.settings().cutoff_hz, 300.0);
        // A document edit keeps the automated value on top.
        p.patch.values[SynthParam::Resonance as usize] = 0.9;
        ch.set_params(p);
        assert_eq!(ch.synth.settings().cutoff_hz, 300.0);
        assert_eq!(ch.synth.settings().resonance, 0.9);
        ch.clear_controls();
        assert_eq!(ch.synth.settings().cutoff_hz, 1000.0);
    }

    #[test]
    fn volume_control_ramps_over_the_given_frames_and_respects_mute() {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch.control(ChannelParam::Volume, 0.0, 32);
        let ones = vec![1.0; 32];
        let (mut l, mut r) = (vec![0.0; 32], vec![0.0; 32]);
        ch.mix_into((&ones, &ones), (&mut l, &mut r));
        assert!(l[0] > 0.6 && l[31].abs() < 1e-6, "{} {}", l[0], l[31]);
        ch.control(ChannelParam::Volume, 1.0, 0);
        ch.set_params(ChannelParams {
            gain: 1.0,
            silenced: true,
            ..ChannelParams::default()
        });
        let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
        let ones = vec![1.0; 1024];
        ch.mix_into((&ones, &ones), (&mut l, &mut r));
        assert_eq!(l[1000], 0.0, "muted wins over automation");
    }

    fn sampler_channel() -> ChannelSlot {
        let mut ch = ChannelSlot::new(48_000.0);
        let data: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        ch.set_sample(Some(Arc::new(SampleData::mono(48_000, data))));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            looped: true,
            bend_range: 12.0,
            ..ChannelParams::default()
        });
        ch
    }

    fn slope(ch: &mut ChannelSlot) -> f32 {
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        ch.render_voices(&mut l, &mut r);
        (l[40] - l[20]) / 20.0 * 48_000.0
    }

    #[test]
    fn bend_follows_the_channels_range() {
        let mut ch = sampler_channel();
        ch.note_on(60, 1.0, 0);
        assert!((slope(&mut ch) - 1.0).abs() < 1e-3);
        ch.set_bend(1.0);
        assert!(
            (slope(&mut ch) - 2.0).abs() < 1e-3,
            "full bend: +12 st here"
        );
        ch.set_bend(-0.5);
        assert!((slope(&mut ch) - 0.707).abs() < 1e-2, "half down: -6 st");
        // New notes start bent too.
        ch.note_on(72, 1.0, 1);
        ch.set_bend(0.0);
        let mut p = ch.params;
        p.bend_range = 2.0;
        ch.set_params(p);
        ch.set_bend(1.0);
        assert!((ch.expression().bend_semis - 2.0).abs() < 1e-6);
    }

    #[test]
    fn sustain_holds_released_sampler_voices_until_the_pedal_lifts() {
        let mut ch = sampler_channel();
        let held = |ch: &ChannelSlot| ch.voices.iter().filter(|v| v.is_held()).count();
        ch.set_sustain(true);
        ch.note_on(60, 1.0, 0);
        ch.note_on(64, 1.0, 1);
        ch.note_off(60);
        assert_eq!(held(&ch), 2, "the pedal keeps 60");
        // Struck again under the pedal: a second voice, the first still sustained.
        ch.note_on(60, 1.0, 2);
        assert_eq!(held(&ch), 3);
        ch.set_sustain(false);
        assert_eq!(held(&ch), 2, "only the let-go voice released");
        ch.note_off(60);
        ch.note_off(64);
        assert_eq!(held(&ch), 0);
    }

    fn zoned(zones: Vec<crate::zones::Zone>, sp: crate::song::SamplerParams) -> ChannelSlot {
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_zones(Some(Arc::new(ZoneMap {
            zones,
            routes: Vec::new(),
        })));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            looped: true,
            sampler: sp,
            ..ChannelParams::default()
        });
        ch
    }

    fn dc(level: f32) -> Arc<SampleData> {
        Arc::new(SampleData::mono(48_000, vec![level; 48_000]))
    }

    fn playing(ch: &ChannelSlot) -> Vec<(u8, u16)> {
        let mut v: Vec<(u8, u16)> = ch
            .voices
            .iter()
            .filter(|v| v.is_active())
            .map(|v| (v.key(), v.zone))
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn notes_pick_zones_by_key_velocity_and_turn() {
        use crate::zones::Zone;
        let z = |lo, hi, vlo, vhi, level| Zone {
            key_lo: lo,
            key_hi: hi,
            vel_lo: vlo,
            vel_hi: vhi,
            ..Zone::new(dc(level), 60)
        };
        let sp = crate::song::SamplerParams {
            round_robin: true,
            ..Default::default()
        };
        let mut ch = zoned(
            vec![
                z(0, 59, 0.0, 0.5, 0.1),
                z(0, 59, 0.5, 1.0, 0.2),
                z(60, 127, 0.0, 1.0, 0.3),
                z(60, 127, 0.0, 1.0, 0.4),
            ],
            sp,
        );
        ch.note_on(40, 0.3, 1);
        ch.note_on(40, 0.9, 2);
        assert_eq!(playing(&ch), [(40, 0), (40, 1)]);
        ch.kill_all();
        // Round robin alternates the two takes of the upper range.
        let mut order = Vec::new();
        for age in 0..4 {
            ch.note_on(72, 1.0, age);
            order.push(ch.voices.iter().find(|v| v.age == age).unwrap().zone);
        }
        assert_eq!(order, [2, 3, 2, 3]);
        // A note outside every zone plays nothing.
        let mut ch = zoned(vec![z(60, 72, 0.0, 1.0, 0.3)], Default::default());
        ch.note_on(30, 1.0, 9);
        assert!(!ch.is_active());
    }

    #[test]
    fn voice_limit_mono_and_legato() {
        use crate::zones::Zone;
        let one = || vec![Zone::new(dc(0.5), 60)];
        let mut ch = zoned(
            one(),
            crate::song::SamplerParams {
                voices: 2,
                ..Default::default()
            },
        );
        for age in 0..5 {
            ch.note_on(60 + age as u8, 1.0, age);
        }
        assert_eq!(playing(&ch), [(63, 0), (64, 0)]);

        // Mono without legato: the old note releases, the new one starts.
        let mut ch = zoned(
            one(),
            crate::song::SamplerParams {
                mono: true,
                ..Default::default()
            },
        );
        ch.note_on(60, 1.0, 1);
        ch.note_on(64, 1.0, 2);
        let held: Vec<u8> = ch
            .voices
            .iter()
            .filter(|v| v.is_held())
            .map(|v| v.key())
            .collect();
        assert_eq!(held, [64]);

        // Legato: one voice, gliding to the new key without restarting.
        let mut ch = zoned(
            one(),
            crate::song::SamplerParams {
                mono: true,
                legato: true,
                glide_ms: 10.0,
                ..Default::default()
            },
        );
        ch.note_on(60, 1.0, 1);
        let (mut l, mut r) = (vec![0.0; 960], vec![0.0; 960]);
        ch.render_voices(&mut l, &mut r);
        ch.note_on(72, 1.0, 2);
        assert_eq!(playing(&ch), [(72, 0)]);
        let v = ch.voices.iter().find(|v| v.is_active()).unwrap();
        assert_eq!(v.age, 1, "the first voice carries on");
        ch.render_voices(&mut l, &mut r);
        let v = ch.voices.iter().find(|v| v.is_active()).unwrap();
        assert!(
            (v.step() - 2.0).abs() < 1e-6,
            "glided an octave up: {}",
            v.step()
        );
    }

    fn kit_channel() -> ChannelSlot {
        use gt_core::{DrumKit, SampleSource};
        let mut kit = DrumKit::default();
        let src = |n: &str| SampleSource::File(n.into());
        kit.banks[0][0].set_layers(vec![src("open")]);
        kit.banks[0][0].choke = 1;
        kit.banks[0][1].set_layers(vec![src("closed")]);
        kit.banks[0][1].choke = 1;
        kit.banks[0][2].set_layers(vec![src("snare")]);
        kit.banks[0][2].output = Some(3);
        let map = ZoneMap::kit(&kit, |_| Some(dc(0.5))).unwrap();
        assert_eq!(map.routes, [3]);
        let mut ch = ChannelSlot::new(48_000.0);
        ch.set_zones(Some(Arc::new(map)));
        ch.set_params_now(ChannelParams {
            gain: 1.0,
            ..ChannelParams::default()
        });
        ch
    }

    #[test]
    fn a_pad_chokes_its_group() {
        let mut ch = kit_channel();
        ch.note_on(60, 1.0, 1); // open hat
        ch.note_on(62, 1.0, 2); // snare, no group
        let (mut l, mut r) = (vec![0.0; 480], vec![0.0; 480]);
        ch.render_voices(&mut l, &mut r);
        ch.note_on(61, 1.0, 3); // closed hat cuts the open one
        assert!(!ch.voices.iter().any(|v| v.key() == 60 && v.is_held()));
        // A 5 ms fade: gone well within 20 ms.
        ch.render_voices(&mut l, &mut r);
        ch.render_voices(&mut l, &mut r);
        assert_eq!(playing(&ch), [(61, 1), (62, 2)]);
    }

    #[test]
    fn a_pad_with_its_own_output_renders_there() {
        let mut ch = kit_channel();
        ch.note_on(62, 1.0, 1);
        let (mut l, mut r) = (vec![0.0; 32], vec![0.0; 32]);
        ch.render_voices_at(&mut l, &mut r, Some(0));
        assert!(l.iter().all(|&x| x == 0.0), "not in the channel's output");
        let mut got = Vec::new();
        ch.drain_aux(|route, a, _, gain| got.push((route, a[10] * gain)));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 3);
        assert!((got[0].1 - 0.5).abs() < 1e-3, "{got:?}");
        // Without an offset (preview, tests) it plays in the channel's output.
        ch.render_voices(&mut l, &mut r);
        assert!(l[10] > 0.4);
    }
}
