//! The audio-thread half of the engine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::{Tick, MAX_CHANNELS, ROOT_KEY};
use gt_dsp::{db_to_gain, Click, LinearRamp, NoteExpr, SineOsc};
use rtrb::{Consumer, Producer};

use crate::channel::ChannelSlot;
use crate::command::{EngineCommand, EngineEvent, Garbage, TransportState};
use crate::control::{beats_of, ModClock, ModPlan, ParamDest};
use crate::denormal::DenormalGuard;
use crate::input::InputEngine;
use crate::live::{clock_ns, LiveEvent, LiveKind, LiveRoutes, KEYBOARD, ROUTE_PORTS};
use crate::mixer::MixerEngine;
use crate::plugin::{PluginContext, PluginTable};
use crate::song::{ChannelParams, SongSnapshot};
use crate::transport::{EventBuf, EventKind, ScheduledEvent, Transport};
use crate::{EngineConfig, Telemetry, FADE_SECONDS, RENDER_QUANTUM, SCOPE_LEN};

/// Most MIDI messages forwarded from plugins to other channels per block.
const MIDI_FORWARD: usize = 1024;

/// Frequency of the device test tone (concert A).
pub const TEST_TONE_HZ: f32 = 440.0;
/// Level of the device test tone. -12 dBFS leaves headroom and is comfortable on speakers.
pub const TEST_TONE_DBFS: f32 = -12.0;
/// Linear gain of the sample-browser preview voice.
pub const PREVIEW_GAIN: f32 = 0.7;

/// Metronome levels and pitches. The downbeat is higher so bar starts are audible.
const CLICK_DBFS: f32 = -9.0;
const CLICK_HZ: f32 = 1000.0;
const CLICK_DOWNBEAT_HZ: f32 = 1600.0;

/// Audio clips fade in and out over this time at their edges, so a cut never clicks.
const CLIP_FADE_S: f64 = 0.002;

/// Commands applied per quantum at most, so a flood of commands cannot stall a callback.
const MAX_COMMANDS_PER_QUANTUM: usize = 64;
/// Live events taken from the queue per quantum at most.
const MAX_LIVE_PER_QUANTUM: usize = 64;
/// Live events waiting for their frame (a device period's worth at most).
const LIVE_WAIT: usize = 256;
/// Held-note sources: the routable ports, then one for the keyboard and every other port.
const LIVE_SOURCES: usize = ROUTE_PORTS + 1;

/// A count-in in progress: clicks on every beat from `start`, then play at `end`.
#[derive(Debug, Clone, Copy)]
struct CountIn {
    /// Engine frame of the first click.
    start: u64,
    /// Engine frame where playback starts (a quantum boundary).
    end: u64,
    /// Frames per beat.
    beat_frames: f64,
    /// Clicks so far, and in total.
    done: u32,
    beats: u32,
    /// Beats per bar (the downbeat clicks higher).
    per_bar: u32,
}

const Q: usize = RENDER_QUANTUM;

/// Renders audio. Lives on the audio thread; every method here is real-time safe:
/// no allocation, no locks, no I/O, no panics for any buffer length.
pub struct AudioProcessor {
    telemetry: Arc<Telemetry>,
    commands: Consumer<EngineCommand>,
    events: Producer<EngineEvent>,
    garbage: Producer<Garbage>,
    live: Consumer<LiveEvent>,
    out_channels: usize,
    sample_rate: f64,

    transport: Transport,
    scheduled: EventBuf,
    /// Engine frame at the start of the next quantum.
    frame_clock: u64,

    /// The current quantum, stereo (planar).
    quantum_l: [f32; Q],
    quantum_r: [f32; Q],
    /// Read position inside the quantum; `Q` means "render the next one".
    fifo_pos: usize,
    /// Mono scratch for the metronome and test tone.
    mono: [f32; Q],
    /// Per-channel scratch, reused for each channel in turn.
    scratch_l: [f32; Q],
    /// Live stretch output of one clip for one quantum (Prompt 15 part 2).
    live_l: [f32; Q],
    live_r: [f32; Q],
    scratch_r: [f32; Q],

    song: Option<Box<SongSnapshot>>,
    /// Song tick where the last control step ended (NaN when stopped).
    control_tick: f64,
    /// Song time where audio clips continue next quantum (anything else is a jump).
    clip_resume_s: f64,
    /// Song time of the last jump (start, locate, wrap) for the clips' de-click.
    clip_jump_s: f64,
    /// Offline render of one strip (`set_tap`): the strip replaces the output.
    tapped: bool,
    /// Modulators, grouped by parameter.
    mods: Option<Box<ModPlan>>,
    /// `MAX_CHANNELS` slots, allocated once in `new`.
    channels: Vec<ChannelSlot>,
    /// Per channel slot: the delay lining a plugin instrument up with the slowest one on its
    /// strip (D115).
    channel_pdc: Vec<crate::mixer::PdcDelay>,
    /// MIDI plugins sent to other channels (target slot, offset, message), played in the next
    /// block. Capacity fixed in `new`.
    midi_fwd: Vec<(u16, u32, [u8; 3])>,
    preview: ChannelSlot,
    /// Increments with every note-on; orders voices for stealing.
    voice_age: u64,
    /// Channel slot that feeds the oscilloscope.
    scope_slot: Option<usize>,
    /// Layers, key settings and choke groups between scheduling and rendering.
    router: crate::route::NoteRouter,
    /// Mixer strips, effects and routing.
    mixer: Box<MixerEngine>,
    /// Hosted plugins (instruments and effects).
    plugins: Box<PluginTable>,

    click: Click,
    click_gain: f32,
    metronome: bool,
    count_in: Option<CountIn>,

    /// Slot that live input plays unless routed elsewhere.
    live_slot: Option<u16>,
    /// Where each input port and MIDI channel plays.
    live_routes: Box<LiveRoutes>,
    /// Parameters being recorded: their lanes do not play.
    writing: Vec<ParamDest>,
    /// Per source, MIDI channel and key, the slot + 1 holding a live note (0: none).
    live_held: Box<[u16]>,
    /// Live events waiting for their frame: (engine frame, event), in arrival order.
    live_wait: Box<[(u64, LiveEvent); LIVE_WAIT]>,
    live_wait_len: usize,
    /// The current device callback: [`clock_ns`] at its start, its frames, and the engine
    /// frame it starts to output.
    cb_ns: u64,
    cb_frames: u64,
    cb_out: u64,

    tone: SineOsc,
    tone_ramp: LinearRamp,
    tone_gain: f32,
    tone_on: bool,

    master: LinearRamp,
    fade_frames: u32,

    /// Device input: monitoring, recording, latency measurement.
    input: Box<InputEngine>,
}

impl std::fmt::Debug for AudioProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioProcessor")
            .field("out_channels", &self.out_channels)
            .field("frame_clock", &self.frame_clock)
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl AudioProcessor {
    pub(crate) fn new(
        config: EngineConfig,
        telemetry: Arc<Telemetry>,
        commands: Consumer<EngineCommand>,
        events: Producer<EngineEvent>,
        garbage: Producer<Garbage>,
        live: Consumer<LiveEvent>,
    ) -> Self {
        let sr = config.sample_rate.max(1) as f32;
        let mut preview = ChannelSlot::new(sr);
        preview.set_params_now(ChannelParams {
            gain: PREVIEW_GAIN,
            ..ChannelParams::default()
        });
        Self {
            telemetry,
            commands,
            events,
            garbage,
            live,
            out_channels: config.out_channels.max(1),
            sample_rate: f64::from(config.sample_rate.max(1)),
            transport: Transport::new(config.sample_rate, Box::default()),
            scheduled: EventBuf::default(),
            frame_clock: 0,
            quantum_l: [0.0; Q],
            quantum_r: [0.0; Q],
            fifo_pos: Q,
            mono: [0.0; Q],
            scratch_l: [0.0; Q],
            live_l: [0.0; Q],
            live_r: [0.0; Q],
            scratch_r: [0.0; Q],
            song: None,
            control_tick: f64::NAN,
            clip_resume_s: f64::NEG_INFINITY,
            clip_jump_s: f64::NEG_INFINITY,
            tapped: false,
            mods: None,
            channels: (0..MAX_CHANNELS).map(|_| ChannelSlot::new(sr)).collect(),
            channel_pdc: (0..MAX_CHANNELS)
                .map(|_| crate::mixer::PdcDelay::new())
                .collect(),
            midi_fwd: Vec::with_capacity(MIDI_FORWARD),
            preview,
            voice_age: 0,
            scope_slot: None,
            router: crate::route::NoteRouter::new(),
            mixer: Box::new(MixerEngine::new(sr)),
            plugins: Box::new(PluginTable::new()),
            click: Click::new(sr),
            click_gain: db_to_gain(CLICK_DBFS),
            metronome: true,
            count_in: None,
            live_slot: None,
            live_routes: Box::default(),
            writing: Vec::new(),
            live_held: vec![0; LIVE_SOURCES * 16 * 128].into_boxed_slice(),
            live_wait: Box::new([(0, LiveEvent::from(crate::LiveNote::off(0))); LIVE_WAIT]),
            live_wait_len: 0,
            cb_ns: 0,
            cb_frames: 0,
            cb_out: 0,
            tone: SineOsc::new(sr, TEST_TONE_HZ),
            tone_ramp: LinearRamp::new(0.0),
            tone_gain: db_to_gain(TEST_TONE_DBFS),
            tone_on: false,
            master: LinearRamp::new(1.0),
            fade_frames: (sr * FADE_SECONDS).round() as u32,
            input: Box::new(InputEngine::new()),
        }
    }

    /// Takes every plugin out of the engine, stopped (not real-time safe: for offline renders,
    /// which reuse the plugins for the next pass).
    pub fn take_plugins(&mut self) -> Vec<Box<crate::PluginBox>> {
        let telemetry = Arc::clone(&self.telemetry);
        (0..crate::MAX_PLUGINS)
            .filter_map(|i| self.plugins.set(i, None, &telemetry))
            .collect()
    }

    /// Offline renders: outputs the signal of strip `strip` at `point` instead of the master
    /// (render to audio, consolidate, freeze; D105). `None` restores the master output. Not
    /// for a running device stream: call it before or between offline passes.
    pub fn set_tap(&mut self, tap: Option<(u16, crate::Tap)>) {
        self.tapped = tap.is_some();
        self.mixer.set_tap(tap.map(|(s, p)| (usize::from(s), p)));
    }

    /// Frames by which delay compensation holds back what clips and channels send into
    /// `strip` (so a render taken at its [`crate::Tap::Input`] lags the timeline by this much).
    pub fn strip_input_delay(&self, strip: u16) -> usize {
        self.mixer.input_delay(usize::from(strip))
    }

    /// How late `strip`'s output is against the timeline (its delay compensation plus its
    /// effects' latency), in frames.
    pub fn strip_output_latency(&self, strip: u16) -> usize {
        self.mixer.output_latency(usize::from(strip))
    }

    /// Total latency of the mix, in frames.
    pub fn mix_latency(&self) -> usize {
        self.mixer.latency()
    }

    /// Number of strips the mixer has.
    pub(crate) fn mixer_capacity(&self) -> usize {
        self.mixer.capacity()
    }

    /// The telemetry this processor publishes (also visible through the `EngineHandle`).
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Fills `out`, an interleaved buffer of `frames * out_channels` samples, from the stream of
    /// fixed-size quanta. Left and right go to the first two device channels (further channels
    /// get silence); a mono device gets their average. A trailing partial frame (which a correct
    /// device never delivers) is zeroed.
    pub fn process(&mut self, out: &mut [f32]) {
        self.process_at(out, clock_ns());
    }

    /// [`Self::process`] for a callback that started at `now_ns` on the [`clock_ns`] clock,
    /// which places live events within it (tests pass their own times).
    pub fn process_at(&mut self, out: &mut [f32], now_ns: u64) {
        // Subnormals flush to zero while rendering, on the device thread and in export alike.
        let _ftz = DenormalGuard::new();
        let ch = self.out_channels;
        let frames = out.len() / ch;
        self.cb_ns = now_ns;
        self.cb_frames = frames as u64;
        self.cb_out = self.frame_clock - (Q - self.fifo_pos) as u64;
        let mut peak = 0.0_f32;
        let mut written = 0;
        while written < frames {
            if self.fifo_pos == Q {
                self.render_quantum();
                self.fifo_pos = 0;
            }
            let n = (Q - self.fifo_pos).min(frames - written);
            let range = self.fifo_pos..self.fifo_pos + n;
            let src = self.quantum_l[range.clone()]
                .iter()
                .zip(&self.quantum_r[range]);
            let dst = &mut out[written * ch..(written + n) * ch];
            for (frame, (&l, &r)) in dst.chunks_exact_mut(ch).zip(src) {
                if let [mono] = frame {
                    *mono = 0.5 * (l + r);
                } else {
                    frame[0] = l;
                    frame[1] = r;
                    frame[2..].fill(0.0);
                }
                peak = peak.max(l.abs()).max(r.abs());
            }
            self.fifo_pos += n;
            written += n;
        }
        out[frames * ch..].fill(0.0);

        self.publish_anchor();
        let t = &*self.telemetry;
        t.last_block_frames.store(frames as u32, Ordering::Relaxed);
        t.blocks.fetch_add(1, Ordering::Relaxed);
        t.peak.fetch_max(peak, Ordering::Relaxed);
    }

    /// Renders the next `RENDER_QUANTUM` frames into the quantum buffers.
    fn render_quantum(&mut self) {
        self.apply_commands();
        let count_click = self.advance_count_in();
        self.input.read(&self.telemetry);

        self.quantum_l.fill(0.0);
        self.quantum_r.fill(0.0);
        self.mono.fill(0.0);
        self.transport.schedule(
            self.frame_clock,
            Q as u32,
            self.song.as_deref(),
            &mut self.scheduled,
        );
        self.apply_live();
        self.router.route(&mut self.scheduled, &self.channels);
        // Recorded controllers, at once for built-in instruments (within a quantum).
        for i in 0..self.scheduled.as_slice().len() {
            if let EventKind::Control { slot, kind, value } = self.scheduled.as_slice()[i].kind {
                let offset = self.scheduled.as_slice()[i].offset;
                self.slot_expression(
                    usize::from(slot),
                    crate::live::control_kind(kind, value),
                    offset,
                );
            }
        }

        // Metronome, sample-accurate: render up to each beat, trigger, continue.
        let mut cursor = 0;
        for e in self.scheduled.as_slice() {
            let EventKind::Beat { downbeat } = e.kind else {
                continue;
            };
            let at = (e.offset as usize).min(Q);
            self.click.add_to(&mut self.mono[cursor..at]);
            cursor = at;
            if self.metronome {
                let hz = if downbeat {
                    CLICK_DOWNBEAT_HZ
                } else {
                    CLICK_HZ
                };
                self.click.trigger(hz, self.click_gain);
            }
        }
        if let Some((offset, downbeat)) = count_click {
            // While counting in the transport is stopped, so no beat events came above.
            let at = offset.min(Q);
            self.click.add_to(&mut self.mono[cursor..at]);
            cursor = at;
            let hz = if downbeat {
                CLICK_DOWNBEAT_HZ
            } else {
                CLICK_HZ
            };
            self.click.trigger(hz, self.click_gain);
        }
        self.click.add_to(&mut self.mono[cursor..]);
        self.input
            .calibrate(&mut self.mono, self.sample_rate as f32);
        let playing = self.transport.state() == TransportState::Playing;
        self.input.record(
            playing,
            self.scheduled.segments(),
            &self.transport,
            self.sample_rate,
            &self.telemetry,
        );

        self.apply_controls();
        let pctx = self.plugin_context();
        self.update_instrument_latency();
        self.deliver_midi();
        for slot in 0..self.channels.len() {
            self.render_channel(slot, &pctx);
        }
        self.render_audio_clips();
        self.input.monitor(&mut self.mixer, playing);
        let bpm = self.transport.bpm_at(self.frame_clock) as f32;
        self.mixer.process(
            &mut self.scratch_l,
            &mut self.scratch_r,
            bpm,
            &mut self.plugins,
            &pctx,
            &self.telemetry,
        );
        self.quantum_l.copy_from_slice(&self.scratch_l);
        self.quantum_r.copy_from_slice(&self.scratch_r);
        if self.tapped {
            // Offline render of one strip: nothing else joins the output.
            self.frame_clock += Q as u64;
            self.publish();
            return;
        }
        if self.preview.is_active() {
            self.scratch_l.fill(0.0);
            self.scratch_r.fill(0.0);
            self.preview
                .render_voices(&mut self.scratch_l, &mut self.scratch_r);
            self.preview.mix_into(
                (&self.scratch_l, &self.scratch_r),
                (&mut self.quantum_l, &mut self.quantum_r),
            );
        }

        let tone_idle = !self.tone_on && self.tone_ramp.is_settled();
        for i in 0..Q {
            let mut m = self.mono[i];
            if !tone_idle {
                m += self.tone.next_sample() * self.tone_ramp.next_value();
            }
            let g = self.master.next_value();
            self.quantum_l[i] = (self.quantum_l[i] + m) * g;
            self.quantum_r[i] = (self.quantum_r[i] + m) * g;
        }

        self.frame_clock += Q as u64;
        self.publish();
    }

    /// Musical context of the quantum about to render, for plugins.
    fn plugin_context(&self) -> PluginContext {
        let now = self.frame_clock;
        let pos = self.transport.position_at(now).max(0.0);
        let sigs = self.transport.signatures();
        let tick = pos.floor() as i64;
        let bar = sigs.bar_of(tick);
        let sig = sigs.sig_at(Tick(tick));
        let ppq = gt_core::PPQ as f64;
        PluginContext {
            bpm: self.transport.bpm_at(now),
            playing: self.transport.state() == TransportState::Playing,
            beats: pos / ppq,
            bar_start_beats: sigs.bar_start(bar) as f64 / ppq,
            bar: i32::try_from(bar).unwrap_or(0),
            sig_num: u16::from(sig.num),
            sig_den: u16::from(sig.den),
            steady_frames: now,
        }
    }

    /// Instrument delay compensation (D115): each strip learns the largest latency of the
    /// running plugin instruments routed to it, and each of those channels is delayed by the
    /// difference to that. Cheap enough to run every quantum (one pass over the channels).
    fn update_instrument_latency(&mut self) {
        let mut lat = [0_usize; gt_core::STRIPS];
        for ch in self.channels.iter().filter(|c| c.is_plugin()) {
            let l = self.plugins.latency(ch.plugin);
            let s = usize::from(ch.route()).min(gt_core::STRIPS - 1);
            lat[s] = lat[s].max(l);
            for k in 0..self.plugins.extra_outputs(ch.plugin) {
                if let Some(s) = ch.extra_routes()[k] {
                    let s = usize::from(s).min(gt_core::STRIPS - 1);
                    lat[s] = lat[s].max(l);
                }
            }
        }
        if self.mixer.set_instrument_latency(&lat) {
            self.telemetry
                .latency_frames
                .store(self.mixer.latency() as u32, Ordering::Relaxed);
        }
    }

    /// Plays the MIDI plugins sent to other channels in the last block, at the same offsets.
    fn deliver_midi(&mut self) {
        for i in 0..self.midi_fwd.len() {
            let (target, offset, msg) = self.midi_fwd[i];
            let slot = usize::from(target);
            let Some(ch) = self.channels.get(slot) else {
                continue;
            };
            let (status, key, value) = (msg[0] & 0xF0, msg[1] & 0x7F, msg[2] & 0x7F);
            let note_on = status == 0x90 && value > 0;
            let note_off = status == 0x80 || (status == 0x90 && value == 0);
            if ch.is_plugin() {
                let index = ch.plugin;
                if note_on {
                    self.plugins
                        .note_on(index, offset, key, f32::from(value) / 127.0);
                } else if note_off {
                    self.plugins.note_off(index, offset, key);
                } else if status == 0xA0 {
                    self.plugins
                        .key_pressure(index, offset, key, f32::from(value) / 127.0);
                } else if matches!(status, 0xB0 | 0xD0 | 0xE0) {
                    self.plugins.midi(index, offset, msg);
                }
            } else if note_on {
                self.slot_note_on(slot, key, f32::from(value) / 127.0);
            } else if note_off {
                self.slot_note_off(slot, key);
            }
        }
        self.midi_fwd.clear();
    }

    /// Renders a plugin channel: its notes go to the plugin at their offsets, one block is
    /// processed, then gain and pan as for any channel.
    fn render_plugin_channel(&mut self, slot: usize, pctx: &PluginContext) {
        let index = self.channels[slot].plugin;
        if !self.plugins.is_live(index) {
            if self.scope_slot == Some(slot) {
                self.scratch_l.fill(0.0);
                self.write_scope();
            }
            return;
        }
        for e in self.scheduled.as_slice() {
            match e.kind {
                EventKind::NoteOn {
                    slot: s,
                    key,
                    velocity,
                    expr,
                }
                | EventKind::Chase {
                    slot: s,
                    key,
                    velocity,
                    held: true,
                    expr,
                    ..
                } if usize::from(s) == slot => {
                    self.plugins
                        .note_on_expr(index, e.offset, key, velocity, &expr);
                }
                EventKind::NoteOff { slot: s, key } if usize::from(s) == slot => {
                    self.plugins.note_off(index, e.offset, key);
                }
                EventKind::Wrap => self.plugins.release_all(index, e.offset),
                EventKind::Choke { slot: s } if usize::from(s) == slot => {
                    self.plugins.release_all(index, e.offset);
                }
                _ => {}
            }
        }
        self.plugins.run_instrument(
            index,
            &mut self.scratch_l,
            &mut self.scratch_r,
            pctx,
            &self.telemetry,
        );
        let ch = &mut self.channels[slot];
        let route = usize::from(ch.route());
        let own = self.plugins.latency(index);
        let pdc = &mut self.channel_pdc[slot];
        pdc.set_delay(self.mixer.instrument_latency(route).saturating_sub(own));
        pdc.process(&mut self.scratch_l, &mut self.scratch_r);
        let (ml, mr) = self.mixer.latent_mut(route);
        let peak = ch.mix_into((&self.scratch_l, &self.scratch_r), (ml, mr));
        // Extra outputs of a multi-output instrument, each into its own insert.
        let gain = ch.extra_gain();
        for k in 0..self.plugins.extra_outputs(index) {
            let (Some(s), Some((a, b))) =
                (ch.extra_routes()[k], self.plugins.extra_output(index, k))
            else {
                continue;
            };
            let (ml, mr) = self.mixer.latent_mut(usize::from(s));
            for (((m, &x), n), &y) in ml.iter_mut().zip(a).zip(mr.iter_mut()).zip(b) {
                *m += x * gain;
                *n += y * gain;
            }
        }
        // MIDI the plugin sent (an arpeggiator) goes to its target channel next block.
        if let Some(target) = ch.midi_out() {
            let fwd = &mut self.midi_fwd;
            self.plugins.midi_out(index, &mut |offset, msg| {
                if fwd.len() < fwd.capacity() {
                    fwd.push((target, offset, msg));
                }
            });
        }
        self.telemetry.channel_peaks[slot].fetch_max(peak, Ordering::Relaxed);
        if self.scope_slot == Some(slot) {
            for (l, r) in self.scratch_l.iter_mut().zip(&self.scratch_r) {
                *l = 0.5 * (*l + r);
            }
            self.write_scope();
        }
    }

    /// Starts a note on a channel slot now (UI audition, live input).
    fn slot_note_on(&mut self, slot: usize, key: u8, velocity: f32) {
        let Some(ch) = self.channels.get_mut(slot) else {
            return;
        };
        let p = *ch.params();
        if p.kind == crate::song::InstrumentKind::Layer {
            // Every child that answers the velocity (layers of layers never exist).
            let n = usize::from(p.layer.count);
            for i in gt_core::rack::layer_candidates(n, p.layer.split, velocity) {
                self.slot_note_on(usize::from(p.layer.children[i]), key, velocity);
            }
            return;
        }
        let key = match p.keys.apply.then_some(p.keys.limits) {
            Some(k) if key < k.lo || key > k.hi => return,
            Some(k) => {
                (i16::from(key) + i16::from(ROOT_KEY) - i16::from(k.root)).clamp(0, 127) as u8
            }
            None => key,
        };
        if p.choke > 0 {
            for (other, c) in self.channels.iter_mut().enumerate() {
                if other != slot && c.params().choke == p.choke {
                    if c.is_plugin() {
                        self.plugins.release_all(c.plugin, 0);
                    } else {
                        c.choke();
                    }
                }
            }
        }
        let ch = &mut self.channels[slot];
        if ch.is_plugin() {
            self.plugins.note_on(ch.plugin, 0, key, velocity);
        } else {
            self.voice_age += 1;
            ch.note_on(key, velocity, self.voice_age);
        }
    }

    /// Releases a note on a channel slot now.
    fn slot_note_off(&mut self, slot: usize, key: u8) {
        let Some(ch) = self.channels.get_mut(slot) else {
            return;
        };
        let p = *ch.params();
        if p.kind == crate::song::InstrumentKind::Layer {
            for &c in &p.layer.children[..usize::from(p.layer.count)] {
                self.slot_note_off(usize::from(c), key);
            }
            return;
        }
        let key = match p.keys.apply.then_some(p.keys.limits) {
            Some(k) => {
                (i16::from(key) + i16::from(ROOT_KEY) - i16::from(k.root)).clamp(0, 127) as u8
            }
            None => key,
        };
        let ch = &mut self.channels[slot];
        if ch.is_plugin() {
            self.plugins.note_off(ch.plugin, 0, key);
        } else {
            ch.note_off(key);
        }
    }

    /// Points every plugin channel and effect slot at the table entry running its plugin.
    fn resolve_plugins(&mut self) {
        for ch in &mut self.channels {
            ch.plugin = ch.plugin_instance().and_then(|id| self.plugins.find(id));
        }
        self.mixer.resolve_plugins(&self.plugins);
        self.telemetry
            .latency_frames
            .store(self.mixer.latency() as u32, Ordering::Relaxed);
    }

    /// Renders one channel: its voices split at the channel's note events (sample-accurate),
    /// then gain and pan, mixed into the quantum. Idle channels cost one scan of the events.
    fn render_channel(&mut self, slot: usize, pctx: &PluginContext) {
        if self.channels[slot].is_plugin() {
            self.render_plugin_channel(slot, pctx);
            return;
        }
        let touches = |e: &&ScheduledEvent| match e.kind {
            EventKind::Wrap => true,
            k => k.slot() == Some(slot as u16),
        };
        let events = self.scheduled.as_slice();
        let ch = &mut self.channels[slot];
        if !ch.is_active() && !events.iter().any(|e| touches(&e)) {
            if self.scope_slot == Some(slot) {
                self.scratch_l.fill(0.0);
                self.write_scope();
            }
            return;
        }
        let (sl, sr) = (&mut self.scratch_l, &mut self.scratch_r);
        sl.fill(0.0);
        sr.fill(0.0);
        let mut cursor = 0;
        for e in events.iter().filter(touches) {
            let at = (e.offset as usize).min(Q);
            ch.render_voices_at(&mut sl[cursor..at], &mut sr[cursor..at], Some(cursor));
            cursor = at;
            match e.kind {
                EventKind::NoteOn {
                    key,
                    velocity,
                    expr,
                    ..
                } => {
                    self.voice_age += 1;
                    ch.note_on_expr(key, velocity, self.voice_age, expr);
                }
                EventKind::Chase {
                    key,
                    velocity,
                    into_s,
                    held,
                    expr,
                    ..
                } => {
                    self.voice_age += 1;
                    ch.chase(key, velocity, into_s, held, self.voice_age, expr);
                }
                EventKind::NoteOff { key, .. } => ch.note_off(key),
                EventKind::Wrap => ch.release_all(),
                EventKind::Choke { .. } => ch.choke(),
                EventKind::Beat { .. } | EventKind::Control { .. } => {}
            }
        }
        ch.render_voices_at(&mut sl[cursor..], &mut sr[cursor..], Some(cursor));
        let (ml, mr) = self.mixer.direct_mut(usize::from(ch.route()));
        let mut peak = ch.mix_into((sl, sr), (ml, mr));
        // Pads with their own output (Prompt 23), each into its insert.
        if !ch.aux_routes().is_empty() {
            let mixer = &mut self.mixer;
            ch.drain_aux(|route, l, r, gain| {
                let (ml, mr) = mixer.direct_mut(usize::from(route).min(gt_core::STRIPS - 1));
                for (((m, &x), n), &y) in ml.iter_mut().zip(l).zip(mr.iter_mut()).zip(r) {
                    *m += x * gain;
                    *n += y * gain;
                    peak = peak.max((x * gain).abs()).max((y * gain).abs());
                }
            });
        }
        self.telemetry.channel_peaks[slot].fetch_max(peak, Ordering::Relaxed);
        if self.scope_slot == Some(slot) {
            for (l, r) in self.scratch_l.iter_mut().zip(&self.scratch_r) {
                *l = 0.5 * (*l + r);
            }
            self.write_scope();
        }
    }

    /// Adds the audio clips sounding in this quantum to their strips. Each stretch of
    /// continuous playback maps engine frames to song seconds linearly, so a clip's source
    /// position is `(song time - origin) · sample rate`, read with linear interpolation.
    fn render_audio_clips(&mut self) {
        if self.scheduled.segments().is_empty() {
            self.clip_resume_s = f64::NEG_INFINITY;
        }
        let Some(song) = self.song.as_deref_mut() else {
            return;
        };
        if song.audio.is_empty() {
            return;
        }
        let sr = self.sample_rate;
        for seg in self.scheduled.segments() {
            let a = seg.seconds;
            let n = (seg.frames as usize).min(Q - (seg.offset as usize).min(Q));
            let b = a + n as f64 / sr;
            // Playback started, located or wrapped here: clips already under way fade in
            // over the de-click time instead of starting mid-waveform.
            if (a - self.clip_resume_s).abs() * sr > 0.5 {
                self.clip_jump_s = a;
            }
            self.clip_resume_s = b;
            let jump = self.clip_jump_s;
            for clip in &mut song.audio {
                if clip.start_s >= b {
                    break;
                }
                if clip.end_s <= a {
                    continue;
                }
                let i0 = ((clip.start_s - a) * sr).ceil().max(0.0) as usize;
                let i1 = (((clip.end_s - a) * sr).ceil().max(0.0) as usize).min(n);
                let rate = f64::from(clip.sample.sample_rate.max(1));
                let (src_l, src_r) = (clip.sample.left(), clip.sample.right());
                let len = src_l.len().min(src_r.len());
                let declick_jump = clip.start_s < jump;
                let (dl, dr) = if clip.frozen {
                    self.mixer.frozen_mut(usize::from(clip.route))
                } else {
                    self.mixer.direct_mut(usize::from(clip.route))
                };
                // A clip stretching live renders its frames for this segment in one go.
                let mut live_at = None;
                if let Some(live) = clip.live.as_mut() {
                    let first = (i0..i1).find(|&i| a + i as f64 / sr >= clip.origin_s);
                    if let Some(first) = first {
                        let at = ((a + first as f64 / sr - clip.origin_s) * rate).round() as usize;
                        let m = (i1 - first).min(live.frames.saturating_sub(at));
                        live.fill(
                            &clip.sample,
                            at,
                            &mut self.live_l[..m],
                            &mut self.live_r[..m],
                        );
                        live_at = Some((first, first + m));
                    } else {
                        continue;
                    }
                }
                for i in i0..i1.min(n) {
                    let t = a + i as f64 / sr;
                    let (vl, vr) = if let Some((first, end)) = live_at {
                        if i < first {
                            continue;
                        }
                        if i >= end {
                            break;
                        }
                        (self.live_l[i - first], self.live_r[i - first])
                    } else {
                        let mut pos = (t - clip.origin_s) * rate * clip.speed;
                        if pos < 0.0 {
                            continue;
                        }
                        if clip.reverse {
                            pos = (len as f64 - 1.0) - pos;
                            if pos < 0.0 {
                                break;
                            }
                        }
                        let k = pos as usize;
                        if k >= len {
                            break;
                        }
                        let frac = (pos - k as f64) as f32;
                        let k1 = (k + 1).min(len - 1);
                        (
                            src_l[k] + (src_l[k1] - src_l[k]) * frac,
                            src_r[k] + (src_r[k1] - src_r[k]) * frac,
                        )
                    };
                    let mut edge = 1.0_f64;
                    if clip.fade_in {
                        edge = edge.min((t - clip.start_s) / CLIP_FADE_S);
                    }
                    if clip.fade_out {
                        edge = edge.min((clip.end_s - t) / CLIP_FADE_S);
                    }
                    if declick_jump {
                        edge = edge.min((t - jump) / CLIP_FADE_S);
                    }
                    let edge = edge.clamp(0.0, 1.0) * clip.fade_gain(t);
                    let g = clip.gain * edge as f32;
                    let o = seg.offset as usize + i;
                    dl[o] += vl * g;
                    dr[o] += vr * g;
                }
            }
        }
    }

    /// Moves automated mixer parameters to their values at the start of this quantum (song
    /// mode, while playing). Faders and balances glide over the mixer's ramp; effect
    /// parameters glide in the effect's own smoothing.
    /// Evaluates automation lanes (while the song plays) and modulators (always) at the end
    /// of this quantum and writes the values to their parameters. Gains ramp to them across
    /// the quantum, so between control points they move sample by sample.
    fn apply_controls(&mut self) {
        let song = self.song.as_deref();
        let has_lanes = song.is_some_and(|s| !s.automation.is_empty());
        // Pattern mode: the pattern's lanes repeat with it (D112).
        let wrap = |t: f64| match song {
            Some(s) if s.repeat && s.length > 0 => t.rem_euclid(s.length as f64),
            _ => t,
        };
        if !has_lanes && self.mods.is_none() {
            return;
        }
        // Song tick at the start and end of the quantum, while playing.
        let segs = self.scheduled.segments();
        let start = segs
            .first()
            .map(|seg| self.transport.tick_at_seconds(seg.seconds));
        let tick = segs.last().map(|seg| {
            self.transport
                .tick_at_seconds(seg.seconds + f64::from(seg.frames) / self.sample_rate)
        });
        // Playback started, located or wrapped exactly here: lanes jump to their value at the
        // new position first, so the first ramp does not start from wherever they were.
        let jump = start
            .map(wrap)
            .filter(|t| (t - self.control_tick).abs() > 0.5 || self.control_tick.is_nan());
        self.control_tick = tick.map_or(f64::NAN, wrap);
        let lane_tick = tick.filter(|_| has_lanes).map(wrap);
        let ramp = Q as u32;
        let writing = &self.writing;
        if let (Some(t), Some(song)) = (lane_tick, song) {
            for (i, lane) in song.automation.iter().enumerate() {
                if lane.modulated || writing.contains(&lane.dest) {
                    continue;
                }
                for (at, frames) in jump.map(|j| (j, 0)).into_iter().chain([(t, ramp)]) {
                    let v = lane
                        .info
                        .from_normalized(gt_core::playlist::value_at(&lane.points, at));
                    apply_param(
                        &mut self.channels,
                        &mut self.mixer,
                        &mut self.plugins,
                        lane.dest,
                        v,
                        frames,
                    );
                    if let Some(cell) = self.telemetry.lane_values.get(i) {
                        cell.store(v, Ordering::Relaxed);
                    }
                }
            }
        }
        if let Some(plan) = self.mods.as_deref_mut() {
            let clock = ModClock {
                dt: Q as f64 / self.sample_rate,
                bpm: self.transport.bpm_at(self.frame_clock),
                beats: tick.map(beats_of),
                mod_wheel: 0.0,
                aftertouch: 0.0,
            };
            let live_slot = self.live_slot;
            for (i, target) in plan.targets.iter_mut().enumerate() {
                // MIDI sources follow the target's own channel, else the selected one.
                let slot = match target.dest {
                    ParamDest::Channel { slot, .. }
                    | ParamDest::Synth { slot, .. }
                    | ParamDest::SynthMod { slot, .. } => Some(slot),
                    _ => live_slot,
                };
                let expr = slot
                    .and_then(|s| self.channels.get(usize::from(s)))
                    .map(ChannelSlot::expression)
                    .unwrap_or_default();
                let clock = ModClock {
                    mod_wheel: expr.mod_wheel,
                    aftertouch: expr.pressure,
                    ..clock
                };
                let lane = target.lane.filter(|_| !writing.contains(&target.dest));
                let base_at = |t: Option<f64>| match (lane, t.filter(|_| has_lanes), song) {
                    (Some(i), Some(t), Some(song)) => song
                        .automation
                        .get(i)
                        .map_or(target.base, |l| gt_core::playlist::value_at(&l.points, t)),
                    _ => target.base,
                };
                if jump.is_some() && lane.is_some() {
                    let prev: f32 = target
                        .mods
                        .iter()
                        .map(|m| m.amount * m.state.output())
                        .sum();
                    apply_param(
                        &mut self.channels,
                        &mut self.mixer,
                        &mut self.plugins,
                        target.dest,
                        target
                            .info
                            .from_normalized((base_at(jump) + prev).clamp(0.0, 1.0)),
                        0,
                    );
                }
                let mut v = base_at(lane_tick);
                for m in &mut target.mods {
                    let mixer = &self.mixer;
                    v += m.amount * m.state.step(&m.source, &clock, |s| mixer.strip_peak(s));
                }
                apply_param(
                    &mut self.channels,
                    &mut self.mixer,
                    &mut self.plugins,
                    target.dest,
                    target.info.from_normalized(v.clamp(0.0, 1.0)),
                    ramp,
                );
                if let Some(cell) = self.telemetry.mod_values.get(i) {
                    cell.store(
                        target.info.from_normalized(v.clamp(0.0, 1.0)),
                        Ordering::Relaxed,
                    );
                }
            }
        }
        for ch in &mut self.channels {
            ch.flush_controls();
        }
    }

    /// Marks the song's lanes that a modulator target also drives, and points those targets
    /// at their lane. Called whenever the song or the modulation plan changes.
    fn link_lanes(&mut self) {
        let song = self.song.as_deref_mut();
        let lanes = match song {
            Some(s) => {
                for l in &mut s.automation {
                    l.modulated = false;
                }
                &mut s.automation[..]
            }
            None => &mut [],
        };
        if let Some(plan) = self.mods.as_deref_mut() {
            for t in &mut plan.targets {
                t.lane = lanes.iter().position(|l| l.dest == t.dest);
                if let Some(i) = t.lane {
                    lanes[i].modulated = true;
                }
            }
        }
    }

    /// Drops every automation and modulation value so the document's settings apply again
    /// (the next `apply_controls` sets the ones still driven).
    fn clear_controls(&mut self) {
        self.mixer.clear_automation();
        for ch in &mut self.channels {
            ch.clear_controls();
        }
    }

    /// Appends `scratch_l` to the telemetry oscilloscope ring.
    fn write_scope(&self) {
        let t = &*self.telemetry;
        let start = t.scope_write.load(Ordering::Relaxed);
        for (i, &x) in self.scratch_l.iter().enumerate() {
            t.scope[((start + i as u64) % SCOPE_LEN as u64) as usize].store(x, Ordering::Relaxed);
        }
        t.scope_write
            .store(start + self.scratch_l.len() as u64, Ordering::Relaxed);
    }

    fn apply_commands(&mut self) {
        for _ in 0..MAX_COMMANDS_PER_QUANTUM {
            // Commands that hand something back are only taken if the garbage queue has room
            // for it, so the audio thread never has to free anything. Otherwise retry next
            // quantum.
            let needs_gc = self
                .commands
                .peek()
                .is_ok_and(EngineCommand::returns_garbage);
            if needs_gc && self.garbage.slots() == 0 {
                break;
            }
            let Ok(cmd) = self.commands.pop() else {
                break;
            };
            self.apply(cmd);
        }
    }

    fn release_all_channels(&mut self) {
        for ch in &mut self.channels {
            if ch.is_plugin() {
                self.plugins.release_all(ch.plugin, 0);
            } else {
                ch.release_all();
            }
        }
    }

    fn apply(&mut self, cmd: EngineCommand) {
        let now = self.frame_clock;
        let before = self.transport.state();
        if matches!(
            cmd,
            EngineCommand::Play | EngineCommand::Pause | EngineCommand::Stop
        ) {
            self.cancel_count_in();
        }
        match cmd {
            EngineCommand::Play => {
                self.router.reset();
                self.transport.play(now);
            }
            EngineCommand::Pause => self.transport.pause(now),
            EngineCommand::Stop => self.transport.stop(),
            EngineCommand::Locate(t) => {
                self.transport.locate(t, now);
                self.release_all_channels();
            }
            EngineCommand::SetLoop(region) => self.transport.set_loop(region),
            EngineCommand::SetSignatures(sig) => {
                let old = self.transport.set_signatures(sig);
                self.retire(Garbage::Signatures(old));
            }
            EngineCommand::SetTempoMap(map) => {
                let old = self.transport.set_tempo_map(map, now);
                self.retire(Garbage::TempoMap(old));
            }
            EngineCommand::SetMetronome(on) => self.metronome = on,
            EngineCommand::SetTestTone(on) => {
                if on && !self.tone_on && self.tone_ramp.value() == 0.0 {
                    // Start from a zero crossing so the fade-in begins cleanly.
                    self.tone.reset();
                }
                self.tone_on = on;
                let target = if on { self.tone_gain } else { 0.0 };
                self.tone_ramp.set_target(target, self.fade_frames);
            }
            EngineCommand::FadeOut => self.master.set_target(0.0, self.fade_frames),
            EngineCommand::FadeIn => self.master.set_target(1.0, self.fade_frames),
            EngineCommand::SetSong(song) => {
                // Note-offs of the old pattern may never come; release what it started.
                self.release_all_channels();
                // Parameters follow the document again until the new song's automation moves
                // them (in this same quantum, while playing).
                self.clear_controls();
                if let Some(old) = self.song.replace(song) {
                    self.retire(Garbage::Song(old));
                }
                self.link_lanes();
                forget_values(&self.telemetry.lane_values);
            }
            EngineCommand::UpdateSong(song) => {
                if let Some(old) = self.song.replace(song) {
                    self.retire(Garbage::Song(old));
                }
                self.link_lanes();
                forget_values(&self.telemetry.lane_values);
            }
            EngineCommand::SetModulation(mut plan) => {
                self.clear_controls();
                forget_values(&self.telemetry.mod_values);
                if let Some(old) = self.mods.take() {
                    plan.inherit(&old);
                    self.retire(Garbage::Modulation(old));
                }
                // An empty plan is dropped right away: nothing to run.
                if plan.targets.is_empty() {
                    self.retire(Garbage::Modulation(plan));
                } else {
                    self.mods = Some(plan);
                }
                self.link_lanes();
            }
            EngineCommand::SetChannelParams { slot, params } => {
                if let Some(ch) = self.channels.get_mut(usize::from(slot)) {
                    let before = ch.plugin;
                    if ch.is_active() {
                        ch.set_params(*params);
                    } else {
                        ch.set_params_now(*params);
                    }
                    ch.plugin = ch.plugin_instance().and_then(|id| self.plugins.find(id));
                    if before.is_some() && before != ch.plugin {
                        // The channel stopped playing that plugin: let its notes end.
                        self.plugins.release_all(before, 0);
                    }
                }
                self.retire(Garbage::Params(params));
            }
            EngineCommand::SetScopeChannel(slot) => self.scope_slot = slot.map(usize::from),
            EngineCommand::SetChannelSample { slot, sample } => {
                let old = match self.channels.get_mut(usize::from(slot)) {
                    Some(ch) => ch.set_sample(sample),
                    None => sample,
                };
                if let Some(old) = old {
                    self.retire(Garbage::Sample(old));
                }
            }
            EngineCommand::SetChannelZones { slot, zones } => {
                let old = match self.channels.get_mut(usize::from(slot)) {
                    Some(ch) => ch.set_zones(zones),
                    None => zones,
                };
                if let Some(old) = old {
                    self.retire(Garbage::Zones(old));
                }
            }
            EngineCommand::SetChannelTables { slot, tables } => {
                let old = match self.channels.get_mut(usize::from(slot)) {
                    Some(ch) => ch.set_tables(tables),
                    None => tables,
                };
                if let Some(old) = old {
                    self.retire(Garbage::Tables(old));
                }
            }
            EngineCommand::NoteOn {
                slot,
                key,
                velocity,
            } => self.slot_note_on(usize::from(slot), key, velocity),
            EngineCommand::NoteOff { slot, key } => self.slot_note_off(usize::from(slot), key),
            EngineCommand::GrowMixer(mut strips) => {
                self.mixer.grow(&mut strips);
                self.retire(Garbage::NewStrips(strips));
            }
            EngineCommand::SetMixer(params) => {
                self.mixer.set_params(&params);
                self.mixer.resolve_plugins(&self.plugins);
                self.telemetry
                    .latency_frames
                    .store(self.mixer.latency() as u32, Ordering::Relaxed);
                self.retire(Garbage::Mixer(params));
            }
            EngineCommand::SetPlugin { index, plugin } => {
                let old = self
                    .plugins
                    .set(usize::from(index), plugin, &self.telemetry);
                self.resolve_plugins();
                if let Some(old) = old {
                    self.retire(Garbage::Plugin(old));
                }
            }
            EngineCommand::SetPluginParam {
                index,
                param,
                value,
            } => self.plugins.set_param_at(index, param, value),
            EngineCommand::RetryPlugin(index) => {
                self.plugins.retry(usize::from(index), &self.telemetry);
            }
            EngineCommand::SetEffect {
                strip,
                slot,
                effect,
            } => {
                let old = self
                    .mixer
                    .set_effect(usize::from(strip), usize::from(slot), effect);
                self.telemetry
                    .latency_frames
                    .store(self.mixer.latency() as u32, Ordering::Relaxed);
                if let Some(old) = old {
                    self.retire(Garbage::Effect(old));
                }
            }
            EngineCommand::SetEffectParam {
                strip,
                slot,
                index,
                value,
            } => {
                if self.mixer.set_effect_param(
                    usize::from(strip),
                    usize::from(slot),
                    usize::from(index),
                    value,
                ) {
                    self.telemetry
                        .latency_frames
                        .store(self.mixer.latency() as u32, Ordering::Relaxed);
                }
            }
            EngineCommand::PreviewSample(sample) => {
                let start = sample.is_some();
                if let Some(old) = self.preview.set_sample(sample) {
                    self.retire(Garbage::Sample(old));
                }
                if start {
                    self.voice_age += 1;
                    self.preview.note_on(ROOT_KEY, 1.0, self.voice_age);
                }
            }
            EngineCommand::SetLiveChannel(slot) => {
                let slot = slot.filter(|&s| usize::from(s) < self.channels.len());
                if slot != self.live_slot {
                    // The pedal and the wheels belonged to the old channel: let them go.
                    if let Some(old) = self.live_slot {
                        self.reset_expression(usize::from(old));
                    }
                    self.live_slot = slot;
                }
            }
            EngineCommand::SetLiveRoutes(routes) => {
                let old = std::mem::replace(&mut self.live_routes, routes);
                self.retire(Garbage::LiveRoutes(old));
            }
            EngineCommand::SetWriting(mut list) => {
                // The old list goes back in the box, so nothing is freed here.
                std::mem::swap(&mut *list, &mut self.writing);
                self.retire(Garbage::Writing(list));
                // Lanes that stop playing leave the document value; those that resume set
                // theirs again in the next quantum.
                self.clear_controls();
            }
            EngineCommand::CountIn { bars } => self.start_count_in(bars),
            EngineCommand::SetInput(feed) => {
                if let Some(old) = self.input.set_feed(feed) {
                    self.retire(Garbage::Input(old));
                }
            }
            EngineCommand::SetInputRoutes(routes) => {
                if let Some(old) = self.input.set_routes(routes) {
                    self.retire(Garbage::InputRoutes(old));
                }
            }
            EngineCommand::SetRecorder(sink) => {
                if let Some(old) = self.input.set_sink(sink) {
                    self.retire(Garbage::Recorder(old));
                }
                self.telemetry.recording.store(false, Ordering::Relaxed);
            }
            EngineCommand::Calibrate(sink) => {
                if let Some(old) = self.input.set_calibration(sink) {
                    self.retire(Garbage::Calibration(old));
                }
            }
        }
        self.after_transport_change(before);
    }

    /// Releases notes, resets effects and tells the UI when the transport changed state.
    fn after_transport_change(&mut self, before: TransportState) {
        let now = self.frame_clock;
        let after = self.transport.state();
        if before == TransportState::Playing && after != TransportState::Playing {
            self.release_all_channels();
            // Recorded bends and pedals end with playback.
            let n = self.song.as_ref().map_or(0, |s| s.control_slots.len());
            for i in 0..n {
                let slot = self.song.as_ref().map_or(0, |s| s.control_slots[i].0);
                self.reset_expression(usize::from(slot));
            }
        }
        if before == TransportState::Stopped && after == TransportState::Playing {
            // Effects start from silence at play, so a capture from a stopped transport matches
            // an offline export of the same range (ARCHITECTURE.md §7.5).
            self.mixer.reset_effects();
            self.plugins.reset_all();
        }
        if before != after {
            self.post(EngineEvent::TransportChanged {
                state: after,
                position: Tick(self.transport.position_at(now).floor() as i64),
            });
        }
    }

    /// Starts counting `bars` bars at the playhead's tempo and time signature.
    fn start_count_in(&mut self, bars: u8) {
        let now = self.frame_clock;
        if self.transport.state() == TransportState::Playing {
            return;
        }
        let pos = self.transport.position_at(now);
        let sig = self.transport.signatures().sig_at(Tick(pos.floor() as i64));
        let bpm = self.transport.bpm_at(now).max(1.0);
        let beat_frames = 60.0 / bpm * self.sample_rate * 4.0 / f64::from(sig.den.max(1));
        let per_bar = u32::from(sig.num.max(1));
        let beats = u32::from(bars) * per_bar;
        if beats == 0 {
            self.transport.play(now);
            return;
        }
        // Playback starts on the first quantum boundary at or after the last beat's end, at
        // most half a quantum (0.33 ms at 48 kHz) late.
        let len = (f64::from(beats) * beat_frames / Q as f64).round() as u64 * Q as u64;
        self.count_in = Some(CountIn {
            start: now,
            end: now + len.max(Q as u64),
            beat_frames,
            done: 0,
            beats,
            per_bar,
        });
    }

    fn cancel_count_in(&mut self) {
        if self.count_in.take().is_some() {
            self.telemetry.count_in_beats.store(0, Ordering::Relaxed);
        }
    }

    /// Moves a count-in on by one quantum: returns the click in this quantum (offset,
    /// downbeat), or starts playback when the count is over.
    fn advance_count_in(&mut self) -> Option<(usize, bool)> {
        let c = self.count_in.as_mut()?;
        let q0 = self.frame_clock;
        if q0 >= c.end {
            self.cancel_count_in();
            let before = self.transport.state();
            self.transport.play(q0);
            self.after_transport_change(before);
            return None;
        }
        if c.done >= c.beats {
            return None;
        }
        let next = c.start + (f64::from(c.done) * c.beat_frames).round() as u64;
        if next >= q0 + Q as u64 {
            return None;
        }
        let click = ((next.saturating_sub(q0)) as usize, c.done % c.per_bar == 0);
        let left = c.beats - c.done;
        c.done += 1;
        self.telemetry.count_in_beats.store(left, Ordering::Relaxed);
        Some(click)
    }

    /// Takes the live events that arrived, and plays those due in this quantum at their
    /// frames. An event plays one device period after it arrived, at the same place within the
    /// period (D109): notes keep their timing however the callbacks fall. Events with no time
    /// play at once.
    fn apply_live(&mut self) {
        let q0 = self.frame_clock;
        for _ in 0..MAX_LIVE_PER_QUANTUM {
            let Ok(e) = self.live.pop() else {
                break;
            };
            let at = self.live_frame(e.at_ns);
            if self.live_wait_len < LIVE_WAIT {
                self.live_wait[self.live_wait_len] = (at, e);
                self.live_wait_len += 1;
            } else {
                self.apply_live_event(&e, 0);
            }
        }
        // Due events leave the list in order; the rest move up.
        let mut kept = 0;
        for i in 0..self.live_wait_len {
            let (at, e) = self.live_wait[i];
            if at < q0 + Q as u64 {
                self.apply_live_event(&e, at.saturating_sub(q0) as u32);
            } else {
                self.live_wait[kept] = (at, e);
                kept += 1;
            }
        }
        self.live_wait_len = kept;
    }

    /// Tells the UI where the song is against the clock, after a callback: an event that
    /// arrives as the callback starts plays at the next callback's first frame.
    fn publish_anchor(&self) {
        let next = self.cb_out + self.cb_frames;
        let playing = self.transport.state() == TransportState::Playing;
        let tick = if playing {
            self.transport.position_at(next)
        } else {
            f64::NAN
        };
        let rate = self.transport.bpm_at(next) / 60.0 * gt_core::PPQ as f64 / 1e9;
        self.telemetry.anchor.publish(self.cb_ns, tick, rate);
    }

    /// The engine frame where an event that arrived at `at_ns` plays.
    fn live_frame(&self, at_ns: u64) -> u64 {
        if at_ns == 0 || self.cb_frames == 0 {
            return 0;
        }
        let period_ns = self.cb_frames as f64 * 1e9 / self.sample_rate;
        let into = (at_ns as f64 + period_ns - self.cb_ns as f64) * self.sample_rate / 1e9;
        self.cb_out + into.clamp(0.0, (self.cb_frames - 1) as f64) as u64
    }

    /// Plays one live event at frame `offset` of this quantum: notes through the event list
    /// (so they start at their frame), expression at once.
    fn apply_live_event(&mut self, e: &LiveEvent, offset: u32) {
        let offset = offset.min(Q as u32 - 1);
        let slot = self
            .live_routes
            .slot(e.port, e.channel, self.live_slot)
            .filter(|&s| usize::from(s) < self.channels.len());
        let playing = self.transport.state() == TransportState::Playing;
        let tick = self
            .transport
            .position_at(self.frame_clock + u64::from(offset));
        let n = match e.kind {
            LiveKind::Note(n) => n,
            kind => {
                if let Some(slot) = slot {
                    self.slot_expression(usize::from(slot), kind, offset);
                    let control = match kind {
                        LiveKind::Bend(v) => Some((gt_core::ControlKind::Bend, v)),
                        LiveKind::Sustain(d) => {
                            Some((gt_core::ControlKind::Sustain, if d { 1.0 } else { 0.0 }))
                        }
                        LiveKind::ModWheel(v) => Some((gt_core::ControlKind::ModWheel, v)),
                        LiveKind::Pressure(v) => Some((gt_core::ControlKind::Pressure, v)),
                        _ => None,
                    };
                    if let Some((kind, value)) = control.filter(|_| playing) {
                        self.post(EngineEvent::LiveControl {
                            slot,
                            kind,
                            value,
                            tick,
                        });
                    }
                }
                return;
            }
        };
        let source = match e.port {
            p if usize::from(p) < ROUTE_PORTS => usize::from(p),
            _ => ROUTE_PORTS,
        };
        let _ = KEYBOARD;
        let h = (source * 16 + usize::from(e.channel & 0x0F)) * 128 + usize::from(n.key & 0x7F);
        let at = Tick(tick as i64);
        let ev = |kind| ScheduledEvent {
            offset,
            tick: at,
            kind,
        };
        let mut played = None;
        if n.is_on() {
            // A key struck again before its release ends the old note first.
            if let Some(old) = self.live_held[h].checked_sub(1) {
                self.scheduled.insert(ev(EventKind::NoteOff {
                    slot: old,
                    key: n.key,
                }));
                self.live_held[h] = 0;
            }
            if let Some(slot) = slot {
                self.scheduled.insert(ev(EventKind::NoteOn {
                    slot,
                    key: n.key,
                    velocity: n.velocity,
                    expr: NoteExpr::default(),
                }));
                self.live_held[h] = slot + 1;
                played = Some(slot);
            }
        } else if let Some(old) = self.live_held[h].checked_sub(1) {
            self.scheduled.insert(ev(EventKind::NoteOff {
                slot: old,
                key: n.key,
            }));
            self.live_held[h] = 0;
            played = Some(old);
        }
        if let Some(slot) = played {
            self.post(EngineEvent::LiveNote {
                slot,
                key: n.key,
                velocity: n.velocity,
                tick,
                playing,
            });
        }
    }

    /// Bend, sustain, mod wheel or aftertouch on a channel slot: built-in instruments take it
    /// at once, plugins as MIDI (polyphonic aftertouch as a note expression) at `offset`.
    fn slot_expression(&mut self, slot: usize, kind: LiveKind, offset: u32) {
        let Some(ch) = self.channels.get_mut(slot) else {
            return;
        };
        if ch.is_plugin() {
            let seven = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
            let msg = match kind {
                LiveKind::Bend(b) => {
                    let v = ((b.clamp(-1.0, 1.0) + 1.0) * 8192.0).round().min(16383.0) as u16;
                    [0xE0, (v & 0x7F) as u8, (v >> 7) as u8]
                }
                LiveKind::Sustain(down) => [0xB0, 64, if down { 127 } else { 0 }],
                LiveKind::ModWheel(v) => [0xB0, 1, seven(v)],
                LiveKind::Pressure(v) => [0xD0, seven(v), 0],
                LiveKind::Cc { cc, value } => [0xB0, cc & 0x7F, seven(value)],
                LiveKind::KeyPressure { key, value } => {
                    self.plugins.key_pressure(ch.plugin, offset, key, value);
                    return;
                }
                LiveKind::Note(_) => return,
            };
            self.plugins.midi(ch.plugin, offset, msg);
            return;
        }
        match kind {
            LiveKind::Bend(b) => ch.set_bend(b),
            LiveKind::Sustain(down) => ch.set_sustain(down),
            LiveKind::ModWheel(v) => ch.set_mod_wheel(v),
            LiveKind::Pressure(v) => ch.set_pressure(v),
            LiveKind::KeyPressure { key, value } => ch.set_key_pressure(key, value),
            LiveKind::Note(_) | LiveKind::Cc { .. } => {}
        }
    }

    /// Centres the bend, lifts the pedal and zeroes the wheels of a slot.
    fn reset_expression(&mut self, slot: usize) {
        for kind in [
            LiveKind::Sustain(false),
            LiveKind::Bend(0.0),
            LiveKind::ModWheel(0.0),
            LiveKind::Pressure(0.0),
        ] {
            self.slot_expression(slot, kind, 0);
        }
    }

    /// Hands an object back to the UI thread for freeing.
    fn retire(&mut self, g: Garbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Unreachable: callers check for a free slot first. Leaking is the safe fallback;
            // freeing here would break the real-time contract.
            std::mem::forget(g);
        }
    }

    fn post(&mut self, e: EngineEvent) {
        if self.events.push(e).is_err() {
            self.telemetry
                .events_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn publish(&self) {
        let t = &*self.telemetry;
        let pos = self.transport.position_at(self.frame_clock);
        t.position_ticks
            .store(pos.floor() as i64, Ordering::Relaxed);
        t.transport_state
            .store(self.transport.state() as u8, Ordering::Relaxed);
        t.frames_rendered.store(self.frame_clock, Ordering::Relaxed);
        t.silent.store(
            self.master.is_settled() && self.master.value() == 0.0,
            Ordering::Relaxed,
        );
        let dropped = self.scheduled.dropped();
        if dropped > 0 {
            t.events_dropped.fetch_max(dropped, Ordering::Relaxed);
        }
    }
}

/// Writes a plain value to a parameter. Gains ramp to it over `ramp` frames.
fn apply_param(
    channels: &mut [ChannelSlot],
    mixer: &mut MixerEngine,
    plugins: &mut PluginTable,
    dest: ParamDest,
    value: f32,
    ramp: u32,
) {
    use gt_core::StripParam;
    match dest {
        ParamDest::Channel { slot, param } => {
            if let Some(ch) = channels.get_mut(usize::from(slot)) {
                ch.control(param, value, ramp);
            }
        }
        ParamDest::Synth { slot, index } => {
            if let Some(ch) = channels.get_mut(usize::from(slot)) {
                ch.control_synth(usize::from(index), value);
            }
        }
        ParamDest::SynthMod { slot, row } => {
            if let Some(ch) = channels.get_mut(usize::from(slot)) {
                ch.control_synth_mod(usize::from(row), value);
            }
        }
        ParamDest::Strip { strip, param } => {
            let s = usize::from(strip);
            match param {
                StripParam::Volume => mixer.automate(s, Some(value), None, ramp),
                StripParam::Pan => mixer.automate(s, None, Some(value), ramp),
                StripParam::Send(k) => mixer.automate_send(s, k, value, ramp),
            }
        }
        ParamDest::Effect { strip, slot, index } => {
            mixer.set_effect_param(
                usize::from(strip),
                usize::from(slot),
                usize::from(index),
                value,
            );
        }
        ParamDest::Plugin { instance, index } => plugins.set_param(instance, index, value),
    }
}

/// Marks live values unknown (their lanes or targets changed).
fn forget_values(cells: &[crate::AtomicF32]) {
    for c in cells {
        c.store(f32::NAN, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use crate::{create, AudioProcessor, EngineCommand, EngineConfig, EngineHandle};
    use std::sync::atomic::Ordering;

    fn engine(sr: u32, ch: usize) -> (EngineHandle, AudioProcessor) {
        create(EngineConfig {
            sample_rate: sr,
            out_channels: ch,
        })
    }

    #[test]
    fn silent_until_something_plays() {
        let (_h, mut p) = engine(48_000, 2);
        let mut buf = vec![1.0; 512];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_tone_plays_at_minus_12_dbfs_on_all_channels() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 2 * 48_000];
        p.process(&mut buf);
        let peak = buf[2 * 4800..].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.251_188_64).abs() < 1e-3, "{peak}");
        for f in buf.chunks_exact(2) {
            assert_eq!(f[0], f[1]);
        }
    }

    fn impulse_sample(sr: u32) -> std::sync::Arc<gt_core::SampleData> {
        let mut v = vec![0.0; 4800];
        v[0] = 1.0;
        std::sync::Arc::new(gt_core::SampleData::mono(sr, v))
    }

    fn full_params() -> Box<crate::ChannelParams> {
        Box::new(crate::ChannelParams {
            gain: 1.0,
            ..crate::ChannelParams::default()
        })
    }

    #[test]
    fn synth_channel_plays_and_feeds_the_scope() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetChannelParams {
            slot: 5,
            params: Box::new(crate::ChannelParams {
                kind: crate::InstrumentKind::Synth,
                gain: 1.0,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::SetScopeChannel(Some(5))).unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 5,
            key: 57,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 2 * 4800];
        p.process(&mut buf);
        assert!(buf.iter().any(|s| s.abs() > 0.05));
        let mut scope = vec![0.0; 1024];
        h.telemetry().read_scope(&mut scope);
        assert!(scope.iter().any(|s| s.abs() > 0.05));
        assert!(h.telemetry().scope_write.load(Ordering::Relaxed) >= 4800);
    }

    #[test]
    fn audition_note_plays_the_sample_panned() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetChannelSample {
            slot: 3,
            sample: Some(impulse_sample(48_000)),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 3,
            params: Box::new(crate::ChannelParams {
                gain: 1.0,
                pan: 1.0,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 3,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 2 * 128];
        p.process(&mut buf);
        assert!(buf[0].abs() < 1e-6, "hard right: left is silent");
        assert!((buf[1] - 1.0).abs() < 1e-6, "{}", buf[1]);
        assert!(buf[2..].iter().all(|&s| s == 0.0));
        let peak = h.telemetry().channel_peaks[3].load(Ordering::Relaxed);
        assert!((peak - 1.0).abs() < 1e-6);
        assert_eq!(h.collect_garbage(), 1, "the params box comes back");
    }

    #[test]
    fn velocity_is_squared_and_mute_ramps_to_silence() {
        let (mut h, mut p) = engine(48_000, 1);
        let s = std::sync::Arc::new(gt_core::SampleData::mono(48_000, vec![1.0; 48_000]));
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(s),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: full_params(),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 0.5,
        })
        .unwrap();
        let mut buf = vec![0.0; 64];
        p.process(&mut buf);
        // Centre pan is -3 dB per side; a mono device averages the sides.
        let want = 0.25 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((buf[10] - want).abs() < 1e-6, "{}", buf[10]);
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(crate::ChannelParams::default()),
        })
        .unwrap();
        let mut buf = vec![0.0; 4800];
        p.process(&mut buf);
        let steps = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(steps < 1e-3, "gain change is smoothed: {steps}");
        assert_eq!(buf[4799], 0.0);
    }

    #[test]
    fn preview_plays_and_replaced_samples_come_back() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::PreviewSample(Some(impulse_sample(48_000))))
            .unwrap();
        let mut buf = vec![0.0; 2 * 64];
        p.process(&mut buf);
        let g = crate::PREVIEW_GAIN * std::f32::consts::FRAC_1_SQRT_2;
        assert!((buf[0] - g).abs() < 1e-6, "{}", buf[0]);
        h.send(EngineCommand::PreviewSample(None)).unwrap();
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
        // Out-of-range slots are ignored, and their payload still comes back.
        h.send(EngineCommand::SetChannelSample {
            slot: 999,
            sample: Some(impulse_sample(48_000)),
        })
        .unwrap();
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
    }

    #[test]
    fn stop_releases_looped_voices() {
        let (mut h, mut p) = engine(48_000, 1);
        let s = std::sync::Arc::new(gt_core::SampleData::mono(48_000, vec![1.0; 100]));
        h.send(EngineCommand::SetChannelSample {
            slot: 0,
            sample: Some(s),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(crate::ChannelParams {
                gain: 1.0,
                looped: true,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::Play).unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 48_000];
        p.process(&mut buf);
        assert!(buf[47_999] > 0.5, "loop keeps sounding while held");
        h.send(EngineCommand::Stop).unwrap();
        p.process(&mut buf);
        assert_eq!(buf[47_999], 0.0, "released by stop");
    }

    #[test]
    fn tone_fades_have_no_steps() {
        let (mut h, mut p) = engine(44_100, 1);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 4410];
        p.process(&mut buf);
        h.send(EngineCommand::SetTestTone(false)).unwrap();
        let mut tail = vec![0.0; 4410];
        p.process(&mut tail);
        buf.extend_from_slice(&tail);
        // Largest step of a 440 Hz sine at amplitude a is a * 2π * 440 / 44100 ≈ 0.0157.
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.02, "{max_step}");
        assert_eq!(tail[tail.len() - 1], 0.0);
    }

    #[test]
    fn fade_out_reports_silent() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        let mut buf = vec![0.0; 2048];
        p.process(&mut buf);
        assert!(!h.telemetry().silent.load(Ordering::Relaxed));
        h.send(EngineCommand::FadeOut).unwrap();
        for _ in 0..10 {
            p.process(&mut buf);
        }
        assert!(h.telemetry().silent.load(Ordering::Relaxed));
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn odd_block_sizes_and_partial_frames_are_handled() {
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetTestTone(true)).unwrap();
        for len in [0, 1, 2, 3, 441 * 2, 1023] {
            let mut buf = vec![9.0; len];
            p.process(&mut buf);
            assert!(buf.iter().all(|s| s.abs() <= 0.26), "len {len}");
        }
        assert_eq!(h.telemetry().last_block_frames.load(Ordering::Relaxed), 511);
    }

    #[test]
    fn transport_state_is_published_and_evented() {
        let (mut h, mut p) = engine(48_000, 1);
        h.send(EngineCommand::Play).unwrap();
        let mut buf = vec![0.0; 48_000];
        p.process(&mut buf);
        let t = h.telemetry();
        assert_eq!(t.transport_state(), crate::TransportState::Playing);
        // 1 s at 120 BPM = 2 beats = 1920 ticks (rendered up to the next quantum boundary).
        assert!(
            (1920..1920 + 64).contains(&t.position().0),
            "{:?}",
            t.position()
        );
        let mut events = Vec::new();
        h.poll_events(|e| events.push(e));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn tempo_map_comes_back_as_garbage() {
        let (mut h, mut p) = engine(48_000, 1);
        h.send(EngineCommand::SetTempoMap(Box::new(
            gt_core::TempoMap::constant(90.0),
        )))
        .unwrap();
        let mut buf = vec![0.0; 64];
        p.process(&mut buf);
        assert_eq!(h.collect_garbage(), 1);
    }

    /// Frames where the output rises above a small threshold after at least 1000 quiet frames.
    fn onsets(buf: &[f32]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut quiet = usize::MAX / 2;
        for (i, s) in buf.iter().enumerate() {
            if s.abs() > 1e-3 {
                if quiet >= 1000 {
                    out.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        out
    }

    #[test]
    fn count_in_clicks_each_beat_then_plays() {
        let (mut h, mut p) = engine(48_000, 1);
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        h.send(EngineCommand::CountIn { bars: 1 }).unwrap();
        // 120 BPM, 4/4: a beat is 24 000 frames, the bar 96 000.
        let mut buf = vec![0.0; 95_000];
        p.process(&mut buf);
        assert_eq!(onsets(&buf), vec![0, 24_000, 48_000, 72_000]);
        let t = std::sync::Arc::clone(&p.telemetry);
        assert_eq!(t.transport_state(), crate::TransportState::Stopped);
        assert_eq!(t.count_in_beats.load(Ordering::Relaxed), 1);
        let mut buf = vec![0.0; 2_000];
        p.process(&mut buf);
        assert_eq!(t.transport_state(), crate::TransportState::Playing);
        assert_eq!(t.count_in_beats.load(Ordering::Relaxed), 0);
        // Playing from bar 1 with the metronome off: no more clicks.
        assert!(onsets(&buf).is_empty());
        // Stop during a count-in cancels it.
        h.send(EngineCommand::Stop).unwrap();
        h.send(EngineCommand::CountIn { bars: 2 }).unwrap();
        let mut buf = vec![0.0; 1_000];
        p.process(&mut buf);
        h.send(EngineCommand::Stop).unwrap();
        let mut buf = vec![0.0; 200_000];
        p.process(&mut buf);
        assert_eq!(t.transport_state(), crate::TransportState::Stopped);
        assert_eq!(t.count_in_beats.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn live_notes_play_the_live_channel_and_are_reported_while_playing() {
        let (mut h, mut p) = engine(48_000, 1);
        let live = crate::LiveInput::default();
        assert!(!live.send(crate::LiveNote::on(60, 1.0)));
        live.connect(h.take_live_producer().unwrap());
        h.send(EngineCommand::SetChannelParams {
            slot: 2,
            params: Box::new(crate::ChannelParams {
                kind: crate::InstrumentKind::Synth,
                gain: 1.0,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        // No live channel yet: silence.
        assert!(live.send(crate::LiveNote::on(60, 1.0)));
        let mut buf = vec![0.0; 4800];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
        assert!(live.send(crate::LiveNote::off(60)));
        h.send(EngineCommand::SetLiveChannel(Some(2))).unwrap();
        p.process(&mut buf);
        assert!(live.send(crate::LiveNote::on(64, 0.8)));
        p.process(&mut buf);
        assert!(buf.iter().any(|&s| s.abs() > 0.01));
        // Stopped: reported as not playing (for step recording).
        let mut events = Vec::new();
        h.poll_events(|e| events.push(e));
        assert!(
            matches!(
                events[..],
                [crate::EngineEvent::LiveNote {
                    slot: 2,
                    key: 64,
                    playing: false,
                    ..
                }]
            ),
            "{events:?}"
        );
        events.clear();

        h.send(EngineCommand::Play).unwrap();
        p.process(&mut buf);
        assert!(live.send(crate::LiveNote::off(64)));
        p.process(&mut buf);
        h.poll_events(|e| events.push(e));
        let notes: Vec<_> = events
            .iter()
            .filter_map(|e| match *e {
                crate::EngineEvent::LiveNote {
                    key,
                    velocity,
                    tick,
                    playing: true,
                    slot: 2,
                } => Some((key, velocity, tick)),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 1);
        let (key, velocity, tick) = notes[0];
        assert_eq!((key, velocity), (64, 0.0));
        // 4800 frames at 120 BPM = 192 ticks after play started.
        assert!((tick - 192.0).abs() < 2.0, "{tick}");
        // The key went up: the voice releases to silence.
        let mut tail = vec![0.0; 48_000];
        p.process(&mut tail);
        assert!(tail[40_000..].iter().all(|s| s.abs() < 1e-4));
    }

    #[test]
    fn the_clock_anchor_places_a_moment_where_a_live_event_then_plays() {
        let (mut h, mut p, live) = live_synth(&[0]);
        h.send(EngineCommand::SetLiveChannel(Some(0))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        let ms = 1_000_000;
        let t = std::sync::Arc::clone(&p.telemetry);
        let mut buf = vec![0.0; 480];
        assert_eq!(t.anchor.tick_at(1_000 * ms), None, "nothing published yet");
        for k in 0..5 {
            p.process_at(&mut buf, (1_000 + 10 * k) * ms);
        }
        // A moment 2.5 ms into the period after the last callback.
        let at = 1_042 * ms + ms / 2;
        let expected = t.anchor.tick_at(at).unwrap();
        live.send(crate::LiveEvent {
            kind: crate::LiveKind::Note(crate::LiveNote::on(60, 1.0)),
            port: 0,
            channel: 0,
            at_ns: at,
        });
        p.process_at(&mut buf, 1_050 * ms);
        p.process_at(&mut buf, 1_060 * ms);
        let mut ticks = Vec::new();
        h.poll_events(|e| {
            if let crate::EngineEvent::LiveNote { tick, .. } = e {
                ticks.push(tick);
            }
        });
        assert_eq!(ticks.len(), 1);
        assert!(
            (ticks[0] - expected).abs() < 0.05,
            "{} vs {expected}",
            ticks[0]
        );
        // Stopped: no position.
        h.send(EngineCommand::Stop).unwrap();
        p.process_at(&mut buf, 1_070 * ms);
        assert_eq!(t.anchor.tick_at(1_070 * ms), None);
    }

    #[test]
    fn lanes_report_their_values_and_stop_while_their_parameter_is_written() {
        use gt_core::{AutoPoint, Automation, ClipKind, ParamId};
        let (mut h, mut p) = engine(48_000, 2);
        let mut project = gt_core::Project::demo();
        let track = project.playlist.tracks[6].id;
        let target = ParamId::strip_pan(2);
        project.playlist.add_clip(
            track,
            0,
            3840,
            ClipKind::Automation(Automation {
                target,
                points: vec![AutoPoint::new(0, 0.75)],
            }),
        );
        let song = crate::SongSnapshot::compile_song(&project, |_| None);
        let i = song
            .automation
            .iter()
            .position(|l| l.target == target)
            .unwrap();
        let dest = song.automation[i].dest;
        h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        let t = std::sync::Arc::clone(&p.telemetry);
        let cell = &t.lane_values[i];
        assert!(cell.load(Ordering::Relaxed).is_nan());
        let mut buf = vec![0.0; 960];
        p.process(&mut buf);
        assert!(
            (cell.load(Ordering::Relaxed) - 0.5).abs() < 1e-6,
            "pan 0.75 is +0.5"
        );
        // Written: the lane is not applied (its value is not reported) until writing ends.
        h.send(EngineCommand::SetWriting(Box::new(vec![dest])))
            .unwrap();
        cell.store(9.0, Ordering::Relaxed);
        p.process(&mut buf);
        assert_eq!(cell.load(Ordering::Relaxed), 9.0);
        h.send(EngineCommand::SetWriting(Box::default())).unwrap();
        p.process(&mut buf);
        assert!((cell.load(Ordering::Relaxed) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pattern_lanes_repeat_with_the_pattern() {
        use gt_core::{AutoPoint, Automation, ParamId};
        let (mut h, mut p) = engine(48_000, 2);
        let mut project = gt_core::Project::empty();
        let target = ParamId::strip_pan(2);
        // Left for the first half bar, right for the second.
        project.current_pattern_mut().automation.push(Automation {
            target,
            points: vec![
                AutoPoint::new(0, 0.0),
                AutoPoint::new(1920, 0.0),
                AutoPoint::new(1921, 1.0),
            ],
        });
        let song = crate::SongSnapshot::compile(&project);
        assert_eq!(song.length, 3840);
        h.send(EngineCommand::SetSong(Box::new(song))).unwrap();
        h.send(EngineCommand::Play).unwrap();
        let t = std::sync::Arc::clone(&p.telemetry);
        // 25 frames per tick at 120 BPM: run to a frame, read the pan.
        let mut done = 0usize;
        let mut pan_at = |frame: usize| {
            let mut buf = vec![0.0; 2 * (frame - done)];
            p.process(&mut buf);
            done = frame;
            t.lane_values[0].load(Ordering::Relaxed)
        };
        assert_eq!(pan_at(25 * 960), -1.0);
        assert_eq!(pan_at(25 * 2880), 1.0);
        assert_eq!(pan_at(25 * (3840 + 960)), -1.0, "second pass");
        assert_eq!(pan_at(25 * (3840 + 2880)), 1.0);
    }

    /// A synth on `slot` with the metronome off and a connected live input.
    fn live_synth(slots: &[u16]) -> (EngineHandle, AudioProcessor, crate::LiveInput) {
        let (mut h, p) = engine(48_000, 1);
        let live = crate::LiveInput::default();
        live.connect(h.take_live_producer().unwrap());
        for &slot in slots {
            h.send(EngineCommand::SetChannelParams {
                slot,
                params: Box::new(crate::ChannelParams {
                    kind: crate::InstrumentKind::Synth,
                    gain: 1.0,
                    ..crate::ChannelParams::default()
                }),
            })
            .unwrap();
        }
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        (h, p, live)
    }

    fn first_sound(buf: &[f32]) -> Option<usize> {
        buf.iter().position(|s| s.abs() > 1e-6)
    }

    #[test]
    fn live_notes_play_one_period_later_at_their_place_in_the_period() {
        let ms = 1_000_000;
        // 480-frame callbacks (10 ms at 48 kHz), the first at 1 s. A key struck `into` ns after
        // the second callback started plays that far into the third.
        for (into, frame) in [(2_500_000, 120), (7_500_000, 360), (0, 0)] {
            let (mut h, mut p, live) = live_synth(&[0]);
            h.send(EngineCommand::SetLiveChannel(Some(0))).unwrap();
            let mut buf = vec![0.0; 480];
            p.process_at(&mut buf, 1_000 * ms);
            p.process_at(&mut buf, 1_010 * ms);
            assert_eq!(first_sound(&buf), None);
            live.send(crate::LiveEvent {
                kind: crate::LiveKind::Note(crate::LiveNote::on(60, 1.0)),
                port: 0,
                channel: 0,
                at_ns: 1_010 * ms + into,
            });
            p.process_at(&mut buf, 1_020 * ms);
            let f = first_sound(&buf).unwrap();
            assert!(
                (frame..frame + 3).contains(&f),
                "{into} ns: at {f}, not {frame}"
            );
        }
        // No time (the computer keyboard): at the next quantum.
        let (mut h, mut p, live) = live_synth(&[0]);
        h.send(EngineCommand::SetLiveChannel(Some(0))).unwrap();
        let mut buf = vec![0.0; 480];
        p.process_at(&mut buf, 1_000 * ms);
        live.send(crate::LiveNote::on(60, 1.0));
        p.process_at(&mut buf, 1_010 * ms);
        assert!(first_sound(&buf).unwrap() < 3);
    }

    #[test]
    fn routed_ports_and_channels_play_their_own_instruments() {
        let (mut h, mut p, live) = live_synth(&[1]);
        let mut routes = crate::LiveRoutes::default();
        routes.set(0, Some(3), Some(1));
        h.send(EngineCommand::SetLiveRoutes(Box::new(routes)))
            .unwrap();
        // The selected channel (slot 0) has no instrument: only the route sounds.
        h.send(EngineCommand::SetLiveChannel(Some(0))).unwrap();
        let note = |port, channel, on| crate::LiveEvent {
            kind: crate::LiveKind::Note(if on {
                crate::LiveNote::on(60, 1.0)
            } else {
                crate::LiveNote::off(60)
            }),
            port,
            channel,
            at_ns: 0,
        };
        let mut buf = vec![0.0; 4800];
        for (port, channel) in [(0, 0), (1, 3), (crate::KEYBOARD, 3)] {
            live.send(note(port, channel, true));
            p.process(&mut buf);
            assert_eq!(first_sound(&buf), None, "port {port} channel {channel}");
            live.send(note(port, channel, false));
        }
        live.send(note(0, 3, true));
        p.process(&mut buf);
        assert!(first_sound(&buf).is_some());
        // Rerouting while the key is down: its release still reaches the instrument it played.
        h.send(EngineCommand::SetLiveRoutes(Box::default()))
            .unwrap();
        p.process(&mut buf);
        live.send(note(0, 3, false));
        let mut tail = vec![0.0_f32; 96_000];
        p.process(&mut tail);
        assert!(tail[80_000..].iter().all(|s| s.abs() < 1e-4));
    }

    #[test]
    fn plugins_play_as_instruments_and_effects_and_are_bypassed_when_they_fail() {
        use crate::plugin::tests::fake;
        use gt_core::{PluginInstanceId, MASTER};
        let (mut h, mut p) = engine(48_000, 2);
        let synth = PluginInstanceId(9001);
        let fx = PluginInstanceId(9002);
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        // The channel names its plugin before the plugin arrives: silent until it does.
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(crate::ChannelParams {
                kind: crate::InstrumentKind::Plugin,
                plugin: Some(synth),
                gain: 1.0,
                pan: 0.0,
                route: MASTER as u16,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        let mut buf = vec![0.0; 2 * 256];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
        h.send(EngineCommand::SetPlugin {
            index: 5,
            plugin: Some(fake(synth, 1.0)),
        })
        .unwrap();
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        p.process(&mut buf);
        let level = buf[buf.len() - 2];
        // The fake instrument outputs 0.5 while a note is held; centre pan is -3 dB.
        assert!(
            (level - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3,
            "{level}"
        );

        // An effect plugin on the master halves the signal; its parameter sets the gain.
        let mut mixer = crate::MixerParams::default();
        mixer.strips[MASTER].fx_plugin[0] = Some(fx);
        mixer.strips[MASTER].enabled[0] = true;
        h.send(EngineCommand::SetMixer(Box::new(mixer))).unwrap();
        h.send(EngineCommand::SetPlugin {
            index: 6,
            plugin: Some(fake(fx, 0.5)),
        })
        .unwrap();
        p.process(&mut buf);
        let half = buf[buf.len() - 2];
        assert!((half - level * 0.5).abs() < 1e-3, "{half}");
        h.send(EngineCommand::SetPluginParam {
            index: 6,
            param: 0,
            value: 0.25,
        })
        .unwrap();
        p.process(&mut buf);
        assert!((buf[buf.len() - 2] - level * 0.25).abs() < 1e-3);

        // The effect starts producing NaN: it is bypassed and flagged, the mix stays finite.
        h.send(EngineCommand::SetPluginParam {
            index: 6,
            param: 1,
            value: 1.0,
        })
        .unwrap();
        p.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()));
        assert!((buf[buf.len() - 2] - level).abs() < 1e-3, "passes through");
        assert!(h.telemetry().plugin_failed[6].load(Ordering::Relaxed));
        assert!(!h.telemetry().plugin_failed[5].load(Ordering::Relaxed));

        // Taking a plugin out hands it back as garbage; its channel goes quiet.
        h.send(EngineCommand::SetPlugin {
            index: 5,
            plugin: None,
        })
        .unwrap();
        p.process(&mut buf);
        assert_eq!(
            h.collect_garbage(),
            3,
            "the mixer box, the params box and the plugin"
        );
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn plugin_instruments_with_latency_are_lined_up_with_the_others() {
        use crate::plugin::tests::FakePlugin;
        use gt_core::{PluginInstanceId, MASTER};
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        for (slot, id, latency) in [(0u16, 9101u64, 0usize), (1, 9102, 40)] {
            h.send(EngineCommand::SetChannelParams {
                slot,
                params: Box::new(crate::ChannelParams {
                    kind: crate::InstrumentKind::Plugin,
                    plugin: Some(PluginInstanceId(id)),
                    gain: 1.0,
                    pan: 0.0,
                    route: MASTER as u16,
                    ..crate::ChannelParams::default()
                }),
            })
            .unwrap();
            h.send(EngineCommand::SetPlugin {
                index: slot as u8,
                plugin: Some(Box::new(crate::PluginBox {
                    instance: PluginInstanceId(id),
                    processor: Box::new(FakePlugin {
                        gain: 1.0,
                        held: 0,
                        nan: false,
                        latency,
                    }),
                })),
            })
            .unwrap();
        }
        let mut buf = vec![0.0; 2 * 128];
        p.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
        // The instrument without latency waits for the one with 40 frames.
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        p.process(&mut buf);
        let first = buf.iter().position(|&s| s != 0.0).unwrap() / 2;
        assert_eq!(first, 40);
        assert_eq!(
            h.telemetry().latency_frames.load(Ordering::Relaxed),
            40,
            "the master reports the instrument latency"
        );
    }

    /// An instrument with one extra output (a constant) that echoes each note an octave up
    /// to its MIDI output.
    struct ArpPlugin {
        extra: [f32; crate::RENDER_QUANTUM],
        echo: Option<(u32, u8)>,
    }

    impl crate::PluginProcessor for ArpPlugin {
        fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &crate::PluginContext) -> bool {
            l.fill(0.0);
            r.fill(0.0);
            true
        }
        fn note_on(&mut self, offset: u32, key: u8, _velocity: f32) {
            self.echo = Some((offset, key + 12));
        }
        fn note_off(&mut self, _offset: u32, _key: u8) {}
        fn release_all(&mut self, _offset: u32) {}
        fn set_param(&mut self, _index: u32, _value: f32) {}
        fn reset(&mut self) {}
        fn latency(&self) -> usize {
            0
        }
        fn extra_outputs(&self) -> usize {
            1
        }
        fn extra_output(&self, k: usize) -> Option<(&[f32], &[f32])> {
            (k == 0).then_some((&self.extra[..], &self.extra[..]))
        }
        fn midi_out(&mut self, sink: &mut dyn FnMut(u32, [u8; 3])) {
            if let Some((offset, key)) = self.echo.take() {
                sink(offset, [0x90, key, 100]);
            }
        }
    }

    #[test]
    fn extra_outputs_play_into_their_inserts_and_plugin_midi_reaches_another_channel() {
        use crate::plugin::tests::FakePlugin;
        use gt_core::{PluginInstanceId, MASTER};
        let (mut h, mut p) = engine(48_000, 2);
        h.send(EngineCommand::SetMetronome(false)).unwrap();
        let arp = |extra: Option<u16>, midi_out: Option<u16>| crate::ChannelParams {
            kind: crate::InstrumentKind::Plugin,
            plugin: Some(PluginInstanceId(9201)),
            gain: 1.0,
            route: MASTER as u16,
            extra_routes: {
                let mut r = [None; gt_core::MAX_EXTRA_OUTPUTS];
                r[0] = extra;
                r
            },
            midi_out,
            ..crate::ChannelParams::default()
        };
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(arp(None, Some(1))),
        })
        .unwrap();
        h.send(EngineCommand::SetChannelParams {
            slot: 1,
            params: Box::new(crate::ChannelParams {
                kind: crate::InstrumentKind::Plugin,
                plugin: Some(PluginInstanceId(9202)),
                gain: 1.0,
                route: MASTER as u16,
                ..crate::ChannelParams::default()
            }),
        })
        .unwrap();
        let fake = FakePlugin {
            gain: 1.0,
            held: 0,
            nan: false,
            latency: 0,
        };
        for (index, id, processor) in [
            (
                0u8,
                9201u64,
                Box::new(ArpPlugin {
                    extra: [0.25; crate::RENDER_QUANTUM],
                    echo: None,
                }) as Box<dyn crate::PluginProcessor>,
            ),
            (1, 9202, Box::new(fake)),
        ] {
            h.send(EngineCommand::SetPlugin {
                index,
                plugin: Some(Box::new(crate::PluginBox {
                    instance: PluginInstanceId(id),
                    processor,
                })),
            })
            .unwrap();
        }
        let mut buf = vec![0.0; 2 * 128];
        p.process(&mut buf);
        assert!(
            buf.iter().all(|&s| s == 0.0),
            "an unrouted extra output is not heard"
        );

        // Routed to the master, the extra output is heard.
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(arp(Some(MASTER as u16), Some(1))),
        })
        .unwrap();
        for _ in 0..20 {
            p.process(&mut buf);
        }
        assert!(buf.iter().all(|&s| s > 0.1), "{:?}", &buf[..4]);

        // A note on the arpeggiator comes out of its MIDI output and plays channel 2.
        h.send(EngineCommand::SetChannelParams {
            slot: 0,
            params: Box::new(arp(None, Some(1))),
        })
        .unwrap();
        for _ in 0..20 {
            p.process(&mut buf);
        }
        assert!(buf.iter().all(|&s| s == 0.0));
        h.send(EngineCommand::NoteOn {
            slot: 0,
            key: 60,
            velocity: 1.0,
        })
        .unwrap();
        p.process(&mut buf);
        let first = buf.iter().position(|&s| s != 0.0).unwrap() / 2;
        assert_eq!(first, crate::RENDER_QUANTUM, "played in the next block");
    }
}
