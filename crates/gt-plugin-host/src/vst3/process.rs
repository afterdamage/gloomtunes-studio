//! A VST3 plugin as the engine drives it ([`gt_engine::PluginProcessor`]).
//!
//! The parameter change lists, their queues, the event lists and the audio buffers of every
//! bus are made when the plugin is activated, with fixed capacities, so processing never
//! allocates. Each block copies the engine's stereo block into the main input, hands the
//! queued notes and parameter changes over with their frame offsets and the transport, runs
//! the plugin and copies the main output back. Parameter changes the plugin reports go to the
//! UI thread through a wait-free ring.

use std::cell::{Cell, UnsafeCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use gt_engine::{PluginContext, PluginProcessor, RENDER_QUANTUM};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper};

use crate::processor::{ParamOut, MAX_EVENTS, MIDI_OUT};

/// Parameters changed in one block at most (the rest of a burst follows a block later).
const MAX_CHANGED: usize = 256;
/// Points per parameter per block.
const QUEUE_POINTS: usize = 16;

/// The changes of one parameter within a block.
pub(crate) struct ParamQueue {
    id: Cell<u32>,
    points: UnsafeCell<[(i32, f64); QUEUE_POINTS]>,
    len: Cell<usize>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ParamQueue {
    fn new() -> Self {
        Self {
            id: Cell::new(0),
            points: UnsafeCell::new([(0, 0.0); QUEUE_POINTS]),
            len: Cell::new(0),
        }
    }

    fn add(&self, offset: i32, value: f64) -> i32 {
        // SAFETY: only this object's methods touch the points, one call at a time (the
        // plugin's process call or the processor owning the list).
        let points = unsafe { &mut *self.points.get() };
        let n = self.len.get();
        if n < QUEUE_POINTS && (n == 0 || points[n - 1].0 < offset) {
            points[n] = (offset, value);
            self.len.set(n + 1);
            n as i32
        } else {
            // Same offset, out of order, or full: the latest value wins at the last point.
            let last = &mut points[n - 1];
            *last = (last.0.max(offset), value);
            (n - 1) as i32
        }
    }

    fn last(&self) -> Option<f64> {
        let n = self.len.get();
        // SAFETY: as in `add`.
        (n > 0).then(|| unsafe { (*self.points.get())[n - 1].1 })
    }
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.len.get() as i32
    }

    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut ParamValue) -> tresult {
        let i = index as usize;
        if index < 0 || i >= self.len.get() || offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        let (o, v) = (*self.points.get())[i];
        *offset = o;
        *value = v;
        kResultOk
    }

    unsafe fn addPoint(&self, offset: i32, value: ParamValue, index: *mut i32) -> tresult {
        let i = self.add(offset, value);
        if let Some(index) = index.as_mut() {
            *index = i;
        }
        kResultOk
    }
}

/// The parameter changes of one block.
pub(crate) struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            queues: (0..MAX_CHANGED)
                .map(|_| ComWrapper::new(ParamQueue::new()))
                .collect(),
            used: Cell::new(0),
        })
    }

    fn queue(&self, id: u32) -> Option<(usize, &ComWrapper<ParamQueue>)> {
        let used = self.used.get();
        if let Some(i) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            return Some((i, &self.queues[i]));
        }
        let q = self.queues.get(used)?;
        q.id.set(id);
        q.len.set(0);
        self.used.set(used + 1);
        Some((used, q))
    }

    /// Queues `value` for parameter `id` at frame `offset`.
    pub(crate) fn add(&self, id: u32, offset: i32, value: f64) {
        if let Some((_, q)) = self.queue(id) {
            q.add(offset, value);
        }
    }

    /// Each changed parameter with its last value.
    pub(crate) fn last_values(&self) -> impl Iterator<Item = (u32, f64)> + '_ {
        self.queues[..self.used.get()]
            .iter()
            .filter_map(|q| Some((q.id.get(), q.last()?)))
    }

    pub(crate) fn clear(&self) {
        self.used.set(0);
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        self.used.get() as i32
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.used.get() {
            return std::ptr::null_mut();
        }
        self.queues[index as usize]
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        let Some(&id) = id.as_ref() else {
            return std::ptr::null_mut();
        };
        match self.queue(id) {
            Some((i, q)) => {
                if let Some(index) = index.as_mut() {
                    *index = i as i32;
                }
                q.as_com_ref::<IParamValueQueue>()
                    .map_or(std::ptr::null_mut(), |r| r.as_ptr())
            }
            None => std::ptr::null_mut(),
        }
    }
}

/// The events of one block.
pub(crate) struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)),
        })
    }

    #[allow(clippy::mut_from_ref)]
    fn list(&self) -> &mut Vec<Event> {
        // SAFETY: one caller at a time (the processor, or the plugin inside its process
        // call); no reference outlives the call that took it.
        unsafe { &mut *self.events.get() }
    }

    /// Adds an event if there is room (never grows).
    pub(crate) fn push(&self, e: Event) -> bool {
        let list = self.list();
        if list.len() < list.capacity() {
            list.push(e);
            true
        } else {
            false
        }
    }

    pub(crate) fn clear(&self) {
        self.list().clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.list().len()
    }

    pub(crate) fn get(&self, i: usize) -> Option<Event> {
        self.list().get(i).copied()
    }

    /// Sorts by frame offset, keeping the order of events at the same offset (an insertion
    /// sort: in place, and the list is short and nearly sorted).
    pub(crate) fn sort(&self) {
        let list = self.list();
        for i in 1..list.len() {
            let mut j = i;
            while j > 0 && list[j - 1].sampleOffset > list[j].sampleOffset {
                list.swap(j - 1, j);
                j -= 1;
            }
        }
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        self.len() as i32
    }

    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        match (
            usize::try_from(index).ok().and_then(|i| self.get(i)),
            e.as_mut(),
        ) {
            (Some(ev), Some(out)) => {
                *out = ev;
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        match e.as_ref() {
            Some(ev) if self.push(*ev) => kResultOk,
            Some(_) => kResultFalse,
            None => kInvalidArgument,
        }
    }
}

/// Audio buffers of one direction: per bus, per channel.
struct Buses {
    bufs: Vec<Vec<[f32; RENDER_QUANTUM]>>,
    ptrs: Vec<Vec<*mut f32>>,
    raw: Vec<AudioBusBuffers>,
}

impl Buses {
    fn new(channels: &[usize]) -> Self {
        Self {
            bufs: channels
                .iter()
                .map(|&n| vec![[0.0; RENDER_QUANTUM]; n])
                .collect(),
            ptrs: channels
                .iter()
                .map(|&n| vec![std::ptr::null_mut(); n])
                .collect(),
            raw: channels
                .iter()
                .map(|&n| AudioBusBuffers {
                    numChannels: n as i32,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 {
                        channelBuffers32: std::ptr::null_mut(),
                    },
                })
                .collect(),
        }
    }

    /// Clears the buffers and points the bus records at them.
    fn prepare(&mut self, n: usize) {
        for ((bufs, ptrs), raw) in self
            .bufs
            .iter_mut()
            .zip(self.ptrs.iter_mut())
            .zip(self.raw.iter_mut())
        {
            for (b, p) in bufs.iter_mut().zip(ptrs.iter_mut()) {
                b[..n].fill(0.0);
                *p = b.as_mut_ptr();
            }
            raw.silenceFlags = 0;
            raw.__field0.channelBuffers32 = ptrs.as_mut_ptr();
        }
    }
}

/// What activation found out about the plugin.
pub(crate) struct Setup {
    pub(crate) inputs: Vec<usize>,
    pub(crate) outputs: Vec<usize>,
    pub(crate) note_input: bool,
    pub(crate) effect: bool,
    pub(crate) latency: usize,
    pub(crate) sample_rate: f64,
    pub(crate) offline: bool,
    /// Parameter ids for continuous controllers 0..127, channel pressure (128) and pitch
    /// bend (129), from `IMidiMapping`.
    pub(crate) midi_map: [Option<u32>; 130],
}

/// The engine-side plugin.
pub(crate) struct Vst3Processor {
    processor: ComPtr<IAudioProcessor>,
    /// Held while the engine has the processor; the instance deactivates only without it.
    _token: Arc<()>,
    ids: Arc<[u32]>,
    /// (id, index) sorted by id, for the plugin's reports.
    by_id: Vec<(u32, u32)>,
    in_changes: ComWrapper<ParamChanges>,
    out_changes: ComWrapper<ParamChanges>,
    in_events: ComWrapper<EventList>,
    out_events: ComWrapper<EventList>,
    inputs: Buses,
    outputs: Buses,
    context: ProcessContext,
    setup: Setup,
    queue: rtrb::Producer<ParamOut>,
    held: [bool; 128],
    processing: bool,
    /// MIDI the plugin sent in the last block (notes and controllers), and how much.
    midi: [(u32, [u8; 3]); MIDI_OUT],
    midi_len: usize,
    /// Frames of the last block.
    frames: usize,
}

// SAFETY: the processor is used by one thread at a time: built on the UI thread, then only
// touched by the audio thread (or the export) until it is handed back and dropped. VST3
// requires `process` and `setProcessing` to be called from one thread at a time, which this
// guarantees. The raw pointers point into buffers this struct owns.
#[allow(unsafe_code)]
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    pub(crate) fn new(
        processor: ComPtr<IAudioProcessor>,
        token: Arc<()>,
        ids: Arc<[u32]>,
        setup: Setup,
        queue: rtrb::Producer<ParamOut>,
    ) -> Self {
        let mut by_id: Vec<(u32, u32)> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();
        by_id.sort_unstable();
        // SAFETY: ProcessContext is plain data; all zero is a valid value.
        #[allow(unsafe_code)]
        let context: ProcessContext = unsafe { std::mem::zeroed() };
        Self {
            processor,
            _token: token,
            ids,
            by_id,
            in_changes: ParamChanges::new(),
            out_changes: ParamChanges::new(),
            in_events: EventList::new(),
            out_events: EventList::new(),
            inputs: Buses::new(&setup.inputs),
            outputs: Buses::new(&setup.outputs),
            context,
            setup,
            queue,
            held: [false; 128],
            processing: false,
            midi: [(0, [0; 3]); MIDI_OUT],
            midi_len: 0,
            frames: 0,
        }
    }

    fn event(offset: u32, kind: u32) -> Event {
        // SAFETY: Event is plain data; all zero is a valid value.
        #[allow(unsafe_code)]
        let mut e: Event = unsafe { std::mem::zeroed() };
        e.busIndex = 0;
        e.sampleOffset = offset.min(RENDER_QUANTUM as u32 - 1) as i32;
        e.r#type = kind as u16;
        e
    }

    fn note(&mut self, offset: u32, key: u8, velocity: f32, on: bool) {
        if !self.setup.note_input {
            return;
        }
        let key = key & 0x7F;
        self.held[usize::from(key)] = on;
        let e = if on {
            let mut e = Self::event(offset, Event_::EventTypes_::kNoteOnEvent as u32);
            e.__field0.noteOn = NoteOnEvent {
                channel: 0,
                pitch: i16::from(key),
                tuning: 0.0,
                velocity: velocity.clamp(0.0, 1.0),
                length: 0,
                noteId: -1,
            };
            e
        } else {
            let mut e = Self::event(offset, Event_::EventTypes_::kNoteOffEvent as u32);
            e.__field0.noteOff = NoteOffEvent {
                channel: 0,
                pitch: i16::from(key),
                velocity: 0.0,
                noteId: -1,
                tuning: 0.0,
            };
            e
        };
        self.in_events.push(e);
    }

    fn update_context(&mut self, ctx: &PluginContext) {
        use ProcessContext_::StatesAndFlags_::*;
        let c = &mut self.context;
        let mut state = kTempoValid
            | kTimeSigValid
            | kProjectTimeMusicValid
            | kBarPositionValid
            | kContTimeValid;
        if ctx.playing {
            state |= kPlaying;
        }
        c.state = state as u32;
        c.sampleRate = self.setup.sample_rate;
        c.tempo = ctx.bpm;
        c.projectTimeMusic = ctx.beats;
        c.barPositionMusic = ctx.bar_start_beats;
        c.projectTimeSamples =
            (ctx.beats * 60.0 / ctx.bpm.max(1.0) * self.setup.sample_rate).round() as i64;
        c.continousTimeSamples = ctx.steady_frames as i64;
        c.timeSigNumerator = i32::from(ctx.sig_num);
        c.timeSigDenominator = i32::from(ctx.sig_den);
    }

    /// Hands the plugin's parameter reports to the UI and clears its output lists.
    fn drain_outputs(&mut self) {
        for (id, value) in self.out_changes.last_values() {
            if let Ok(k) = self.by_id.binary_search_by_key(&id, |&(id, _)| id) {
                let _ = self.queue.push(ParamOut {
                    index: self.by_id[k].1,
                    value: value.clamp(0.0, 1.0) as f32,
                });
            }
        }
        self.out_changes.clear();
        self.midi_len = 0;
        for i in 0..self.out_events.len() {
            let Some(e) = self.out_events.get(i) else {
                break;
            };
            let at = e.sampleOffset.max(0) as u32;
            let seven = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
            use Event_::EventTypes_::*;
            // SAFETY: the union member read is the one the event's type names.
            #[allow(unsafe_code)]
            let msg = unsafe {
                match u32::from(e.r#type) {
                    t if t == kNoteOnEvent as u32 => {
                        let n = e.__field0.noteOn;
                        Some([0x90, (n.pitch & 0x7F) as u8, seven(n.velocity).max(1)])
                    }
                    t if t == kNoteOffEvent as u32 => {
                        Some([0x80, (e.__field0.noteOff.pitch & 0x7F) as u8, 0])
                    }
                    t if t == kPolyPressureEvent as u32 => {
                        let p = e.__field0.polyPressure;
                        Some([0xA0, (p.pitch & 0x7F) as u8, seven(p.pressure)])
                    }
                    t if t == kLegacyMIDICCOutEvent as u32 => {
                        let c = e.__field0.midiCCOut;
                        let (v, v2) = ((c.value as u8) & 0x7F, (c.value2 as u8) & 0x7F);
                        match c.controlNumber {
                            n @ 0..=127 => Some([0xB0, n, v]),
                            128 => Some([0xD0, v, 0]),
                            129 => Some([0xE0, v, v2]),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            };
            if let Some(msg) = msg {
                if self.midi_len < MIDI_OUT {
                    self.midi[self.midi_len] = (at, msg);
                    self.midi_len += 1;
                }
            }
        }
        self.out_events.clear();
    }
}

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], ctx: &PluginContext) -> bool {
        let n = l.len().min(r.len()).min(RENDER_QUANTUM);
        if n == 0 {
            return true;
        }
        if !self.processing {
            // SAFETY: an activated processor, on the processing thread.
            #[allow(unsafe_code)]
            unsafe {
                self.processor.setProcessing(1);
            }
            self.processing = true;
        }
        self.inputs.prepare(n);
        self.outputs.prepare(n);
        if let Some(bus) = self.inputs.bufs.first_mut() {
            match bus.as_mut_slice() {
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
        self.update_context(ctx);
        self.frames = n;
        self.in_events.sort();
        let iface = |c: &ComWrapper<ParamChanges>| {
            c.as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr())
        };
        let events = |e: &ComWrapper<EventList>| {
            e.as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr())
        };
        let mut data = ProcessData {
            processMode: if self.setup.offline {
                ProcessModes_::kOffline
            } else {
                ProcessModes_::kRealtime
            } as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: n as i32,
            numInputs: self.inputs.raw.len() as i32,
            numOutputs: self.outputs.raw.len() as i32,
            inputs: self.inputs.raw.as_mut_ptr(),
            outputs: self.outputs.raw.as_mut_ptr(),
            inputParameterChanges: iface(&self.in_changes),
            outputParameterChanges: iface(&self.out_changes),
            inputEvents: if self.setup.note_input {
                events(&self.in_events)
            } else {
                std::ptr::null_mut()
            },
            outputEvents: events(&self.out_events),
            processContext: &mut self.context,
        };
        let processor = &self.processor;
        // SAFETY: every pointer in `data` points into buffers and lists this processor owns,
        // sized for `n` frames and the bus layout the plugin reported at activation.
        #[allow(unsafe_code)]
        let result = catch_unwind(AssertUnwindSafe(|| unsafe { processor.process(&mut data) }));
        self.in_changes.clear();
        self.in_events.clear();
        self.drain_outputs();
        if !matches!(result, Ok(r) if r == kResultOk) {
            return false;
        }
        match self.outputs.bufs.first() {
            Some(bus) => match bus.as_slice() {
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
            None if !self.setup.effect => {
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

    fn midi(&mut self, offset: u32, msg: [u8; 3]) {
        // VST3 has no raw MIDI input: controllers reach the parameters the plugin maps them
        // to (`IMidiMapping`).
        let (slot, value) = match msg[0] & 0xF0 {
            0xB0 => (usize::from(msg[1] & 0x7F), f64::from(msg[2] & 0x7F) / 127.0),
            0xD0 => (128, f64::from(msg[1] & 0x7F) / 127.0),
            0xE0 => (
                129,
                f64::from((u16::from(msg[2] & 0x7F) << 7) | u16::from(msg[1] & 0x7F)) / 16383.0,
            ),
            _ => return,
        };
        if let Some(id) = self.setup.midi_map[slot] {
            let offset = offset.min(RENDER_QUANTUM as u32 - 1) as i32;
            self.in_changes.add(id, offset, value);
        }
    }

    fn key_pressure(&mut self, offset: u32, key: u8, value: f32) {
        if !self.setup.note_input {
            return;
        }
        let mut e = Self::event(offset, Event_::EventTypes_::kPolyPressureEvent as u32);
        e.__field0.polyPressure = PolyPressureEvent {
            channel: 0,
            pitch: i16::from(key & 0x7F),
            pressure: value.clamp(0.0, 1.0),
            noteId: -1,
        };
        self.in_events.push(e);
    }

    fn set_param(&mut self, index: u32, value: f32) {
        if let Some(&id) = self.ids.get(index as usize) {
            self.in_changes.add(id, 0, f64::from(value.clamp(0.0, 1.0)));
        }
    }

    fn reset(&mut self) {
        self.held = [false; 128];
        // Notes are dropped with the voices; parameter changes still queued are kept.
        self.in_events.clear();
        // Stopping and restarting processing is how VST3 clears voices and tails.
        if self.processing {
            // SAFETY: an activated processor, on the processing thread.
            #[allow(unsafe_code)]
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
                self.processor.setProcessing(0);
            }));
            self.processing = false;
        }
    }

    fn latency(&self) -> usize {
        self.setup.latency
    }

    fn extra_outputs(&self) -> usize {
        self.outputs.bufs.len().saturating_sub(1)
    }

    fn extra_output(&self, k: usize) -> Option<(&[f32], &[f32])> {
        let n = self.frames;
        match self.outputs.bufs.get(k + 1)?.as_slice() {
            [mono] => Some((&mono[..n], &mono[..n])),
            [left, right, ..] => Some((&left[..n], &right[..n])),
            [] => None,
        }
    }

    fn midi_out(&mut self, sink: &mut dyn FnMut(u32, [u8; 3])) {
        for &(offset, msg) in &self.midi[..self.midi_len] {
            sink(offset, msg);
        }
        self.midi_len = 0;
    }

    fn stop(&mut self) {
        if self.processing {
            // SAFETY: as in `reset`.
            #[allow(unsafe_code)]
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
                self.processor.setProcessing(0);
            }));
            self.processing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_lists_merge_points_and_never_grow() {
        let c = ParamChanges::new();
        c.add(7, 0, 0.1);
        c.add(7, 0, 0.2);
        c.add(9, 3, 0.5);
        c.add(7, 5, 0.3);
        let got: Vec<(u32, f64)> = c.last_values().collect();
        assert_eq!(got, vec![(7, 0.3), (9, 0.5)]);
        assert_eq!(c.queues[0].len.get(), 2, "same offset replaced");
        for id in 0..2 * MAX_CHANGED as u32 {
            c.add(1000 + id, 0, 0.0);
        }
        assert_eq!(c.used.get(), MAX_CHANGED);
        c.clear();
        assert_eq!(c.last_values().count(), 0);

        let e = EventList::new();
        for off in [5, 1, 5, 0] {
            let mut ev = Vst3Processor::event(off, 0);
            ev.flags = off as u16;
            e.push(ev);
        }
        e.sort();
        let offs: Vec<i32> = (0..e.len())
            .map(|i| e.get(i).unwrap().sampleOffset)
            .collect();
        assert_eq!(offs, vec![0, 1, 5, 5]);
        for _ in 0..MAX_EVENTS + 10 {
            e.push(Vst3Processor::event(0, 0));
        }
        assert_eq!(e.len(), MAX_EVENTS);
    }
}
