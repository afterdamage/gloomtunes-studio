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
//! Per-note values ([`NoteExpr`]): pan adds to the voice's pan, fine pitch retunes it, mod X
//! moves the cutoff (±3 octaves) and mod Y the resonance (±0.5), both also matrix sources, and
//! release velocity scales the release times. A slide note glides the newest held voice
//! linearly (in semitones) to its key over the slide note's length; a portamento note starts
//! its own voice at the previous pitch and glides the same way over the glide time.
//!
//! Real-time safety: all voices are allocated in [`GloomSynth::new`]; nothing allocates, locks or
//! panics afterwards.

use crate::adsr::{Adsr, AdsrParams};
use crate::blep::{blep_sample, Phase, Wave};
use crate::ladder::{ladder_g, ladder_k, Ladder};
use crate::lfo::{Lfo, LfoWave, Noise};
use crate::note::{Glide, NoteExpr};

mod common;
pub mod fm;
pub mod wavetable;

use std::sync::Arc;

use common::{key_hz, pan_unit, ModInputs};
pub use common::{EnvSettings, Expression, LfoSettings, ModDest, ModSlot, ModSource};
use fm::{FmSettings, FmVoice};
use wavetable::{mip_for, SynthTables, Warp};

/// Which sound generator the voices run; envelopes, LFOs, the matrix and the voice manager
/// are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// Two band-limited oscillators, sub and noise into the ladder filter.
    #[default]
    Subtractive,
    /// Six operators in phase modulation ([`fm`]); no filter.
    Fm,
    /// Two wavetable oscillators ([`wavetable`]), sub and noise into the ladder filter.
    Wavetable,
    /// The subtractive path played as a mono bass line (Prompt 28): one voice; a note
    /// overlapping the held one (or a slide note) glides to it without retriggering; a note at
    /// or above the accent velocity opens the filter and gets louder, more so on consecutive
    /// accents.
    Bass,
}

/// Settings of the bass-line engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BassSettings {
    /// How much an accent opens the filter and lifts the level, 0 to 1.
    pub accent: f32,
    /// Velocity at which a note is accented, 0 to 1.
    pub threshold: f32,
    /// Glide time of a slide (overlapping notes), ms.
    pub slide_ms: f32,
}

impl Default for BassSettings {
    fn default() -> Self {
        Self {
            accent: 0.6,
            threshold: 0.79,
            slide_ms: 60.0,
        }
    }
}

/// Filter opening of a fresh accent at full accent amount, octaves.
const ACCENT_OCTAVES: f32 = 2.5;
/// Further opening per unit of accumulated charge (consecutive accents), octaves.
const CHARGE_OCTAVES: f32 = 1.2;
/// Level lift of a fresh accent at full amount.
const ACCENT_GAIN: f32 = 0.8;
/// Time constants of an accent's decay and of the charge's, s.
const ACCENT_DECAY_S: f32 = 0.2;
const CHARGE_DECAY_S: f32 = 0.6;

/// One wavetable oscillator's settings (its pitch and level are the matching
/// [`OscSettings`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WtOscSettings {
    /// Factory table index (a user table, when loaded, replaces it).
    pub table: u8,
    /// Frame position, 0 to 1.
    pub position: f32,
    /// Phase warp.
    pub warp: Warp,
    /// Warp amount, 0 to 1.
    pub warp_amount: f32,
}

/// An envelope's coefficients with its times scaled per voice (attack, decay, release).
fn env_params(e: &EnvSettings, sr: f32, scale: [f32; 3]) -> AdsrParams {
    if scale == [1.0; 3] {
        return e.params(sr);
    }
    AdsrParams::new(
        sr,
        e.attack_ms * scale[0],
        e.decay_ms * scale[1],
        e.sustain,
        e.release_ms * scale[2],
    )
}

/// Voices per synth.
pub const SYNTH_VOICES: usize = 16;
/// Most unison copies per oscillator.
pub const MAX_UNISON: usize = 7;
/// Modulation matrix slots.
pub const MOD_SLOTS: usize = 8;
/// Samples per control-rate update.
pub const CONTROL_FRAMES: u32 = 16;
/// Portamento time of a porta note when the patch's glide time is zero, in seconds.
pub const PORTA_DEFAULT_S: f32 = 0.08;
/// Filter cutoff movement at full note mod X, in octaves.
const NOTE_X_OCTAVES: f32 = 3.0;
/// Resonance change at full note mod Y.
const NOTE_Y_RESONANCE: f32 = 0.5;

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
    /// Sound generator.
    pub engine: Engine,
    /// FM engine settings.
    pub fm: FmSettings,
    /// Wavetable oscillators.
    pub wt: [WtOscSettings; 2],
    /// Bass-line engine.
    pub bass: BassSettings,
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
            engine: Engine::Subtractive,
            fm: FmSettings::default(),
            wt: [WtOscSettings::default(); 2],
            bass: BassSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    held: bool,
    /// Released while the sustain pedal was down: keeps sounding until the pedal comes up.
    sustained: bool,
    /// Polyphonic aftertouch on this voice's key, 0 to 1.
    pressure: f32,
    /// MPE pitch on this voice's key, semitones.
    tuning: f32,
    key: u8,
    age: u64,
    velocity: f32,
    random: f32,
    pitch: f32,
    target_pitch: f32,
    /// A linear glide in progress (slide or portamento): semitones per control block, and
    /// blocks left.
    slide_step: f32,
    slide_left: u32,
    /// Per-note pan, mod X/Y (bipolar) and release time multiplier.
    note_pan: f32,
    mod_x: f32,
    mod_y: f32,
    release_scale: f32,
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
    /// FM operators.
    fm: FmVoice,
    /// Wavetable position, warp amount and mip level of each oscillator.
    wt_pos: [f32; 2],
    wt_warp: [f32; 2],
    wt_mip: [usize; 2],
    /// Bass engine: this note's accent (decaying from 1) and the charge of the accents
    /// before it.
    acc: f32,
    charge: f32,
    /// Matrix modulation of the LFO depths, from the last control update.
    lfo_depth: [f32; 2],
    /// Envelope time multipliers (attack, decay, release) taken at the note start.
    env_scale: [f32; 3],
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            held: false,
            sustained: false,
            pressure: 0.0,
            tuning: 0.0,
            key: 0,
            age: 0,
            velocity: 0.0,
            random: 0.0,
            pitch: 60.0,
            target_pitch: 60.0,
            slide_step: 0.0,
            slide_left: 0,
            note_pan: 0.0,
            mod_x: 0.0,
            mod_y: 0.0,
            release_scale: 1.0,
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
            fm: FmVoice::default(),
            wt_pos: [0.0; 2],
            wt_warp: [0.0; 2],
            wt_mip: [0; 2],
            acc: 0.0,
            charge: 0.0,
            lfo_depth: [0.0; 2],
            env_scale: [1.0; 3],
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
        self.sustained = false;
        self.pressure = 0.0;
        self.tuning = 0.0;
        self.key = key;
        self.age = age;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.random = self.noise.next_value();
        self.target_pitch = f32::from(key);
        self.pitch = from_pitch;
        self.slide_left = 0;
        self.note_pan = 0.0;
        self.mod_x = 0.0;
        self.mod_y = 0.0;
        self.release_scale = 1.0;
        self.lfo_depth = [0.0; 2];
        self.env_scale = [1.0; 3];
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

    /// Glides linearly in semitones from the current pitch to `target` over `seconds`.
    fn slide_to(&mut self, target: f32, seconds: f32, sr: f32) {
        self.target_pitch = target;
        let blocks = (seconds.max(0.0) * sr / CONTROL_FRAMES as f32).round() as u32;
        if blocks == 0 {
            self.pitch = target;
            self.slide_left = 0;
        } else {
            self.slide_step = (target - self.pitch) / blocks as f32;
            self.slide_left = blocks;
        }
    }

    /// Recomputes modulation and everything derived from it, for the next `CONTROL_FRAMES`
    /// samples.
    fn control(&mut self, p: &SynthSettings, x: &Expression, sr: f32, glide_coef: f32) {
        let ctl = CONTROL_FRAMES as f32;
        let depth = |d: f32| (1.0 + d).clamp(0.0, 2.0);
        let lfo_v = [
            self.lfo[0].value(p.lfo[0].wave) * depth(self.lfo_depth[0]),
            self.lfo[1].value(p.lfo[1].wave) * depth(self.lfo_depth[1]),
        ];
        let m = ModInputs {
            lfo: lfo_v,
            mod_env: self.menv.level(),
            amp_env: self.amp.level(),
            velocity: self.velocity,
            key: self.key,
            random: self.random,
            mod_wheel: x.mod_wheel,
            pressure: x.pressure.max(self.pressure),
            mod_x: self.mod_x,
            mod_y: self.mod_y,
        }
        .eval(&p.mods);
        let md = |d: ModDest| m[d as usize];
        self.lfo_depth = [md(ModDest::Lfo1Depth), md(ModDest::Lfo2Depth)];

        for (j, lfo) in self.lfo.iter_mut().enumerate() {
            let rate_mod = if j == 0 {
                md(ModDest::Lfo1Rate)
            } else {
                md(ModDest::Lfo2Rate)
            };
            let rate = p.lfo[j].rate_hz * (rate_mod * 4.0).exp2();
            lfo.advance(rate * ctl / sr);
        }

        if self.slide_left > 0 {
            self.slide_left -= 1;
            self.pitch = if self.slide_left == 0 {
                self.target_pitch
            } else {
                self.pitch + self.slide_step
            };
        } else {
            self.pitch += (self.target_pitch - self.pitch) * glide_coef;
        }
        let base = self.pitch + self.tuning + x.bend_semis + md(ModDest::Pitch) * 12.0;
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
        match p.engine {
            Engine::Subtractive | Engine::Bass => {}
            Engine::Fm => {
                let index = md(ModDest::FmIndex);
                let feedback = md(ModDest::FmFeedback);
                self.fm.control(&p.fm, key_hz(base), sr, index, feedback);
            }
            Engine::Wavetable => {
                for o in 0..2 {
                    let w = &p.wt[o];
                    let (pos, warp) = if o == 0 {
                        (ModDest::Wt1Pos, ModDest::Wt1Warp)
                    } else {
                        (ModDest::Wt2Pos, ModDest::Wt2Warp)
                    };
                    self.wt_pos[o] = (w.position + md(pos)).clamp(0.0, 1.0);
                    let amount = (w.warp_amount + md(warp)).clamp(0.0, 1.0);
                    self.wt_warp[o] = amount;
                    self.wt_mip[o] = mip_for(self.inc[o][n - 1] * w.warp.speed(amount));
                }
            }
        }

        let mut accent_gain = 1.0;
        let mut accent_oct = 0.0;
        if p.engine == Engine::Bass && (self.acc > 0.0 || self.charge > 0.0) {
            let a = p.bass.accent.clamp(0.0, 1.0);
            accent_oct = a * (self.acc * ACCENT_OCTAVES + self.charge * CHARGE_OCTAVES);
            accent_gain = 1.0 + a * ACCENT_GAIN * self.acc;
            self.acc *= (-ctl / (ACCENT_DECAY_S * sr)).exp();
            self.charge *= (-ctl / (CHARGE_DECAY_S * sr)).exp();
        }
        let octaves = (p.filter_env_octaves + md(ModDest::ModEnvAmount) * 6.0) * self.menv.level()
            + accent_oct
            + p.key_track * (self.pitch - 60.0) / 12.0
            + md(ModDest::Cutoff) * 6.0
            + self.mod_x * NOTE_X_OCTAVES;
        let g = ladder_g(p.cutoff_hz * octaves.clamp(-12.0, 12.0).exp2(), sr);
        self.k = ladder_k(p.resonance + md(ModDest::Resonance) + self.mod_y * NOTE_Y_RESONANCE);
        self.drive = 1.0 + 3.0 * p.drive.clamp(0.0, 1.0);

        let vs = p.velocity_sensitivity.clamp(0.0, 1.0);
        let vel = 1.0 - vs + vs * self.velocity * self.velocity;
        let amp = p.volume.max(0.0) * vel * (1.0 + md(ModDest::Amp)).clamp(0.0, 2.0) * accent_gain;
        let (pl, pr) = pan_unit((md(ModDest::Pan) + self.note_pan).clamp(-1.0, 1.0));
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

    /// The wavetable oscillators' next sample, added into `l` and `r`. Without tables (not
    /// sent yet) they play sines.
    #[inline]
    fn wt_oscs(
        &mut self,
        p: &SynthSettings,
        tables: Option<&SynthTables>,
        l: &mut f32,
        r: &mut f32,
    ) {
        for o in 0..2 {
            let level = self.level[o];
            if level == 0.0 {
                continue;
            }
            let w = &p.wt[o];
            let table = tables.and_then(|t| t.get(o, usize::from(w.table)));
            let (pos, amount, mip) = (self.wt_pos[o], self.wt_warp[o], self.wt_mip[o]);
            let (mut sl, mut sr_) = (0.0, 0.0);
            for u in 0..self.n_uni {
                let t = self.phases[o][u].advance(self.inc[o][u]);
                let t = w.warp.apply(t, amount);
                let s = match table {
                    Some(tb) => tb.read(pos, t, mip),
                    None => (t * core::f32::consts::TAU).sin(),
                };
                sl += s * self.uni_l[u];
                sr_ += s * self.uni_r[u];
            }
            *l += sl * level;
            *r += sr_ * level;
        }
    }

    /// Adds this voice into the buffers. Ends the voice when its amp envelope is done (or, in
    /// FM, when its carriers are).
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        p: &SynthSettings,
        x: &Expression,
        sr: f32,
        glide_coef: f32,
        tables: Option<&SynthTables>,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let waves = [p.osc[0].wave, p.osc[1].wave];
        for (ol, or) in out_l.iter_mut().zip(out_r.iter_mut()) {
            if self.ctl_left == 0 {
                self.control(p, x, sr, glide_coef);
                self.ctl_left = CONTROL_FRAMES;
            }
            self.ctl_left -= 1;
            let a = self.amp.next_level();
            self.menv.next_level();
            if !self.amp.is_active() {
                self.active = false;
                self.held = false;
                self.sustained = false;
                return;
            }
            if p.engine == Engine::Fm {
                if !self.fm.is_sounding() {
                    self.amp.reset();
                    self.menv.reset();
                    self.active = false;
                    self.held = false;
                    self.sustained = false;
                    return;
                }
                let y = self.fm.tick();
                self.gain_l += self.gain_l_step;
                self.gain_r += self.gain_r_step;
                *ol += y * a * self.gain_l;
                *or += y * a * self.gain_r;
                continue;
            }
            let (mut l, mut r) = (0.0_f32, 0.0_f32);
            if p.engine == Engine::Wavetable {
                self.wt_oscs(p, tables, &mut l, &mut r);
            } else {
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
    expr: Expression,
    sustain: bool,
    sr: f32,
    settings: SynthSettings,
    amp_params: AdsrParams,
    menv_params: AdsrParams,
    glide_coef: f32,
    voices: [Voice; SYNTH_VOICES],
    last_pitch: Option<f32>,
    tables: Option<Arc<SynthTables>>,
    /// Bass engine: charge left by recent accents (decays with time, sounding or not).
    charge: f32,
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
            expr: Expression::default(),
            sustain: false,
            sr,
            settings: SynthSettings::default(),
            amp_params: AdsrParams::default(),
            menv_params: AdsrParams::default(),
            glide_coef: 1.0,
            voices,
            last_pitch: None,
            tables: None,
            charge: 0.0,
        };
        fm::warm_up();
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
        let sr = self.sr;
        for v in &mut self.voices {
            v.amp.set_params(env_params(&s.amp_env, sr, v.env_scale));
            v.menv.set_params(env_params(&s.mod_env, sr, v.env_scale));
            if s.engine == Engine::Fm && v.active {
                v.fm.update(&s.fm, v.key, v.velocity, self.sr);
            }
        }
    }

    /// Hands the wavetable oscillators their tables; returns the previous ones (drop them off
    /// the audio thread).
    pub fn set_tables(&mut self, tables: Option<Arc<SynthTables>>) -> Option<Arc<SynthTables>> {
        std::mem::replace(&mut self.tables, tables)
    }

    /// Starts a note. `age` orders voices for stealing (larger is newer).
    pub fn note_on(&mut self, key: u8, velocity: f32, age: u64) {
        self.note_on_expr(key, velocity, age, NoteExpr::default());
    }

    /// Starts a note with per-note values. A slide note retunes the newest held voice instead
    /// (or plays as a plain note when none is held); a portamento note starts at the previous
    /// note's pitch and glides to its own over the glide time ([`PORTA_DEFAULT_S`] when the
    /// patch has none).
    pub fn note_on_expr(&mut self, key: u8, velocity: f32, age: u64, e: NoteExpr) {
        if self.settings.engine == Engine::Bass {
            self.bass_note_on(key, velocity, age, e);
            return;
        }
        let target = f32::from(key) + e.fine / 100.0;
        if let Glide::Slide { seconds } = e.glide {
            let sr = self.sr;
            if let Some(v) = self
                .voices
                .iter_mut()
                .filter(|v| v.active && v.held)
                .max_by_key(|v| v.age)
            {
                v.slide_to(target, seconds, sr);
                self.last_pitch = Some(target);
                return;
            }
        }
        let idx = self.pick_voice();
        let from = match self.last_pitch {
            Some(p) if self.glide_coef < 1.0 || e.glide == Glide::Porta => p,
            _ => target,
        };
        let unison = usize::from(self.settings.unison.clamp(1, MAX_UNISON as u8));
        let stolen = self.voices[idx].active;
        self.voices[idx].start(
            key,
            velocity,
            age,
            from,
            unison,
            self.amp_params,
            self.menv_params,
        );
        let v = &mut self.voices[idx];
        if self.settings.engine == Engine::Fm {
            let fm = &self.settings.fm;
            v.fm.start(fm, key, v.velocity, self.sr, stolen);
        }
        v.note_pan = e.pan.clamp(-1.0, 1.0);
        v.mod_x = e.x();
        v.mod_y = e.y();
        v.release_scale = e.release_scale();
        v.target_pitch = target;
        self.start_env_mods(idx);
        let v = &mut self.voices[idx];
        if e.glide == Glide::Porta && from != target {
            let ms = self.settings.glide_ms;
            let s = if ms > 0.0 {
                ms * 0.001
            } else {
                PORTA_DEFAULT_S
            };
            v.slide_to(target, s, self.sr);
        }
        self.last_pitch = Some(target);
    }

    /// The bass-line engine's note: one voice. A note while the voice is held (overlapping
    /// notes) or a slide note glides there without retriggering; any other note restarts the
    /// voice (from its current level, so there is no click). Accents are decided by velocity.
    fn bass_note_on(&mut self, key: u8, velocity: f32, age: u64, e: NoteExpr) {
        let b = self.settings.bass;
        let target = f32::from(key) + e.fine / 100.0;
        let accent = velocity >= b.threshold;
        let sr = self.sr;
        // Only voice 0 plays; any other (left from another engine) is released.
        for v in &mut self.voices[1..] {
            if v.active {
                v.held = false;
                v.amp.release();
                v.menv.release();
            }
        }
        let v = &mut self.voices[0];
        let legato = v.active && v.held;
        let slide = matches!(e.glide, Glide::Slide { .. });
        if legato || (slide && v.active) {
            let seconds = match e.glide {
                Glide::Slide { seconds } => seconds,
                _ => b.slide_ms.max(0.0) * 0.001,
            };
            v.slide_to(target, seconds, sr);
            v.key = key;
            v.held = true;
            v.age = age;
            if accent {
                v.charge = self.charge;
                v.acc = 1.0;
                self.charge = (self.charge + 1.0).min(2.0);
            }
            self.last_pitch = Some(target);
            return;
        }
        let charge = if accent { self.charge } else { 0.0 };
        if accent {
            self.charge = (self.charge + 1.0).min(2.0);
        }
        let from = match self.last_pitch {
            Some(p) if self.glide_coef < 1.0 => p,
            _ => target,
        };
        v.start(
            key,
            velocity,
            age,
            from,
            1,
            self.amp_params,
            self.menv_params,
        );
        v.note_pan = e.pan.clamp(-1.0, 1.0);
        v.mod_x = e.x();
        v.mod_y = e.y();
        v.release_scale = e.release_scale();
        v.target_pitch = target;
        v.acc = if accent { 1.0 } else { 0.0 };
        v.charge = charge;
        self.last_pitch = Some(target);
        self.start_env_mods(0);
    }

    /// Per-voice envelope times: the matrix at the note start (LFOs and envelopes at zero)
    /// scales this voice's attack, decay and release.
    fn start_env_mods(&mut self, idx: usize) {
        let s = &self.settings;
        let wanted = |d: ModDest| {
            s.mods
                .iter()
                .any(|m| m.dest == d && m.source != ModSource::Off)
        };
        if !(wanted(ModDest::EnvAttack) || wanted(ModDest::EnvDecay) || wanted(ModDest::EnvRelease))
        {
            return;
        }
        let x = self.expr;
        let v = &mut self.voices[idx];
        let m = ModInputs {
            velocity: v.velocity,
            key: v.key,
            random: v.random,
            mod_wheel: x.mod_wheel,
            pressure: x.pressure.max(v.pressure),
            mod_x: v.mod_x,
            mod_y: v.mod_y,
            ..ModInputs::default()
        }
        .eval(&s.mods);
        let k = |d: ModDest| (m[d as usize].clamp(-1.0, 1.0) * 4.0).exp2();
        v.env_scale = [
            k(ModDest::EnvAttack),
            k(ModDest::EnvDecay),
            k(ModDest::EnvRelease),
        ];
        v.amp
            .set_params(env_params(&s.amp_env, self.sr, v.env_scale));
        v.menv
            .set_params(env_params(&s.mod_env, self.sr, v.env_scale));
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

    /// Releases held voices playing `key`; with the sustain pedal down they keep sounding
    /// until it comes up.
    pub fn note_off(&mut self, key: u8) {
        let sustain = self.sustain;
        for v in &mut self.voices {
            if v.active && v.held && !v.sustained && v.key == key {
                if sustain {
                    v.sustained = true;
                } else {
                    v.held = false;
                    v.amp.release_scaled(v.release_scale);
                    v.menv.release_scaled(v.release_scale);
                    v.fm.release(v.release_scale);
                }
            }
        }
    }

    /// Sustain pedal down or up. Up releases every voice whose key was let go meanwhile.
    pub fn set_sustain(&mut self, down: bool) {
        self.sustain = down;
        if down {
            return;
        }
        for v in &mut self.voices {
            if v.active && v.sustained {
                v.sustained = false;
                v.held = false;
                v.amp.release_scaled(v.release_scale);
                v.menv.release_scaled(v.release_scale);
                v.fm.release(v.release_scale);
            }
        }
    }

    /// Bend, mod wheel and channel aftertouch, applied at each voice's next control update.
    pub fn set_expression(&mut self, x: Expression) {
        self.expr = x;
    }

    /// The current expression.
    pub fn expression(&self) -> Expression {
        self.expr
    }

    /// Polyphonic aftertouch on `key` (0 to 1), for the voices holding it.
    pub fn set_key_pressure(&mut self, key: u8, value: f32) {
        for v in &mut self.voices {
            if v.active && v.key == key {
                v.pressure = value.clamp(0.0, 1.0);
            }
        }
    }

    /// Per-note pitch on `key` (MPE), in semitones from the note's own pitch.
    pub fn set_key_tuning(&mut self, key: u8, semitones: f32) {
        for v in &mut self.voices {
            if v.active && v.key == key {
                // The note's own fine pitch already moved its target; this is the rest.
                let own = v.target_pitch - f32::from(v.key);
                v.tuning = semitones.clamp(-96.0, 96.0) - own;
            }
        }
    }

    /// Per-note timbre on `key` (MPE CC 74), 0 to 1: moves mod X like the note's own value.
    pub fn set_key_timbre(&mut self, key: u8, value: f32) {
        for v in &mut self.voices {
            if v.active && v.key == key {
                v.mod_x = (value.clamp(0.0, 1.0) - 0.5) * 2.0;
            }
        }
    }

    /// Releases every voice.
    pub fn release_all(&mut self) {
        for v in &mut self.voices {
            if v.active {
                v.held = false;
                v.sustained = false;
                v.amp.release();
                v.menv.release();
                v.fm.release(1.0);
            }
        }
    }

    /// Cuts every voice with a 5 ms fade (choke groups).
    pub fn choke_all(&mut self) {
        let fast = AdsrParams::new(self.sr, 0.0, 0.0, 1.0, 5.0);
        // A 5 ms release is about 0.004 of the default release scale of the FM envelopes.
        for v in &mut self.voices {
            if v.active {
                v.held = false;
                v.sustained = false;
                v.amp.release_with(fast);
                v.menv.release_with(fast);
                v.fm.release(0.01);
            }
        }
    }

    /// Silences every voice at once.
    pub fn kill_all(&mut self) {
        for v in &mut self.voices {
            v.active = false;
            v.held = false;
            v.sustained = false;
            v.amp.reset();
            v.menv.reset();
            v.fm.reset();
        }
        self.last_pitch = None;
        self.charge = 0.0;
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
        if self.charge > 0.0 {
            self.charge *= (-(n as f32) / (CHARGE_DECAY_S * self.sr)).exp();
            if self.charge < 1e-4 {
                self.charge = 0.0;
            }
        }
        let (out_l, out_r) = (&mut out_l[..n], &mut out_r[..n]);
        for v in &mut self.voices {
            if v.active {
                v.render(
                    &self.settings,
                    &self.expr,
                    self.sr,
                    self.glide_coef,
                    self.tables.as_deref(),
                    out_l,
                    out_r,
                );
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
    fn velocity_sets_each_voices_attack_and_lfo_depth() {
        // Velocity to attack time, -100 %: soft notes take 16 times as long as loud ones.
        let mut p = sine_patch();
        p.amp_env.attack_ms = 40.0;
        p.mods[0] = ModSlot {
            source: ModSource::Velocity,
            dest: ModDest::EnvAttack,
            amount: -1.0,
        };
        let rise = |velocity: f32| {
            let mut s = GloomSynth::new(SR);
            s.set_settings(&p);
            s.note_on(69, velocity, 1);
            let (l, _) = render(&mut s, 9600);
            let peak = l.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
            l.iter().position(|x| x.abs() > 0.5 * peak).unwrap()
        };
        let (loud, soft) = (rise(1.0), rise(0.0));
        assert!(loud < 300, "40 ms / 16 = 2.5 ms: {loud}");
        assert!(soft > 700, "the full 40 ms: {soft}");

        // LFO 1 on pitch at no depth of its own, opened by velocity: only the loud voice wobbles.
        let mut p = sine_patch();
        p.lfo[0].rate_hz = 5.0;
        p.mods[0] = ModSlot {
            source: ModSource::Lfo1,
            dest: ModDest::Pitch,
            amount: 1.0 / 12.0,
        };
        p.mods[1] = ModSlot {
            source: ModSource::Velocity,
            dest: ModDest::Lfo1Depth,
            amount: -1.0,
        };
        let swing = |velocity: f32| {
            let mut s = GloomSynth::new(SR);
            s.set_settings(&p);
            s.note_on(69, velocity, 1);
            let (l, _) = render(&mut s, 48_000);
            let w: Vec<f32> = l.chunks(2400).map(pitch_hz).collect();
            w.iter().cloned().fold(0.0, f32::max) - w.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(swing(0.0) > 20.0, "{}", swing(0.0));
        assert!(swing(1.0) < 2.0, "{}", swing(1.0));
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

    #[test]
    fn bend_moves_the_pitch_by_its_semitones() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.set_expression(Expression {
            bend_semis: 12.0,
            ..Expression::default()
        });
        s.note_on(57, 1.0, 1); // A3 bent up an octave
        let (l, _) = render(&mut s, 48_000);
        let f = pitch_hz(&l);
        assert!((f - 440.0).abs() <= 1.0, "{f}");
    }

    #[test]
    fn sustain_holds_released_keys_until_the_pedal_lifts() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.set_sustain(true);
        s.note_on(60, 1.0, 1);
        s.note_on(64, 1.0, 2);
        s.note_off(60);
        let (l, _) = render(&mut s, 4800);
        assert!(rms(&l[4000..]) > 0.05, "held by the pedal");
        assert_eq!(s.active_voices(), 2);
        s.set_sustain(false);
        render(&mut s, 4800);
        // 60 was let go: it released. 64 is still down: it keeps sounding.
        assert_eq!(s.active_voices(), 1);
        s.note_off(64);
        render(&mut s, 4800);
        assert!(!s.is_active());
    }

    #[test]
    fn mod_wheel_and_aftertouch_drive_the_matrix() {
        let mut p = sine_patch();
        p.mods[0] = ModSlot {
            source: ModSource::ModWheel,
            dest: ModDest::Pitch,
            amount: 1.0,
        };
        p.mods[1] = ModSlot {
            source: ModSource::Aftertouch,
            dest: ModDest::Amp,
            amount: -1.0,
        };
        let mut s = GloomSynth::new(SR);
        s.set_settings(&p);
        s.note_on(57, 1.0, 1);
        let (l, _) = render(&mut s, 24_000);
        assert!((pitch_hz(&l) - 220.0).abs() <= 1.0);
        s.set_expression(Expression {
            mod_wheel: 1.0,
            ..Expression::default()
        });
        let (l, _) = render(&mut s, 24_000);
        assert!(
            (pitch_hz(&l[2400..]) - 440.0).abs() <= 2.0,
            "wheel up: +12 st"
        );
        // Polyphonic pressure on the key silences it through the amp row.
        s.set_key_pressure(57, 1.0);
        let (l, _) = render(&mut s, 4800);
        assert!(rms(&l[2400..]) < 1e-3);
    }

    fn sounding_pitch(s: &GloomSynth) -> f32 {
        s.voices.iter().find(|v| v.active).map_or(0.0, |v| v.pitch)
    }

    #[test]
    fn slide_glides_linearly_in_semitones_over_its_length() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.note_on(60, 1.0, 1);
        render(&mut s, 480);
        let slide = NoteExpr {
            glide: Glide::Slide { seconds: 0.5 },
            ..NoteExpr::default()
        };
        s.note_on_expr(72, 1.0, 2, slide);
        assert_eq!(s.active_voices(), 1, "a slide starts no voice");
        // Every 50 ms the pitch has moved a tenth of the way: a straight line in semitones.
        for k in 1..=10 {
            render(&mut s, 2400);
            let want = 60.0 + 12.0 * k as f32 / 10.0;
            let got = sounding_pitch(&s);
            assert!((got - want).abs() < 0.1, "{k}: {got} vs {want}");
        }
        let (l, _) = render(&mut s, 48_000);
        let f = pitch_hz(&l);
        assert!((f - 523.25).abs() <= 1.0, "{f}");
        // The voice still belongs to the note it started as.
        s.note_off(60);
        render(&mut s, 9600);
        assert!(!s.is_active());
    }

    #[test]
    fn a_slide_with_nothing_held_plays_as_a_note() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        let slide = NoteExpr {
            glide: Glide::Slide { seconds: 0.5 },
            ..NoteExpr::default()
        };
        s.note_on_expr(69, 1.0, 1, slide);
        assert_eq!(s.active_voices(), 1);
        assert_eq!(sounding_pitch(&s), 69.0);
    }

    #[test]
    fn portamento_starts_from_the_last_note() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        s.note_on(60, 1.0, 1);
        render(&mut s, 480);
        s.note_off(60);
        let porta = NoteExpr {
            glide: Glide::Porta,
            ..NoteExpr::default()
        };
        s.note_on_expr(64, 1.0, 2, porta);
        render(&mut s, 64);
        let p = s.voices.iter().find(|v| v.key == 64).unwrap().pitch;
        assert!(p > 60.0 && p < 61.0, "{p}");
        render(&mut s, (PORTA_DEFAULT_S * SR) as usize);
        let p = s.voices.iter().find(|v| v.key == 64).unwrap().pitch;
        assert!((p - 64.0).abs() < 1e-3, "{p}");
    }

    #[test]
    fn per_note_fine_pitch_and_pan_reach_the_output() {
        let mut s = GloomSynth::new(SR);
        s.set_settings(&sine_patch());
        let e = NoteExpr {
            fine: 50.0,
            pan: -1.0,
            ..NoteExpr::default()
        };
        s.note_on_expr(69, 1.0, 1, e);
        let (l, r) = render(&mut s, 48_000);
        let f = pitch_hz(&l);
        let want = 440.0 * (50.0_f32 / 1200.0).exp2();
        assert!((f - want).abs() <= 1.0, "{f} vs {want}");
        assert!(rms(&l) > 0.1 && rms(&r) < 1e-4, "{} {}", rms(&l), rms(&r));
    }

    #[test]
    fn release_velocity_scales_the_release_time() {
        let tail = |release: f32| {
            let mut s = GloomSynth::new(SR);
            let mut p = sine_patch();
            p.amp_env.release_ms = 100.0;
            s.set_settings(&p);
            let e = NoteExpr {
                release,
                ..NoteExpr::default()
            };
            s.note_on_expr(69, 1.0, 1, e);
            render(&mut s, 4800);
            s.note_off(69);
            let (l, _) = render(&mut s, 4800);
            rms(&l[2400..])
        };
        assert!(tail(0.0) > tail(0.5) && tail(0.5) > tail(1.0));
    }

    /// Energy outside the harmonics of `f0` relative to the total, in dB (Blackman-Harris
    /// window, five bins either side of each harmonic counted as the harmonic).
    fn alias_db(x: &[f32], f0: f32) -> f32 {
        use rustfft::num_complex::Complex32;
        let n = x.len();
        let mut buf: Vec<Complex32> = x
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let t = std::f32::consts::TAU * i as f32 / n as f32;
                let w = 0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos()
                    - 0.01168 * (3.0 * t).cos();
                Complex32::new(v * w, 0.0)
            })
            .collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(n)
            .process(&mut buf);
        let bin_hz = SR / n as f32;
        let (mut total, mut alias) = (0.0_f64, 0.0_f64);
        for (k, c) in buf.iter().take(n / 2).enumerate().skip(1) {
            let e = f64::from(c.norm_sqr());
            total += e;
            let f = k as f32 * bin_hz;
            let h = (f / f0).round();
            if h < 1.0 || (f - h * f0).abs() > 5.0 * bin_hz {
                alias += e;
            }
        }
        (10.0 * (alias / total).log10()) as f32
    }

    fn wt_patch() -> SynthSettings {
        let mut s = sine_patch();
        s.engine = Engine::Wavetable;
        s.wt[0] = WtOscSettings {
            table: 0,
            // Frame 10 of 16 in "Basic Shapes" is the saw.
            position: 10.0 / 15.0,
            ..WtOscSettings::default()
        };
        s
    }

    #[test]
    fn wavetable_high_notes_do_not_alias() {
        let mut s = GloomSynth::new(SR);
        s.set_tables(Some(Arc::new(SynthTables::factory())));
        s.set_settings(&wt_patch());
        // Key 100 is 2637 Hz: only 9 harmonics fit under Nyquist.
        s.note_on(100, 1.0, 1);
        render(&mut s, 4800);
        let (l, _) = render(&mut s, 32_768);
        let f0 = 440.0 * 2f32.powf(31.0 / 12.0);
        let db = alias_db(&l, f0);
        assert!(db < -70.0, "{db} dB");
        // The same saw read from the full-band mip aliases badly: the test can see it.
        let bank = wavetable::factory_bank();
        let inc = f0 / SR;
        let naive: Vec<f32> = (0..32_768)
            .map(|i| bank.tables[0].read(10.0 / 15.0, (i as f32 * inc).fract(), 0))
            .collect();
        assert!(alias_db(&naive, f0) > -40.0);
    }

    #[test]
    fn wavetable_position_and_warp_follow_the_matrix() {
        let mut p = wt_patch();
        p.wt[0].position = 0.0;
        p.mods[0] = ModSlot {
            source: ModSource::Velocity,
            dest: ModDest::Wt1Pos,
            amount: 1.0,
        };
        p.wt[0].warp = Warp::Sync;
        p.mods[1] = ModSlot {
            source: ModSource::Velocity,
            dest: ModDest::Wt1Warp,
            amount: 0.5,
        };
        let mut s = GloomSynth::new(SR);
        s.set_tables(Some(Arc::new(SynthTables::factory())));
        s.set_settings(&p);
        s.note_on(60, 0.5, 1);
        render(&mut s, 64);
        let v = s.voices.iter().find(|v| v.active).unwrap();
        assert!((v.wt_pos[0] - 0.5).abs() < 1e-6);
        assert!((v.wt_warp[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn fm_engine_plays_and_ends_with_its_carriers() {
        let mut p = sine_patch();
        p.engine = Engine::Fm;
        p.amp_env.release_ms = 5000.0;
        for op in &mut p.fm.ops {
            op.env = EnvSettings {
                attack_ms: 0.0,
                decay_ms: 50.0,
                sustain: 0.0,
                release_ms: 10.0,
            };
        }
        let mut s = GloomSynth::new(SR);
        s.set_settings(&p);
        s.note_on(60, 1.0, 1);
        let (l, _) = render(&mut s, 2400);
        assert!(rms(&l[..480]) > 0.05);
        // The carrier decays to nothing well before the amp envelope would.
        render(&mut s, 48_000);
        assert!(!s.is_active());
    }

    fn bass_patch() -> SynthSettings {
        let mut p = sine_patch();
        p.engine = Engine::Bass;
        p.velocity_sensitivity = 0.0;
        p.amp_env.sustain = 1.0;
        p.amp_env.release_ms = 20.0;
        p
    }

    /// Brightness: RMS of the first difference over RMS (rises with the high partials).
    fn brightness(x: &[f32]) -> f32 {
        let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&d) / rms(x).max(1e-9)
    }

    #[test]
    fn bass_slides_overlapping_notes_without_retriggering() {
        let mut s = GloomSynth::new(SR);
        let mut p = bass_patch();
        p.bass.slide_ms = 100.0;
        s.set_settings(&p);
        s.note_on(48, 0.5, 1);
        render(&mut s, 9600);
        let level = s.voices[0].amp.level();
        // The next note starts while the first is held: one voice glides there.
        s.note_on(60, 0.5, 2);
        s.note_off(48);
        assert_eq!(s.active_voices(), 1);
        assert_eq!(s.voices[0].amp.level(), level, "no retrigger");
        let (l, _) = render(&mut s, 9600);
        let start = pitch_hz(&l[..1200]);
        let end = pitch_hz(&l[7200..]);
        assert!(start < 200.0, "starts near C3: {start}");
        assert!((end - 261.6).abs() < 6.0, "ends on C4: {end}");
        // The first note's late note-off did not end it; its own does.
        assert!(s.voices[0].held);
        s.note_off(60);
        assert!(!s.voices[0].held);

        // Notes that do not overlap restart at their own pitch at once.
        let mut s = GloomSynth::new(SR);
        s.set_settings(&p);
        s.note_on(48, 0.5, 1);
        render(&mut s, 4800);
        s.note_off(48);
        s.note_on(60, 0.5, 2);
        let (l, _) = render(&mut s, 4800);
        assert!((pitch_hz(&l[..2400]) - 261.6).abs() < 12.0);
    }

    #[test]
    fn bass_accents_by_velocity_and_builds_on_consecutive_accents() {
        let mut p = bass_patch();
        p.osc[0].wave = Wave::Saw;
        p.cutoff_hz = 300.0;
        p.resonance = 0.5;
        p.filter_env_octaves = 1.0;
        p.mod_env = EnvSettings {
            attack_ms: 0.0,
            decay_ms: 200.0,
            sustain: 0.0,
            release_ms: 50.0,
        };
        p.bass.accent = 1.0;
        let play = |velocities: &[f32]| {
            let mut s = GloomSynth::new(SR);
            s.set_settings(&p);
            let mut out = Vec::new();
            for (i, &v) in velocities.iter().enumerate() {
                s.note_on(36, v, i as u64 + 1);
                let (l, _) = render(&mut s, 2400);
                s.note_off(36);
                let (l2, _) = render(&mut s, 2400);
                out.push((rms(&l[..1200]), brightness(&l[..1200])));
                let _ = l2;
            }
            out
        };
        // Below the accent velocity, velocity changes nothing.
        assert_eq!(play(&[0.3]), play(&[0.7]));
        let plain = play(&[0.5])[0];
        let accented = play(&[1.0])[0];
        assert!(accented.0 > plain.0 * 1.3, "louder: {accented:?} {plain:?}");
        assert!(
            accented.1 > plain.1 * 1.1,
            "brighter: {accented:?} {plain:?}"
        );
        // A second accent right after the first opens the filter further.
        let two = play(&[1.0, 1.0]);
        assert!(two[1].1 > two[0].1, "{two:?}");
    }
}
