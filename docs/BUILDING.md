# Building, testing and releasing


You need a stable Rust toolchain from [rustup.rs](https://rustup.rs) (the repository pins
`stable` with rustfmt and clippy in `rust-toolchain.toml`).

## Ubuntu 22.04+

```sh
sudo apt install build-essential pkg-config libasound2-dev libudev-dev \
                 libxkbcommon-dev libwayland-dev libxkbcommon-x11-0
cargo run --release -p gt-app
```

Audio goes through ALSA, which reaches PipeWire or PulseAudio through their ALSA plugins on a
standard desktop. Optional backends: `--features jack` (needs `libjack-jackd2-dev`) or
`--features pipewire` (needs `libpipewire-0.3-dev`), then select with `GT_AUDIO_HOST=jack`.

## Windows 10/11

Install Rust with the MSVC toolchain (rustup offers the Visual Studio Build Tools), then:

```powershell
cargo run --release -p gt-app
```

Audio goes through WASAPI (shared mode). ASIO is an optional feature (`--features asio`) that
needs the Steinberg ASIO SDK, which is not shipped with this project.

## macOS 11+

Install the Xcode command line tools (`xcode-select --install`), then:

```sh
cargo run --release -p gt-app
```

Audio goes through CoreAudio, MIDI through CoreMIDI and drawing through Metal; nothing else is
needed. Plugin editors open in their own window.

## Checks

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

## Packaging and releases

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
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=1.2.0 packaging\windows\gloomtunes.iss
```

**Making a release:** set the version in the workspace `Cargo.toml` (`[workspace.package]
version`), update `Cargo.lock` (`cargo build`), merge, then push a matching tag:

```sh
git tag v1.2.0 && git push origin v1.2.0
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

## Environment variables

| Variable | Effect |
|---|---|
| `GT_RENDERER=glow` | Use OpenGL instead of wgpu (wgpu falls back to OpenGL automatically if it cannot start). |
| `GT_AUDIO_HOST=<name>` | Pick a compiled-in audio host, e.g. `jack`, `pipewire`, `asio`. |
| `RUST_LOG=debug` | More log output on the console. |
| `CLAP_PATH` | Extra folders searched for CLAP plugins (separated like `PATH`). |

[Back to the README](../README.md)
