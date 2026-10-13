//! Sample playback voice.
//!
//! The voice reads the sample at a fractional position that advances by `step` frames per
//! output frame: `step = 2^(semitones / 12) * sample_rate / engine_rate`. Between stored frames
//! it interpolates linearly (draft) or with a windowed sinc ([`crate::interp`], Prompt 23).
//! Linear is cheap and fine for drums and moderate transposition; it attenuates the top octave
//! slightly and, when pitching up, lets aliasing through. The sinc reader low-passes at the
//! output Nyquist as it reads, so pitched-up voices stay clean.
//!
//! Prompt 23 adds, per voice: reverse playback, a state-variable filter with its own envelope,
//! and two LFOs (pitch, cutoff, volume or pan), updated every [`CONTROL_FRAMES`]. With all of
//! them off and linear reading the output is bit-identical to the earlier voice.
//!
//! One-shot voices play from start to end and ignore note-off; the last 2 ms before the end
//! point fade out, so trimming the end mid-waveform does not click. Loop voices repeat the
//! start..end region until note-off, then fade with the envelope's release.

use crate::fx::filter::{FilterMode, Svf, SvfCoeffs};
use crate::interp::SincTable;
use crate::{Adsr, AdsrParams, Lfo, LfoWave};

/// Frames between updates of the LFOs and the filter cutoff.
pub const CONTROL_FRAMES: u32 = 16;
/// Semitones of pitch LFO at full amount.
const LFO_PITCH_SEMIS: f32 = 12.0;
/// Octaves of cutoff LFO at full amount.
const LFO_CUTOFF_OCT: f32 = 4.0;

/// What a voice LFO moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LfoDest {
    /// Nothing.
    #[default]
    Off,
    /// Pitch (±12 semitones at full amount).
    Pitch,
    /// Filter cutoff (±4 octaves).
    Cutoff,
    /// Volume (tremolo).
    Volume,
    /// Pan.
    Pan,
}

/// One voice LFO's settings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VoiceLfo {
    /// Waveform.
    pub wave: LfoWave,
    /// Rate in Hz.
    pub rate: f32,
    /// What it moves.
    pub dest: LfoDest,
    /// Depth, 0 to 1 (negative turns the wave upside down).
    pub amount: f32,
}

/// How a voice reads and shapes the sample (Prompt 23). Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceSettings {
    /// Sinc kernel half-width (0: linear).
    pub sinc: usize,
    /// Play backwards.
    pub reverse: bool,
    /// Filter response (`None`: no filter).
    pub filter: Option<FilterMode>,
    /// Cutoff in Hz.
    pub cutoff: f32,
    /// Filter Q.
    pub q: f32,
    /// Filter envelope depth in octaves.
    pub env_octaves: f32,
    /// Filter envelope.
    pub filter_env: AdsrParams,
    /// The two LFOs.
    pub lfos: [VoiceLfo; 2],
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            sinc: 0,
            reverse: false,
            filter: None,
            cutoff: 8000.0,
            q: 0.707,
            env_octaves: 0.0,
            filter_env: AdsrParams::default(),
            lfos: [VoiceLfo::default(); 2],
        }
    }
}

/// End-of-sample fade for one-shot voices, in seconds.
const DECLICK_S: f64 = 0.002;

/// The part of the sample a voice plays, in sample frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceRegion {
    /// First frame played (and the loop start).
    pub start: f64,
    /// Frame where playback stops or wraps.
    pub end: f64,
    /// Loop the region while the note is held.
    pub looped: bool,
}

impl VoiceRegion {
    /// Converts fractional start/end points (0..1) into frames of a sample `frames` long.
    /// Guarantees `0 <= start < end <= frames` for a non-empty sample.
    pub fn from_fractions(frames: usize, start: f32, end: f32, looped: bool) -> Self {
        let len = frames as f64;
        let s = f64::from(start.clamp(0.0, 1.0)) * len;
        let e = f64::from(end.clamp(0.0, 1.0)) * len;
        let (s, e) = if e - s < 1.0 {
            // Degenerate: keep at least one frame.
            let s = s.min((len - 1.0).max(0.0));
            (s, (s + 1.0).min(len))
        } else {
            (s, e)
        };
        Self {
            start: s,
            end: e,
            looped,
        }
    }
}

/// One playing note of a sampler.
#[derive(Debug, Clone, Copy)]
pub struct SamplerVoice {
    active: bool,
    key: u8,
    pos: f64,
    step: f64,
    region: VoiceRegion,
    gain: f32,
    env: Adsr,
    declick_frames: f64,
    /// Per-note balance gains (1 and 1 at centre).
    pan: (f32, f32),
    /// One-pole low-pass from the note's mod X: coefficient (1 = open) and state.
    tone: f32,
    tone_state: (f32, f32),
    /// A glide in progress: the step is multiplied by `glide_mul` for `glide_left` frames.
    glide_mul: f64,
    glide_left: u32,
    release_scale: f32,
    /// Trigger order, for stealing the oldest voice.
    pub age: u64,
    /// The zone (sample) it plays, for the instrument to pick the data.
    pub zone: u16,
    set: VoiceSettings,
    sinc: Option<&'static SincTable>,
    sr: f32,
    filter_env: Adsr,
    svf: [Svf; 2],
    coeffs: SvfCoeffs,
    lfos: [Lfo; 2],
    /// Control-rate values: pitch multiplier, volume multiplier, pan offset.
    pitch_mul: f64,
    vol_mul: f32,
    pan_mod: Option<(f32, f32)>,
    control_left: u32,
    note_pan: f32,
}

impl SamplerVoice {
    /// Creates an idle voice for an engine running at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            active: false,
            key: 0,
            pos: 0.0,
            step: 1.0,
            region: VoiceRegion {
                start: 0.0,
                end: 0.0,
                looped: false,
            },
            gain: 0.0,
            env: Adsr::default(),
            declick_frames: DECLICK_S * f64::from(sample_rate.max(1.0)),
            pan: (1.0, 1.0),
            tone: 1.0,
            tone_state: (0.0, 0.0),
            glide_mul: 1.0,
            glide_left: 0,
            release_scale: 1.0,
            age: 0,
            zone: 0,
            set: VoiceSettings::default(),
            sinc: None,
            sr: sample_rate.max(1.0),
            filter_env: Adsr::default(),
            svf: [Svf::default(); 2],
            coeffs: SvfCoeffs::new(8000.0, 0.707, sample_rate.max(1.0)),
            lfos: [Lfo::new(0x1234_5678), Lfo::new(0x8765_4321)],
            pitch_mul: 1.0,
            vol_mul: 1.0,
            pan_mod: None,
            control_left: 0,
            note_pan: 0.0,
        }
    }

    /// Sets how the voice reads and shapes the sample. Call right after [`Self::trigger`];
    /// builds the sinc tables on first use (call [`SincTable::get`] off the audio thread
    /// first, as [`Self::warm_up`] does).
    pub fn set_settings(&mut self, set: VoiceSettings) {
        self.set = set;
        self.sinc = (set.sinc > 0).then(|| SincTable::get(set.sinc));
        self.filter_env.trigger(set.filter_env);
        self.svf = [Svf::default(); 2];
        for l in &mut self.lfos {
            l.reset();
        }
        self.pitch_mul = 1.0;
        self.vol_mul = 1.0;
        self.pan_mod = None;
        self.control_left = 0;
    }

    /// Builds the interpolation tables (allocates once per process): call off the audio
    /// thread before any voice reads with sinc.
    pub fn warm_up() {
        SincTable::get(8);
        SincTable::get(32);
    }

    /// Recomputes the LFOs and the filter cutoff (every [`CONTROL_FRAMES`]).
    fn control(&mut self) {
        let dt = CONTROL_FRAMES as f32 / self.sr;
        let mut pitch = 0.0;
        let mut cutoff_oct = 0.0;
        let mut vol = 1.0;
        let mut pan = None::<f32>;
        for (lfo, s) in self.lfos.iter_mut().zip(&self.set.lfos) {
            if s.dest == LfoDest::Off || s.amount == 0.0 {
                continue;
            }
            // A negative amount turns the wave upside down.
            let v = lfo.value(s.wave) * s.amount.signum();
            let amount = s.amount.abs();
            lfo.advance(s.rate * dt);
            match s.dest {
                LfoDest::Pitch => pitch += v * amount * LFO_PITCH_SEMIS,
                LfoDest::Cutoff => cutoff_oct += v * amount * LFO_CUTOFF_OCT,
                LfoDest::Volume => vol *= 1.0 - amount * (1.0 - v) * 0.5,
                LfoDest::Pan => *pan.get_or_insert(0.0) += v * amount,
                LfoDest::Off => {}
            }
        }
        self.pitch_mul = if pitch == 0.0 {
            1.0
        } else {
            f64::from((pitch / 12.0).exp2())
        };
        self.vol_mul = vol;
        self.pan_mod = pan.map(|p| {
            let p = (self.note_pan + p).clamp(-1.0, 1.0);
            (1.0 - p.max(0.0), 1.0 + p.min(0.0))
        });
        if self.set.filter.is_some() {
            let oct = self.set.env_octaves * self.filter_env.level() + cutoff_oct;
            let hz = (self.set.cutoff * oct.exp2()).clamp(20.0, self.sr * 0.45);
            self.coeffs = SvfCoeffs::new(hz, self.set.q, self.sr);
        }
    }

    /// Starts the voice. `step` is the read increment (see module docs), `gain` the linear
    /// amplitude (velocity curve already applied).
    pub fn trigger(
        &mut self,
        key: u8,
        gain: f32,
        step: f64,
        region: VoiceRegion,
        env: AdsrParams,
        age: u64,
    ) {
        self.active = region.end > region.start && step > 0.0;
        self.key = key;
        self.pos = region.start;
        self.step = step;
        self.region = region;
        self.gain = gain;
        self.env.trigger(env);
        self.age = age;
        self.pan = (1.0, 1.0);
        self.tone = 1.0;
        self.tone_state = (0.0, 0.0);
        self.glide_left = 0;
        self.release_scale = 1.0;
        self.note_pan = 0.0;
        if self.set != VoiceSettings::default() {
            self.set_settings(VoiceSettings::default());
        }
    }

    /// Per-note values after [`Self::trigger`]: balance `pan` (-1 to 1), a one-pole low-pass
    /// coefficient `tone` (1 is open) and the release time multiplier.
    pub fn set_note_values(&mut self, pan: f32, tone: f32, release_scale: f32) {
        let p = pan.clamp(-1.0, 1.0);
        self.note_pan = p;
        self.pan = (1.0 - p.max(0.0), 1.0 + p.min(0.0));
        self.tone = tone.clamp(0.0, 1.0);
        self.release_scale = release_scale;
    }

    /// Glides the read step to `target_step` over `frames` output frames, evenly in
    /// semitones (slide notes and portamento).
    pub fn glide_to(&mut self, target_step: f64, frames: u32) {
        let valid = target_step.is_finite() && target_step > 0.0;
        if frames == 0 || !valid {
            if valid {
                self.step = target_step;
            }
            self.glide_left = 0;
            return;
        }
        self.glide_mul = (target_step / self.step).powf(1.0 / f64::from(frames));
        self.glide_left = frames;
    }

    /// The region it plays.
    pub fn region(&self) -> VoiceRegion {
        self.region
    }

    /// The current read step.
    pub fn step(&self) -> f64 {
        self.step
    }

    /// Moves the play position to frame `pos` of the sample (chasing a note that started
    /// earlier). Positions outside the region are clamped into it.
    pub fn seek(&mut self, pos: f64) {
        self.pos = pos.clamp(self.region.start, self.region.end);
    }

    /// Note-off: loop voices enter their release, one-shot voices play on.
    pub fn release(&mut self) {
        if self.region.looped {
            self.env.release_scaled(self.release_scale);
            self.filter_env.release_scaled(self.release_scale);
        }
    }

    /// Note-off for any voice (also one-shots): the amplitude envelope releases. Used by
    /// choke groups and mono voices.
    pub fn release_any(&mut self) {
        self.env.release_scaled(self.release_scale);
        self.filter_env.release_scaled(self.release_scale);
    }

    /// Cuts the voice with a 5 ms fade (choke groups).
    pub fn choke(&mut self) {
        let fast = AdsrParams::new(self.sr, 0.0, 0.0, 1.0, 5.0);
        self.env.release_with(fast);
        self.filter_env.release_with(fast);
    }

    /// Retargets a sounding voice to a new read step without restarting it (legato), gliding
    /// over `frames`.
    pub fn legato_to(&mut self, key: u8, step: f64, frames: u32) {
        self.key = key;
        self.glide_to(step, frames);
    }

    /// Silences the voice immediately.
    pub fn kill(&mut self) {
        self.active = false;
        self.env.reset();
    }

    /// True while the voice produces sound.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// True while the voice is held (active and not releasing).
    pub fn is_held(&self) -> bool {
        self.active && !self.env.is_releasing()
    }

    /// Multiplies the read step (retunes a sounding voice; 2 is an octave up).
    pub fn scale_step(&mut self, ratio: f64) {
        if ratio.is_finite() && ratio > 0.0 {
            self.step *= ratio;
        }
    }

    /// The key that started the voice.
    pub fn key(&self) -> u8 {
        self.key
    }

    /// Adds the voice into `out_l` / `out_r` (same length), reading `left` / `right` (the same
    /// slice twice for a mono sample).
    pub fn render(&mut self, left: &[f32], right: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let len = left.len().min(right.len());
        let end = self.region.end.min(len as f64);
        let start = self.region.start;
        // Stored frames the voice may read: the region (rounded out) for loops, and up to the
        // end for one-shots (past it reads as silence).
        let lo = start as i64;
        let hi = end as i64;
        let looped = self.region.looped;
        let index = move |i: i64| -> Option<usize> {
            if looped && hi > lo {
                Some((lo + (i - lo).rem_euclid(hi - lo)) as usize)
            } else if (0..hi).contains(&i) {
                usize::try_from(i).ok()
            } else {
                None
            }
        };
        let reverse = self.set.reverse;
        let modulated = self.set.filter.is_some()
            || self
                .set
                .lfos
                .iter()
                .any(|l| l.dest != LfoDest::Off && l.amount != 0.0);
        for (ol, or) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if !self.active {
                return;
            }
            if self.pos >= end {
                if self.region.looped && end > start {
                    self.pos = start + (self.pos - end) % (end - start);
                } else {
                    self.active = false;
                    return;
                }
            }
            if modulated {
                if self.control_left == 0 {
                    self.control();
                    self.control_left = CONTROL_FRAMES;
                }
                self.control_left -= 1;
            }
            // Where in the sample this playback position reads.
            let x = if reverse {
                (start + end - 1.0 - self.pos).max(start)
            } else {
                self.pos
            };
            let step = self.step * self.pitch_mul;
            let (mut a, mut b) = match self.sinc {
                Some(t) => t.read(left, right, x, step, index),
                None if reverse || looped => {
                    let i = x.floor() as i64;
                    let frac = (x - i as f64) as f32;
                    let get = |d: &[f32], k: i64| index(k).and_then(|k| d.get(k)).copied();
                    let a = get(left, i).unwrap_or(0.0);
                    let b = get(left, i + 1).unwrap_or(0.0);
                    let c = get(right, i).unwrap_or(0.0);
                    let d = get(right, i + 1).unwrap_or(0.0);
                    (a + (b - a) * frac, c + (d - c) * frac)
                }
                None => {
                    // The original linear reader (bit-exact with earlier versions).
                    let i = x as usize;
                    let frac = (x - i as f64) as f32;
                    let j = if i + 1 < end as usize { i + 1 } else { len };
                    let read = |d: &[f32]| {
                        let a = d.get(i).copied().unwrap_or(0.0);
                        let b = d.get(j).copied().unwrap_or(0.0);
                        a + (b - a) * frac
                    };
                    (read(left), read(right))
                }
            };
            let mut g = self.gain * self.env.next_level();
            if modulated {
                g *= self.vol_mul;
                let _ = self.filter_env.next_level();
            }
            if !self.region.looped {
                let remaining = (end - self.pos) / step;
                if remaining < self.declick_frames {
                    g *= (remaining / self.declick_frames) as f32;
                }
            }
            if self.tone < 1.0 {
                self.tone_state.0 += (a - self.tone_state.0) * self.tone;
                self.tone_state.1 += (b - self.tone_state.1) * self.tone;
                (a, b) = self.tone_state;
            }
            if let Some(mode) = self.set.filter {
                a = self.svf[0].tick(&self.coeffs, a, mode);
                b = self.svf[1].tick(&self.coeffs, b, mode);
            }
            let pan = self.pan_mod.unwrap_or(self.pan);
            *ol += a * g * pan.0;
            *or += b * g * pan.1;
            if !self.env.is_active() {
                self.active = false;
            }
            self.pos += step;
            if self.glide_left > 0 {
                self.glide_left -= 1;
                self.step *= self.glide_mul;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn flat_env() -> AdsrParams {
        AdsrParams::new(SR, 0.0, 0.0, 1.0, 10.0)
    }

    fn render(v: &mut SamplerVoice, data: &[f32], frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        v.render(data, data, &mut l, &mut r);
        assert_eq!(l, r);
        l
    }

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 / n as f32).collect()
    }

    #[test]
    fn unity_step_reproduces_the_sample() {
        let data = ramp(4800);
        let mut v = SamplerVoice::new(SR);
        let region = VoiceRegion::from_fractions(data.len(), 0.0, 1.0, false);
        v.trigger(60, 1.0, 1.0, region, flat_env(), 0);
        let out = render(&mut v, &data, 5000);
        // Exact until the 2 ms end fade (96 frames).
        let fade = (0.002 * SR) as usize;
        assert_eq!(&out[..4800 - fade], &data[..4800 - fade]);
        assert!(out[4799] < 0.02);
        assert!(out[4800..].iter().all(|&s| s == 0.0));
        assert!(!v.is_active());
    }

    #[test]
    fn octave_up_plays_twice_as_fast() {
        let data = ramp(4800);
        let mut v = SamplerVoice::new(SR);
        let region = VoiceRegion::from_fractions(data.len(), 0.0, 1.0, false);
        v.trigger(72, 1.0, 2.0, region, flat_env(), 0);
        let out = render(&mut v, &data, 4800);
        assert_eq!(out[100], data[200]);
        assert!(out[2400..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn fractional_step_interpolates() {
        let data = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut v = SamplerVoice::new(1.0); // no declick at this rate
        v.trigger(
            60,
            1.0,
            0.5,
            VoiceRegion::from_fractions(8, 0.0, 1.0, false),
            AdsrParams::new(1.0, 0.0, 0.0, 1.0, 0.0),
            0,
        );
        let out = render(&mut v, &data, 5);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn start_and_end_points_trim() {
        let data = ramp(1000);
        let mut v = SamplerVoice::new(1.0);
        let region = VoiceRegion::from_fractions(1000, 0.25, 0.5, false);
        v.trigger(
            60,
            1.0,
            1.0,
            region,
            AdsrParams::new(1.0, 0.0, 0.0, 1.0, 0.0),
            0,
        );
        let out = render(&mut v, &data, 400);
        assert_eq!(out[0], data[250]);
        assert_eq!(out[248], data[498]);
        assert!(out[250..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn one_shot_ignores_note_off() {
        let data = vec![0.5; 4800];
        let mut v = SamplerVoice::new(SR);
        v.trigger(
            60,
            1.0,
            1.0,
            VoiceRegion::from_fractions(4800, 0.0, 1.0, false),
            flat_env(),
            0,
        );
        render(&mut v, &data, 100);
        v.release();
        let out = render(&mut v, &data, 100);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn loop_repeats_while_held_then_releases() {
        let data = ramp(100);
        let mut v = SamplerVoice::new(SR);
        v.trigger(
            60,
            1.0,
            1.0,
            VoiceRegion::from_fractions(100, 0.5, 1.0, true),
            flat_env(),
            0,
        );
        let out = render(&mut v, &data, 200);
        assert_eq!(out[0], data[50]);
        assert_eq!(out[50], data[50]);
        assert_eq!(out[150], data[50]);
        v.release();
        let tail = render(&mut v, &data, 4800);
        assert!(tail[0] > 0.0);
        assert!(!v.is_active());
        assert_eq!(tail[4799], 0.0);
    }

    #[test]
    fn degenerate_regions_are_safe() {
        let r = VoiceRegion::from_fractions(10, 0.9, 0.1, false);
        assert!(r.end > r.start && r.end <= 10.0);
        let r = VoiceRegion::from_fractions(0, 0.0, 1.0, true);
        let mut v = SamplerVoice::new(SR);
        v.trigger(60, 1.0, 1.0, r, flat_env(), 0);
        assert!(!v.is_active());
        let out = render(&mut v, &[], 16);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    /// A sine sweeping from `f0` to `f1` Hz over `n` frames at 48 kHz.
    fn sweep(f0: f32, f1: f32, n: usize) -> Vec<f32> {
        let mut phase = 0.0_f64;
        (0..n)
            .map(|i| {
                let f = f0 + (f1 - f0) * i as f32 / n as f32;
                phase += f64::from(f) / f64::from(SR);
                (phase * std::f64::consts::TAU).sin() as f32
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn play(data: &[f32], step: f64, set: VoiceSettings, frames: usize) -> Vec<f32> {
        let mut v = SamplerVoice::new(SR);
        let region = VoiceRegion::from_fractions(data.len(), 0.0, 1.0, false);
        v.trigger(60, 1.0, step, region, flat_env(), 0);
        v.set_settings(set);
        render(&mut v, data, frames)
    }

    #[test]
    fn sinc_removes_the_aliasing_of_a_pitched_up_sweep() {
        SamplerVoice::warm_up();
        // Everything above 13 kHz, played an octave up, lands above the output's Nyquist:
        // a perfect reader outputs nothing.
        let high = sweep(13_000.0, 20_000.0, 48_000);
        let level = |sinc: usize| {
            let set = VoiceSettings {
                sinc,
                ..VoiceSettings::default()
            };
            let out = play(&high, 2.0, set, 20_000);
            20.0 * (rms(&out[1000..19_000]) / rms(&high)).log10()
        };
        let (linear, good, best) = (level(0), level(8), level(32));
        eprintln!("aliasing: linear {linear:.1} dB, good {good:.1} dB, best {best:.1} dB");
        assert!(linear > -20.0, "linear aliases: {linear:.1} dB");
        assert!(good < -40.0, "good: {good:.1} dB");
        assert!(best < -60.0, "best: {best:.1} dB");
        // Below the new Nyquist the sweep passes at full level.
        let low = sweep(500.0, 9_000.0, 48_000);
        let set = VoiceSettings {
            sinc: 32,
            ..VoiceSettings::default()
        };
        let out = play(&low, 2.0, set, 20_000);
        let gain = rms(&out[1000..19_000]) / rms(&low);
        assert!((gain - 1.0).abs() < 0.02, "{gain}");
    }

    #[test]
    fn reverse_plays_the_region_backwards() {
        let data = ramp(1000);
        let set = VoiceSettings {
            reverse: true,
            ..VoiceSettings::default()
        };
        let mut v = SamplerVoice::new(1.0);
        let region = VoiceRegion::from_fractions(1000, 0.0, 0.5, false);
        v.trigger(
            60,
            1.0,
            1.0,
            region,
            AdsrParams::new(1.0, 0.0, 0.0, 1.0, 0.0),
            0,
        );
        v.set_settings(set);
        let out = render(&mut v, &data, 600);
        assert_eq!(out[0], data[499]);
        assert_eq!(out[99], data[400]);
        assert_eq!(out[499], data[0]);
        assert!(out[500..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_filter_and_its_envelope_shape_the_tone() {
        // Bright noise through a closed low-pass is much quieter.
        let mut n = crate::Noise::new(3);
        let data: Vec<f32> = (0..48_000).map(|_| n.next_value()).collect();
        let open = play(&data, 1.0, VoiceSettings::default(), 9600);
        let closed = VoiceSettings {
            filter: Some(FilterMode::LowPass),
            cutoff: 300.0,
            ..VoiceSettings::default()
        };
        let low = play(&data, 1.0, closed, 9600);
        assert!(rms(&low) < 0.25 * rms(&open));
        // An envelope opening it 6 octaves at the start, decaying to nothing: bright first.
        let swept = VoiceSettings {
            env_octaves: 6.0,
            filter_env: AdsrParams::new(SR, 0.0, 50.0, 0.0, 10.0),
            ..closed
        };
        let out = play(&data, 1.0, swept, 9600);
        assert!(rms(&out[..480]) > 2.0 * rms(&out[7200..]));
    }

    #[test]
    fn lfos_move_volume_and_pitch() {
        let data = vec![0.5; 48_000];
        let trem = VoiceSettings {
            lfos: [
                VoiceLfo {
                    wave: LfoWave::Square,
                    rate: 10.0,
                    dest: LfoDest::Volume,
                    amount: 1.0,
                },
                VoiceLfo::default(),
            ],
            ..VoiceSettings::default()
        };
        let out = play(&data, 1.0, trem, 9600);
        // Square at 10 Hz: on for 50 ms, off for 50 ms.
        assert!((out[1000] - 0.5).abs() < 1e-6);
        assert!(out[3500].abs() < 1e-6);
        // A pitch LFO makes the voice run faster and slower than its step.
        let mut v = SamplerVoice::new(SR);
        let ramp = ramp(48_000);
        v.trigger(
            60,
            1.0,
            1.0,
            VoiceRegion::from_fractions(48_000, 0.0, 1.0, false),
            flat_env(),
            0,
        );
        v.set_settings(VoiceSettings {
            lfos: [
                VoiceLfo::default(),
                VoiceLfo {
                    wave: LfoWave::Square,
                    rate: 1.0,
                    dest: LfoDest::Pitch,
                    amount: 1.0,
                },
            ],
            ..VoiceSettings::default()
        });
        let out = render(&mut v, &ramp, 4800);
        // Square high: an octave up, two frames per frame.
        assert!((out[2000] - ramp[4000]).abs() < 1e-3, "{}", out[2000]);
    }
}
