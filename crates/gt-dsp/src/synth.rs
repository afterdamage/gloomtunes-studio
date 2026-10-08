//! Gloom Synth: a 16-voice subtractive synthesizer.
//!
//! Signal path per voice:
//!
//! ```text
//! osc 1 ─┐ (each up to 7 detuned unison copies, panned across the stereo field)
//! osc 2 ─┼─▶ ladder low-pass (L and R) ─▶ × amp envelope × velocity × volume ─▶ pan ─▶ out
//! sub ───┤   cutoff ← knob · 2^(env amount·mod env + key track + mod matrix)
//! noise ─┘
//! ```
//!
//! Modulation (two LFOs, the mod envelope, the amp envelope, velocity, note and a per-note random
//! value through an 8-slot matrix) is evaluated at control rate, every [`CONTROL_FRAMES`] samples
//! (3 kHz at 48 kHz). That is fast enough for envelopes and LFOs to sound smooth; the filter
//! coefficient and the output gains are ramped linearly across each control block so there is no
//! zipper noise. Oscillators, filters and the amp envelope run per sample.
//!
//! Voice allocation: a free voice, else the oldest voice in its release, else the oldest voice.
//! A stolen voice keeps its oscillator phases and filter state and restarts its envelopes from
//! their current level, so stealing does not click.
//!
//! Portamento: each new note glides from the pitch of the previous note, exponentially in
//! semitones (constant speed in octaves), reaching 99 % of the interval in the glide time.
//!
//! Real-time safety: all voices are allocated in [`GloomSynth::new`]; nothing allocates, locks or
//! panics afterwards.

use crate::adsr::{Adsr, AdsrParams};
use crate::blep::{blep_sample, Phase, Wave};
use crate::ladder::{ladder_g, ladder_k, Ladder};
use crate::lfo::{Lfo, LfoWave, Noise};

/// Voices per synth.
pub const SYNTH_VOICES: usize = 16;
/// Most unison copies per oscillator.
pub const MAX_UNISON: usize = 7;
/// Modulation matrix slots.
pub const MOD_SLOTS: usize = 8;
/// Samples per control-rate update.
pub const CONTROL_FRAMES: u32 = 16;

/// One oscillator's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OscSettings {
    /// Waveform.
    pub wave: Wave,
    /// Octave offset.
    pub octave: f32,
    /// Semitone offset.
    pub semitones: f32,
    /// Fine tune in cents.
    pub cents: f32,
    /// Level, 0 to 1.
    pub level: f32,
    /// Pulse width of the square, 0.05 to 0.95.
    pub pulse_width: f32,
}

/// Envelope times in milliseconds and sustain level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvSettings {
    /// Attack time.
    pub attack_ms: f32,
    /// Decay time.
    pub decay_ms: f32,
    /// Sustain level, 0 to 1.
    pub sustain: f32,
    /// Release time.
    pub release_ms: f32,
}

impl EnvSettings {
    fn params(&self, sr: f32) -> AdsrParams {
        AdsrParams::new(
            sr,
            self.attack_ms,
            self.decay_ms,
            self.sustain,
            self.release_ms,
        )
    }
}

/// LFO settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoSettings {
    /// Waveform.
    pub wave: LfoWave,
    /// Rate in Hz.
    pub rate_hz: f32,
}

/// Modulation sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModSource {
    /// Unused slot.
    #[default]
    Off,
    /// LFO 1, -1 to 1.
    Lfo1,
    /// LFO 2, -1 to 1.
    Lfo2,
    /// Mod envelope, 0 to 1.
    ModEnv,
    /// Amp envelope, 0 to 1.
    AmpEnv,
    /// Note velocity, 0 to 1.
    Velocity,
    /// Key position around middle C (60), -1 to 1 over ±5 octaves (clamped above key 120).
    Note,
    /// A random value per note, -1 to 1.
    Random,
}

/// Modulation destinations. Amount 1.0 means the range given for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModDest {
    /// Unused slot.
    #[default]
    Off,
    /// Both oscillators, ±12 semitones.
    Pitch,
    /// Oscillator 1, ±12 semitones.
    Osc1Pitch,
    /// Oscillator 2, ±12 semitones.
    Osc2Pitch,
    /// Oscillator 1 pulse width, ±0.45.
    Osc1Pw,
    /// Oscillator 2 pulse width, ±0.45.
    Osc2Pw,
    /// Oscillator 1 level, ±1.
    Osc1Level,
    /// Oscillator 2 level, ±1.
    Osc2Level,
    /// Noise level, ±1.
    NoiseLevel,
    /// Filter cutoff, ±6 octaves.
    Cutoff,
    /// Filter resonance, ±1.
    Resonance,
    /// Pan, ±1.
    Pan,
    /// Amplitude, ±100 %.
    Amp,
    /// LFO 1 rate, ±4 octaves.
    Lfo1Rate,
    /// LFO 2 rate, ±4 octaves.
    Lfo2Rate,
    /// Unison detune, ±100 cents.
    UnisonDetune,
}

impl ModDest {
    /// Number of destinations (including `Off`).
    pub const COUNT: usize = 16;
}

/// One row of the modulation matrix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModSlot {
    /// Source.
    pub source: ModSource,
    /// Destination.
    pub dest: ModDest,
    /// Amount, -1 to 1.
    pub amount: f32,
}

/// Every setting of the synth, in plain units. Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SynthSettings {
    /// The two main oscillators.
    pub osc: [OscSettings; 2],
    /// Sub oscillator (sine, one octave below oscillator 1) level.
    pub sub_level: f32,
    /// White noise level.
    pub noise_level: f32,
    /// Unison copies per oscillator, 1 to 7.
    pub unison: u8,
    /// Spread between the outermost unison copies, in cents either side.
    pub unison_detune_cents: f32,
    /// Stereo width of the unison copies, 0 to 1.
    pub unison_spread: f32,
    /// Filter cutoff in Hz.
    pub cutoff_hz: f32,
    /// Filter resonance, 0 to 1 (self-oscillates near 1).
    pub resonance: f32,
    /// Mod envelope to cutoff, in octaves (-8 to 8).
    pub filter_env_octaves: f32,
    /// Cutoff follows the key: 1 means one octave per octave.
    pub key_track: f32,
    /// Filter drive, 0 (clean) to 1.
    pub drive: f32,
    /// Amplitude envelope.
    pub amp_env: EnvSettings,
    /// Modulation envelope (filter and matrix).
    pub mod_env: EnvSettings,
    /// The two LFOs.
    pub lfo: [LfoSettings; 2],
    /// Portamento time in ms (0 = off).
    pub glide_ms: f32,
    /// How much velocity changes loudness, 0 to 1.
    pub velocity_sensitivity: f32,
    /// Output gain (linear).
    pub volume: f32,
    /// Modulation matrix.
    pub mods: [ModSlot; MOD_SLOTS],
}

impl Default for SynthSettings {
    fn default() -> Self {
        let osc = OscSettings {
            wave: Wave::Saw,
            octave: 0.0,
            semitones: 0.0,
            cents: 0.0,
            level: 0.8,
            pulse_width: 0.5,
        };
        Self {
            osc: [
                osc,
                OscSettings {
                    wave: Wave::Square,
                    cents: 7.0,
                    level: 0.5,
                    ..osc
                },
            ],
            sub_level: 0.0,
            noise_level: 0.0,
            unison: 1,
            unison_detune_cents: 20.0,
            unison_spread: 0.5,
            cutoff_hz: 2500.0,
            resonance: 0.25,
            filter_env_octaves: 2.0,
            key_track: 0.5,
            drive: 0.1,
            amp_env: EnvSettings {
                attack_ms: 2.0,
                decay_ms: 300.0,
                sustain: 0.7,
                release_ms: 200.0,
            },
            mod_env: EnvSettings {
                attack_ms: 5.0,
                decay_ms: 400.0,
                sustain: 0.2,
                release_ms: 300.0,
            },
            lfo: [
                LfoSettings {
                    wave: LfoWave::Sine,
                    rate_hz: 4.0,
                },
                LfoSettings {
                    wave: LfoWave::Triangle,
                    rate_hz: 0.5,
                },
            ],
            glide_ms: 0.0,
            velocity_sensitivity: 0.6,
            volume: 0.5,
            mods: [ModSlot::default(); MOD_SLOTS],
        }
    }
}

/// Equal-power pan gains scaled so the centre is 1.0 on both sides.
#[inline]
fn pan_unit(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * core::f32::consts::FRAC_PI_4;
    (
        theta.cos() * core::f32::consts::SQRT_2,
        theta.sin() * core::f32::consts::SQRT_2,
    )
}

#[inline]
fn key_hz(semitones: f32) -> f32 {
    440.0 * ((semitones - 69.0) / 12.0).exp2()
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    held: bool,
    key: u8,
    age: u64,
    velocity: f32,
    random: f32,
    pitch: f32,
    target_pitch: f32,
    phases: [[Phase; MAX_UNISON]; 2],
    sub: Phase,
    noise: Noise,
    amp: Adsr,
    menv: Adsr,
    lfo: [Lfo; 2],
    filt: [Ladder; 2],
    /// Samples left in the current control block.
    ctl_left: u32,
    /// The first control update after a note start jumps instead of ramping.
    fresh: bool,
    n_uni: usize,
    inc: [[f32; MAX_UNISON]; 2],
    uni_l: [f32; MAX_UNISON],
    uni_r: [f32; MAX_UNISON],
    sub_inc: f32,
    level: [f32; 2],
    pw: [f32; 2],
    sub_level: f32,
    noise_level: f32,
    g: f32,
    g_step: f32,
    k: f32,
    drive: f32,
    gain_l: f32,
    gain_r: f32,
    gain_l_step: f32,
    gain_r_step: f32,
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            held: false,
            key: 0,
            age: 0,
            velocity: 0.0,
            random: 0.0,
            pitch: 60.0,
            target_pitch: 60.0,
            phases: [[Phase::default(); MAX_UNISON]; 2],
            sub: Phase::default(),
            noise: Noise::new(seed),
            amp: Adsr::default(),
            menv: Adsr::default(),
            lfo: [Lfo::new(seed ^ 0x5bd1_e995), Lfo::new(seed ^ 0x1b87_3593)],
            filt: [Ladder::default(); 2],
            ctl_left: 0,
            fresh: true,
            n_uni: 1,
            inc: [[0.0; MAX_UNISON]; 2],
            uni_l: [1.0; MAX_UNISON],
            uni_r: [1.0; MAX_UNISON],
            sub_inc: 0.0,
            level: [0.0; 2],
            pw: [0.5; 2],
            sub_level: 0.0,
            noise_level: 0.0,
            g: 0.0,
            g_step: 0.0,
            k: 0.0,
            drive: 1.0,
            gain_l: 0.0,
            gain_r: 0.0,
            gain_l_step: 0.0,
            gain_r_step: 0.0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        key: u8,
        velocity: f32,
        age: u64,
        from_pitch: f32,
        unison: usize,
        amp: AdsrParams,
        menv: AdsrParams,
    ) {
        let stolen = self.active;
        self.active = true;
        self.held = true;
        self.key = key;
        self.age = age;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.random = self.noise.next_value();
        self.target_pitch = f32::from(key);
        self.pitch = from_pitch;
        self.ctl_left = 0;
        if stolen {
            self.amp.retrigger(amp);
            self.menv.retrigger(menv);
        } else {
            self.fresh = true;
            self.amp.trigger(amp);
            self.menv.trigger(menv);
            for f in &mut self.filt {
                f.reset();
            }
            // A single oscillator starts at phase 0 so every note has the same attack; unison
            // copies start at random phases, otherwise they begin in phase and the chorus
            // effect only builds up over the first beats.
            for osc in &mut self.phases {
                for (u, p) in osc.iter_mut().enumerate() {
                    p.t = if unison > 1 && u > 0 {
                        0.5 * (self.noise.next_value() + 1.0)
                    } else {
                        0.0
                    };
                }
            }
            self.sub.t = 0.0;
        }
        for l in &mut self.lfo {
            l.reset();
        }
    }

    /// Recomputes modulation and everything derived from it, for the next `CONTROL_FRAMES`
    /// samples.
    fn control(&mut self, p: &SynthSettings, sr: f32, glide_coef: f32) {
        let ctl = CONTROL_FRAMES as f32;
        let lfo_v = [
            self.lfo[0].value(p.lfo[0].wave),
            self.lfo[1].value(p.lfo[1].wave),
        ];
        let mut m = [0.0_f32; ModDest::COUNT];
        for slot in &p.mods {
            let s = match slot.source {
                ModSource::Off => continue,
                ModSource::Lfo1 => lfo_v[0],
                ModSource::Lfo2 => lfo_v[1],
                ModSource::ModEnv => self.menv.level(),
                ModSource::AmpEnv => self.amp.level(),
                ModSource::Velocity => self.velocity,
                ModSource::Note => ((f32::from(self.key) - 60.0) / 60.0).clamp(-1.0, 1.0),
                ModSource::Random => self.random,
            };
            m[slot.dest as usize] += slot.amount * s;
        }
        let md = |d: ModDest| m[d as usize];

        for (j, lfo) in self.lfo.iter_mut().enumerate() {
            let rate_mod = if j == 0 {
                md(ModDest::Lfo1Rate)
            } else {
                md(ModDest::Lfo2Rate)
            };
            let rate = p.lfo[j].rate_hz * (rate_mod * 4.0).exp2();
            lfo.advance(rate * ctl / sr);
        }

        self.pitch += (self.target_pitch - self.pitch) * glide_coef;
        let base = self.pitch + md(ModDest::Pitch) * 12.0;
        let n = usize::from(p.unison.clamp(1, MAX_UNISON as u8));
        self.n_uni = n;
        let detune = (p.unison_detune_cents + md(ModDest::UnisonDetune) * 100.0).clamp(0.0, 100.0);
        let spread = p.unison_spread.clamp(0.0, 1.0);
        let norm = 1.0 / (n as f32).sqrt();
        for u in 0..n {
            let pos = if n > 1 {
                u as f32 / (n - 1) as f32 * 2.0 - 1.0
            } else {
                0.0
            };
            let (l, r) = pan_unit(pos * spread);
            self.uni_l[u] = l * norm;
            self.uni_r[u] = r * norm;
        }
        let nyq = 0.49;
        for o in 0..2 {
            let os = &p.osc[o];
            let (pitch_mod, pw_mod, level_mod) = if o == 0 {
                (
                    md(ModDest::Osc1Pitch),
                    md(ModDest::Osc1Pw),
                    md(ModDest::Osc1Level),
                )
            } else {
                (
                    md(ModDest::Osc2Pitch),
                    md(ModDest::Osc2Pw),
                    md(ModDest::Osc2Level),
                )
            };
            let semis =
                base + os.octave * 12.0 + os.semitones + os.cents / 100.0 + pitch_mod * 12.0;
            let inc0 = key_hz(semis) / sr;
            for u in 0..n {
                let pos = if n > 1 {
                    u as f32 / (n - 1) as f32 * 2.0 - 1.0
                } else {
                    0.0
                };
                self.inc[o][u] = (inc0 * (pos * detune / 1200.0).exp2()).min(nyq);
            }
            self.level[o] = (os.level + level_mod).clamp(0.0, 1.0);
            self.pw[o] = (os.pulse_width + pw_mod * 0.45).clamp(0.05, 0.95);
        }
        let o1 = &p.osc[0];
        self.sub_inc = (key_hz(base + o1.octave * 12.0 + o1.semitones - 12.0) / sr).min(nyq);
        self.sub_level = p.sub_level.clamp(0.0, 1.0);
        self.noise_level = (p.noise_level + md(ModDest::NoiseLevel)).clamp(0.0, 1.0);

        let octaves = p.filter_env_octaves * self.menv.level()
            + p.key_track * (self.pitch - 60.0) / 12.0
            + md(ModDest::Cutoff) * 6.0;
        let g = ladder_g(p.cutoff_hz * octaves.clamp(-12.0, 12.0).exp2(), sr);
        self.k = ladder_k(p.resonance + md(ModDest::Resonance));
        self.drive = 1.0 + 3.0 * p.drive.clamp(0.0, 1.0);

        let vs = p.velocity_sensitivity.clamp(0.0, 1.0);
        let vel = 1.0 - vs + vs * self.velocity * self.velocity;
        let amp = p.volume.max(0.0) * vel * (1.0 + md(ModDest::Amp)).clamp(0.0, 2.0);
        let (pl, pr) = pan_unit(md(ModDest::Pan));
        let (tl, tr) = (amp * pl, amp * pr);

        if self.fresh {
            self.fresh = false;
            self.g = g;
            self.g_step = 0.0;
            self.gain_l = tl;
            self.gain_r = tr;
            self.gain_l_step = 0.0;
            self.gain_r_step = 0.0;
        } else {
            self.g_step = (g - self.g) / ctl;
            self.gain_l_step = (tl - self.gain_l) / ctl;
            self.gain_r_step = (tr - self.gain_r) / ctl;
        }
    }

    /// Adds this voice into the buffers. Ends the voice when its amp envelope is done.
    fn render(
        &mut self,
        p: &SynthSettings,
        sr: f32,
        glide_coef: f32,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let waves = [p.osc[0].wave, p.osc[1].wave];
        for (ol, or) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if self.ctl_left == 0 {
                self.control(p, sr, glide_coef);
                self.ctl_left = CONTROL_FRAMES;
            }
            self.ctl_left -= 1;
            let a = self.amp.next_level();
            self.menv.next_level();
            if !self.amp.is_active() {
                self.active = false;
                self.held = false;
                return;
            }
            let (mut l, mut r) = (0.0_f32, 0.0_f32);
            for (o, &wave) in waves.iter().enumerate() {
                let level = self.level[o];
                if level == 0.0 {
                    continue;
                }
                let (mut sl, mut sr_) = (0.0, 0.0);
                for u in 0..self.n_uni {
                    let dt = self.inc[o][u];
                    let t = self.phases[o][u].advance(dt);
                    let s = blep_sample(wave, t, dt, self.pw[o]);
                    sl += s * self.uni_l[u];
                    sr_ += s * self.uni_r[u];
                }
                l += sl * level;
                r += sr_ * level;
            }
            let mut mono = 0.0;
            if self.sub_level > 0.0 {
                let t = self.sub.advance(self.sub_inc);
                mono += (t * core::f32::consts::TAU).sin() * self.sub_level;
            }
            if self.noise_level > 0.0 {
                mono += self.noise.next_value() * self.noise_level;
            }
            l += mono;
            r += mono;
            self.g += self.g_step;
            let yl = self.filt[0].process(l, self.g, self.k, self.drive);
            let yr = self.filt[1].process(r, self.g, self.k, self.drive);
            self.gain_l += self.gain_l_step;
            self.gain_r += self.gain_r_step;
            *ol += yl * a * self.gain_l;
            *or += yr * a * self.gain_r;
        }
    }
}

/// The synth: settings plus a fixed pool of voices.
#[derive(Debug, Clone)]
pub struct GloomSynth {
    sr: f32,
    settings: SynthSettings,
    amp_params: AdsrParams,
    menv_params: AdsrParams,
    glide_coef: f32,
    voices: [Voice; SYNTH_VOICES],
    last_pitch: Option<f32>,
}

impl GloomSynth {
    /// Creates a silent synth with the default settings. Allocates nothing on the heap; the
    /// engine boxes it once at creation.
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut voices = [Voice::new(1); SYNTH_VOICES];
        for (i, v) in voices.iter_mut().enumerate() {
            *v = Voice::new(0x2545_F491 ^ (i as u32 + 1).wrapping_mul(0x9E37_79B9));
        }
        let mut s = Self {
            sr,
            settings: SynthSettings::default(),
            amp_params: AdsrParams::default(),
            menv_params: AdsrParams::default(),
            glide_coef: 1.0,
            voices,
            last_pitch: None,
        };
        s.set_settings(&SynthSettings::default());
        s
    }

    /// Current settings.
    pub fn settings(&self) -> &SynthSettings {
        &self.settings
    }

    /// Applies new settings. Sounding voices follow at their next control update; envelope
    /// changes apply to them at once.
    pub fn set_settings(&mut self, s: &SynthSettings) {
        self.settings = *s;
        self.amp_params = s.amp_env.params(self.sr);
        self.menv_params = s.mod_env.params(self.sr);
        // Exponential glide per control block: 99 % (4.6 time constants) in the glide time.
        let frames = s.glide_ms.max(0.0) * 0.001 * self.sr;
        self.glide_coef = if frames < CONTROL_FRAMES as f32 {
            1.0
        } else {
            1.0 - (-4.6 * CONTROL_FRAMES as f32 / frames).exp()
        };
        for v in &mut self.voices {
            v.amp.set_params(self.amp_params);
            v.menv.set_params(self.menv_params);
        }
    }

    /// Starts a note. `age` orders voices for stealing (larger is newer).
    pub fn note_on(&mut self, key: u8, velocity: f32, age: u64) {
        let idx = self.pick_voice();
        let from = match self.last_pitch {
            Some(p) if self.glide_coef < 1.0 => p,
            _ => f32::from(key),
        };
        let unison = usize::from(self.settings.unison.clamp(1, MAX_UNISON as u8));
        self.voices[idx].start(
            key,
            velocity,
            age,
            from,
            unison,
            self.amp_params,
            self.menv_params,
        );
        self.last_pitch = Some(f32::from(key));
    }

    fn pick_voice(&self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        let oldest = |release_only: bool| {
            self.voices
                .iter()
                .enumerate()
                .filter(|(_, v)| !release_only || !v.held)
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
        };
        oldest(true).or_else(|| oldest(false)).unwrap_or(0)
    }

    /// Releases held voices playing `key`.
    pub fn note_off(&mut self, key: u8) {
        for v in &mut self.voices {
            if v.active && v.held && v.key == key {
                v.held = false;
                v.amp.release();
                v.menv.release();
            }
        }
    }

    /// Releases every voice.
    pub fn release_all(&mut self) {
        for v in &mut self.voices {
            if v.active {
                v.held = false;
                v.amp.release();
                v.menv.release();
            }
        }
    }

    /// Silences every voice at once.
    pub fn kill_all(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.held = false;
            v.amp.reset();
            v.menv.reset();
        }
        self.last_pitch = None;
    }

    /// True if any voice is sounding.
    pub fn is_active(&self) -> bool {
        self.voices.iter().any(|v| v.active)
    }

    /// Number of sounding voices.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Adds every sounding voice into `out_l` and `out_r` (same length).
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        let (out_l, out_r) = (&mut out_l[..n], &mut out_r[..n]);
        for v in &mut self.voices {
            if v.active {
                v.render(&self.settings, self.sr, self.glide_coef, out_l, out_r);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn render(s: &mut GloomSynth, n: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        s.render(&mut l, &mut r);
        (l, r)
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Zero crossings per second, as a rough pitch estimate for a sine.
    fn pitch_hz(x: &[f32]) -> f32 {
        let ups = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        ups as f32 * SR / x.len() as f32
    }

    fn sine_patch() -> SynthSettings {
        let mut s = SynthSettings::default();
        s.osc[0].wave = Wave::Sine;
        s.osc[1].level = 0.0;
        s.cutoff_hz = 20_000.0;
        s.resonance = 0.0;
        s.filter_env_octaves = 0.0;
        s.key_track = 0.0;
        s.drive = 0.0;
        s.amp_env = EnvSettings {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 10.0,
        };
        s
    }

    #[test]
    fn a4_plays_at_440_hz() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.note_on(69, 1.0, 1);
        let (l, _) = render(&mut s, 48_000);
        let f = pitch_hz(&l);
        assert!((f - 440.0).abs() <= 1.0, "{f}");
    }

    #[test]
    fn note_off_releases_to_silence_and_frees_the_voice() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.note_on(60, 1.0, 1);
        render(&mut s, 4800);
        s.note_off(60);
        let (l, _) = render(&mut s, 9600);
        assert!(rms(&l[9000..]) < 1e-4);
        assert!(!s.is_active());
    }

    #[test]
    fn sixteen_voices_then_the_oldest_is_stolen() {
        let mut s = GloomSynth::new(SR);
        for k in 0..SYNTH_VOICES as u8 {
            s.note_on(40 + k, 1.0, u64::from(k) + 1);
        }
        assert_eq!(s.active_voices(), SYNTH_VOICES);
        s.note_on(90, 1.0, 100);
        assert_eq!(s.active_voices(), SYNTH_VOICES);
        assert!(
            !s.voices.iter().any(|v| v.key == 40),
            "oldest (key 40) stolen"
        );
        assert!(s.voices.iter().any(|v| v.key == 90));
    }

    #[test]
    fn releasing_voices_are_stolen_before_held_ones() {
        let mut s = GloomSynth::new(SR);
        for k in 0..SYNTH_VOICES as u8 {
            s.note_on(40 + k, 1.0, u64::from(k) + 1);
        }
        s.note_off(50); // a newer note, but released
        s.note_on(90, 1.0, 100);
        assert!(!s.voices.iter().any(|v| v.key == 50));
        assert!(s.voices.iter().any(|v| v.key == 40));
    }

    #[test]
    fn stealing_does_not_click() {
        let mut s = GloomSynth::new(SR);
        let mut p = sine_patch();
        p.amp_env.attack_ms = 5.0;
        s.set_settings(&p);
        for k in 0..SYNTH_VOICES as u8 {
            s.note_on(60, 1.0, u64::from(k) + 1);
        }
        let (a, _) = render(&mut s, 4800);
        s.note_on(60, 1.0, 100);
        let (b, _) = render(&mut s, 64);
        // The step between the last sample before and the first after the steal is no larger
        // than the waveform's own slope.
        let max_step = a
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            (b[0] - a[4799]).abs() <= max_step * 1.5,
            "{} {}",
            a[4799],
            b[0]
        );
    }

    #[test]
    fn glide_moves_between_pitches() {
        let mut s = GloomSynth::new(SR);
        let mut p = sine_patch();
        p.glide_ms = 200.0;
        s.set_settings(&p);
        s.note_on(57, 1.0, 1); // A3, 220 Hz
        render(&mut s, 4800);
        s.note_off(57);
        s.note_on(69, 1.0, 2); // A4, 440 Hz
        let (l, _) = render(&mut s, 48_000);
        let early = pitch_hz(&l[..2400]);
        let late = pitch_hz(&l[24_000..]);
        assert!(early < 330.0, "{early}");
        assert!((late - 440.0).abs() < 3.0, "{late}");
    }

    #[test]
    fn unison_spreads_into_stereo() {
        let mut s = GloomSynth::new(SR);
        let mut p = SynthSettings::default();
        p.unison = 1;
        s.set_settings(&p);
        s.note_on(60, 1.0, 1);
        let (l, r) = render(&mut s, 4800);
        assert_eq!(l, r, "a single copy is centred");
        p.unison = 5;
        p.unison_spread = 1.0;
        s.kill_all();
        s.set_settings(&p);
        s.note_on(60, 1.0, 2);
        let (l, r) = render(&mut s, 4800);
        assert!(l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 0.01));
    }

    #[test]
    fn filter_envelope_opens_the_filter() {
        let mut p = SynthSettings::default();
        p.osc[1].level = 0.0;
        p.cutoff_hz = 200.0;
        p.resonance = 0.0;
        p.key_track = 0.0;
        p.filter_env_octaves = 0.0;
        let mut closed = GloomSynth::new(SR);
        closed.set_settings(&p);
        p.filter_env_octaves = 6.0;
        p.mod_env.sustain = 1.0;
        let mut open = GloomSynth::new(SR);
        open.set_settings(&p);
        closed.note_on(48, 1.0, 1);
        open.note_on(48, 1.0, 1);
        // Brightness: energy of the first difference (a crude high-pass).
        let hf = |x: &[f32]| rms(&x.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>());
        let a = hf(&render(&mut closed, 9600).0);
        let b = hf(&render(&mut open, 9600).0);
        assert!(b > 3.0 * a, "{a} {b}");
    }

    #[test]
    fn mod_matrix_lfo_to_pitch_makes_vibrato() {
        let mut p = sine_patch();
        p.lfo[0].rate_hz = 5.0;
        p.mods[0] = ModSlot {
            source: ModSource::Lfo1,
            dest: ModDest::Pitch,
            amount: 1.0 / 12.0, // ±1 semitone
        };
        let mut s = GloomSynth::new(SR);
        s.set_settings(&p);
        s.note_on(69, 1.0, 1);
        let (l, _) = render(&mut s, 48_000);
        // Pitch measured over 1/20 s windows swings both ways around 440.
        let windows: Vec<f32> = l.chunks(2400).map(pitch_hz).collect();
        let lo = windows.iter().cloned().fold(f32::MAX, f32::min);
        let hi = windows.iter().cloned().fold(0.0, f32::max);
        assert!(lo < 430.0 && hi > 450.0, "{lo} {hi}");
    }

    #[test]
    fn output_is_finite_and_bounded_for_extreme_settings() {
        let mut p = SynthSettings::default();
        p.unison = 7;
        p.unison_detune_cents = 100.0;
        p.resonance = 1.0;
        p.drive = 1.0;
        p.cutoff_hz = 20_000.0;
        p.noise_level = 1.0;
        p.sub_level = 1.0;
        p.osc[0].octave = 3.0;
        p.osc[1].octave = 3.0;
        p.filter_env_octaves = 8.0;
        p.mods[0] = ModSlot {
            source: ModSource::Lfo1,
            dest: ModDest::Cutoff,
            amount: 1.0,
        };
        let mut s = GloomSynth::new(SR);
        s.set_settings(&p);
        for k in 0..16 {
            s.note_on(100 + k, 1.0, u64::from(k) + 1);
        }
        let (l, r) = render(&mut s, 48_000);
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 50.0));
    }
}
