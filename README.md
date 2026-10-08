# GloomTunes Studio

An original, open-source, pattern-based digital audio workstation written in Rust, for
Windows 10/11 and Ubuntu 22.04+ from a single codebase. Channel rack, piano roll, mixer and
playlist, with built-in instruments and effects, wrapped in a dark, moody "Gloom" interface.

> **Status:** early development. Step 6 of the [roadmap](ROADMAP.md): a mixer with 64 inserts,
> 4 send buses and eight built-in effects (EQ, compressor, delay, reverb, chorus, distortion,
> limiter, stereo width), on top of Gloom Synth, a subtractive polysynth with presets, a piano roll with undo/redo, a channel rack with
> a step sequencer, patterns, a sampler with ADSR, a sample browser, four built-in drums and a
> sample-accurate transport. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design.

## Goals

- Pattern-based workflow: step sequencer, piano roll, mixer, playlist.
- Rock-solid real-time audio: no allocation, locks or I/O on the audio thread.
- Sample-accurate timing, and offline export that matches what you hear.
- Built-in instruments (sampler, Gloom Synth) and effects; CLAP plugin hosting later.
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

- **Space** plays and pauses. The demo pattern (kick, snare, hat, clap and a Gloom Synth bass)
  starts looping.
- Click a step to toggle it; scroll over a lit step to change its velocity.
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
- **Undo** and **Redo** (top right, Ctrl+Z and Ctrl+Shift+Z or Ctrl+Y) cover notes, steps,
  channels, patterns, the mixer and settings.

### Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Piano roll frame time with 10,000 notes (prints the median):
cargo test --release -p gt-ui -- --ignored --nocapture
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

## License

Copyright (C) 2026 Vasil Vasilev.

The four built-in drum sounds are generated by code in `crates/gt-dsp/src/drums.rs`; the audio they
produce is dedicated to the public domain under CC0 1.0.

GloomTunes Studio is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version 3 of
the License, or (at your option) any later version. See [LICENSE](LICENSE).
