# GloomTunes Studio

An original, open-source, pattern-based digital audio workstation written in Rust, for
Windows 10/11, Ubuntu 22.04+ and macOS 11+ from a single codebase. Channel rack, piano roll, mixer and
playlist, with built-in instruments and effects, wrapped in a dark, moody "Gloom" interface.

> **Status:** v1.0, every step of the [roadmap](ROADMAP.md) done, on Windows, Ubuntu and macOS. CLAP instrument and effect
> plugins load into the channel rack and the mixer, with their own editors, automatable
> parameters, saved state and crash protection. A MIDI keyboard (plugged in any
> time) or the computer keyboard plays the selected channel, takes record into the piano roll
> with a count-in, any control can be MIDI-learned, and MIDI files import and export. Projects
> save to `.gloom` files (optionally with their samples inside), autosave guards against
> crashes, and songs, loop regions or per-track stems export to WAV. Every knob and fader can
> be automated with curved automation clips or modulated by LFOs and envelope followers, in a
> playlist that arranges pattern, audio and automation clips on unlimited tracks, with tempo and
> time-signature changes, markers and a loop region, on top of a mixer with up to 256 inserts, free sends and eight built-in
> effects, Gloom Synth (a subtractive polysynth with presets), a piano roll with undo/redo, a
> channel rack with a step sequencer, a sampler, a sample browser, four built-in drums and a
> sample-accurate transport. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design.

## Goals

- Pattern-based workflow: step sequencer, piano roll, mixer, playlist.
- Rock-solid real-time audio: no allocation, locks or I/O on the audio thread.
- Sample-accurate timing, and offline export that matches what you hear.
- Built-in instruments (sampler, Gloom Synth) and effects, plus CLAP plugins.
- All code, UI, sounds and presets original or CC0.

## Installing

Releases on GitHub carry four packages, each with a build-provenance attestation:

- **Windows 10/11:** `GloomTunes-Studio-<version>-x64-setup.exe`. Installs for your user
  without administrator rights (the first page offers all users), adds a Start menu entry and
  can open `.gloom` projects. Until the project has a code-signing certificate, Windows
  SmartScreen warns about an unknown publisher: **More info > Run anyway**.
- **Ubuntu 22.04+ (.deb):** `sudo apt install ./gloomtunes-studio_<version>-1_amd64.deb`, then
  start **GloomTunes Studio** from the app menu or run `gloomtunes`.
- **Any recent Linux (AppImage):** `chmod +x GloomTunes-Studio-<version>-x86_64.AppImage` and
  run it. It uses the system's ALSA and graphics libraries.
- **macOS 11 or later (Apple Silicon or Intel):** open
  `GloomTunes-Studio-<version>-macos-universal.dmg` and drag **GloomTunes Studio** to
  **Applications**. Until the project has an Apple Developer ID, macOS says it cannot check the
  app for malicious software the first time: open it once with right-click (or Control-click)
  **> Open > Open**, or allow it under **System Settings > Privacy & Security > Open Anyway**.
  Releases signed and notarized with a Developer ID open normally.

To check that a file was built by this repository's release workflow:
`gh attestation verify <file> --repo afterdamage/gloomtunes-studio`. When the release also
has `SHA256SUMS.asc`, `gpg --verify SHA256SUMS.asc SHA256SUMS` and `sha256sum -c SHA256SUMS`
check the GPG signature and the checksums.

**Linux audio priority:** the app asks for real-time scheduling for its audio thread. If
Settings > Performance says it was refused, add yourself to the `audio` group
(`sudo usermod -aG audio $USER`, then log out and in) on a system whose
`/etc/security/limits.d` grants that group `rtprio`, or install `rtkit`. Audio plays either
way; real-time priority only makes dropouts under load less likely.

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

### macOS 11+

Install the Xcode command line tools (`xcode-select --install`), then:

```sh
cargo run --release -p gt-app
```

Audio goes through CoreAudio, MIDI through CoreMIDI and drawing through Metal; nothing else is
needed. Plugin editors open in their own window.

### Using it

On macOS every shortcut below that uses Ctrl uses **Cmd** instead (Cmd+S, Cmd+, and so on),
and the app shows it that way. Function keys on a Mac laptop may need **fn** held down.

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
  - Folders: right-click a header and pick **Put in a new folder** (Ctrl+click names first to
    group several). A folder's M and S mute and solo everything in it, its arrow collapses it,
    and **Route folder to a group insert** sends its tracks' audio through one insert.
  - The arrangement box at the left of the toolbar switches arrangements; its **...** menu
    makes, duplicates, renames, moves and deletes them. Patterns are shared; clips, markers and
    tempo belong to each arrangement.
  - **Edit audio...** in an audio clip's popup opens the audio editor: drag to select, click
    to place the cursor, Ctrl+wheel zooms. Trim, cut, copy, paste, silence, normalize, fades,
    reverse, gain and DC removal work on a copy with undo (Ctrl+Z); **Learn noise** on a
    noise-only selection, then **Reduce noise**. **To MIDI** turns a melody or drum loop into
    a pattern for the selected channel. **Apply** saves the edited copy into the clip.
  - Right-click a clip's name strip for its source: select all clips of it, **Make unique**, or
    replace the pattern or audio in place.
  - The ruler: click the bars row to move the playhead, drag it to set the loop. Right-click to
    add a marker, tempo change or time-signature change; drag a flag to move it, double-click to
    edit it.
  - A tempo change's **Ramp** (linear, smooth, bezier) glides the tempo to the next change
    instead of stepping. Right-click the BPM box for **Create tempo automation clip**: while it
    plays, its curve is the tempo.
  - A marker's action (double-click it): **Loop to next** loops up to the next marker,
    **Stop**, **Pause**, or **Skip to** another marker, at the exact sample, in song mode.
    Exports play straight through.
  - **Performance** (toolbar) turns the playlist into a launch grid: a row per track, a cell
    per clip. Click a cell to launch it on the next bar or beat (**Launch on**); it loops until
    another cell or the track's stop button is launched. Keys 1 to 9 launch a column on every
    track; MIDI notes on channel 16 launch too (from C1, eight keys per track, the key after a
    track's last cell stops it). With **Record performance** on, stopping playback turns what
    was played into a new arrangement.
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
  `%APPDATA%\GloomTunes Studio\Presets\Gloom Synth` on Windows,
  `~/Library/Application Support/GloomTunes Studio/Presets/Gloom Synth` on macOS or
  `~/.local/share/gloomtunes-studio/presets/gloom-synth` on Linux, as a `.gloomsynth` JSON file.
  - **Engine: Additive** builds the sound from 64 sine partials. Drag across the harmonic bars
    to draw the levels (or pick Sine, Saw, Square, Triangle, Organ); Tilt, Even and Stretch
    shape the spectrum. **Load…** under Resynthesis analyses a pitched recording into harmonic
    levels over time: Morph blends it with the bars, Position and Scan pick where and how fast
    it plays. The spectral filter (low/high/band-pass, notch, comb, formant vowels) shapes the
    partials directly, so it never rings or clips.
- **+ Patcher** in the rack adds a modular patch, and **Patcher** in a mixer slot's menu adds
  one as an effect. Pick modules from **＋ Module** (voices, oscillators, noise, filters,
  envelopes, LFOs, amps, mixers, maths, transpose, Gloom Synth, any built-in effect, or a
  plugin), drag from an output to an input to wire them (audio in the accent colour,
  modulation gold, notes blue), and drop a modulation output on a module to move one of its
  parameters, plugin ones included. The selected module's knobs and the wires into it, with
  their amounts, sit under the canvas. Modules after **Voices** play once per note (up to 8); loops are allowed.
- **File > Start from an idea…** builds a starter song from a style, tempo, key and mode:
  drums, bass, chords and a lead from the factory sounds, as patterns arranged into a 28-bar
  song with markers. **Variation** (or **Next**) picks another progression and melody; the same
  settings always build the same song. Nothing goes online.
- **First start:** a short wizard picks the audio output and buffer size (with a test tone),
  an accent colour and text size, and asks whether to save crash reports (off unless you turn
  it on). **Skip** keeps the defaults. Everything it sets is in Settings afterwards.
- **Settings** (top right, **File > Settings…** or Ctrl+,), saved in `settings.json` next to
  the presets:
  - *Audio and MIDI*: output device, buffer size, sample rate (the device's default or any
    standard rate it supports), MIDI inputs and recording options. On Windows an *Exclusive
    mode* row explains that it is not available yet (use an ASIO build for the lowest latency).
  - *Shortcuts*: every global command with its keys. **Set** waits for a key (Esc cancels);
    a key already used elsewhere moves to the new command and a message says which.
    **Clear**, **Default** and **Reset all** undo changes. Keys inside the piano roll,
    playlist and rack and the typing keyboard's notes are fixed.
  - *Theme*: accent presets, every colour of the Gloom theme, text size and corner radius;
    **Reset to Gloom** restores the original.
  - *Performance*: audio CPU load and peak, late callbacks and device underruns (**Reset**
    zeroes them), buffer length, UI frame time, memory (Linux), startup time, renderer and
    whether the audio thread got real-time priority.
  - *Privacy*: the crash-report switch (below).
- **CPU meter** (top right, next to the level meter): how much of each audio buffer's time the
  engine needs, with a tick for the recent peak. It turns red for a few seconds after a
  dropout (a callback that came too late, or an underrun reported by the device); if that
  happens often, raise the buffer size. Click it for the Performance page.
- **Crash reports (opt-in):** when switched on and the app crashes, a text report is saved in
  `crashes/` in the data folder. At the next start the app shows it with **Report on
  GitHub…**, which opens a new issue in your browser with the report filled in, for you to
  read, edit and submit. Nothing is sent automatically, and nothing at all is saved while the
  switch is off.
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
- **Mixer** (tab or **F9**) shows the master and the inserts (64 to start; **+** adds more, up to
  256, and the bin next to a strip's name deletes it). Each channel in the
  rack has an insert box (M is the master); new channels get a free insert named after them.
  - Each strip has a fader (double-click resets to 0 dB), balance, mute, solo, polarity (Ø) and
    peak/RMS meters. Click a strip to edit it on the right.
  - The detail panel sets where the strip goes (the master or another insert), the sidechain
    source and, on inserts, up to eight sends to other strips, each with a level and a Pre
    switch (pre-fader). Routes that would make a loop are not offered. The master's panel sets
    the pan law (0, -3, -4.5 or -6 dB at centre).
  - Give a strip a colour and an icon, link a playlist track to it (the track plays into it and
    lends it its name and colour), and save or load strip and effect-chain presets from the
    strip's Presets menus. Export can write stems per track or per mixer insert.
  - Ten effect slots per strip: pick an effect in a slot's menu, switch it On/Off, click it to
    edit. The EQ shows its curve: drag a handle to move a band; the knobs below set type, gain and Q. Compressors
    show gain reduction. Set a compressor's Sidechain knob to 1 to key it from the strip's
    sidechain source.
  - The demo mix sends snare and clap to a reverb insert and the hat to a delay insert, ducks the bass
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
  - **Formula** modulators compute their value from an expression, for example
    `tri(beat / 4) * wheel` or `0.5 * sin(2 * pi * t) * m3` (`m3` is the output of modulator
    3; each row shows its number). Hover the formula field for the list of names and
    functions; a ⚠ marks a formula that does not compile (it outputs 0).
  - **＋** on a modulator's row adds an LFO, follower or formula to its depth or rate, so
    modulators move each other (an LFO whose rate follows another LFO, a depth that follows
    a follower).
  - Gloom Synth's matrix can also move each voice's LFO depths, mod-envelope amount and
    envelope times (attack, decay and release are taken when the note starts, so velocity or
    key can set them per note).
  - **Note curves** (piano roll bottom lane, under **Note curves**): drag over a note to draw
    its own tuning, pressure or timbre curve, played on that note's voice only (polyphonic
    automation; plugins get it as note expressions). With notes selected, only they are
    drawn on; right-click removes a point. A note carries one curve of up to 8 points.
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
    (`%APPDATA%\GloomTunes Studio`, `~/Library/Application Support/GloomTunes Studio` or
    `~/.local/share/gloomtunes-studio`). If the app crashed,
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
    input. **Settings > Audio and MIDI** lists the inputs (untick one to ignore it; the choice
    is remembered).
  - **Keys** (Ctrl+T) turns the computer keyboard into a piano: Z S X D C V G B H N J M , is
    the bottom octave, Q 2 W 3 E R 5 T 6 Y 7 U I the one above; Minus and Equals change the
    octave. Ctrl shortcuts keep working.
  - **Record** (Ctrl+R) records into the current pattern on the selected channel. From stop it
    counts in first (Off, 1 or 2 bars, in settings) and turns the click on while recording; each
    take is one undo step. **Latency** in settings moves recorded notes earlier by the output
    buffer plus an extra amount if your notes land late.
  - Right-click a knob, fader or control and pick **MIDI learn**, then move a controller (Esc
    cancels). **Forget MIDI** removes the binding. Bindings are saved with the project.
  - **+ MIDI out** in the rack adds a channel that plays an external synth: pick its output
    port, MIDI channel, program and bank, and the device's latency (notes go out that much
    early). Its patterns, recorded controllers and live playing go to the device.
  - **Settings > Audio and MIDI > MIDI outputs and sync**: tick **Clock** or **Time code** on
    an output to send MIDI clock (with start, stop and song position) or MTC at 24, 25 or 30
    fps. **Follow clock from** makes the transport and tempo follow another device's clock.
  - Tick **MPE** on an input for an MPE controller: each note's own bend, pressure and timbre
    (CC 74) play on its voice and its starting tuning and timbre are recorded into the note.
  - **Controller mappings**: each input picks a mapping (the generic one unless a mapping
    file names the port): knobs CC 21 to 28 move the selected channel's volume, pan, cutoff,
    resonance and envelope, CC 7 on channels 1 to 8 (16) the mixer faders (master), CC 112 to
    119 are previous/next channel, metronome, loop, previous pattern, stop, play and record.
    Pick another or **None** per input in settings. Your own `*.gtmap.json` files go in the
    `controllers` folder of the data folder (shown in settings, **Reload** rereads them):
    `{"name": "...", "ports": ["part of the port name"], "controls": [{"name": "Pad 1",
    "type": "pad", "channel": 10, "note": 36, "transport": "play"}]}`, where a control takes
    `cc` or `note` and one of `param` (a key like `channel/selected/volume`,
    `channel/selected/synth/filter.cutoff`, `mixer/3/pan`, or a list tried in order),
    `transport`, `play_note` or `script`; `"relative": true` for endless encoders. A
    mapping's `script` (or `script_file`) is Rhai with `fn on_cc(channel, cc, value)` and
    `fn on_note(channel, key, velocity)` returning `true` when it handled the message; it may
    call `play()`, `stop()`, `record()`, `toggle_loop()`, `toggle_metronome()`,
    `set_tempo(bpm)`, `select_channel(i)`, `set_param(key, v)`, `set_volume(strip, v)`,
    `set_pan(strip, v)`, `play_note(key, vel)`, `release_note(key)` and read `tempo()`,
    `playing()`, `channel()`, `channel_name()`, `param(key)`. Learned bindings win over
    mappings.
- **CLAP plugins:**
  - At start the app looks for `.clap` files in `CLAP_PATH`, the standard folders
    (`~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` on Linux; `Common Files\CLAP` and
    `%LOCALAPPDATA%\Programs\Common\CLAP` on Windows; `~/Library/Audio/Plug-Ins/CLAP` and
    `/Library/Audio/Plug-Ins/CLAP` on macOS, where plugins are `.clap` bundles) and any folder
    added under **Folders**
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
  - Tick **Own process** next to a plugin in the browser to run it in a process of its own:
    if it crashes, only that process stops, the plugin is bypassed for a moment and started
    again with its last saved state (three times at most), and the app keeps playing. Its
    instances reload when you switch it. Such a plugin has no extra outputs, MIDI out or
    factory presets, and costs some extra CPU.
  - Plugin settings are saved inside the project; a plugin missing on another computer keeps
    its settings and shows "Not loaded".
- **Undo** and **Redo** (top right, Ctrl+Z and Ctrl+Shift+Z or Ctrl+Y) cover notes, steps,
  channels, patterns, the mixer, the playlist, modulators, tempo and time signatures and settings.

### Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Licenses, banned crates and sources against deny.toml (cargo install cargo-deny):
cargo deny check
# Golden export test on its own (exports a stored reference project and checks its hash):
cargo test -p gt-export --test golden
# Plugin host tests with the in-tree test plugins (gt-test-plugin):
cargo test -p gt-plugin-host
# Try the test plugins in the app: build them and install as a .clap file
cargo build -p gt-test-plugin
cp target/debug/libgt_test_plugin.so ~/.clap/gt_test_plugin.clap   # Windows: gt_test_plugin.dll to %LOCALAPPDATA%\Programs\Common\CLAP\gt_test_plugin.clap
# macOS: cp target/debug/libgt_test_plugin.dylib ~/Library/Audio/Plug-Ins/CLAP/gt_test_plugin.clap
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

### Packaging and releases

Shipped builds use the `dist` profile (thin LTO, no line tables; about 29 MB):

```sh
cargo build --profile dist --locked -p gt-app

# Ubuntu .deb (cargo install cargo-deb) into dist/:
cargo deb -p gt-app --profile dist --no-build --no-strip --output dist/
# AppImage into dist/ (needs desktop-file-utils; downloads a pinned appimagetool once):
packaging/linux/build-appimage.sh
```

```sh
# macOS: universal GloomTunes Studio.app and a .dmg into dist/ (builds both architectures
# itself; ARCHS=aarch64 for a quicker Apple Silicon-only build)
packaging/macos/build-app.sh
```

```powershell
# Windows installer into dist\ (Inno Setup 6: choco install innosetup)
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=1.1.0 packaging\windows\gloomtunes.iss
```

**Making a release:** set the version in the workspace `Cargo.toml` (`[workspace.package]
version`), update `Cargo.lock` (`cargo build`), merge, then push a matching tag:

```sh
git tag v1.1.0 && git push origin v1.1.0
```

`.github/workflows/release.yml` builds the four packages, writes `SHA256SUMS`, attests every
file with Sigstore build provenance and opens a **draft** release with generated notes; check it
and press **Publish**. **Actions > Release > Run workflow** builds the same files as artifacts
without a release, and pull requests that change `packaging/` build them too. Optional
repository secrets add signatures:

| Secret | Adds |
|---|---|
| `WINDOWS_CERT_PFX` (base64 of a .pfx), `WINDOWS_CERT_PASSWORD` | Authenticode signatures on `gloomtunes.exe` and the installer |
| `MACOS_CERT_P12` (base64 of a .p12 with the "Developer ID Application" certificate and key), `MACOS_CERT_PASSWORD` | Developer ID signature (hardened runtime) on the macOS app and .dmg instead of an ad hoc one |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` (an app-specific password), with the two above | Notarization of the .dmg, so Gatekeeper opens it without asking |
| `GPG_PRIVATE_KEY` (armored), `GPG_PASSPHRASE` | `SHA256SUMS.asc` |

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
