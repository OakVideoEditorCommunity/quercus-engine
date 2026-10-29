# Build Guide

Quercus Nonlinear Video Editing Engine is a Rust workspace that produces
`libquercus_engine` (cdylib) and the `quercus-engine-rs` SDK. The editor
application, CLI and worker live in a separate repository and consume this
workspace through the SDK.

## Prerequisites

- **git**
- **Rust stable** via [rustup](https://rustup.rs/) (edition 2024, MSRV 1.85)
- **C/C++ toolchain, cmake, ninja, nasm, pkg-config** (the vendored
  OpenColorIO and the FFmpeg dependency builds)
- **System libraries** used by the engine: ALSA/PulseAudio/PipeWire/JACK,
  Vulkan/GL drivers and XKB on Linux
- **FFmpeg 8.1**, built by the project script (distro FFmpeg is too old for
  `ffmpeg-next` 9)

## One-time setup

```sh
tooling/install-deps.sh         # Homebrew / apt / dnf / pacman / MSYS2
tooling/ffmpeg/build-ffmpeg.sh  # builds every codec dependency + FFmpeg 8.1
```

`tooling/ffmpeg/build-ffmpeg.sh` installs into `.cache/ffmpeg`. The
committed `.cargo/config.toml` sets `FFMPEG_DIR` relative to the workspace
root, so no environment variable needs exporting. CI uses the same two
scripts (the Windows job uses a prebuilt FFmpeg archive instead).

## Build and test

```sh
cargo build -p quercus-engine
cargo build --workspace
cargo test --workspace          # Linux: headless (xvfb) for GPU tests
cargo doc -p quercus-engine-rs --no-deps
```

Outputs:

- `target/debug/libquercus_engine.dylib` / `.so` / `quercus_engine.dll`
- `target/debug/libquercus_engine_rs.rlib`

## C ABI header

```sh
python3 tooling/gen_c_header.py --write   # regenerate
python3 tooling/gen_c_header.py --check   # CI guard
```

The public C interface is `crates/quercus-engine/include/quercus_engine.h`.

## Linking the dylib into an application

The SDK links the engine at build time. Packaged applications must ship
`libquercus_engine` next to the executable (or in `lib/`) and set an rpath:

- macOS: `-Wl,-rpath,@loader_path` (or `@loader_path/../lib`)
- Linux: `-Wl,-rpath,$ORIGIN`
- Windows: copy `quercus_engine.dll` next to the `.exe`

## Headless Linux

GPU/window tests need an X server and a software Vulkan driver:

```sh
xvfb-run -a -s "-screen 0 1920x1080x24" cargo test --workspace
```

## Windows

Use the MSYS2 UCRT64 shell for `tooling/install-deps.sh`, or point
`FFMPEG_DIR` at a prebuilt FFmpeg 8.x shared build (BtbN's GPL archive is
what CI uses).

## macOS

`tooling/install-deps.sh` uses Homebrew. The bundled OpenColorIO links IOKit,
ColorSync and CoreGraphics; the SDK/engine build scripts forward them for
test binaries.

## License

GPL-3.0-or-later. See [../LICENSE](../LICENSE).
