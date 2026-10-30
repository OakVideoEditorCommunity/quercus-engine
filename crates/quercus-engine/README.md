# quercus-engine

The Quercus video engine: the domain implementation (project/timeline graph,
codecs, audio, render workers, storage, OpenFX host, tasks) plus a stable
`extern "C"` facade.

Frontends should normally depend on [`quercus-engine-rs`](../quercus-engine-rs),
which wraps this ABI in safe Rust. C and C++ frontends can link the cdylib
directly.

## Layout

- `src/editor*`, `src/node.rs`, `src/timeline.rs`, `src/effect.rs` — project,
  timeline, node graph and parameter ABI.
- `src/preview_*`, `src/render.rs`, `src/task.rs` — preview frames, audio
  blocks, the render manager and unified tasks (export, OTIO, thumbnails,
  waveforms).
- `src/storage.rs`, `src/state.rs`, `src/audio.rs`, `src/codec*` — project
  library, configuration, audio devices and codec helpers.
- `src/ofx/` — plugin host, interact and progress/cancel plumbing.

## docs.rs

The engine is a C ABI cdylib. docs.rs builds it with the dependency-free
`docs-only` feature (see `Cargo.toml`), so the page documents the crate's
role and links to the C header and the SDK; the full API lives in
`include/quercus_engine.h` and the `quercus-engine-rs` docs.

## Build prerequisite

The engine is pinned to the project FFmpeg 8.1 static build. Do not use a
system/Homebrew/distro FFmpeg and do not leave `FFMPEG_DIR` pointing at one:
run `tooling/install-deps.sh` and `tooling/ffmpeg/build-ffmpeg.sh` from the
repository, then build with the generated `.cache/ffmpeg` prefix. Otherwise
the link fails or the dylib loads the wrong FFmpeg.

## C API

The public C surface is `include/quercus_engine.h`, generated from the Rust SDK's
extern declarations so the two languages cannot drift:

```bash
python3 tooling/gen_c_header.py --write   # regenerate
python3 tooling/gen_c_header.py --check   # CI guard
```

Notes:

- Opaque objects are `void*` handles. `*_add_ref` / `*_release` pairs are
  exported for every handle; `NULL` is always accepted and is a no-op.
- `QuercusRational` is `{int64_t num; int64_t den;}` (seconds, reduced);
  `QuercusTimeRange` is a pair of them. `0/0` is the null sentinel.
- String outputs use the snprintf convention: pass `buf`/`len`, get the
  would-be length (negative on error). Side-effecting calls never require a
  two-step "size then execute" pattern.
- Errors are reported as return codes plus an error buffer
  (`quercus_editor_edit`, `quercus_task_error`, `quercus_engine_last_error`); the engine
  never unwinds across the boundary.
- ABI changes are append-only within a major version; enum discriminants are
  part of the ABI and covered by the generated header.

## Linking

```bash
cargo build -p quercus-engine --release   # target/release/libquercus_engine.dylib|so|dll
cc app.c -I crates/quercus-engine/include -L target/release -lquercus_engine
```

On Windows the import library is `quercus_engine.dll.lib`; the SDK's `#[link]`
attributes show the exact names per platform. The dylib must be on the
loader search path at runtime.

## License

GPL-3.0-or-later.
