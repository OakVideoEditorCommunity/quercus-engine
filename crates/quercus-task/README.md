> **Internal crate — do not depend on it directly.**
>
> This crate is an implementation detail of `quercus-engine` and is
> published only to satisfy Cargo's dependency resolution. Its API is
> **not stable** and may change in any release without notice. Use
> `quercus-engine-rs` (or the C ABI via `quercus-engine`) instead.

# quercustask — Quercus task execution module (Rust)

Reimplements the C++ task module behind its frozen C ABI
(`include/task/*.h`). Every public type, trait, function and C ABI export is
implemented; the C++ code in `src/task/src` remains the parity source of
truth (`// CPP-PARITY:` markers point at the exact C++ file).

## Scope

This crate replaces `src/task/src` (the C++ `olive::Task` family: the `Task`
base class, the `TaskManager` singleton, the codec submitter bridge, and the
concrete task classes `ConformTask`, `CustomCacheTask`, `ExportTask`,
`PreCacheTask`, `ProxyTask`, `RenderTask`, and the project
load/save/import/OTIO tasks). The behavior it reproduces is defined by:

- the frozen C ABI headers `include/task/*.h` (authoritative),
- the C++ implementation in `src/task/src` (parity source of truth),
- the C++ unit tests in `src/task/tests/task_test.cpp` (golden source).

## Architectural decisions

1. **C++ inheritance → one module per class + trait objects.**
   `olive::Task` is an abstract base with a virtual `run()`. In Rust each
   concrete task is a `struct` and the shared behavior lives on a common
   `Task` base that exposes a `run: Box<dyn FnMut() -> Result<()>>`-style
   field (or an equivalent `TaskBehavior` trait object). `RenderTask` and its
   subclasses (`ExportTask`, `PreCacheTask`) use the same trait-object
   approach: the abstract base's `download_frame` / `frame_downloaded` /
   `audio_downloaded` / `encode_subtitle` virtuals become a single trait the
   subclasses implement. Rationale: Rust has no virtual inheritance, and a
   trait object is the minimal faithful mapping; it keeps each concrete task
   independent so the crate stays modular.

2. **Cancellation goes through the quercusrender C ABI.**
   The C++ `Task` holds an `olive::CancelAtom` (from `render/cancelatom.h`).
   Until quercusrender is rewritten, the Rust crate reaches it through
   `bridge::render` (`quercusrender_cancelatom_*`), with a borrowed `QuercusCancelAtom`
   stored on the base task. No quercusrender type is reimplemented here.

3. **Event listeners are the one async return channel.**
   The C++ `Task` exposes an event listener (`k_event_started` /
   `k_event_progress` / `k_event_finished`) delivered on the task's own
   thread. Qt signals are gone; the Rust equivalent is a callback
   (`std::function`-shaped closure) invoked under a `Mutex`, matching the
   `quercustask_task_subscribe` C ABI contract (event ids `STARTED`/`PROGRESS`/
   `FINISHED`, progress in 0..1). This is the single deliberate async
   exception to the otherwise fully synchronous model.

4. **Render-driven tasks reuse quercusrender tickets.**
   `RenderTask::render()` drives quercusrender tickets
   (`bridge::render`, `quercusrender_ticket_render_frame` /
   `quercusrender_ticket_render_audio`). The `ForceParams` struct maps to the
   `quercusrender_video_ticket_params` force fields. The ticket's
   `quercusrender_ticket_finished_fn` callback (fired on the render thread) is
   the only other async return channel.

5. **Project tasks borrow quercusnode handles, take ownership only on take\*.**
   `ProjectSaveTask` borrows its `QuercusNodeProject`; `ProjectImportTask` borrows
   the folder/project and produces an `QuercusUndoCommand`
   (`quercustask_import_take_command`) and a list of imported `QuercusNodeFootage`.
   `ProjectLoadTask` / `LoadOTIOTask` transfer ownership on
   `take_project()`. This mirrors the C ABI's borrow/take split exactly.

6. **OTIO load/save parses with the pure-Rust `quercusotio` binding.**
   `SaveOTIOTask` / `LoadOTIOTask` parse and serialize OpenTimelineIO JSON
   (`.otio`) and FCPXML (`.fcpxml`) through the `quercusotio` crate
   (`crates/quercusotio`, a self-contained serde model of exactly the
   object graph the C++ loadotio/saveotio tasks use) instead of the C++
   OTIO library. The format is inferred from the filename extension
   (case-insensitive; `src/project/format.rs`), so the frozen C ABI
   (`include/task/project.h`) needs no format parameter. Format handling
   ends at the `quercusotio` parse/serialize call — the track/clip/footage
   building (load) and the `serialize_*` helpers (save) are shared between
   both formats, mirroring the C++ tasks. No OTIO or FCPXML type crosses
   the quercustask C ABI — the tasks build the project through the
   quercusnode/quercustimeline C ABIs (load) and write `quercusotio::Timeline` /
   `SerializableCollection` documents (save), mirroring the C++
   `serialize_*` helpers. `ExportTask`/`PreCacheTask` use
   `quercuscore_rs::{Rational, TimeRange}` for their frame/audio timing (the
   reason this crate depends on `quercuscore-rs`).

## Layout

```
src/
  lib.rs            crate doc + module declarations
  error.rs          QUERCUSTASK_* codes + Error enum (module number 08)
  handle.rs         owned-handle box + get/get_mut views (facade task boxes)
  task.rs           Task base class + TaskEvent / EventListener
  manager.rs        TaskManager singleton
  codecbridge.rs    codec task submitter registration
  conform.rs        ConformTask + derive_filenames
  proxy.rs          ProxyTask + build_arguments / parse_progress
  customcache.rs    CustomCacheTask
  render.rs         RenderTask base + ForceParams
  export.rs         ExportTask
  precache.rs       PreCacheTask
  project.rs        project module (declares submodules)
  project/load.rs   ProjectLoadBaseTask + ProjectLoadTask
  project/save.rs   ProjectSaveTask
  project/import.rs ProjectImportTask
  project/format.rs interchange-format dispatch (.otio / .fcpxml)
  project/loadotio.rs LoadOTIOTask
  project/saveotio.rs SaveOTIOTask
  bridge/           extern "C" imports of other modules' C ABIs
    mod.rs, codec.rs, common.rs, node.rs, render.rs, timeline.rs, undo.rs
  ffi.rs            C ABI export layer (one pub mod per include/task header)
tests/              contract + parity tests
```

## Hard rules

- Every C ABI export checks its handle before dereferencing and maps panics
  through `handle::guard*` / explicit `catch_unwind`; panics never cross FFI.
- Handles are opaque refcounted boxes; `ctx` never points at Rust data
  directly except through `RefBox`.
- All C++ coupling comments carry a `// CPP-PARITY: <file>` marker so a C++
  change that breaks parity is greppable.
- Free functions are `no-op` on null handles; `free(NULL)` is always safe.
- Error codes mirror `include/task/error.h` verbatim (module 08, plus
  `QUERCUSTASK_E_CANCELLED = -80006`).

## Test strategy

`cargo test` runs the full suite against **link-time stubs** of the
other-module C ABIs (`tests/common/mod.rs`, pulled into each integration
test binary via `#[path]`). The quercustask library declares its cross-module
imports as real `extern "C"` symbols (production builds link them against
the quercus dylibs); the test binaries provide `#[no_mangle]` stub definitions
so plain `cargo test` links and runs without any quercus dylib. The stubs are
faithful-in-spirit: serializer round-trips write/validate a marker file,
footage validity checks real file existence, the codec submitter routes
conform/proxy requests into the task module, and a fake `ffmpeg` executable
drives the proxy run end to end. The same scenarios against the real dylibs
are covered by the C++ gtest suite (`src/task/tests/task_test.cpp`).

Since quercusrender's ticket C ABI is live, the render loop is also verified
against the **real** quercusrender arena (no stubs): `tests/
render_real_integration_test.rs` links the quercusrender crate into the test
binary and drives `RenderTask::render` through `bridge::render`'s
`extern "C"` declarations, getting real frames back in timestamp order via
the CPU path (no GPU). Both rlibs expose `#[no_mangle]` exports that cannot
coexist with the stub-based test binaries in one link, so the test is gated
behind the `real-quercusrender` feature and built alone:

```text
cargo test --features real-quercusrender --test render_real_integration_test
```

The render stubs in `tests/common/mod.rs` are compiled out under the same
feature (`#[cfg(not(feature = "real-quercusrender"))]`), so plain `cargo test`
is unaffected.

OTIO load/save is implemented end to end: `quercusotio` (path dep,
`crates/quercusotio`) parses the document inside `LoadOTIOTask::run` and
serializes the project in `SaveOTIOTask::run`; the tasks keep driving the
quercusnode/quercustimeline C ABIs exactly like the C++ implementations. `tests/
otio_test.rs` builds synthetic documents with the `quercusotio` model, imports
them, and re-parses exports for a round-trip check (README decision #6).
The tasks are format-aware (`.otio` → OpenTimelineIO JSON, `.fcpxml` →
FCPXML, dispatched from the extension in `src/project/format.rs`): the
FCPXML tests build documents with `quercusotio`'s fcpxml writer, cover the
extension dispatch matrix (`.otio`/`.fcpxml`/`.OTIO`/`.FCPXML`/unknown)
and the FCPXML error paths (corrupt XML, unsupported version), and run a
save → load cycle through both tasks.

## Dependency policy

Prefer mature third-party crates (MIT/Apache-2.0/BSD, GPL-compatible)
over hand-rolling; register each addition (name + reason) here. Large
existing C++ libraries (OTIO, OCIO, OIIO, FFmpeg) are NEVER rewritten
— they are consumed through their C ABI / bridge layers.

Current dependency inventory:
- `quercuscore-rs` (path dep, `Rational`/`TimeRange` for frame/audio timing).
- `quercusotio` (path dep, `crates/quercusotio` — pure-Rust serde model of the
  OpenTimelineIO JSON format used by `LoadOTIOTask`/`SaveOTIOTask`; brings in
  `serde`/`serde_json` transitively).
- `quercusrender` (path dep, **optional**, enabled only by the
  `real-quercusrender` feature): links the real quercusrender crate into the
  `render_real_integration_test` binary. The library never references it —
  all render access stays on the `bridge::render` C ABI.
