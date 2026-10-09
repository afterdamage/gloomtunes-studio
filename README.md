# GloomTunes Studio

An original, open-source, pattern-based digital audio workstation written in Rust, for
Windows 10/11 and Ubuntu 22.04+ from a single codebase. Channel rack, piano roll, mixer and
playlist, with built-in instruments and effects, wrapped in a dark, moody "Gloom" interface.

> **Status:** v0.1 alpha, Step 11 of the [roadmap](ROADMAP.md): CLAP instrument and effect
> plugins load into the channel rack and the mixer, with their own editors, automatable
> parameters, saved state and crash protection. A MIDI keyboard (plugged in any
> time) or the computer keyboard plays the selected channel, takes record into the piano roll
> with a count-in, any control can be MIDI-learned, and MIDI files import and export. Projects
> save to `.gloom` files (optionally with their samples inside), autosave guards against
> crashes, and songs, loop regions or per-track stems export to WAV. Every knob and fader can
> be automated with curved automation clips or modulated by LFOs and envelope followers, in a
> playlist that arranges pattern, audio and automation clips on unlimited tracks, with tempo and
> time-signature changes, markers and a loop region, on top of a mixer with 64 inserts, 4 send buses and eight built-in
> effects, Gloom Synth (a subtractive polysynth with presets), a piano roll with undo/redo, a
> channel rack with a step sequencer, a sampler, a sample browser, four built-in drums and a
> sample-accurate transport. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design.

## Goals

- Pattern-based workflow: step sequencer, piano roll, mixer, playlist.
- Rock-solid real-time audio: no allocation, locks or I/O on the audio thread.
- Sample-accurate timing, and offline export that matches what you hear.
- Built-in instruments (sampler, Gloom Synth) and effects, plus CLAP plugins.
- All code, UI, sounds and presets original or CC0.

## Building and running

You need a stable Rust toolchain from [rustup.rs](https://rustup.rs) (the repository pins
`stable` with rustfmt and clippy in `rust-toolchain.toml`).

### Ubuntu 22.04+

```sh
sudo apt install build-essential pkg-config libasound2-dev libudev-dev \
                 libxkbcommon-dev libwayland-dev libxkbcommon-x11-0
cargo run --release -p gt-app
```

Audio goes through ALSA, which reaches PipeWire or PulseAudio through their ALSA plugins on a
standard desktop. Optional backends: `--features jack` (needs `libjack-jackd2-dev`) or
`--features pipewire` (needs `libpipewire-0.3-dev`), then select with `GT_AUDIO_HOST=jack`.

### Windows 10/11

Install Rust with the MSVC toolchain (rustup offers the Visual Studio Build Tools), then:

```powershell
cargo run --release -p gt-app
```

Audio goes through WASAPI (shared mode). ASIO is an optional feature (`--features asio`) that
needs the Steinberg ASIO SDK, which is not shipped with this project.

### Using it

- **Space** plays and pauses. The app opens on the **Playlist** with a 12-bar demo song (an
  intro, the main beat, a break with a delay swell and the beat again) in **Song** mode. **Pat**
  (or **L** to toggle) loops just the current pattern instead.
- **Playlist** (tab or **F5**):
  - Tools: Draw (**P**) places the pattern picked in the toolbar, Select (**E**) or Ctrl+drag
    draws a selection box, Slice (**C**) splits a clip where you click. Snap is bar, beat, 1/8,
    1/16 or off; hold Alt to ignore it.
  - Drag a clip to move it (also to another track), drag either edge to resize, Shift+drag to
    slip the content inside the clip, right-click or right-drag to delete, double-click a
    pattern clip to open it in the piano roll. Delete, Ctrl+A, Ctrl+D (duplicate after the
    selection) and **M** (mute clips) work while the mouse is over the playlist.
  - Drag a file from the browser onto a track to add an audio clip; its waveform appears once it
    has loaded. The track's "audio to" box picks the mixer insert its audio goes to.
  - **+ Automation** adds a clip for any parameter (channels, synth knobs, mixer, effects).
    Click inside it to add a point, drag points, right-click one to delete it. Right-click
    between two points to pick that segment's curve (hold, linear, smooth, bezier); Ctrl+drag
    between them to bend it.
  - Track names are editable; M and S mute and solo a track; right-click a header for colours,
    insert and delete. **+ Track** adds one; there is no limit.
  - The ruler: click the bars row to move the playhead, drag it to set the loop. Right-click to
    add a marker, tempo change or time-signature change; drag a flag to move it, double-click to
    edit it.
  - Wheel scrolls tracks, Shift+wheel scrolls time, Ctrl+wheel zooms, Alt+wheel changes track
    height.
- **Channel rack** (tab or **F6**): click a step to toggle it; scroll over a lit step to change its velocity.
- **M** and **S** mute and solo a channel; the two small knobs are volume and pan (drag up/down,
  Shift for fine, double-click to reset).
- Hold a channel's name to hear it; right-click it to delete the channel. **+ Sampler** and
  **+ Gloom Synth** add one. A small mark in a step means a note starts inside it (drawn in the
  piano roll, off the step grid).
- The pattern bar creates, clones, renames and selects patterns, and switches 16 or 32 steps and
  swing.
- The browser on the left plays a sound when you click it and loads it into the selected channel
  when you double-click it (wav, flac, mp3, ogg). The panel at the bottom edits the selected
  channel's sample: pitch, start/end, one-shot or loop, and the ADSR envelope.
- Select a Gloom Synth channel and the bottom panel shows the synth: two oscillators, sub, noise
  and unison, the filter with its response curve, amp and mod envelopes, two LFOs, glide, an
  8-slot modulation matrix (source, destination, amount) and an oscilloscope of that channel.
  Knobs drag up/down (Shift for fine, double-click to reset). The **Preset** menu lists the
  factory sounds and your own; **Save preset** writes the sound under the name beside it to
  `%APPDATA%\GloomTunes Studio\Presets\Gloom Synth` on Windows or
  `~/.local/share/gloomtunes-studio/presets/gloom-synth` on Linux, as a `.gloomsynth` JSON file.
- **Audio** (top right) opens the device settings.
- **Piano roll** (tab, **F7**, or right-click a channel name) edits the selected channel's notes
  in the current pattern; **F6** goes back to the channel rack.
  - Click to draw a note, drag it to move, drag its right edge to resize, right-click to delete.
    Ctrl+drag (or the Select tool, **E**) draws a selection box; Shift adds to the selection.
  - Wheel scrolls, Shift+wheel scrolls sideways, Ctrl+wheel zooms time, Alt+wheel zooms keys.
  - Drag in the velocity lane at the bottom to set velocities.
  - Keys while the mouse is over the roll: Delete, Ctrl+A, Ctrl+C/X/V, Ctrl+D (duplicate),
    arrows (move by the snap or a semitone), Shift+Up/Down (an octave), Q (quantize).
  - The toolbar sets snap (1/4 to 1/32, triplets), scale highlighting, ghost notes from the other
    channels, quantize strength and humanize amounts.
- **Mixer** (tab or **F9**) shows the master, 64 inserts and 4 send buses. Each channel in the
  rack has an insert box (M is the master); new channels get a free insert named after them.
  - Each strip has a fader (double-click resets to 0 dB), balance, mute, solo, polarity (Ø) and
    peak/RMS meters. Click a strip to edit it on the right.
  - The detail panel sets where the strip goes (master, an insert or a send bus), the sidechain
    source and, on inserts, four post-fader send levels. Routes that would make a loop are not
    offered.
  - Ten effect slots per strip: pick an effect in a slot's menu, switch it On/Off, click it to
    edit. The EQ shows its curve: drag a handle to move a band; the knobs below set type, gain and Q. Compressors
    show gain reduction. Set a compressor's Sidechain knob to 1 to key it from the strip's
    sidechain source.
  - The demo mix sends snare and clap to a reverb bus and the hat to a delay bus, ducks the bass
    with the kick and has a limiter on the master. Latency added by the limiter is compensated on
    every path and shown on the master strip.
- **Automation and modulation from any control:** right-click a knob or fader (rack volume and
  pan, sampler, Gloom Synth knobs and mod-matrix amounts, mixer faders, pans, sends and effect
  knobs) for **Create automation clip** (a four-bar clip at the playhead, in the Playlist),
  **Add LFO** or **Add envelope follower**. A ring on a control means automation drives it, a dot
  means a modulator does.
  - **Modulators** (button next to the view tabs) lists every LFO and envelope follower. LFOs
    have six shapes, a rate in Hz or synced to the song (4 bars to 1/16 triplets) and a start
    phase; followers track any mixer strip's level with attack, release and gain. The depth is
    in percent of the control's travel; negative inverts (a follower at -50 % ducks).
  - The demo pans the hat with a one-bar LFO and ducks the reverb bus under the kick.
- **File** (top left):
  - **New** (Ctrl+N), **Open** (Ctrl+O), **Save** (Ctrl+S) and **Save as** (Ctrl+Shift+S).
    Projects are `.gloom` files; tick **Embed samples** in Save as to put copies of every
    audio file inside, so the project opens on another computer. Without embedding, sample
    paths are stored relative to the project file, so moving the project folder keeps working.
    `gloomtunes path/to/song.gloom` opens a project at start.
  - If a sample cannot be found on opening, **Missing samples** lets you pick a folder to
    search (with its subfolders) or locate each file. The title bar shows `*` while there are
    unsaved edits; New, Open and closing the window ask to save them first.
  - Every 60 s with unsaved edits, the app autosaves to `recovery/` in its data folder
    (`%APPDATA%\GloomTunes Studio` or `~/.local/share/gloomtunes-studio`). If the app crashed,
    the next start offers **Recover**.
  - **Export audio** (Ctrl+Shift+E) writes WAV: 16-bit, 24-bit or 32-bit float, 44.1 to
    96 kHz, optional dither (16/24-bit), optional normalization to a peak level, the whole song
    or the loop region, the reverb and delay tail until it fades (up to 10 s), and
    optionally one file per track ("stems", named `<file> - 01 <track>.wav`). It runs in the
    background with a progress bar and Cancel.
  - **Import MIDI file…** reads a `.mid` (format 0 or 1) into a new pattern, with one Gloom
    Synth channel per MIDI channel; tick **Use the file's tempo** to take its tempo and time
    signatures too. **Export MIDI file…** writes format 1 (one track per channel) from the
    current pattern or the whole song.
- **MIDI and live playing:**
  - Every MIDI input is connected automatically, including ones plugged in while the app runs.
    Notes play the channel selected in the rack; the dot in the transport bar blinks on MIDI
    input. The **Audio** button (top right) opens settings, which list the inputs (untick one
    to ignore it).
  - **Keys** (Ctrl+T) turns the computer keyboard into a piano: Z S X D C V G B H N J M , is
    the bottom octave, Q 2 W 3 E R 5 T 6 Y 7 U I the one above; Minus and Equals change the
    octave. Ctrl shortcuts keep working.
  - **Record** (Ctrl+R) records into the current pattern on the selected channel. From stop it
    counts in first (Off, 1 or 2 bars, in settings) and turns the click on while recording; each
    take is one undo step. **Latency** in settings moves recorded notes earlier by the output
    buffer plus an extra amount if your notes land late.
  - Right-click a knob, fader or control and pick **MIDI learn**, then move a controller (Esc
    cancels). **Forget MIDI** removes the binding. Bindings are saved with the project.
- **CLAP plugins:**
  - At start the app looks for `.clap` files in `CLAP_PATH`, the standard folders
    (`~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` on Linux; `Common Files\CLAP` and
    `%LOCALAPPDATA%\Programs\Common\CLAP` on Windows) and any folder added under **Folders**
    in the plugin browser. Each file is opened by a separate process, so a broken one cannot
    crash the app; **Rescan** looks again.
  - **+ Plugin** in the channel rack opens the browser on instruments: **Add** makes a new
    channel. In the mixer, **Plugin…** at the bottom of a slot's effect list opens it on effects
    and adds to that slot.
  - The plugin's panel (below the rack, or under the slot in the mixer) shows its status,
    **Open editor** for its own window, and a slider per parameter (right-click to automate,
    add an LFO or MIDI learn, like any knob). Changes made in the editor move the sliders.
  - A plugin that fails (an error, or NaN in its output) is bypassed and shows **Retry**. If
    the app closes while loading a plugin or opening its editor, that file is switched off at
    the next start until you choose **Allow again** under **Problems** in the browser. After a
    crash, **Recover without plugins** opens the autosave with its plugins off; retry each one.
  - Plugin settings are saved inside the project; a plugin missing on another computer keeps
    its settings and shows "Not loaded".
- **Undo** and **Redo** (top right, Ctrl+Z and Ctrl+Shift+Z or Ctrl+Y) cover notes, steps,
  channels, patterns, the mixer, the playlist, modulators, tempo and time signatures and settings.

### Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Golden export test on its own (exports a stored reference project and checks its hash):
cargo test -p gt-export --test golden
# Plugin host tests with the in-tree test plugins (gt-test-plugin):
cargo test -p gt-plugin-host
# Try the test plugins in the app: build them and install as a .clap file
cargo build -p gt-test-plugin
cp target/debug/libgt_test_plugin.so ~/.clap/gt_test_plugin.clap   # Windows: gt_test_plugin.dll to %LOCALAPPDATA%\Programs\Common\CLAP\gt_test_plugin.clap
# Piano roll frame time with 10,000 notes (prints the median):
cargo test --release -p gt-ui -- --ignored --nocapture
# Engine cost of the demo song, as a share of real time:
cargo run --release -p gt-engine --example render_cost
# Gloom Synth and effect benchmarks (1 s of audio per iteration):
cargo bench -p gt-dsp
cargo bench -p gt-dsp --bench fx
# After an intended change to the synth's sound, review the rendered snapshot
# (cargo install cargo-insta) or accept it directly:
cargo insta review          # or: INSTA_UPDATE=always cargo test -p gt-dsp --test synth_snapshot
```

### Environment variables

| Variable | Effect |
|---|---|
| `GT_RENDERER=glow` | Use OpenGL instead of wgpu (wgpu falls back to OpenGL automatically if it cannot start). |
| `GT_AUDIO_HOST=<name>` | Pick a compiled-in audio host, e.g. `jack`, `pipewire`, `asio`. |
| `RUST_LOG=debug` | More log output on the console. |
| `CLAP_PATH` | Extra folders searched for CLAP plugins (separated like `PATH`). |

## License

Copyright (C) 2026 Vasil Vasilev.

The four built-in drum sounds are generated by code in `crates/gt-dsp/src/drums.rs`; the audio they
produce is dedicated to the public domain under CC0 1.0.

GloomTunes Studio is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version 3 of
the License, or (at your option) any later version. See [LICENSE](LICENSE).
