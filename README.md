# Quercus Nonlinear Video Editing Engine

The Quercus engine is a headless nonlinear video editing engine: project and
timeline model, codecs, audio, effects/OpenFX, rendering, storage, export, and
the render-worker runtime. It exposes a stable C ABI (`libquercus_engine`) and
the safe Rust SDK [`quercus-engine-rs`](crates/quercus-engine-rs).

This repository is the publishable engine workspace. The Quercus editor
application and its CLI/worker live in a separate application repository and
consume this engine through the SDK.

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
