# Quercus Nonlinear Video Editing Engine

The Quercus engine is a headless nonlinear video editing engine: project and
timeline model, codecs, audio, effects/OpenFX, rendering, storage, export, and
the render-worker runtime. It exposes a stable C ABI (`libquercus_engine`) and
the safe Rust SDK [`quercus-engine-rs`](crates/quercus-engine-rs).

This repository is the publishable engine workspace. The Quercus editor
application and its CLI/worker live in a separate application repository and
consume this engine through the SDK.

## Relationship to Oak Video Editor

Quercus is the headless engine and Rust SDK extracted from
[Oak Video Editor](https://github.com/OakVideoEditorCommunity/oak). Oak Video
Editor is the reference application and the primary consumer: the editor, its
CLI and its render worker depend on the published `quercus-engine-rs` crate
instead of carrying engine sources.

The split keeps the engine reusable by other frontends (C/C++ through the C
ABI, Rust through the SDK) while Oak Video Editor remains the place where the
end-user application, UI and packaging live.

## Workspace

All crates except `quercus-engine` and `quercus-engine-rs` are **internal
implementation details**: they are published only so Cargo can resolve the
engine's dependency graph. Do not depend on them directly; their APIs are not
stable. Use the SDK (or the C ABI) instead.

| crate | role |
|---|---|
| `quercus-core` | base types, color, config, platform helpers |
| `quercus-ffmpeg-bootstrap` | pinned static FFmpeg download/build bootstrap |
| `quercus-ffmpeg-sys` / `quercus-ffmpeg-next` | FFmpeg FFI and safe wrapper |
| `quercus-ffmpeg-link` | static FFmpeg link helper (`links` crate) |
| `quercus-codec` | decode/probe/encode, hardware decode |
| `quercus-audio` | audio backend, processing, waveform |
| `quercus-undo` | undo/redo command framework |
| `quercus-node` | project graph, clips, tracks, sequences, serialization |
| `quercus-render` | render manager, workers, IPC, OpenGL/CPU rendering |
| `quercus-ofx-plugin` | OpenFX host and plugin runtime |
| `quercus-otio` | OpenTimelineIO import/export |
| `quercus-storage` | project storage backends |
| `quercus-task` | export/preview/conform/proxy tasks |
| `quercus-timeline` | semantic timeline edit commands |
| `quercus-engine` | C ABI facade (cdylib) + domain dispatch |
| `quercus-engine-rs` | safe Rust SDK over the C ABI |

## Build

> **FFmpeg is mandatory and must be the project build.**
>
> Do **not** point `FFMPEG_DIR` at a system, Homebrew or distro FFmpeg, and do
> not rely on `pkg-config` picking one up. The engine is pinned to the project
> FFmpeg 8.1 build (static, all codec dependencies in one prefix); any other
> FFmpeg produces link errors or a binary that loads the wrong libraries.
>
> A normal `cargo add quercus-engine-rs` builds FFmpeg automatically on
> Linux/macOS and downloads the SHA256-pinned static BtbN development archive
> on Windows. No FFmpeg shared libraries need to be copied beside the app.

The engine links FFmpeg statically. An existing `FFMPEG_DIR` is honored.
Otherwise `quercus-ffmpeg-bootstrap` builds it on Linux/macOS, while Windows
downloads the pinned BtbN 8.1 static development archive and verifies its
SHA256. Set `QUERCUS_FFMPEG_OFFLINE=1` to forbid that fallback.

For a manual build, run the dependency scripts once (see
[`docs/build.md`](docs/build.md)); they install into `.cache/ffmpeg` and the
committed `.cargo/config.toml` points `FFMPEG_DIR` there:

```sh
tooling/install-deps.sh
tooling/ffmpeg/build-ffmpeg.sh
cargo build -p quercus-engine
cargo test --workspace
cargo doc -p quercus-engine-rs --no-deps
```

The SDK uses the engine rlib by default. Enable `engine-cdylib` instead of
`engine-rlib` when a C-compatible dynamic library is specifically required;
only that mode needs `libquercus_engine` on the loader path. The native CD
publishes the dynamic library, static library, C header and Windows import
library; rlib remains a Cargo implementation detail and is not an artifact.

## Publishing

Publish in dependency order (all internal crates are published, but they are
implementation details and are not intended for direct use):

```sh
tooling/publish_workspace.sh --dry-run
tooling/publish_workspace.sh
```

Order: ffmpeg-bootstrap → core → ffmpeg-link → ffmpeg-sys → ffmpeg-next →
undo → codec → node → otio → render → audio → timeline → storage → task →
ofx-plugin → engine → engine-rs.

Version 0.7.0 must pass the Linux, macOS and Windows native CD matrix before
the publish job runs. The publish job uses the same static native builds.

## Contributing

Contributions are accepted under the [Contributor License Agreement](CLA.md)
(inbound = outbound, GPL-3.0-or-later).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
