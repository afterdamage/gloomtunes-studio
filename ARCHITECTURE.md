# GloomTunes Studio: Architecture

Status: **design baseline (Prompt 0); Steps 1 and 2 implemented** (workspace, device panel, CI, command queue, transport, scheduler, metronome). This document is the contract that
Prompts 1 to 12 implement; when an implementation step has to deviate, the step updates this file
and adds an entry to the [decision log](#9-decision-log).

GloomTunes Studio is an original, open-source, pattern-based DAW written in Rust for Windows 10/11
and Ubuntu 22.04+ (x86_64) from one codebase. License: GPL-3.0-or-later. Author: Vasil Vasilev.

Contents

1. [Guiding principles](#1-guiding-principles)
2. [Workspace layout and crate APIs](#2-workspace-layout-and-crate-apis)
3. [Threads and the real-time contract](#3-threads-and-the-real-time-contract)
4. [UI and engine communication](#4-ui-and-engine-communication)
5. [Time, tempo and the scheduler](#5-time-tempo-and-the-scheduler)
6. [Data model](#6-data-model)
7. [Engine internals](#7-engine-internals)
8. [Top 10 technical risks](#8-top-10-technical-risks)
9. [Decision log](#9-decision-log)
10. [Dependencies and licenses](#10-dependencies-and-licenses)
11. [Testing strategy](#11-testing-strategy)

---

## 1. Guiding principles

1. **The project document is the single source of truth.** The UI edits a plain-data `Project`.
   The engine never reads it; it receives *compiled*, immutable, audio-thread-ready snapshots
   derived from it. Undo/redo therefore only has to deal with the document.
2. **The audio thread is a guest in its own house.** It never allocates, frees, locks, does I/O,
   logs, or panics. Everything it needs is built for it elsewhere and handed over by pointer.
3. **One render path.** Real-time playback and offline export call the exact same
   `AudioProcessor::process` function. There is no "export renderer".
4. **Integer musical time, derived sample time.** Positions are stored in ticks (960 PPQ).
   Sample positions are always computed from the tempo map, never accumulated, so there is no
   drift.
5. **Small, demoable steps.** Each crate starts with the minimum API the current prompt needs;
   the sketches below are the destination, not a mandate to build everything up front.
6. **Original everything.** Code, UI, icons, sounds and presets are original or CC0. No
   proprietary assets or trade dress from any commercial DAW.

---

## 2. Workspace layout and crate APIs

```
gloomtunes-studio/
├── Cargo.toml                 # [workspace], shared [workspace.dependencies], lints, profiles
├── rust-toolchain.toml        # stable, with rustfmt + clippy
├── ARCHITECTURE.md  ROADMAP.md  README.md  LICENSE
├── assets/                    # original/CC0 only: fonts, generated drum samples, icons
├── crates/
│   ├── gt-core/               # shared plain-data types: time, IDs, the Project document
│   ├── gt-dsp/                # pure DSP building blocks, instruments and effects
│   ├── gt-engine/             # graph, transport, scheduler, mixer, voices, offline render
│   ├── gt-project/            # edits + undo/redo, save/load, migrations, validation
│   ├── gt-ui/                 # egui widgets, views, theme.rs
│   ├── gt-plugin-host/        # CLAP hosting (Prompt 11; absent until then)
│   └── gt-app/                # the binary: device I/O (cpal), MIDI I/O (midir), wiring
├── tests/fixtures/            # golden projects and expected render hashes
└── .github/workflows/         # CI: build, test, clippy, fmt on windows-latest + ubuntu-22.04
```

### Dependency graph (arrows point to dependencies)

```
             gt-app
           /   |    \  \
       gt-ui   |     \  gt-plugin-host (later)
       /  |    |      \   |
gt-project |  gt-engine ──┘
       \   |   /    \
        gt-core     gt-dsp
```

- `gt-dsp` depends on **nothing** in the workspace. It knows samples and parameters, not
  projects, IDs or threads. This keeps it trivially testable and reusable.
- `gt-engine` depends on `gt-core` (to compile the document) and `gt-dsp` (to run DSP). It does
  **not** depend on cpal: it exposes a `process(&mut [f32])` function and the device layer in
  `gt-app` calls it. Offline export and unit tests drive the same function with no device.
- `gt-project` depends only on `gt-core` plus serialization crates.
- `gt-ui` depends on `gt-core`, `gt-project` and `gt-engine` (for the `EngineHandle` telemetry
  types). It contains no cpal or midir code.
- `gt-app` is the only crate that touches audio/MIDI devices, the window, the filesystem
  locations of settings, and logging setup.

Workspace-wide lints: `clippy::all` denied in CI, `unsafe_code = "forbid"` in `gt-core`,
`gt-dsp`, `gt-project` and `gt-ui`. `unsafe` is permitted only in `gt-engine` (FTZ/DAZ flags,
see §7.6), `gt-app` (platform thread priority) and `gt-plugin-host` (FFI), and every block needs
a `// SAFETY:` comment.

### 2.1 gt-core

Shared vocabulary. Plain data, `Clone`, `serde` derives behind a default `serde` feature. No
threads, no I/O.

```rust
// time
pub const PPQ: i64 = 960;
pub struct Tick(pub i64);                 // musical time, 960 per quarter note
pub struct SamplePos(pub u64);            // absolute engine frame index
pub struct Bpm(pub f64);
pub struct TimeSig { pub num: u8, pub den: u8 }
pub struct TempoMap { /* sorted Vec<TempoPoint { at: Tick, bpm: Bpm }> */ }
impl TempoMap {
    pub fn tick_to_seconds(&self, t: Tick) -> f64;
    pub fn seconds_to_tick(&self, s: f64) -> f64;      // fractional tick
}
pub struct BarBeatTick { pub bar: u32, pub beat: u32, pub tick: u32 } // display only

// typed IDs: u32 newtypes, allocated by a per-project counter, never reused
pub struct ChannelId(u32);  pub struct PatternId(u32);  pub struct TrackId(u32);
pub struct ClipId(u32);     pub struct InsertId(u32);   pub struct AutomationId(u32);
pub struct SampleId(u32);   pub struct NoteId(u32);

// the document (full field lists in §6)
pub struct Project { .. }   pub struct Channel { .. }   pub struct Pattern { .. }
pub struct Note { .. }      pub struct Playlist { .. }  pub struct Track { .. }
pub struct Clip { .. }      pub struct Mixer { .. }     pub struct MixerInsert { .. }
pub struct AutomationClip { .. }

// parameters
pub struct ParamAddress { pub owner: ParamOwner, pub index: u32 }
pub struct ParamInfo { /* name, range, default, curve, unit, flags, smoothing_ms */ }
```

### 2.2 gt-dsp

Pure DSP: `#![forbid(unsafe_code)]`, `#![no_std]`-friendly in spirit (uses `std` only for
`f32` math), zero allocation after construction. All state is preallocated in `new(..)` /
`prepare(sample_rate, max_block)`. Every unit has unit tests and, where it matters, a criterion
benchmark.

```rust
pub trait Mono   { fn process(&mut self, x: f32) -> f32; fn reset(&mut self); }
pub struct Smoother;        // one-pole or linear ramp, used for every live parameter
pub struct PolyBlepOsc;     // saw/square/triangle (Prompt 5), naive sine
pub struct Adsr;  pub struct Lfo;  pub struct Noise /* seeded, deterministic */;
pub struct Biquad;  pub struct Svf;  pub struct Ladder /* ZDF 4-pole, Prompt 5 */;
pub struct DelayLine /* fixed capacity */;
// complete processors with block APIs over planar stereo buffers
pub struct GloomSynth;  pub struct SamplerVoice;  pub struct ParamEq;  pub struct Compressor;
pub struct Delay;  pub struct Reverb;  pub struct Chorus;  pub struct Waveshaper;
pub struct Limiter;  pub struct StereoWidth;
```

> **As built (Step 6, D40-D41):** effects live in `gt_dsp::fx` behind the `Effect` trait:
> `ParamEq`, `Compressor`, `Delay`, `Reverb` (FDN), `Chorus`, `Distortion` (the waveshaper),
> `Limiter`, `StereoWidth`, plus `Biquad`, `Smooth` (one-pole) and a fixed `DelayLine`.
>
> **As built (Step 5, D32-D34):** the oscillator is a function, `blep_sample(wave, phase, dt, pw)`,
> over a `Phase` accumulator rather than a struct, so unison copies share one code path; `Lfo`,
> `Noise` (xorshift32), `Ladder` (+ `ladder_response` for the UI) and `GloomSynth` exist as
> sketched. Dev-dependencies added: criterion, insta (and their permissive transitive crates).

Instruments in gt-dsp take a slice of `(offset, NoteEvent)` for the current block and render up to
each offset in turn, so they are sample-accurate without knowing about the engine.

### 2.3 gt-engine

The real-time half of the application.

```rust
pub struct EngineConfig { pub sample_rate: u32, pub max_device_block: usize, pub out_channels: usize }

/// Builds the two halves. Called on the UI thread; allocates everything.
pub fn create(cfg: EngineConfig) -> (EngineHandle, AudioProcessor);

/// Lives on the audio thread (moved into the cpal callback) or an export thread.
pub struct AudioProcessor { .. }
impl AudioProcessor {
    /// Real-time safe. Fills an interleaved output buffer of `frames * out_channels`.
    pub fn process(&mut self, out: &mut [f32], live_in: Option<&[f32]>);
}

/// Lives on the UI thread. Non-blocking, never waits for the audio thread.
pub struct EngineHandle { .. }
impl EngineHandle {
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), EngineCommand>; // full queue => Err
    pub fn poll_events(&mut self, f: impl FnMut(EngineEvent));
    pub fn telemetry(&self) -> &Telemetry;                 // atomics, read any time
    pub fn collect_garbage(&mut self);                     // drop what the engine handed back
}

/// Turns the document into engine form. Runs on the UI thread or a worker thread.
pub mod compile {
    pub fn song(project: &gt_core::Project, sr: u32) -> Box<SongSnapshot>;
    pub fn graph_patch(old: &GraphLayout, project: &gt_core::Project, sr: u32) -> GraphPatch;
}

/// Offline export: owns its own AudioProcessor, same code path.
pub struct OfflineRenderer { .. }
impl OfflineRenderer {
    pub fn new(project: &gt_core::Project, sr: u32, range: RenderRange) -> Self;
    pub fn render_next(&mut self, out: &mut [f32]) -> RenderProgress; // call until Done
}
```

### 2.4 gt-project

Everything about *changing and storing* the document.

```rust
pub struct Document { pub project: Project, history: History, dirty: DirtyFlags, ids: IdAllocator }
pub trait Edit: Send {
    /// Applies the change and returns its exact inverse. Validation happens here.
    fn apply(self: Box<Self>, p: &mut Project) -> Result<Box<dyn Edit>, EditError>;
    fn label(&self) -> &str;
    fn merge(&mut self, next: &dyn Edit) -> bool { false } // coalesce knob drags, nudges
    fn dirties(&self) -> DirtyFlags;                       // what the engine must recompile
}
impl Document {
    pub fn apply(&mut self, e: Box<dyn Edit>) -> Result<(), EditError>;
    pub fn undo(&mut self) -> bool;   pub fn redo(&mut self) -> bool;
    pub fn take_dirty(&mut self) -> DirtyFlags;   // consumed by the engine sync step
}
pub mod io {
    pub fn save(doc: &Project, path: &Path, opts: SaveOptions) -> Result<(), IoError>;
    pub fn load(path: &Path) -> Result<LoadResult, IoError>; // LoadResult lists missing samples
}
pub mod migrate { pub const CURRENT: u32; pub fn upgrade(v: serde_json::Value) -> Result<..>; }
```

> **As built (Step 4, D28):** undo is a snapshot-diff `History` instead of the `Edit` trait
> above. The UI edits `Project` directly; when a gesture ends the app calls
> `History::commit(&project, label)`, which diffs against the last committed state and stores
> per-(pattern, channel) note lists, or a whole-document copy for rarer structural changes.
> `gt_project::ops` holds quantize and humanize. Validation-on-apply arrives with the mixer
> graph (Prompt 6), where an `Edit`-style API can still sit on top of the same history.

Undo history is per session (not saved), as Prompt 9 specifies. Edits that would create a mixer
routing cycle are rejected in `apply`, so the engine never sees an invalid graph.

### 2.5 gt-ui

egui views and custom-painted widgets. Holds *view state* (zoom, scroll, selection, open panels),
never musical state.

```rust
pub mod theme;          // GloomTheme: every colour, size and font in one place (theme.rs)
pub mod widgets;        // Knob, Fader, Meter, StepButton, TimeDisplay, ...
pub mod views;          // TransportBar, ChannelRack, PianoRoll, Playlist, Mixer, Browser, ...
pub struct UiContext<'a> { pub doc: &'a mut Document, pub engine: &'a mut EngineHandle, .. }
pub struct GloomUi { /* view state per panel */ }
impl GloomUi { pub fn show(&mut self, ctx: &egui::Context, app: &mut UiContext); }
```

Theme: near-black backgrounds (several close greys for depth), muted desaturated secondary
colours, **one** strong accent, high-contrast text, compact spacing. Channel/track colours are
user data but drawn desaturated through the theme so the accent stays dominant.

### 2.6 gt-plugin-host (Prompt 11)

CLAP hosting via `clack-host`. A `PluginNode` implements the engine's `Processor` trait. Scanning,
instantiation and GUI windows run on the main thread; `process` runs on the audio thread. VST3 is
designed but not implemented (§10 covers licensing).

### 2.7 gt-app

`main()`, the `eframe::App`, and the edges of the world:

- `audio_io`: cpal host/device enumeration, stream creation, sample-format conversion
  (f32/i16/u16 at the edge), device-lost recovery, `#[cfg(windows)]` ASIO behind the `asio`
  feature, `#[cfg(target_os = "linux")]` JACK behind the `jack` feature.
- `midi_io`: midir ports, polling-based hot-plug detection on a non-RT thread.
- `sync`: after each UI frame, takes `Document::take_dirty()`, compiles what changed and sends it
  to the engine (§4.4).
- `settings`: audio/MIDI/UI preferences in the OS config directory.
- Logging (`log` + `env_logger`), never called from the audio thread.

---

## 3. Threads and the real-time contract

| Thread | Owner | Does | Must not |
|---|---|---|---|
| UI / main | eframe | input, drawing, edits, undo, compile small changes, drain engine events and garbage | block on the audio thread |
| Audio | cpal callback | `AudioProcessor::process` | allocate, free, lock, syscall, log, panic, wait |
| MIDI in | midir callback | timestamp + push raw messages into its own queue | anything slow; it is effectively RT |
| Workers (small pool) | gt-app | sample decode + resample, waveform peaks, large compiles, export, autosave serialization | touch the engine directly |
| Plugin main (later) | gt-plugin-host | CLAP main-thread calls, GUIs | run on the audio thread |

**Allowed on the audio thread:** arithmetic on preallocated buffers, reading/writing atomics,
`rtrb` push/pop (wait-free), swapping `Box`/pointer ownership, `Arc` clone (an atomic increment).
**Forbidden:** `Vec::push` beyond capacity, `Box::new`, dropping anything that owns heap memory,
`Mutex`, `RwLock`, channels that may block, `println!`/`log`, file or network I/O, unbounded loops
over user data that is not bounded by a capacity.

Enforcement:

1. Design: everything heap-backed reaches the audio thread pre-built and leaves through the
   garbage queue (§4.3) to be dropped elsewhere.
2. Debug builds wrap `process` in `assert_no_alloc` so any allocation or free aborts loudly in
   tests and during development.
3. Every engine unit test runs `process` under that guard.
4. Panics: the code avoids panicking paths (no unchecked indexing into user-sized data, no
   `unwrap` on the audio thread). As a last line of defence the callback body runs inside
   `std::panic::catch_unwind`; on a panic the engine outputs silence, sets a `faulted` flag in
   telemetry, and the UI offers an engine restart. (Release builds keep `panic = "unwind"` for
   this reason.)
5. Code review checklist item for every PR touching `gt-engine` or `gt-dsp`.

---

## 4. UI and engine communication

All queues are `rtrb::RingBuffer` (single-producer single-consumer, wait-free, MIT/Apache).
Each producer thread gets its own queue, so there is never a multi-producer queue on the RT path.

```
              EngineCommand (rtrb, 1024)
  UI thread ─────────────────────────────────▶ ┐
              MidiMessage (rtrb, 512)           │
  MIDI thread ───────────────────────────────▶ ├─▶ Audio thread (AudioProcessor)
                                                │        │   │   │
  UI thread ◀── EngineEvent (rtrb, 256) ────────┘        │   │   │
  UI thread ◀── Garbage (rtrb, 512) ─────────────────────┘   │   │
  UI thread ◀── Telemetry (Arc of atomics, no queue) ────────┘   │
  Export    ◀── same AudioProcessor type, driven by a worker ────┘
```

### 4.1 UI to engine: `EngineCommand`

Every variant is small (target: at most 32 bytes); large payloads travel as a `Box` whose
allocation happened on the UI side.

As of Step 3 the implemented variants are `Play`, `Pause`, `Stop`, `Locate`, `SetLoop`,
`SetTempoMap(Box<TempoMap>)` (the tempo part of `ApplySong`, until the full song snapshot exists),
`SetTimeSig` (`SetSignatures` from Step 7), `SetMetronome`, `SetTestTone`, `FadeOut`, `FadeIn`, and from Step 3
`SetSong(Box<SongSnapshot>)` (the playing pattern), `SetChannelParams { slot, Box<ChannelParams> }`
(stand-in for `SetParam` until the parameter system of Prompt 8), `SetChannelSample { slot,
Option<Arc<SampleData>> }` (stand-in for `ApplyGraph` until the mixer graph of Prompt 6),
`NoteOn`, `NoteOff` and `PreviewSample(Option<Arc<SampleData>>)` (`None` replaces
`StopPreview`). Step 5 added `SetScopeChannel(Option<u16>)`, and Step 6 `SetMixer(Box<MixerParams>)`,
`SetEffect { strip, slot, Option<EffectBox> }` and `SetEffectParam { strip, slot, index, value }`
(stand-ins for `ApplyGraph` and `SetParam`, D39). Commands that hand something back are only taken when the garbage queue has a
free slot. The size limit is enforced by a compile-time assertion. The rest of the enum below
arrives with the steps that need it.

```rust
pub enum EngineCommand {
    // transport
    Play, Stop, Pause,
    Locate(Tick),
    SetLoop { start: Tick, end: Tick, enabled: bool },
    SetPlayMode(PlayMode),                  // Pattern(PatternSlot) | Song
    SetMetronome { enabled: bool, count_in_bars: u8 },

    // real-time parameter changes (knob drags); applied at the next quantum, then smoothed
    SetParam { slot: ParamSlot, value: f32, generation: u32 },

    // live input from the UI (computer keyboard, preview clicks in the piano roll)
    NoteOn  { channel: ChannelSlot, key: u8, velocity: f32 },
    NoteOff { channel: ChannelSlot, key: u8 },
    PreviewSample(Arc<SampleData>),         // browser preview, on a dedicated preview voice
    StopPreview,
    Panic,                                  // all notes off, reset tails

    // structure; built off-thread, ownership moves to the engine
    ApplySong(Box<SongSnapshot>),           // event lists, tempo map, automation lanes
    ApplyGraph(Box<GraphPatch>),            // add/remove nodes, new topology, routing
}
```

- `ParamSlot` is a dense index into the engine's flat parameter array, resolved from a stable
  `ParamAddress` by the UI using the layout of the graph generation it last sent. The
  `generation` field lets the engine discard a `SetParam` that was resolved against an older
  graph (it can only arrive in the gap between a graph swap and the UI noticing it).
- If the command queue is full the UI keeps the latest value per `ParamSlot` in a small map and
  retries next frame. Parameter changes coalesce, so nothing important is lost; structural
  commands are never coalesced and simply retry.

### 4.2 MIDI thread to engine: `MidiMessage`

```rust
pub struct MidiMessage { pub port: u8, pub time_us: u64, pub len: u8, pub bytes: [u8; 3] }
```

SysEx is not passed to the engine. Until Prompt 10, messages are applied at the start of the next
quantum; Prompt 10 maps `time_us` to a sample offset using the measured callback clock, which
removes up to one buffer of jitter.

### 4.3 Engine to UI: events, garbage, telemetry

```rust
pub enum EngineEvent {
    TransportChanged { state: TransportState, pos: Tick },
    SongApplied { generation: u32 },
    GraphApplied { generation: u32 },
    LoopWrapped,
    VoiceStolen { channel: ChannelSlot },     // rate-limited
    ClipDetected { slot: InsertSlot },        // > 0 dBFS on an insert
    ParamTouched { slot: ParamSlot, value: f32 }, // automation/MIDI learn feedback (Prompt 8/10)
    Faulted,                                  // panic caught, engine muted
}

pub enum Garbage {                            // dropped on the UI thread
    Song(Box<SongSnapshot>),
    Graph(Box<GraphPatch>),                   // contains removed nodes
    Sample(Arc<SampleData>),
}
```

The engine **never drops** anything that owns memory. Before swapping in a new snapshot it checks
that the garbage queue has a free slot; if not, it postpones the swap by one quantum. The garbage
queue is drained every UI frame, so in practice this never triggers.

`Telemetry` is a struct of atomics allocated once in `create()` and shared by `Arc`:

```rust
pub struct Telemetry {
    pub playing: AtomicBool,
    pub position_ticks: AtomicI64,            // for the time display and playheads
    pub position_samples: AtomicU64,
    pub cpu_load: AtomicF32,                  // process time / buffer duration, smoothed
    pub xruns: AtomicU32,                     // callbacks that overran or were late
    pub faulted: AtomicBool,
    pub meters: Box<[MeterCell]>,             // fixed: master + 64 inserts + 4 sends
    pub channel_activity: Box<[AtomicF32]>,   // fixed capacity, step-rack LEDs
}
pub struct MeterCell { pub peak: [AtomicF32; 2], pub rms: [AtomicF32; 2] } // AtomicF32 = AtomicU32 bits
```

The engine computes peak (with hold handled by the UI) and RMS per quantum and stores them with
`Relaxed` ordering; tearing between L and R is harmless for metering. Meter ballistics (decay,
hold) are applied in the UI, so the audio thread does the minimum.

### 4.4 The sync step (document to engine)

```
user gesture ─▶ Edit ─▶ Document::apply ─▶ DirtyFlags
                                             │  (end of UI frame)
                                             ▼
                   gt-app::sync ── compile::song / compile::graph_patch ──▶ EngineCommand
```

- `DirtyFlags` says which parts changed: `NOTES`, `PLAYLIST`, `TEMPO`, `AUTOMATION`, `GRAPH`,
  `PARAMS`.
- Musical edits (`NOTES | PLAYLIST | TEMPO | AUTOMATION`) rebuild the whole `SongSnapshot`. That is
  a sort over at most tens of thousands of events, which costs well under a millisecond; if
  profiling (Prompt 12) shows otherwise it moves to a worker thread and becomes incremental.
- Structural edits (`GRAPH`: add channel, insert effect, change routing) build a `GraphPatch`
  that creates only the *new* nodes and lists the ones to remove. Existing nodes keep their state,
  so a reverb tail does not cut when you add a channel.
- Continuous gestures (dragging a knob) send `SetParam` immediately for responsiveness and record a
  single merged undo entry when the gesture ends.

---

## 5. Time, tempo and the scheduler

### 5.1 Units

- **Tick**: `i64`, 960 per quarter note (PPQ). Divisible by 2, 3, 4, 5, 6, 8, 10, 12, 15, 16, 20,
  24, 32, 64, so straight notes down to 1/256 and triplets down to 1/128 are exact integers.
- **Sample**: `u64` frame index on the engine clock.
- **Conversion**: the tempo map is a list of constant-tempo segments (tempo ramps are a later
  extension). For each segment the compiler precomputes its start in seconds as an exact `f64`.
  An event at tick `t` in a segment starting at tick `t0`, second `s0`, with tempo `bpm`, happens at
  `s0 + (t - t0) * 60 / (bpm * 960)` seconds; its sample position relative to the play anchor is
  that time times the sample rate, rounded to the nearest frame (ties go to the later frame).
  Example: at 120 BPM and 48 kHz one tick is exactly 25 samples; at 44.1 kHz it is 22.96875 samples,
  so rounding is required and is done once per event from absolute time, never accumulated.

### 5.2 Transport model

The transport holds an **anchor**: `(anchor_tick, anchor_sample)`. The current musical position is
always `anchor_tick + tempo_map.seconds_to_tick(seconds_since(anchor_sample))`. Play, locate, loop
wrap and tempo-map swaps re-anchor. Because every position is derived from the anchor and the tempo
map, there is no cumulative rounding error however long the song plays.

### 5.3 Fixed render quantum

The engine renders in fixed **quanta of 64 frames** (`RENDER_QUANTUM`, a constant), counted from the
play anchor. The device callback may ask for any number of frames (cpal does not guarantee the
requested size; WASAPI shared mode commonly delivers 480 or 441); a small output FIFO inside
`AudioProcessor` adapts quanta to callback sizes. For power-of-two device buffers of 64 or more this
adds zero latency; otherwise it adds at most 63 frames (1.3 ms at 48 kHz).

Why: anything that runs at "control rate" (automation evaluation every 32 frames, LFOs, meter
computation, smoother targets) then happens at the same frames in real time and offline, which is
what makes export bit-identical to playback (§7.5).

### 5.4 Scheduler: splitting at event boundaries

Per quantum `[q0, q0 + 64)`:

1. **Timeline splits.** If the loop end falls inside the quantum, the quantum is split at that
   frame and the second part is scheduled from a new anchor at the loop start. Whether the loop
   end is "ahead" is decided in the frame domain with the same rounding as events; if playback was
   located past the loop end it plays on without wrapping. Tempo changes need no split: the
   tempo map's piecewise conversion places events after a change exactly. Transport commands
   take effect at quantum boundaries (they come from the UI, whose timing is not sample-exact
   anyway). Implemented in `gt-engine/src/transport.rs`.
2. **Event gathering.** For each sub-block the scheduler binary-searches the compiled event array
   (sorted by sample time, see §7.2) for events in range and writes `(offset, event)` pairs into
   preallocated per-node event lists (fixed capacity, overflow counted in telemetry and the excess
   deferred to the next quantum).
3. **Node processing.** Instruments render up to each event offset, apply it, and continue, so note
   starts are sample-accurate without splitting the whole graph per note. Parameter events from
   automation arrive at control-rate boundaries and feed the parameter smoothers.

Tests (Prompt 2): events land on the expected frame at 44.1, 48 and 96 kHz; across a tempo change;
across a loop wrap; for patterns longer than a buffer; with buffer sizes of 64 to 1024 and odd sizes
(e.g. 441).

Step 3 status: the scheduler gathers metronome beats, loop wraps and the notes of the playing
pattern into one per-quantum list (capacity 256), sorted in place by frame and then wrap,
note-off, note-on, beat. A wrap releases held notes. Each channel then walks only its own events
(step 3 above). Tests: step onsets with swing land on exact frames at three rates and are
bit-identical for device buffers of 1, 37/512/3 and 1024 frames; scheduling is quantum-invariant
across loop wraps.

---

## 6. Data model

**Implemented so far (Step 3), simplified on purpose** (D21): `Project { channels: Vec<Channel>,
patterns: Vec<Pattern>, current_pattern, swing }`; the `Vec` order is the rack order and a
channel's index is its engine slot. `Channel { id, name, volume, pan, mute, solo, sampler:
SamplerSettings { sample: Option<SampleSource>, pitch, start, end, loop_mode, adsr } }`, where
`SampleSource` is `BuiltIn(kind)` or `File(path)`. `Pattern { id, name, steps: 16 | 32, notes:
BTreeMap<ChannelId, Vec<Note { start, length, key, velocity }>> }`. A step is a note on the 1/16
grid at key 60 (D8). Swing is one rack-wide value. The full model below remains the target; IDs,
the sample table and serde arrive with save/load (Prompt 9).

The document lives in `gt-core`. All collections are keyed by typed IDs; order-sensitive lists
(channel rack rows, playlist tracks, mixer strip order) keep an explicit `order: Vec<Id>`.

```rust
pub struct Project {
    pub schema_version: u32,
    pub meta: ProjectMeta,                         // title, author, created, app version
    pub tempo: TempoMap,                           // at least one point at tick 0
    pub time_sig: Vec<(Tick, TimeSig)>,
    pub channels: IdMap<ChannelId, Channel>,  pub channel_order: Vec<ChannelId>,
    pub patterns: IdMap<PatternId, Pattern>,  pub pattern_order: Vec<PatternId>,
    pub playlist: Playlist,
    pub mixer: Mixer,
    pub automation: IdMap<AutomationId, AutomationClip>,
    pub samples: IdMap<SampleId, SampleRef>,
    pub view: ViewState,                           // saved zoom/scroll, purely cosmetic
}
```

### Channel (a row in the channel rack; anything that makes sound)

```rust
pub struct Channel {
    pub id: ChannelId, pub name: String, pub color: Rgb,
    pub kind: ChannelKind,
    pub volume: f32, pub pan: f32, pub pitch_cents: f32,   // normalized storage, see Param below
    pub mute: bool, pub solo: bool,
    pub swing: f32,                                        // 0..1, delays every second step
    pub target: InsertId,                                  // mixer routing
}
pub enum ChannelKind {
    Sampler(SamplerSettings),       // sample: SampleId, start/end, loop mode, ADSR, root key
    Synth(GloomSynthPatch),         // Prompt 5
    AudioClips,                     // plays the audio clips that reference this channel
    Plugin(PluginInstanceRef),      // Prompt 11: plugin id + opaque state blob
}
```

### Pattern and Note

A pattern holds notes per channel. The step sequencer and the piano roll are two views of the same
notes: a step is a note whose start is on the step grid and whose length is one step.

```rust
pub struct Pattern {
    pub id: PatternId, pub name: String, pub color: Rgb,
    pub length: Tick,                               // e.g. 1 bar = 3840; default auto-fit
    pub notes: IdMap<ChannelId, Vec<Note>>,         // kept sorted by (start, key)
}
pub struct Note {
    pub id: NoteId,
    pub start: Tick, pub length: Tick,              // length >= 1
    pub key: u8,                                    // MIDI key, 60 = C4
    pub velocity: f32,                              // 0..1
    pub release_velocity: f32,
    pub pan: f32,                                   // -1..1 per-note offset
    pub fine_pitch: f32,                            // cents
    pub muted: bool,
}
```

### Playlist, Track, Clip

Clips are not owned by tracks semantically; a track is a lane for organisation, mute and solo.
This keeps the pattern-based workflow flexible: any clip can sit on any lane.

```rust
pub struct Playlist {
    pub tracks: IdMap<TrackId, Track>, pub track_order: Vec<TrackId>,
    pub clips: IdMap<ClipId, Clip>,
    pub markers: Vec<Marker>,
    pub loop_region: Option<(Tick, Tick)>,
}
pub struct Track { pub id: TrackId, pub name: String, pub color: Rgb, pub height: f32,
                   pub mute: bool, pub solo: bool }
pub struct Clip {
    pub id: ClipId, pub track: TrackId,
    pub start: Tick, pub length: Tick,
    pub offset: Tick,                               // slip edit: where inside the source we start
    pub muted: bool,
    pub kind: ClipKind,
}
pub enum ClipKind {
    Pattern(PatternId),
    Audio { sample: SampleId, channel: ChannelId, gain_db: f32, stretch: StretchMode /* None in v1 */ },
    Automation(AutomationId),
}
```

> **As built (Step 7, D44-D50):** `gt_core::playlist` keeps tracks and clips in plain `Vec`s
> (order is the vector order; clips carry their `TrackId`) and markers sorted by tick. The loop
> region lives in the transport, not the document. `ClipKind::Audio` holds a `SampleSource`
> (path or built-in) and a linear gain; the track's `insert` says where its audio goes.
> `ClipKind::Automation` embeds its `Automation { target, points }` instead of pointing into a
> separate map (D50). Signatures are a `TimeSigMap` of changes on bar boundaries (D46).

### Automation and parameters

```rust
pub struct AutomationClip {
    pub id: AutomationId,
    pub target: ParamAddress,
    pub points: Vec<AutoPoint>,                     // sorted by tick, relative to clip start
}
pub struct AutoPoint { pub at: Tick, pub value: f32 /* normalized 0..1 */, pub curve: Curve }
pub enum Curve { Hold, Linear, Smooth /* cosine */, Bezier { tension: f32 } }

pub struct ParamAddress { pub owner: ParamOwner, pub index: u32 }
pub enum ParamOwner { Channel(ChannelId), Insert(InsertId), Slot(InsertId, u8), Send(InsertId, u8),
                      Transport /* tempo, later */ }
```

Every parameter is stored **normalized (0..1)** in the document. `ParamInfo` maps normalized to
plain values with a curve (linear, logarithmic for frequency, dB for gain, stepped for enums),
defines display formatting and a smoothing time. Parameter indices for built-in devices are
declared explicitly and never renumbered, so saved projects and automation stay valid as devices
gain parameters.

### Mixer and Insert

```rust
pub struct Mixer {
    pub master: MixerInsert,
    pub inserts: IdMap<InsertId, MixerInsert>, pub insert_order: Vec<InsertId>,  // up to 64
    pub sends: IdMap<InsertId, MixerInsert>,   pub send_order: Vec<InsertId>,    // up to 4
}
pub struct MixerInsert {
    pub id: InsertId, pub name: String, pub color: Rgb,
    pub volume: f32, pub pan: f32, pub width: f32,
    pub mute: bool, pub solo: bool, pub phase_invert: bool,
    pub slots: [Option<EffectSlot>; 10],
    pub sends: Vec<SendLevel>,                     // { target: InsertId, level: f32, pre_fader: bool }
    pub output: InsertId,                          // default: master
    pub sidechain_from: Vec<InsertId>,             // compressor sidechain sources
}
pub struct EffectSlot { pub kind: EffectKind, pub enabled: bool, pub mix: f32, pub params: Vec<f32> }
```

Routing (outputs, sends, sidechains) must form a directed acyclic graph. `gt-project` rejects edits
that would create a cycle; `compile::graph_patch` topologically sorts the inserts into a flat
processing order.

### Samples

```rust
pub struct SampleRef { pub id: SampleId, pub name: String,
                       pub location: SampleLocation /* Embedded(path in zip) | Relative(path) | Absolute(path) */,
                       pub hash: [u8; 32] /* content hash for relinking */ }
```

Decoded audio (`SampleData`: planar f32, resampled to the engine rate) lives in a sample pool on the
UI side and is shared with the engine via `Arc`. The pool always keeps one reference, so the last
`Arc` is never dropped on the audio thread.

### Project file

A `.gloom` file is a zip container:

```
project.json        # { "format": "gloomtunes-project", "schema_version": N, ... }
samples/<hash>.flac # optional embedded samples (lossless)
```

JSON (not RON) was chosen because migrations operate on a `serde_json::Value` tree before typed
deserialization, and every tool can read it. Each schema change bumps `schema_version` and adds a
migration function `vN -> vN+1` plus a fixture project for that version in `tests/fixtures`.

---

## 7. Engine internals

### 7.1 Graph

```rust
pub trait Processor: Send {
    fn prepare(&mut self, sample_rate: f32, max_block: usize);   // non-RT, may allocate
    fn reset(&mut self);                                         // RT-safe
    fn latency(&self) -> u32 { 0 }                               // for PDC
    fn tail_frames(&self) -> Option<u64> { Some(0) }             // export tail length; None = infinite
    fn process(&mut self, ctx: &mut ProcessCtx);                 // RT-safe
}
pub struct ProcessCtx<'a> {
    pub frames: usize,                       // <= RENDER_QUANTUM
    pub inputs: &'a [StereoRef<'a>], pub outputs: &'a mut [StereoMut<'a>],
    pub events: &'a [TimedEvent],            // sorted by offset
    pub params: ParamView<'a>,               // smoothed values for this node
    pub transport: &'a TransportInfo,        // tempo, position, playing, for tempo-synced FX
}
```

- Nodes are `Box<dyn Processor>` stored in an arena (`Vec<Option<NodeBox>>` with fixed capacity)
  indexed by `NodeSlot`. A `GraphPatch` carries new nodes (already `prepare`d) and a new
  `GraphLayout`: a topologically sorted list of node slots, buffer assignments and gain stages.
  Applying a patch moves boxes in and moves removed boxes into the garbage queue.
- Buffers: a fixed pool of planar stereo `f32` buffers of `RENDER_QUANTUM` frames, allocated at
  `create()`. The compiler assigns buffers to edges (simple liveness-based reuse).
- All internal audio is 32-bit float, planar, stereo. Interleaving and format conversion happen only
  at the device edge in `gt-app`.

Typical topology: channel nodes (instrument) → channel strip (vol/pan) → insert input sum →
10 effect slots → insert fader/pan/width → sends + output sum → master → device.

### 7.2 Compiled song

`SongSnapshot` is everything time-related the engine needs, flattened:

- Tempo map segments with precomputed start seconds.
- **Song mode**: one array of `ScheduledEvent { tick, channel: ChannelSlot, kind }` sorted by tick,
  produced by expanding each pattern clip (respecting clip offset and length, clipping notes that
  cross the clip end) plus audio clip starts/stops. Swing is applied here.
- **Pattern mode**: one event array per pattern, looped at the pattern length.
- Automation lanes: per `ParamSlot`, a sorted breakpoint array; overlapping clips on the same
  parameter resolve "last clip wins".
- A coarse index (first event per bar) for fast locate.

Sample times are resolved from ticks at schedule time using the anchor (§5.2), so a snapshot does
not depend on where playback starts.

> **As built (Step 7, D44-D49):** `SongSnapshot { length, repeat, events, audio, automation }`.
> Pattern mode compiles the current pattern with `repeat = true`; song mode (`compile_song`)
> expands every audible pattern clip into absolute ticks with `repeat = false`, so notes play once
> per pass and the loop region decides what repeats (D44). Audio clips become `AudioPlay`
> entries in seconds, read at `(song seconds - origin) x rate` with linear interpolation and a
> 2 ms fade only at cut edges (D45). Automation becomes one `AutoLane` per target (D47). There is
> no per-bar locate index yet.

### 7.3 Voices

Each instrument node owns a fixed voice pool (sampler 32, Gloom Synth 16, configurable at `prepare`).
Allocation order: free voice, else the oldest voice in release, else the oldest voice overall
(with a 2 ms fade to avoid a click). No allocation ever happens at note-on.

Step 3 status: the engine preallocates 64 channel slots, each with 16 sampler voices; a full pool
steals the oldest voice with a hard cut (the fade and release-first order come with the mixer
graph in Prompt 6). A sampler voice reads the sample with linear interpolation at
`2^(semitones/12) * sample_rate / engine_rate` frames per frame, applies an ADSR (linear attack,
exponential decay/release reaching 1 % at the set time) and, for one-shots, a 2 ms fade before the
end point. Velocity maps to gain as `v²`; pan is equal-power (-3 dB at centre). The engine
quantum is planar stereo (D19).

Step 5 status: each channel slot also preallocates a boxed `GloomSynth` (16 voices × up to 7
unison copies) and dispatches notes to it when the channel's instrument is a synth. The synth
follows the allocation order above, release-first included; instead of a 2 ms fade, a stolen
voice keeps its oscillator and filter state and restarts its envelopes from their current level
(D35). Modulation and filter coefficients run at a 16-frame control rate with per-sample linear
ramps of filter `g` and output gains (D33).

### 7.4 Parameters, smoothing and automation

- The engine holds a flat array of `ParamState { target, smoother }` indexed by `ParamSlot`.
- Sources, in priority order per quantum: automation lane value (if the transport is playing and the
  lane exists), otherwise the last `SetParam`/MIDI-learn value.
- Automation is evaluated at control rate (every 32 frames, two points per quantum); the smoother
  turns those steps into a per-sample ramp. Note events stay sample-accurate. Prompt 8 documents
  the trade-off in detail and may make cheap parameters sample-accurate.
- Default smoothing: 10 ms one-pole for gains and pans, 20 ms for filter cutoff (smoothed in the
  log-frequency domain so sweeps sound even).

### 7.5 Offline equals real time

Guaranteed by construction:

- The same `AudioProcessor::process` runs both.
- The quantum grid is anchored to the play start, so control-rate work happens on the same frames.
- No DSP code reads wall-clock time; random sources (noise, humanize, chorus drift) are seeded from
  the project and reset on play.
- Nodes are `reset()` at play start in both modes, so a real-time capture started from a stopped
  transport equals the export.

"Identical" means bit-identical for the same build, machine, sample rate and start position with no
live input. Across operating systems `f32` transcendental functions (`sin`, `exp`, `tanh`) can differ
in the last bit, so cross-platform golden tests compare with a tolerance of -120 dBFS rather than by
hash; the per-platform hash test (Prompt 9) catches regressions on each OS.

### 7.6 Denormals and NaN

- On x86_64 the audio thread sets FTZ and DAZ in MXCSR at the start of each callback
  (`#[cfg(target_arch = "x86_64")]`, one small `unsafe` block in gt-engine). The export thread does
  the same, preserving parity.
- FTZ/DAZ makes "add a tiny DC offset" tricks unnecessary on x86_64, but feedback structures
  (filters, delay, reverb) are still unit-tested with 10 s of silence after a signal to confirm the
  CPU cost does not spike as their state decays.
- A NaN/Inf guard on every insert output replaces non-finite samples with silence, resets the
  offending node and raises `ClipDetected`. It costs one comparison per sample per insert.

### 7.7 Plugin delay compensation (groundwork)

Each node reports `latency()`. After the topological sort, the compiler computes for every insert
input the maximum upstream latency and assigns a compensating delay line to faster paths. Built-in
effects are zero-latency in v1 except the limiter (lookahead). The graph design reserves the delay
nodes from Prompt 6 even if all latencies are zero.

> **As built (Step 6, D42):** the strips are a fixed mixer rather than a general node graph, and
> compensation runs in `gt-engine/src/mixer.rs`: per strip a delay line on the channel input, the
> output edge and each send edge, recomputed whenever the mixer settings or an effect change.

### 7.8 Platform specifics

| Area | Windows | Ubuntu |
|---|---|---|
| Audio backend | WASAPI shared (cpal default); ASIO via `asio` feature (needs the Steinberg ASIO SDK, not shipped) | ALSA (cpal default; works on PipeWire through pipewire-alsa); JACK via `jack` feature |
| Exclusive/low latency | WASAPI exclusive is not exposed by cpal; Prompt 12 evaluates a `wasapi`-crate backend behind `#[cfg(windows)]` | PipeWire quantum governs latency; document `PIPEWIRE_QUANTUM` |
| Thread priority | Verify cpal's MMCSS registration; add it in gt-app if missing | Request RT priority via rtkit (`audio_thread_priority` crate, MPL-2.0) |
| MIDI | WinMM via midir | ALSA sequencer via midir |
| Build deps | MSVC toolchain | `libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev pkg-config` (+ `libjack-jackd2-dev` for JACK) |
| Runtime libs | none beyond the OS | `libxkbcommon-x11-0` on X11 sessions (present on standard desktop installs) |
| Renderer | wgpu (DX12/Vulkan); automatic fallback to OpenGL (glow) if wgpu cannot start; `GT_RENDERER=glow` forces it | same |

---

## 8. Top 10 technical risks

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| 1 | **Real-time violations** creep in (a hidden allocation, a `Drop` of a `Vec`, a lock in a dependency) | Dropouts, clicks, priority inversion that only show under load | Snapshot-and-garbage design (§4); `assert_no_alloc` in debug and tests; xrun counter in telemetry; review checklist; no third-party crate on the RT path without inspection |
| 2 | **Backend variance**: cpal callback sizes differ from the requested size, WASAPI shared adds latency, ALSA/PipeWire/PulseAudio behave differently, devices disappear | Glitches, wrong timing, crashes on unplug | Fixed render quantum + FIFO adapter; never assume callback size; handle stream errors by rebuilding the stream on the UI thread; manual test matrix per release on both OSes |
| 3 | **egui performance** for piano roll and playlist (immediate mode redraws everything) | Laggy editing with large projects, high CPU while the audio runs | Custom painting with viewport culling, cached layout/meshes keyed by document generation, repaint only on change or playback, `puffin` profiling, 10k-note benchmark from Prompt 4 |
| 4 | **Offline/real-time mismatch** | Exports that sound different from what the user heard | Single render path, anchored quantum grid, seeded randomness, no wall-clock; golden render tests per platform; cross-platform tolerance tests |
| 5 | **Timing errors**: rounding at 44.1 kHz, tempo changes, loop wraps, swing, long sessions | Flams, drift, doubled or dropped notes at loop points | Integer ticks; absolute tick-to-sample conversion from anchors; half-open intervals everywhere; exhaustive scheduler tests at 44.1/48/96 kHz with odd buffer sizes (Prompt 2) |
| 6 | **Third-party plugin instability** (Prompt 11) | A crashing plugin takes the whole DAW and the unsaved project with it | CLAP first; autosave before plugin load; bypass and quarantine on failure; out-of-process hosting evaluated as a follow-up; keep v1 usable with built-ins only |
| 7 | **Denormals, NaN and Inf** in feedback DSP | CPU spikes on silence, permanently broken channels, speaker-damaging output | FTZ/DAZ; per-insert NaN guard; limiter on master by default in new projects; silence-input CPU tests; filter stability tests at extreme settings |
| 8 | **Document/engine consistency** with undo, rapid edits and in-flight commands | Stale parameter writes, engine state that disagrees with the UI | One source of truth (the document); engine state derived only through compile; generation counters on graph and song; UI coalesces parameter writes |
| 9 | **Project format evolution** | Old projects stop loading; silent data loss | Versioned schema with explicit migrations; on-disk fixtures for every version tested in CI; never remove a field without a migration; backup copy before overwriting an older-version file |
| 10 | **Scope and solo-developer bandwidth** | Many half-finished features, nothing shippable | Demoable milestone per prompt; v1 limited to built-in instruments and effects; "scope check" utility prompt before each phase; explicit cut lists in ROADMAP.md |

Honourable mentions: Linux packaging differences (AppImage vs .deb and audio group membership),
HiDPI and multi-monitor issues in winit, MIDI hot-plug (midir has no notifications, so ports are
polled), and float determinism across compilers.

---

## 9. Decision log

| ID | Decision | Why | Revisit when |
|---|---|---|---|
| D1 | Document in `gt-core`, edits/persistence in `gt-project`, compiler in `gt-engine` | Engine can compile the document without depending on undo or file code; offline export and tests need only gt-core + gt-engine | Never expected |
| D2 | gt-engine does not depend on cpal | Same `process` for device, export and tests | — |
| D3 | `rtrb` SPSC queues, one per producer thread | Wait-free, tiny, MIT/Apache; avoids MPMC on RT path | If a third producer appears, add a queue rather than switching type |
| D4 | Snapshot swap + garbage queue instead of shared mutable state | No locks, no frees on the audio thread | — |
| D5 | Fixed 64-frame render quantum with FIFO adapter | Deterministic control-rate grid; offline parity | If plugin hosting shows overhead from small blocks, raise to 128 |
| D6 | 960 PPQ integer ticks; absolute conversion via anchors | Exact triplets and straight divisions; no drift | — |
| D7 | Control-rate automation (32 frames) + per-sample smoothing; sample-accurate notes | Cheap and click-free; matches Prompt 8 | Prompt 8 may promote cheap params to sample-accurate |
| D8 | Notes are the only storage for steps and piano-roll notes | One model, two views; no sync bugs | — |
| D9 | JSON in a zip container (`.gloom`) | Easy migrations on `serde_json::Value`, universal tooling | — |
| D10 | Parameters stored normalized 0..1 with explicit stable indices | Uniform automation, MIDI learn and plugin parity (CLAP uses the same idea) | — |
| D11 | `catch_unwind` around the audio callback, release keeps `panic = "unwind"` | Last-resort protection; output silence instead of tearing down the process | If it costs measurable CPU (it should not) |
| D12 | Rust edition 2021, stable toolchain | As specified in the project instructions | Edition 2024 migration can be a standalone chore later |
| D13 | License GPL-3.0-or-later | Project instructions say GPL-3.0; "or later" is the FSF-recommended form | Owner may prefer GPL-3.0-only |
| D14 | Step 1 controlled the engine with two atomics instead of the `rtrb` command queue | The only command was start/stop | **Done in Step 2:** replaced by the queue |
| D15 | Ship both eframe renderers: wgpu by default, glow (OpenGL) as automatic fallback | Machines without Vulkan/DX12 drivers (VMs, old GPUs, remote desktops) still get a window | If glow is never needed in practice, drop it in Step 12 to save binary size |
| D16 | cpal 0.18 and eframe/egui 0.36 | Current releases at Step 1 | Upgrade deliberately, one step at a time |
| D17 | Stop returns to where playback last started; Stop while stopped returns to bar 1. Pause holds the position | Familiar from pattern-based DAWs; two keypresses reach bar 1 | — |
| D18 | Loop wrap decided in the frame domain; playback located past the loop end plays on without wrapping | Same rounding as events, so the wrap frame and the loop-start event can never disagree | — |
| D19 | The render quantum is mono until the mixer exists; it is copied to every device channel | Nothing stereo exists yet; avoids half-built bus code | **Done in Step 3:** planar stereo quantum; L/R go to device channels 1 and 2 (others silent), a mono device gets (L+R)/2 |
| D20 | Metronome: 60 ms damped sine burst (1 kHz, 1.6 kHz downbeat, -9 dBFS), cosine start | Original synthesized sound, no sample needed; non-zero first sample makes onsets measurable in tests | — |
| D21 | Step 3 document is a subset of §6: channels in a `Vec` (index = engine slot), `SampleSource` instead of a sample table, one rack-wide swing (0 to 1, up to half a step; 0.67 ≈ triplet) | Enough for the rack without inventing persistence details early | Prompt 9 adds IDs maps, the sample table and serde; per-channel swing if wanted |
| D22 | 64 channel slots × 16 voices preallocated in `create()`; any rack change resends every channel's params (boxed) and the song | No graph yet; resending 64 small boxes is cheaper than tracking diffs | Prompt 6 replaces slots with graph nodes and `SetParam` |
| D23 | Sampler reads with linear interpolation | Cheap; fine for drums and moderate transposition | Offer a sinc/polyphase reader when pitched sampling quality matters (Prompt 12 or earlier) |
| D24 | Pattern mode only: the current pattern repeats from tick 0 along the timeline; the transport loop still applies and a wrap releases held notes; changing the song releases held notes | Simplest correct behaviour before the playlist exists | Prompt 7 adds song mode |
| D25 | Samples are resampled to the engine rate at load (rubato sinc, 128 taps, Blackman-Harris², ×256 oversampled table); voices still include the rate ratio | Load-time quality, zero per-voice cost; cached samples stay in tune if the device rate later changes | Re-resample the cache on a rate change if the ratio path proves audible |
| D26 | Built-in drums are generated by code at load (`gt_dsp::drums`), output dedicated CC0; no audio files in the repo | Satisfies "original CC0 samples" with zero binary assets and exact reproducibility | Ship rendered `.flac` files only if users want to export them |
| D27 | Default channel volume -4 dB, metronome off by default in the app | Three full-scale drums on one step stay under 0 dBFS without a limiter; the beat replaces the click as the first thing you hear | Prompt 6 adds the master limiter/meters; revisit defaults then |
| D28 | Undo by snapshot diff: the UI edits the document, `History::commit` runs when no mouse button is down and no text field is focused, and records `Edit::Notes` (one channel's notes in one pattern, before/after) or `Edit::Whole`. The selected pattern is view state and is never undone | Every existing view (rack, sampler, piano roll) gets undo without writing an inverse for each gesture; a drag is one step; a note edit in a 10,000-note pattern stores only the touched lists | Prompt 6/8: if whole-document copies get large (many channels, automation), add more fine-grained `Edit` kinds to `diff` |
| D29 | A pattern's length is its step count, extended to whole 4/4 bars so every note fits | Notes drawn past the grid in the piano roll play instead of being cut, as in other pattern-based DAWs | Prompt 7 (playlist) may add an explicit pattern length |
| D30 | Piano roll keeps each channel's notes sorted by (start, key); drawing visits only notes found by binary search from `view start - longest note` to the view end, for notes and ghosts alike. Selection is a `Vec<bool>` parallel to the notes, re-sorted with them | O(log n + visible) per frame without a separate cache that could go stale; measured ~4 ms for 10,000 visible notes | Cache tessellated meshes keyed by a document generation if the playlist (Prompt 7) needs it |
| D31 | Piano roll shortcuts (pointer over the roll): Delete/Backspace, Ctrl+A, Ctrl+C/X/V, Ctrl+D duplicate after the selection, arrows move by snap / a semitone, Shift+Up/Down an octave, Q quantize, P draw, E select; app-wide Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y, F6 rack, F7 piano roll. Copy puts a text marker on the system clipboard, because the desktop only sends a paste event when it holds text | Familiar from FL-style DAWs; keys only act where the pointer is, so they never fight text fields | Prompt 10 may add a keymap file |
| D32 | A synth patch is a flat `[f32; N]` of plain values indexed by `SynthParam`, with a `ParamInfo` table (key, range, curve, unit, display) in gt-core, plus 8 `ModSlot`s. gt-dsp has its own typed `SynthSettings`; `gt_engine::synth_settings` maps one to the other. gt-ui now depends on gt-dsp only to draw the filter response with the same formula the filter uses | Keeps gt-core and gt-dsp independent (§2); one table drives knobs, formatting, presets and later automation (Step 8) | Move `ParamInfo` to a shared parameter registry in Step 8 |
| D33 | Gloom Synth computes modulation, pitch and filter coefficients every 16 frames and ramps filter `g` and the per-voice gains linearly across the block | 16 voices × 7 unison × matrix per sample is too costly; 16 frames (0.33 ms) is below audible stepping for LFOs and envelopes | Lower to 8 frames if fast envelope snaps sound stepped |
| D34 | Filter: 4-pole zero-delay-feedback ladder (Zavalishin) solved per sample, `tanh` (Padé) on the input after feedback, input gain `1 + k/2` to keep the passband level, `k ≤ 4`, cutoff clamped to 10 Hz..0.45 fs. No oversampling | ZDF stays stable and in tune up to Nyquist without oversampling, at about 2 ms per second of audio per voice; the UI curve uses the same analytic response | Add 2× oversampling of the nonlinearity behind a quality switch if aliasing at high drive is audible |
| D35 | Voice stealing: free voice, else oldest releasing, else oldest; a stolen voice keeps its state and its envelopes restart from the current level (`Adsr::retrigger`) | Avoids the click of a hard cut without a second voice for a 2 ms fade | Revisit with a fade if retriggered tails sound wrong on pads |
| D36 | Presets are JSON files (`.gloomsynth`, format `gloomtunes-synth-preset` v1) keyed by stable parameter keys, loaded tolerantly (unknown keys ignored, missing keys default, values clamped). Folder: `%APPDATA%\GloomTunes Studio\Presets\Gloom Synth`, or `$XDG_DATA_HOME`/`~/.local/share/gloomtunes-studio/presets/gloom-synth`. Factory presets live in code | Human-readable, survives added or renamed parameters, shareable as single files | Project files (Step 9) embed the same key/value map |
| D37 | The oscilloscope is a 4096-sample ring of `AtomicF32` in `Telemetry`, fed with the mono mix of one channel chosen by `SetScopeChannel` | No queue or lock; tearing between quanta only affects a picture | Move to the mixer's metering taps in Step 6 |
| D38 | Mixer: strip 0 is the master, 1-64 inserts, 65-68 send buses; channels feed the master or an insert. Inserts output to the master, an insert or a send; sends output only to the master or another send, and inserts feed sends through post-fader levels. Sidechain keys are a third kind of edge. `gt_core::Mixer` refuses routes that would close a loop (`can_route`, `can_sidechain`) and sorts the graph (`processing_order`, Kahn, master last). Solo keeps every strip on a path into or out of a soloed strip audible. Strips use a balance law (unity at centre); channels keep equal-power pan | Sends can never feed back into inserts, so send levels never need a cycle check; the UI only lists valid targets | Pre-fader sends if users ask |
| D39 | The app diffs the document mixer against what it last sent, every frame: `SetMixer(Box<MixerParams>)` when strip settings change, `SetEffect` with a new boxed effect only when a slot's kind changes, `SetEffectParam` per changed value otherwise. Effects are created and configured on the UI thread (`create_effect`) and old ones come back as garbage | Edits, undo and (later) loading share one path; effects that stay keep their state, so reverb tails survive adding channels or undoing a fader move | Replace with `ApplyGraph`/`SetParam` once Prompt 8 gives every parameter a slot |
| D40 | Effects implement `gt_dsp::fx::Effect` (`set_param(index, plain value)`, `process` in place on planar stereo, `latency`, `meter`). Their parameter tables live in `gt_core::effects`, in the same index order; a gt-engine test checks the counts match. Continuous parameters glide with a 20 ms one-pole; EQ coefficients are recomputed every 16 frames | Keeps gt-dsp and gt-core independent (§2) like D32 | Move tables into the Prompt 8 parameter registry |
| D41 | Effect designs: RBJ biquads (TDF-II) for the EQ; feed-forward compressor with soft knee, dB-domain attack/release; delay with one-pole "tone" in the feedback and linear interpolation; 8-line FDN reverb with Hadamard feedback, per-line RT60 gains, damping and 4 input allpasses; chorus with one swept line per side; distortion with first-order ADAA (antiderivatives in f64) instead of oversampling; limiter with instant-attack release, running minimum and moving average over a 1.5 ms look-ahead, so the ceiling is guaranteed | Each is the simplest design that meets the step's bar at under 4 ms per second of audio | Oversampled distortion and a Dattorro plate as alternatives later |
| D42 | Delay compensation: each strip's input latency is the largest output latency feeding it; every edge (channel input, output, each send) gets a fixed-capacity delay line (512 frames) set to the difference; the total is published as `latency_frames`. Latency counts enabled effects only | Real PDC at low cost (only the limiter has latency in v1); the same scheme serves CLAP plugins in Prompt 11 | Larger capacity for plugins with long latency |
| D43 | Effects are reset when playback starts from Stop, not on pause or locate | Offline export (Step 9) must match a real-time capture from a stopped transport (§7.5) | Revisit if users want tails to continue across a restart |
| D44 | One `SongSnapshot` type for both play modes: a `repeat` flag says whether events loop at `length` (pattern mode) or sit on the absolute timeline (song mode). Song mode loops 0 to the bar after the last clip when the user loop is off | One scheduler path; switching modes is just another `SetSong` | Per-bar index for faster locate in long songs |
| D45 | Audio clips are scheduled in seconds, not ticks: the compiler converts clip start, end and origin (start minus slip offset) through the tempo map, and the transport records each quantum's `PlaySegment { offset, frames, seconds }` so the processor reads the right sample frame even across tempo changes and loop wraps | Sample-accurate audio under tempo changes without stretching; matches the exact-frame test in `tests/song.rs` | Time-stretch mode per clip (post-v1) |
| D46 | Time signatures are a `TimeSigMap` of changes on bar indices (`SigChange { bar, sig }`), not ticks; the transport derives beats and downbeats from it and the UI snaps to it | A signature change mid-bar has no musical meaning; storing bars makes it impossible | — |
| D47 | Automation: clips on the same target merge into one lane (later-starting clip wins where they overlap), evaluated once per 64-frame quantum while playing in song mode. Strip volume and pan are written to override fields on the strip (`auto_gain`, `auto_pan`) so the document value survives; an override clears on `SetSong` and when the user moves the control. Effect parameters go through the same `set_param` path as the UI, and the app resends document values of parameters the previous song automated | No allocation or locks; the strip smoothing hides the 1.3 ms steps. Replaced by the general parameter registry in Step 8 | Per-sample ramps, curve types (Step 8) |
| D48 | Each playlist track has an `insert`; its audio clips render into that mixer strip's input. Pattern clips still play through their channels' own routing | Audio clips need a destination without a channel; keeps channel routing unchanged | Per-clip routing |
| D49 | Waveform peaks: a min/max pyramid (64 frames per pair at the base, 4x per level) built by the sample loader thread alongside decoding; the UI picks the coarsest level whose pairs are at most 1/16 of a pixel column | One pass on a worker thread; drawing costs a few reads per column at any zoom | Disk cache of peaks next to the project (Step 9) |
| D50 | Automation points live inside their clip (`ClipKind::Automation(Automation)`), so duplicating a clip copies its curve | Simpler undo and duplication than a shared `IdMap<AutomationId, _>`; Step 8 may add linked clips | Linked/"ghost" automation clips |

---

## 10. Dependencies and licenses

Planned dependencies, introduced only when a prompt needs them. All are compatible with
GPL-3.0-or-later. **Flagged** entries are copyleft or have special terms.

| Crate | Purpose | License | Prompt |
|---|---|---|---|
| cpal | audio I/O | Apache-2.0 | 1 |
| eframe / egui | GUI | MIT OR Apache-2.0 | 1 |
| rtrb | lock-free SPSC queues | MIT OR Apache-2.0 | 2 |
| assert_no_alloc | RT-safety checks (dev/debug) | BSD-2-Clause | 2 |
| log, env_logger | logging | MIT OR Apache-2.0 | 1 |
| symphonia 0.6 (`mp3` feature on) | decoding wav/flac/mp3/ogg | **MPL-2.0 (flag: file-level copyleft; compatible with GPL-3.0)**; added in Step 3 | 3 |
| rubato 5 (+ audioadapter crates) | resampling | MIT OR Apache-2.0; added in Step 3 | 3 |
| hound | WAV writing (Step 3: test-only dev-dependency) | Apache-2.0 | 3/9 |
| serde, serde_json | serialization; added in Step 5 for presets | MIT OR Apache-2.0 | 5/9 |
| zip | project container | MIT | 9 |
| midir | MIDI I/O | MIT | 10 |
| midly | SMF import/export | Unlicense | 10 |
| criterion 0.8 (`cargo_bench_support` only, no plotters) | benchmarks (dev); added in Step 5 | MIT OR Apache-2.0 | 5 |
| insta | snapshot tests (dev); added in Step 5 | Apache-2.0 | 5 |
| puffin | profiling | MIT OR Apache-2.0 | 4/12 |
| audio_thread_priority | RT priority on Linux | **MPL-2.0 (flag)** | 12 |
| clack-host | CLAP hosting | MIT OR Apache-2.0 (verify at Prompt 11) | 11 |
| ASIO SDK (Windows, optional feature) | ASIO backend | **Steinberg terms (flag): not redistributed; user supplies it** | 12 |
| VST3 SDK (design only) | VST3 hosting | **Verify at Prompt 11: Steinberg has offered it under GPLv3 and proprietary terms, and more recent releases are reported under MIT** | — |

Licenses are re-checked whenever a dependency is added; `cargo deny` (Prompt 12) enforces an
allow-list in CI.

---

## 11. Testing strategy

| Layer | What | Tool |
|---|---|---|
| gt-dsp | Frequency/impulse responses of filters and EQ, oscillator aliasing bounds, envelope timing, smoother convergence, silence-in/silence-out, no NaN at extreme parameters | `cargo test`, criterion |
| Scheduler | Event frame positions at 44.1/48/96 kHz, tempo changes, loop wrap, odd buffer sizes, long-run drift | `cargo test` |
| Engine | `process` under `assert_no_alloc`; graph patches keep node state; garbage is returned | `cargo test` |
| Render | Short reference renders as `insta` snapshots (downsampled or hashed) | insta |
| Project | Round-trip save/load; every fixture version migrates; cycle rejection | `cargo test` |
| Export | Golden project renders to a stored per-platform hash; cross-platform tolerance check | `cargo test` |
| UI | Piano roll at 10k notes stays under 16 ms per frame | puffin + manual benchmark |
| CI | build, test, clippy `-D warnings`, rustfmt check on windows-latest and ubuntu-22.04 | GitHub Actions |
