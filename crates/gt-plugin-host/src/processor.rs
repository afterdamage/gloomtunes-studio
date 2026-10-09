//! A CLAP plugin as the engine drives it ([`gt_engine::PluginProcessor`]).
//!
//! Everything is allocated when the plugin is activated: the event list, the audio buffers
//! for every port and the queue for the plugin's own parameter changes. Processing copies the
//! engine's stereo block into the plugin's main input, delivers the queued notes and parameter
//! changes at their frame offsets with the transport, runs the plugin and copies its main
//! output back. Parameter changes the plugin makes itself (from its editor, say) go to the UI
//! thread through a wait-free ring.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags,
};
use clack_host::events::io::{OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, Match};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, ClapId, SecondsTime};
use gt_engine::{PluginContext, PluginProcessor, RENDER_QUANTUM};

use crate::host::{GtHost, IN_PROCESS};

/// Events queued for one block at most (notes and parameter changes); more are dropped.
pub(crate) const MAX_EVENTS: usize = 512;
/// Parameter changes from the plugin waiting for the UI thread.
pub(crate) const OUT_QUEUE: usize = 1024;

/// How one parameter maps between its normalized (0..1) and plain values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ParamMap {
    pub(crate) id: u32,
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) stepped: bool,
}

impl ParamMap {
    pub(crate) fn to_plain(self, norm: f32) -> f64 {
        let v = self.min + f64::from(norm.clamp(0.0, 1.0)) * (self.max - self.min);
        if self.stepped {
            v.round()
        } else {
            v
        }
    }

    pub(crate) fn to_norm(self, plain: f64) -> f32 {
        let span = self.max - self.min;
        if span.abs() < 1e-12 || !plain.is_finite() {
            0.0
        } else {
            (((plain - self.min) / span) as f32).clamp(0.0, 1.0)
        }
    }
}

/// A parameter change the plugin made: index into its parameter list, normalized value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ParamOut {
    pub(crate) index: u32,
    pub(crate) value: f32,
}

/// Receives the plugin's output events: parameter changes go to the UI queue, the rest are
/// ignored. Never allocates.
struct OutputSink {
    params: Arc<[ParamMap]>,
    queue: rtrb::Producer<ParamOut>,
}

impl OutputEventBuffer for OutputSink {
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        if let Some(CoreEventSpace::ParamValue(e)) = event.as_core_event() {
            if let Some(id) = e.param_id() {
                if let Some(i) = self.params.iter().position(|p| p.id == id.get()) {
                    let value = self.params[i].to_norm(e.value());
                    let _ = self.queue.push(ParamOut {
                        index: i as u32,
                        value,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Port layout read at activation.
#[derive(Debug, Clone, Default)]
pub(crate) struct Layout {
    /// Channel count of each input port.
    pub(crate) inputs: Vec<usize>,
    /// Channel count of each output port.
    pub(crate) outputs: Vec<usize>,
    /// The main input and output port.
    pub(crate) main_in: Option<usize>,
    pub(crate) main_out: Option<usize>,
    /// The note input port, and whether it only takes MIDI.
    pub(crate) note_port: Option<(u16, bool)>,
}

/// The engine-side plugin.
pub(crate) struct ClapProcessor {
    audio: PluginAudioProcessor<GtHost>,
    start_failed: bool,
    params: Arc<[ParamMap]>,
    events: EventBuffer,
    out: OutputSink,
    layout: Layout,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_bufs: Vec<Vec<[f32; RENDER_QUANTUM]>>,
    out_bufs: Vec<Vec<[f32; RENDER_QUANTUM]>>,
    held: [bool; 128],
    effect: bool,
    latency: usize,
    steady: u64,
}

impl ClapProcessor {
    pub(crate) fn new(
        audio: StoppedPluginAudioProcessor<GtHost>,
        params: Arc<[ParamMap]>,
        layout: Layout,
        effect: bool,
        latency: usize,
        queue: rtrb::Producer<ParamOut>,
    ) -> Self {
        let bufs = |ports: &[usize]| -> Vec<Vec<[f32; RENDER_QUANTUM]>> {
            ports
                .iter()
                .map(|&n| vec![[0.0; RENDER_QUANTUM]; n])
                .collect()
        };
        Self {
            audio: audio.into(),
            start_failed: false,
            events: EventBuffer::with_capacity(MAX_EVENTS),
            out: OutputSink {
                params: Arc::clone(&params),
                queue,
            },
            params,
            in_ports: AudioPorts::with_capacity(layout.inputs.iter().sum(), layout.inputs.len()),
            out_ports: AudioPorts::with_capacity(layout.outputs.iter().sum(), layout.outputs.len()),
            in_bufs: bufs(&layout.inputs),
            out_bufs: bufs(&layout.outputs),
            layout,
            held: [false; 128],
            effect,
            latency,
            steady: 0,
        }
    }

    fn push(&mut self, event: &impl AsRef<UnknownEvent>) {
        if (self.events.len() as usize) < MAX_EVENTS {
            self.events.push(event);
        }
    }

    fn note(&mut self, offset: u32, key: u8, velocity: f32, on: bool) {
        let Some((port, midi_only)) = self.layout.note_port else {
            return;
        };
        let offset = offset.min(RENDER_QUANTUM as u32 - 1);
        self.held[usize::from(key & 0x7F)] = on;
        if midi_only {
            let vel = (velocity.clamp(0.0, 1.0) * 127.0).round() as u8;
            let status = if on { 0x90 } else { 0x80 };
            self.push(&MidiEvent::new(
                offset,
                port,
                [status, key, vel.max(u8::from(on))],
            ));
        } else {
            let pckn = Pckn::new(port, 0u16, u16::from(key), Match::All);
            if on {
                self.push(&NoteOnEvent::new(offset, pckn, f64::from(velocity)));
            } else {
                self.push(&NoteOffEvent::new(offset, pckn, 0.0));
            }
        }
    }

    fn transport(ctx: &PluginContext) -> TransportEvent {
        let mut flags = TransportFlags::HAS_TEMPO
            | TransportFlags::HAS_BEATS_TIMELINE
            | TransportFlags::HAS_TIME_SIGNATURE;
        if ctx.playing {
            flags |= TransportFlags::IS_PLAYING;
        }
        TransportEvent {
            header: EventHeader::new_core(0, EventFlags::empty()),
            flags,
            song_pos_beats: BeatTime::from_float(ctx.beats),
            song_pos_seconds: SecondsTime::from_int(0),
            tempo: ctx.bpm,
            tempo_inc: 0.0,
            loop_start_beats: BeatTime::from_int(0),
            loop_end_beats: BeatTime::from_int(0),
            loop_start_seconds: SecondsTime::from_int(0),
            loop_end_seconds: SecondsTime::from_int(0),
            bar_start: BeatTime::from_float(ctx.bar_start_beats),
            bar_number: ctx.bar,
            time_signature_numerator: ctx.sig_num,
            time_signature_denominator: ctx.sig_den,
        }
    }

    /// Runs the plugin on `n` frames already in the port buffers. False on error or panic.
    fn run(&mut self, n: usize, transport: &TransportEvent) -> bool {
        let Ok(processor) = self.audio.ensure_processing_started() else {
            self.start_failed = true;
            return false;
        };
        self.events.sort();
        let ins = self
            .in_ports
            .with_input_buffers(self.in_bufs.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    port.iter_mut().map(|b| InputChannel::variable(&mut b[..n])),
                ),
            }));
        let mut outs = self
            .out_ports
            .with_output_buffers(self.out_bufs.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    port.iter_mut().map(|b| &mut b[..n]),
                ),
            }));
        let events = self.events.as_input();
        let mut out = OutputEvents::from_buffer(&mut self.out);
        let steady = self.steady;
        IN_PROCESS.with(|f| f.set(true));
        let result = catch_unwind(AssertUnwindSafe(|| {
            processor.process(
                &ins,
                &mut outs,
                &events,
                &mut out,
                Some(steady),
                Some(transport),
            )
        }));
        IN_PROCESS.with(|f| f.set(false));
        matches!(result, Ok(Ok(_)))
    }
}

impl PluginProcessor for ClapProcessor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &PluginContext) -> bool {
        if self.start_failed {
            return false;
        }
        let n = l.len().min(r.len()).min(RENDER_QUANTUM);
        if n == 0 {
            return true;
        }
        for port in self.in_bufs.iter_mut().chain(self.out_bufs.iter_mut()) {
            for ch in port.iter_mut() {
                ch[..n].fill(0.0);
            }
        }
        if let Some(port) = self.layout.main_in.and_then(|i| self.in_bufs.get_mut(i)) {
            match port.as_mut_slice() {
                [mono] => {
                    for (m, (a, b)) in mono.iter_mut().zip(l.iter().zip(r.iter())).take(n) {
                        *m = 0.5 * (a + b);
                    }
                }
                [left, right, ..] => {
                    left[..n].copy_from_slice(&l[..n]);
                    right[..n].copy_from_slice(&r[..n]);
                }
                [] => {}
            }
        }
        let transport = Self::transport(ctx);
        let ok = self.run(n, &transport);
        self.events.clear();
        self.steady += n as u64;
        if !ok {
            return false;
        }
        match self.layout.main_out.and_then(|i| self.out_bufs.get(i)) {
            Some(port) => match port.as_slice() {
                [mono] => {
                    l[..n].copy_from_slice(&mono[..n]);
                    r[..n].copy_from_slice(&mono[..n]);
                }
                [left, right, ..] => {
                    l[..n].copy_from_slice(&left[..n]);
                    r[..n].copy_from_slice(&right[..n]);
                }
                [] => {}
            },
            None if !self.effect => {
                l.fill(0.0);
                r.fill(0.0);
            }
            None => {}
        }
        true
    }

    fn note_on(&mut self, offset: u32, key: u8, velocity: f32) {
        self.note(offset, key, velocity, true);
    }

    fn note_off(&mut self, offset: u32, key: u8) {
        self.note(offset, key, 0.0, false);
    }

    fn release_all(&mut self, offset: u32) {
        for key in 0..128u8 {
            if self.held[usize::from(key)] {
                self.note(offset, key, 0.0, false);
            }
        }
    }

    fn set_param(&mut self, index: u32, value: f32) {
        let Some(&p) = self.params.get(index as usize) else {
            return;
        };
        let event =
            ParamValueEvent::new(0, ClapId::new(p.id), Pckn::match_all(), p.to_plain(value));
        self.push(&event);
    }

    fn reset(&mut self) {
        self.held = [false; 128];
        // Parameter changes still queued must arrive; notes are dropped with the voices.
        let mut keep = [None; 64];
        let mut k = 0;
        for e in self.events.iter() {
            if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event() {
                if k < keep.len() {
                    keep[k] = p.param_id().map(|id| (id, p.value()));
                    k += 1;
                }
            }
        }
        self.events.clear();
        for (id, v) in keep.iter().flatten() {
            self.push(&ParamValueEvent::new(0, *id, Pckn::match_all(), *v));
        }
        let _ = catch_unwind(AssertUnwindSafe(|| self.audio.reset()));
        self.start_failed = false;
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn stop(&mut self) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.audio.ensure_processing_stopped();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_map_between_normalized_and_plain() {
        let gain = ParamMap {
            id: 1,
            min: -24.0,
            max: 24.0,
            stepped: false,
        };
        assert_eq!(gain.to_plain(0.5), 0.0);
        assert_eq!(gain.to_norm(12.0), 0.75);
        assert_eq!(gain.to_norm(100.0), 1.0);
        assert_eq!(gain.to_norm(f64::NAN), 0.0);
        let mode = ParamMap {
            id: 2,
            min: 0.0,
            max: 3.0,
            stepped: true,
        };
        assert_eq!(mode.to_plain(0.4), 1.0);
        assert_eq!(mode.to_plain(0.9), 3.0);
        let fixed = ParamMap {
            id: 3,
            min: 1.0,
            max: 1.0,
            stepped: false,
        };
        assert_eq!(fixed.to_norm(1.0), 0.0);
    }
}
