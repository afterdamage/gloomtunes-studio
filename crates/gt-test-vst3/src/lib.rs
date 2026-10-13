//! Two small VST3 plugins for testing GloomTunes Studio's plugin host (Prompt 18, D113).
//!
//! * **GT Test Gain VST3**: a stereo gain effect with a "Misbehave" switch that makes it output
//!   NaN or report an error, so the host's bypass can be tested.
//! * **GT Test Tone VST3**: a 4-voice sine instrument. It reports the last key played through a
//!   read-only parameter (the plugin to host parameter path), plays an octave lower on a second
//!   stereo output ("Sub", for multi-out routing) and echoes every note an octave higher on its
//!   event output (MIDI out, like an arpeggiator would).
//!
//! Each has a separate processor and edit controller, as most VST3 plugins do, and saves its
//! state. The crate builds as a `cdylib` (the binary inside a `.vst3` bundle) and as an `rlib`,
//! so the host's tests can call [`GetPluginFactory`] in process. It is not shipped with the app.

#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
// Enum constants have different integer types on different systems, so casts that are
// no-ops here are needed elsewhere.
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::{ptr, slice};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{uid, Class, ComPtr, ComRef, ComWrapper};

/// Class id of the gain effect's processor, as the host shows it (hex).
pub const GAIN_CID: TUID = uid(0x4754_5631, 0x4741_494E, 0x5052_4F43, 0x0000_0001);
const GAIN_CTRL_CID: TUID = uid(0x4754_5631, 0x4741_494E, 0x4354_524C, 0x0000_0001);
/// Class id of the tone instrument's processor.
pub const TONE_CID: TUID = uid(0x4754_5631, 0x544F_4E45, 0x5052_4F43, 0x0000_0001);
const TONE_CTRL_CID: TUID = uid(0x4754_5631, 0x544F_4E45, 0x4354_524C, 0x0000_0001);

/// The tone's parameters: volume, waveform (stepped: sine, square), last key (read-only).
pub const TONE_VOLUME: u32 = 10;
/// Waveform: 0 sine, 1 square.
pub const TONE_WAVE: u32 = 11;
/// Last key played, normalized key / 127.
pub const TONE_LAST_KEY: u32 = 12;
/// The gain effect's parameters: gain (normalized 0..1 is 0..2), misbehave (0 fine, 0.5 NaN,
/// 1 error).
pub const GAIN_GAIN: u32 = 0;
/// Misbehave switch.
pub const GAIN_MISBEHAVE: u32 = 1;

const VOICES: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gain,
    Tone,
}

struct Param {
    id: u32,
    name: &'static str,
    units: &'static str,
    steps: i32,
    default: f64,
    flags: i32,
}

const AUTO: i32 = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
const READ_ONLY: i32 = ParameterInfo_::ParameterFlags_::kIsReadOnly as i32;

impl Kind {
    fn params(self) -> &'static [Param] {
        match self {
            Kind::Gain => &[
                Param {
                    id: GAIN_GAIN,
                    name: "Gain",
                    units: "dB",
                    steps: 0,
                    default: 0.5,
                    flags: AUTO,
                },
                Param {
                    id: GAIN_MISBEHAVE,
                    name: "Misbehave",
                    units: "",
                    steps: 2,
                    default: 0.0,
                    flags: AUTO,
                },
            ],
            Kind::Tone => &[
                Param {
                    id: TONE_VOLUME,
                    name: "Volume",
                    units: "%",
                    steps: 0,
                    default: 0.5,
                    flags: AUTO,
                },
                Param {
                    id: TONE_WAVE,
                    name: "Wave",
                    units: "",
                    steps: 1,
                    default: 0.0,
                    flags: AUTO,
                },
                Param {
                    id: TONE_LAST_KEY,
                    name: "Last Key",
                    units: "",
                    steps: 127,
                    default: 60.0 / 127.0,
                    flags: READ_ONLY,
                },
            ],
        }
    }

    fn magic(self) -> &'static [u8; 4] {
        match self {
            Kind::Gain => b"GTV1",
            Kind::Tone => b"GTV2",
        }
    }

    /// The plugin's own text for a value.
    fn text(self, id: u32, v: f64) -> String {
        match (self, id) {
            (Kind::Gain, GAIN_GAIN) => {
                let g = 2.0 * v;
                if g <= 1e-6 {
                    "-inf dB".to_owned()
                } else {
                    format!("{:+.1} dB", 20.0 * g.log10())
                }
            }
            (Kind::Gain, GAIN_MISBEHAVE) => {
                ["Fine", "NaN", "Error"][(v * 2.0).round() as usize % 3].to_owned()
            }
            (Kind::Tone, TONE_VOLUME) => format!("{:.0} %", v * 100.0),
            (Kind::Tone, TONE_WAVE) => if v >= 0.5 { "Square" } else { "Sine" }.to_owned(),
            (Kind::Tone, TONE_LAST_KEY) => format!("{}", (v * 127.0).round()),
            _ => format!("{v:.3}"),
        }
    }
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let n = src.len().min(dst.len() - 1);
    for (d, s) in dst.iter_mut().zip(src.bytes().take(n)) {
        *d = s as c_char;
    }
    dst[n] = 0;
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let (mut n, max) = (0, dst.len() - 1);
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()).take(max) {
        *d = s as TChar;
        n += 1;
    }
    dst[n] = 0;
}

/// Reads up to `n` bytes from a host stream.
unsafe fn read_stream(stream: *mut IBStream, n: usize) -> Vec<u8> {
    let Some(s) = ComRef::from_raw(stream) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; n];
    let mut got = 0;
    if s.read(buf.as_mut_ptr().cast(), n as i32, &mut got) != kResultOk {
        return Vec::new();
    }
    buf.truncate(got.max(0) as usize);
    buf
}

unsafe fn write_stream(stream: *mut IBStream, data: &[u8]) -> tresult {
    let Some(s) = ComRef::from_raw(stream) else {
        return kInvalidArgument;
    };
    let mut done = 0;
    s.write(data.as_ptr() as *mut c_void, data.len() as i32, &mut done)
}

/// State: magic, then (id u32, value f64) for each parameter.
fn encode(kind: Kind, values: &[f64]) -> Vec<u8> {
    let mut out = kind.magic().to_vec();
    for (p, v) in kind.params().iter().zip(values) {
        out.extend_from_slice(&p.id.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn decode(kind: Kind, data: &[u8]) -> Option<Vec<f64>> {
    let n = kind.params().len();
    if data.len() < 4 + 12 * n || &data[..4] != kind.magic() {
        return None;
    }
    let mut values = Vec::with_capacity(n);
    for (i, p) in kind.params().iter().enumerate() {
        let at = 4 + 12 * i;
        let id = u32::from_le_bytes(data[at..at + 4].try_into().ok()?);
        let v = f64::from_le_bytes(data[at + 4..at + 12].try_into().ok()?);
        if id != p.id || !v.is_finite() {
            return None;
        }
        values.push(v.clamp(0.0, 1.0));
    }
    Some(values)
}

#[derive(Clone, Copy, Default)]
struct Voice {
    key: Option<u8>,
    phase: f32,
    level: f32,
    target: f32,
}

/// The audio part of either plugin.
struct Processor {
    kind: Kind,
    values: RefCell<Vec<f64>>,
    rate: Cell<f64>,
    voices: Cell<[Voice; VOICES]>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl Processor {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            values: RefCell::new(kind.params().iter().map(|p| p.default).collect()),
            rate: Cell::new(48_000.0),
            voices: Cell::new([Voice::default(); VOICES]),
        }
    }

    fn set(&self, id: u32, v: f64) {
        if let Some(i) = self.kind.params().iter().position(|p| p.id == id) {
            self.values.borrow_mut()[i] = v.clamp(0.0, 1.0);
        }
    }

    fn value(&self, id: u32) -> f64 {
        let i = self
            .kind
            .params()
            .iter()
            .position(|p| p.id == id)
            .unwrap_or(0);
        self.values.borrow()[i]
    }

    fn audio_buses(&self, dir: BusDirection) -> i32 {
        match (self.kind, dir as BusDirections) {
            (Kind::Gain, _) => 1,
            (Kind::Tone, BusDirections_::kOutput) => 2,
            (Kind::Tone, _) => 0,
        }
    }

    fn event_buses(&self, dir: BusDirection) -> i32 {
        i32::from(self.kind == Kind::Tone && dir == BusDirections_::kInput as BusDirection)
            + i32::from(self.kind == Kind::Tone && dir == BusDirections_::kOutput as BusDirection)
    }

    /// Tone: handles one event and returns the key it started, if any.
    fn event(&self, e: &Event) -> Option<u8> {
        let mut voices = self.voices.get();
        let started = match u32::from(e.r#type) {
            t if t == Event_::EventTypes_::kNoteOnEvent as u32 => {
                // SAFETY: the type says which union member is set.
                let on = unsafe { e.__field0.noteOn };
                let key = on.pitch.clamp(0, 127) as u8;
                let v = voices
                    .iter()
                    .position(|v| v.key.is_none() && v.level < 1e-4)
                    .unwrap_or(0);
                voices[v] = Voice {
                    key: Some(key),
                    phase: 0.0,
                    level: 0.0,
                    target: on.velocity.clamp(0.0, 1.0),
                };
                Some(key)
            }
            t if t == Event_::EventTypes_::kNoteOffEvent as u32 => {
                // SAFETY: as above.
                let off = unsafe { e.__field0.noteOff };
                for v in voices.iter_mut().filter(|v| v.key == Some(off.pitch as u8)) {
                    v.key = None;
                    v.target = 0.0;
                }
                None
            }
            _ => None,
        };
        self.voices.set(voices);
        started
    }

    /// Tone: renders `range` of the block into the main and sub outputs.
    fn render(
        &self,
        main: [&mut [f32]; 2],
        sub: Option<[&mut [f32]; 2]>,
        range: std::ops::Range<usize>,
    ) {
        let volume = self.value(TONE_VOLUME) as f32;
        let square = self.value(TONE_WAVE) >= 0.5;
        let rate = self.rate.get() as f32;
        let mut voices = self.voices.get();
        let [ml, mr] = main;
        let mut sub = sub;
        for i in range {
            let (mut m, mut s) = (0.0, 0.0);
            for v in voices.iter_mut() {
                if v.level < 1e-5 && v.target == 0.0 {
                    continue;
                }
                let key = v.key.map_or(60.0, f32::from);
                let freq = 440.0 * 2f32.powf((key - 69.0) / 12.0);
                v.level += (v.target - v.level) * (1.0 - (-1.0 / (0.005 * rate)).exp());
                let x = (v.phase * std::f32::consts::TAU).sin();
                m += if square { x.signum() } else { x } * v.level;
                s += (v.phase * std::f32::consts::PI).sin() * v.level;
                v.phase = (v.phase + freq / rate).fract();
            }
            ml[i] = m * volume * 0.25;
            mr[i] = m * volume * 0.25;
            if let Some([sl, sr]) = sub.as_mut() {
                sl[i] = s * volume * 0.25;
                sr[i] = s * volume * 0.25;
            }
        }
        self.voices.set(voices);
    }
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        *class_id = match self.kind {
            Kind::Gain => GAIN_CTRL_CID,
            Kind::Tone => TONE_CTRL_CID,
        };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> i32 {
        match media as MediaTypes {
            MediaTypes_::kAudio => self.audio_buses(dir),
            MediaTypes_::kEvent => self.event_buses(dir),
            _ => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        let count = self.getBusCount(media, dir);
        if index < 0 || index >= count || bus.is_null() {
            return kInvalidArgument;
        }
        let bus = &mut *bus;
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = if media == MediaTypes_::kAudio as MediaType {
            2
        } else {
            1
        };
        let name = match (media as MediaTypes, index) {
            (MediaTypes_::kEvent, _) => "Notes",
            (_, 0) => "Main",
            _ => "Sub",
        };
        copy_wstring(name, &mut bus.name);
        bus.busType = if index == 0 {
            BusTypes_::kMain
        } else {
            BusTypes_::kAux
        } as BusType;
        bus.flags = if index == 0 {
            BusInfo_::BusFlags_::kDefaultActive
        } else {
            0
        } as u32;
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        self.voices.set([Voice::default(); VOICES]);
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        match decode(self.kind, &read_stream(state, 1024)) {
            Some(v) => {
                *self.values.borrow_mut() = v;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_stream(state, &encode(self.kind, &self.values.borrow()))
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        num_ins: i32,
        _outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if num_ins == self.audio_buses(BusDirections_::kInput as BusDirection)
            && num_outs == self.audio_buses(BusDirections_::kOutput as BusDirection)
        {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        _d: BusDirection,
        _i: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        *arr = SpeakerArr::kStereo;
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        // The gain reports a little latency so delay compensation can be tested.
        match self.kind {
            Kind::Gain => 16,
            Kind::Tone => 0,
        }
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if let Some(s) = setup.as_ref() {
            self.rate.set(s.sampleRate.max(1.0));
        }
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let Some(d) = data.as_ref() else {
            return kInvalidArgument;
        };
        if let Some(changes) = ComRef::from_raw(d.inputParameterChanges) {
            for i in 0..changes.getParameterCount() {
                if let Some(q) = ComRef::from_raw(changes.getParameterData(i)) {
                    let (mut offset, mut v) = (0, 0.0);
                    let n = q.getPointCount();
                    if n > 0 && q.getPoint(n - 1, &mut offset, &mut v) == kResultOk {
                        self.set(q.getParameterId(), v);
                    }
                }
            }
        }
        let n = d.numSamples.max(0) as usize;
        let outs = if d.outputs.is_null() {
            &mut [][..]
        } else {
            slice::from_raw_parts_mut(d.outputs, d.numOutputs.max(0) as usize)
        };
        let chans = |b: &AudioBusBuffers| -> Option<[&mut [f32]; 2]> {
            let p = b.__field0.channelBuffers32;
            if b.numChannels < 2 || p.is_null() || (*p).is_null() || (*p.add(1)).is_null() {
                return None;
            }
            Some([
                slice::from_raw_parts_mut(*p, n),
                slice::from_raw_parts_mut(*p.add(1), n),
            ])
        };
        match self.kind {
            Kind::Gain => {
                let misbehave = (self.value(GAIN_MISBEHAVE) * 2.0).round() as u32;
                if misbehave == 2 {
                    return kInternalError;
                }
                let gain = if misbehave == 1 {
                    f32::NAN
                } else {
                    2.0 * self.value(GAIN_GAIN) as f32
                };
                if d.numInputs < 1 || outs.is_empty() || d.inputs.is_null() {
                    return kResultOk;
                }
                let (Some(input), Some(output)) = (chans(&*d.inputs), chans(&outs[0])) else {
                    return kResultOk;
                };
                for c in 0..2 {
                    for i in 0..n {
                        output[c][i] = input[c][i] * gain;
                    }
                }
            }
            Kind::Tone => {
                let Some(main) = outs.first().and_then(&chans) else {
                    return kResultOk;
                };
                let sub = outs.get(1).and_then(&chans);
                let (main, mut sub) = (main, sub);
                // Events are sorted by offset; render up to each one.
                let events = ComRef::from_raw(d.inputEvents);
                let out_events = ComRef::from_raw(d.outputEvents);
                let count = events.map_or(0, |e| e.getEventCount());
                let mut at = 0usize;
                let mut last_key = None;
                let [ml, mr] = main;
                for k in 0..=count {
                    let mut e: Event = std::mem::zeroed();
                    let has =
                        k < count && events.is_some_and(|l| l.getEvent(k, &mut e) == kResultOk);
                    let until = if has {
                        (e.sampleOffset.max(0) as usize).min(n)
                    } else {
                        n
                    };
                    if until > at {
                        let s = sub.as_mut().map(|[a, b]| [&mut a[..], &mut b[..]]);
                        self.render([&mut ml[..], &mut mr[..]], s, at..until);
                        at = until;
                    }
                    if has {
                        if let Some(key) = self.event(&e) {
                            last_key = Some(key);
                        }
                        // MIDI out: the same note an octave up.
                        if let Some(out) = out_events {
                            let mut echo = e;
                            let t = u32::from(e.r#type);
                            if t == Event_::EventTypes_::kNoteOnEvent as u32 {
                                echo.__field0.noteOn.pitch =
                                    (e.__field0.noteOn.pitch + 12).min(127);
                                out.addEvent(&mut echo);
                            } else if t == Event_::EventTypes_::kNoteOffEvent as u32 {
                                echo.__field0.noteOff.pitch =
                                    (e.__field0.noteOff.pitch + 12).min(127);
                                out.addEvent(&mut echo);
                            }
                        }
                    }
                }
                if let (Some(key), Some(changes)) =
                    (last_key, ComRef::from_raw(d.outputParameterChanges))
                {
                    let v = f64::from(key) / 127.0;
                    self.set(TONE_LAST_KEY, v);
                    let mut index = 0;
                    if let Some(q) =
                        ComRef::from_raw(changes.addParameterData(&TONE_LAST_KEY, &mut index))
                    {
                        q.addPoint(0, v, &mut index);
                    }
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for Processor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

/// The edit controller of either plugin.
struct Controller {
    kind: Kind,
    values: RefCell<Vec<f64>>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
}

impl Class for Controller {
    type Interfaces = (IEditController, IMidiMapping);
}

impl Controller {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            values: RefCell::new(kind.params().iter().map(|p| p.default).collect()),
            handler: RefCell::new(None),
        }
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.kind.params().iter().position(|p| p.id == id)
    }
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        *self.handler.borrow_mut() = None;
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        match decode(self.kind, &read_stream(state, 1024)) {
            Some(v) => {
                *self.values.borrow_mut() = v;
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // The controller keeps nothing of its own; it checks the stream is its own.
        let data = read_stream(state, 64);
        if data.is_empty() || data == b"CTRL" {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_stream(state, b"CTRL")
    }

    unsafe fn getParameterCount(&self) -> i32 {
        self.kind.params().len() as i32
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let (Some(p), Some(info)) = (self.kind.params().get(index as usize), info.as_mut()) else {
            return kInvalidArgument;
        };
        info.id = p.id;
        copy_wstring(p.name, &mut info.title);
        copy_wstring(p.name, &mut info.shortTitle);
        copy_wstring(p.units, &mut info.units);
        info.stepCount = p.steps;
        info.defaultNormalizedValue = p.default;
        info.unitId = kRootUnitId;
        info.flags = p.flags;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: u32, v: f64, string: *mut String128) -> tresult {
        match (self.index(id), string.as_mut()) {
            (Some(_), Some(s)) => {
                copy_wstring(&self.kind.text(id, v), s);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, v: *mut f64) -> tresult {
        if self.index(id).is_none() || string.is_null() {
            return kInvalidArgument;
        }
        let mut len = 0;
        while *string.add(len) != 0 && len < 128 {
            len += 1;
        }
        let text = String::from_utf16_lossy(slice::from_raw_parts(string as *const u16, len));
        let num: String = text
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
            .collect();
        let Ok(x) = num.parse::<f64>() else {
            return kResultFalse;
        };
        *v = match (self.kind, id) {
            (Kind::Gain, GAIN_GAIN) => 10f64.powf(x / 20.0) / 2.0,
            (Kind::Tone, TONE_VOLUME) => x / 100.0,
            _ => x,
        }
        .clamp(0.0, 1.0);
        kResultOk
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, v: f64) -> f64 {
        v
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, v: f64) -> f64 {
        v
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.index(id).map_or(0.0, |i| self.values.borrow()[i])
    }

    unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
        match self.index(id) {
            Some(i) => {
                self.values.borrow_mut()[i] = v.clamp(0.0, 1.0);
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.borrow_mut() = ComRef::from_raw(handler).map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        ptr::null_mut()
    }
}

impl IMidiMappingTrait for Controller {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: i32,
        _channel: i16,
        number: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        // The mod wheel drives the tone's volume.
        if self.kind == Kind::Tone && bus == 0 && number == 1 && !id.is_null() {
            *id = TONE_VOLUME;
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory, IPluginFactory2);
}

const CLASSES: [(TUID, &str, &str, &str); 4] = [
    (GAIN_CID, "Audio Module Class", "GT Test Gain VST3", "Fx"),
    (
        GAIN_CTRL_CID,
        "Component Controller Class",
        "GT Test Gain VST3",
        "",
    ),
    (
        TONE_CID,
        "Audio Module Class",
        "GT Test Tone VST3",
        "Instrument|Synth",
    ),
    (
        TONE_CTRL_CID,
        "Component Controller Class",
        "GT Test Tone VST3",
        "",
    ),
];

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let Some(info) = info.as_mut() else {
            return kInvalidArgument;
        };
        copy_cstring("GloomTunes", &mut info.vendor);
        copy_cstring(
            "https://github.com/afterdamage/gloomtunes-studio",
            &mut info.url,
        );
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        CLASSES.len() as i32
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let (Some(c), Some(info)) = (CLASSES.get(index as usize), info.as_mut()) else {
            return kInvalidArgument;
        };
        info.cid = c.0;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(c.1, &mut info.category);
        copy_cstring(c.2, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match *(cid as *const TUID) {
            GAIN_CID => ComWrapper::new(Processor::new(Kind::Gain)).to_com_ptr::<FUnknown>(),
            TONE_CID => ComWrapper::new(Processor::new(Kind::Tone)).to_com_ptr::<FUnknown>(),
            GAIN_CTRL_CID => ComWrapper::new(Controller::new(Kind::Gain)).to_com_ptr::<FUnknown>(),
            TONE_CTRL_CID => ComWrapper::new(Controller::new(Kind::Tone)).to_com_ptr::<FUnknown>(),
            _ => None,
        };
        match instance {
            Some(i) => {
                let p = i.as_ptr();
                ((*(*p).vtbl).queryInterface)(p, iid as *const TUID, obj)
            }
            None => kInvalidArgument,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let (Some(c), Some(info)) = (CLASSES.get(index as usize), info.as_mut()) else {
            return kInvalidArgument;
        };
        info.cid = c.0;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring(c.1, &mut info.category);
        copy_cstring(c.2, &mut info.name);
        info.classFlags = 0;
        copy_cstring(c.3, &mut info.subCategories);
        copy_cstring("GloomTunes", &mut info.vendor);
        copy_cstring(env!("CARGO_PKG_VERSION"), &mut info.version);
        copy_cstring("VST 3.8.0", &mut info.sdkVersion);
        kResultOk
    }
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[no_mangle]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[no_mangle]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleEntry(_library: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[no_mangle]
extern "system" fn ModuleExit() -> bool {
    true
}

/// The module's factory (the VST3 entry point).
#[no_mangle]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .map_or(ptr::null_mut(), ComPtr::into_raw)
}
