//! The audio-thread half of the engine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use gt_core::{Tick, MAX_CHANNELS, ROOT_KEY};
use gt_dsp::{db_to_gain, Click, LinearRamp, SineOsc};
use rtrb::{Consumer, Producer};

use crate::channel::ChannelSlot;
use crate::command::{EngineCommand, EngineEvent, Garbage, TransportState};
use crate::control::{beats_of, ModClock, ModPlan, ParamDest};
use crate::live::LiveNote;
use crate::mixer::MixerEngine;
use crate::plugin::{PluginContext, PluginTable};
use crate::song::{ChannelParams, SongSnapshot};
use crate::transport::{EventBuf, EventKind, ScheduledEvent, Transport};
use crate::{EngineConfig, Telemetry, FADE_SECONDS, RENDER_QUANTUM, SCOPE_LEN};

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
/// Live notes applied per quantum at most.
const MAX_LIVE_PER_QUANTUM: usize = 64;

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
    live: Consumer<LiveNote>,
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
    scratch_r: [f32; Q],

    song: Option<Box<SongSnapshot>>,
    /// Song tick where the last control step ended (NaN when stopped).
    control_tick: f64,
    /// Modulators, grouped by parameter.
    mods: Option<Box<ModPlan>>,
    /// `MAX_CHANNELS` slots, allocated once in `new`.
    channels: Vec<ChannelSlot>,
    preview: ChannelSlot,
    /// Increments with every note-on; orders voices for stealing.
    voice_age: u64,
    /// Channel slot that feeds the oscilloscope.
    scope_slot: Option<usize>,
    /// Mixer strips, effects and routing.
    mixer: Box<MixerEngine>,
    /// Hosted plugins (instruments and effects).
    plugins: Box<PluginTable>,

    click: Click,
    click_gain: f32,
    metronome: bool,
    count_in: Option<CountIn>,

    /// Slot that live notes play, and per key the slot + 1 holding a live note (0: none).
    live_slot: Option<u16>,
    live_held: [u16; 128],

    tone: SineOsc,
    tone_ramp: LinearRamp,
    tone_gain: f32,
    tone_on: bool,

    master: LinearRamp,
    fade_frames: u32,
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
        live: Consumer<LiveNote>,
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
            scratch_r: [0.0; Q],
            song: None,
            control_tick: f64::NAN,
            mods: None,
            channels: (0..MAX_CHANNELS).map(|_| ChannelSlot::new(sr)).collect(),
            preview,
            voice_age: 0,
            scope_slot: None,
            mixer: Box::new(MixerEngine::new(sr)),
            plugins: Box::new(PluginTable::new()),
            click: Click::new(sr),
            click_gain: db_to_gain(CLICK_DBFS),
            metronome: true,
            count_in: None,
            live_slot: None,
            live_held: [0; 128],
            tone: SineOsc::new(sr, TEST_TONE_HZ),
            tone_ramp: LinearRamp::new(0.0),
            tone_gain: db_to_gain(TEST_TONE_DBFS),
            tone_on: false,
            master: LinearRamp::new(1.0),
            fade_frames: (sr * FADE_SECONDS).round() as u32,
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

    /// The telemetry this processor publishes (also visible through the `EngineHandle`).
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Fills `out`, an interleaved buffer of `frames * out_channels` samples, from the stream of
    /// fixed-size quanta. Left and right go to the first two device channels (further channels
    /// get silence); a mono device gets their average. A trailing partial frame (which a correct
    /// device never delivers) is zeroed.
    pub fn process(&mut self, out: &mut [f32]) {
        let ch = self.out_channels;
        let frames = out.len() / ch;
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

        let t = &*self.telemetry;
        t.last_block_frames.store(frames as u32, Ordering::Relaxed);
        t.blocks.fetch_add(1, Ordering::Relaxed);
        t.peak.fetch_max(peak, Ordering::Relaxed);
    }

    /// Renders the next `RENDER_QUANTUM` frames into the quantum buffers.
    fn render_quantum(&mut self) {
        self.apply_commands();
        let count_click = self.advance_count_in();
        self.apply_live();

        self.quantum_l.fill(0.0);
        self.quantum_r.fill(0.0);
        self.mono.fill(0.0);
        self.transport.schedule(
            self.frame_clock,
            Q as u32,
            self.song.as_deref(),
            &mut self.scheduled,
        );

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

        self.apply_controls();
        let pctx = self.plugin_context();
        for slot in 0..self.channels.len() {
            self.render_channel(slot, &pctx);
        }
        self.render_audio_clips();
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
                } if usize::from(s) == slot => {
                    self.plugins.note_on(index, e.offset, key, velocity);
                }
                EventKind::NoteOff { slot: s, key } if usize::from(s) == slot => {
                    self.plugins.note_off(index, e.offset, key);
                }
                EventKind::Wrap => self.plugins.release_all(index, e.offset),
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
        let (ml, mr) = self.mixer.direct_mut(usize::from(ch.route()));
        let peak = ch.mix_into((&self.scratch_l, &self.scratch_r), (ml, mr));
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
            ch.render_voices(&mut sl[cursor..at], &mut sr[cursor..at]);
            cursor = at;
            match e.kind {
                EventKind::NoteOn { key, velocity, .. } => {
                    self.voice_age += 1;
                    ch.note_on(key, velocity, self.voice_age);
                }
                EventKind::NoteOff { key, .. } => ch.note_off(key),
                EventKind::Wrap => ch.release_all(),
                EventKind::Beat { .. } => {}
            }
        }
        ch.render_voices(&mut sl[cursor..], &mut sr[cursor..]);
        let (ml, mr) = self.mixer.direct_mut(usize::from(ch.route()));
        let peak = ch.mix_into((sl, sr), (ml, mr));
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
        let Some(song) = self.song.as_deref() else {
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
            for clip in &song.audio {
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
                let (dl, dr) = self.mixer.direct_mut(usize::from(clip.route));
                for i in i0..i1.min(n) {
                    let t = a + i as f64 / sr;
                    let pos = (t - clip.origin_s) * rate;
                    if pos < 0.0 {
                        continue;
                    }
                    let k = pos as usize;
                    if k >= len {
                        break;
                    }
                    let frac = (pos - k as f64) as f32;
                    let k1 = (k + 1).min(len - 1);
                    let mut edge = 1.0_f64;
                    if clip.fade_in {
                        edge = edge.min((t - clip.start_s) / CLIP_FADE_S);
                    }
                    if clip.fade_out {
                        edge = edge.min((clip.end_s - t) / CLIP_FADE_S);
                    }
                    let edge = edge.clamp(0.0, 1.0);
                    let g = clip.gain * edge as f32;
                    let o = seg.offset as usize + i;
                    dl[o] += (src_l[k] + (src_l[k1] - src_l[k]) * frac) * g;
                    dr[o] += (src_r[k] + (src_r[k1] - src_r[k]) * frac) * g;
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
        let has_lanes = song.is_some_and(|s| !s.repeat && !s.automation.is_empty());
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
        let jump =
            start.filter(|t| (t - self.control_tick).abs() > 0.5 || self.control_tick.is_nan());
        self.control_tick = tick.unwrap_or(f64::NAN);
        let lane_tick = tick.filter(|_| has_lanes);
        let ramp = Q as u32;
        if let (Some(t), Some(song)) = (lane_tick, song) {
            for lane in song.automation.iter().filter(|l| !l.modulated) {
                for (at, frames) in jump.map(|j| (j, 0)).into_iter().chain([(t, ramp)]) {
                    let v = gt_core::playlist::value_at(&lane.points, at);
                    apply_param(
                        &mut self.channels,
                        &mut self.mixer,
                        &mut self.plugins,
                        lane.dest,
                        lane.info.from_normalized(v),
                        frames,
                    );
                }
            }
        }
        if let Some(plan) = self.mods.as_deref_mut() {
            let clock = ModClock {
                dt: Q as f64 / self.sample_rate,
                bpm: self.transport.bpm_at(self.frame_clock),
                beats: tick.map(beats_of),
            };
            for target in &mut plan.targets {
                let base_at = |t: Option<f64>| match (target.lane, t.filter(|_| has_lanes), song) {
                    (Some(i), Some(t), Some(song)) => song
                        .automation
                        .get(i)
                        .map_or(target.base, |l| gt_core::playlist::value_at(&l.points, t)),
                    _ => target.base,
                };
                if jump.is_some() && target.lane.is_some() {
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
            EngineCommand::Play => self.transport.play(now),
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
            }
            EngineCommand::UpdateSong(song) => {
                if let Some(old) = self.song.replace(song) {
                    self.retire(Garbage::Song(old));
                }
                self.link_lanes();
            }
            EngineCommand::SetModulation(mut plan) => {
                self.clear_controls();
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
            EngineCommand::NoteOn {
                slot,
                key,
                velocity,
            } => self.slot_note_on(usize::from(slot), key, velocity),
            EngineCommand::NoteOff { slot, key } => self.slot_note_off(usize::from(slot), key),
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
            } => self.mixer.set_effect_param(
                usize::from(strip),
                usize::from(slot),
                usize::from(index),
                value,
            ),
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
                self.live_slot = slot.filter(|&s| usize::from(s) < self.channels.len());
            }
            EngineCommand::CountIn { bars } => self.start_count_in(bars),
        }
        self.after_transport_change(before);
    }

    /// Releases notes, resets effects and tells the UI when the transport changed state.
    fn after_transport_change(&mut self, before: TransportState) {
        let now = self.frame_clock;
        let after = self.transport.state();
        if before == TransportState::Playing && after != TransportState::Playing {
            self.release_all_channels();
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

    /// Plays the live notes that arrived since the last quantum, at its start, and reports
    /// them for recording while the transport plays.
    fn apply_live(&mut self) {
        let playing = self.transport.state() == TransportState::Playing;
        let tick = self.transport.position_at(self.frame_clock);
        for _ in 0..MAX_LIVE_PER_QUANTUM {
            let Ok(n) = self.live.pop() else {
                break;
            };
            let k = usize::from(n.key.min(127));
            if n.is_on() {
                // A key struck again before its release ends the old note first.
                if let Some(old) = self.live_held[k].checked_sub(1) {
                    self.slot_note_off(usize::from(old), n.key);
                    self.live_held[k] = 0;
                }
                if let Some(slot) = self.live_slot {
                    if usize::from(slot) < self.channels.len() {
                        self.slot_note_on(usize::from(slot), n.key, n.velocity);
                        self.live_held[k] = slot + 1;
                    }
                }
            } else if let Some(slot) = self.live_held[k].checked_sub(1) {
                self.slot_note_off(usize::from(slot), n.key);
                self.live_held[k] = 0;
            }
            if playing {
                self.post(EngineEvent::LiveNote {
                    key: n.key,
                    velocity: n.velocity,
                    tick,
                });
            }
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
        ParamDest::Effect { strip, slot, index } => mixer.set_effect_param(
            usize::from(strip),
            usize::from(slot),
            usize::from(index),
            value,
        ),
        ParamDest::Plugin { instance, index } => plugins.set_param(instance, index, value),
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
        // Stopped: played but not reported.
        let mut events = Vec::new();
        h.poll_events(|e| events.push(e));
        assert!(events.is_empty(), "{events:?}");

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
                route: MASTER as u8,
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
}
