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
> Run the two scripts once before the first `cargo build`. If you consume the
> published crates from another project, clone this repository once to get the
> scripts, then set `FFMPEG_DIR` to the generated `.cache/ffmpeg` prefix.

The engine links FFmpeg statically. Run the dependency scripts once (see
[`docs/build.md`](docs/build.md)); they install into `.cache/ffmpeg` and the
committed `.cargo/config.toml` points `FFMPEG_DIR` there:

```sh
tooling/install-deps.sh
tooling/ffmpeg/build-ffmpeg.sh
cargo build -p quercus-engine
cargo test --workspace
cargo doc -p quercus-engine-rs --no-deps
```

The SDK links the engine dylib at build time. Packaged applications must ship
`libquercus_engine` next to the executable (or on the loader path) and set an
rpath; see `crates/quercus-engine-rs/README.md`.

## Publishing

Publish in dependency order (all internal crates are published, but they are
implementation details and are not intended for direct use):

```sh
tooling/publish_workspace.sh --dry-run
tooling/publish_workspace.sh
```

Order: core → ffmpeg-link → undo → codec → node → otio → render → audio →
timeline → storage → task → ofx-plugin → engine → engine-rs.

## Contributing

Contributions are accepted under the [Contributor License Agreement](CLA.md)
(inbound = outbound, GPL-3.0-or-later).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
