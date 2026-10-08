# GloomTunes Studio

An original, open-source, pattern-based digital audio workstation written in Rust, for
Windows 10/11 and Ubuntu 22.04+ from a single codebase. Channel rack, piano roll, mixer and
playlist, with built-in instruments and effects, wrapped in a dark, moody "Gloom" interface.

> **Status:** design stage. No code yet. See [ARCHITECTURE.md](ARCHITECTURE.md) for the design
> and [ROADMAP.md](ROADMAP.md) for the plan and progress.

## Goals

- Pattern-based workflow: step sequencer, piano roll, mixer, playlist.
- Rock-solid real-time audio: no allocation, locks or I/O on the audio thread.
- Sample-accurate timing, and offline export that matches what you hear.
- Built-in instruments (sampler, Gloom Synth) and effects; CLAP plugin hosting later.
- All code, UI, sounds and presets original or CC0.

## Building

Build instructions arrive with the first code milestone (Step 1 in the roadmap). You will need a
stable Rust toolchain; on Ubuntu also `libasound2-dev libudev-dev libxkbcommon-dev libwayland-dev
pkg-config`.

## License

Copyright (C) 2026 Vasil Vasilev.

GloomTunes Studio is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either version 3 of
the License, or (at your option) any later version. See [LICENSE](LICENSE).
