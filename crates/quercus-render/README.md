# quercusrender Rust crate

> Status: **implemented (M7 render wave)**. Every `todo!()` from the
> declaration draft is implemented; the crate builds, tests green
> (`cargo test`), and the C ABI surface in `include/render/*.h` is
> exported from `src/ffi.rs`. Deferred items are documented inline and
> in the **Deferred** section below.

## Scope

Replaces the C++ quercusrender module (`src/render/src`): render manager,
ticket system + worker pool, textures and GPU backend dispatch, playback/
frame-hash caches, color processing (OCIO), the preview auto-cacher, and
the blit/display path.

Public contract: `include/render/*.h` (8 headers, ~165 functions) —
frozen, implemented verbatim by `src/ffi.rs`.

## Key architectural decisions (C++ → Rust mapping)

1. **The ProjectCopier inversion disappears.** C++ render deep-copied
   the node project with raw C++ calls (the biggest render→node
   coupling). In Rust this is impossible by construction: the copier
   calls `quercusnode_project_deep_copy` / `sync_copy` (designed in the
   quercusnode crate) through the C ABI. `copier.rs` here is a thin client.
2. **RenderProcessor's inheritance disappears.** C++
   `RenderProcessor : NodeTraverser` becomes `eval.rs` (the closed
   `JobSpec` executor set + the CPU-side hook implementations; the job
   payload enum itself lives in quercusnode's `jobs.rs`, graph traversal
   stays in quercusnode).
3. **Ticket/watchers.** C++ RenderTicket/RenderTicketWatcher (Qt
   signals) become a ticket arena with completion callbacks —
   exactly-once delivery (`ticket.rs`), `FnOnce` boxes fired on the
   worker thread.
4. **GPU backend = wgpu (v25).** The C++ tree's backend plugin split
   (libquercusgl2/libquercusvulkan behind `renderbackend_c.h`) exists because
   C++ had no portable GPU abstraction. Rust has `wgpu` (Metal/Vulkan/
   GL/DX12 in one safe API), so the Rust crate uses `wgpu` directly —
   no backend plugins, no `renderbackend_c.h`, no dlopen. `backend.rs`
   owns the wgpu instance/device/queue, the texture registry and the
   WGSL blit pipeline. **Headless status: verified** — texture
   create/upload/download and the plain-copy blit run without any
   surface or event loop on macOS Metal (the GPU tests exercise them
   and skip gracefully when no adapter is available).
5. **Threading.** The worker pool is scoped threads with a job
   channel; every shared structure is `Mutex`/`RwLock`. Process
   isolation (`ProcessPool`) is preserved as a documented stub: it
   needs the quercusengine_ipc worker binary, which is not wired this pass.
6. **OFX disappears from render.** pluginrenderer.cpp's functionality
   moves to the quercusplugin crate; this crate only sees plugin jobs as
   opaque C ABI calls.

## Dependencies (registered)

| Crate | Version | Reason |
|---|---|---|
| `quercuscore-rs` | path | Rational/TimeRange/PixelFormat value types (crate-internal) |
| `wgpu` | 25 | portable GPU backend — the direct replacement for the C++ GL/Vulkan backend plugins |
| `ocio-rs` | 0.2 | safe Rust bindings for OpenColorIO v2.5.2 (bundled real-OCIO build); the ColorProcessor implementation — OCIO is never rewritten |

## Layout

```
src/
  lib.rs        crate doc + module map
  error.rs      re-exports quercus_core::error (the QUERCUSRENDER_* codes stay as
                the public-code contract)
  handle.rs     refcounted-handle scaffolding (facade entry points only)
  cache.rs      PlaybackCache / FrameHashCache family + C++-parity disk state
  manager.rs    RenderManager singleton + lifecycle + disk cache
  ticket.rs     Ticket arena, params, exactly-once completion delivery
  worker.rs     JobDispatch seam + thread-free InlineDispatcher (audio
                fallback / test backend) + graph snapshot store
  scheduler.rs  M15 PreviewScheduler: interleaved shard claiming,
                priority lanes (seek/playback/background), crash reclaim,
                per-request slot-bytes capacity filtering (S3)
  ipc.rs        M15 render-worker IPC (moved from quercus-worker): NDJSON
                control protocol (v1 + v2 messages) + the POSIX
                shared-memory frame-slot transport both pipe ends link
  procpool.rs   M15 ProcessDispatcher: spawn/handshake quercus-worker
                processes, main-assigned slot batches, crash detection +
                restart, zero-copy ShmFrameRef / ShmAudioRef completions,
                grow-on-demand segment geometry (S3)
  autocacher.rs PreviewAutoCacher
  eval.rs       RenderHooks impl: the CPU evaluation seam
  shaderfx.rs   effect GLSL→WGSL translation (naga) + std140 uniform
                packing + the effect runner
  copier.rs     Render-side project copy client (bridge::node)
  cancelatom.rs the cancellation primitive
tests/          contract + golden tests (common/ has shared helpers)
```

The value/GPU types — `backend.rs` (wgpu device/queue/texture management
+ DisplayRenderer), `color.rs` (ColorProcessor over ocio-rs + default
config + LUT library), `texture.rs`, `frame.rs` and the `commonutil.rs`
config helpers — moved to `quercus-core` in the quercus-common/quercus-core merge;
this crate uses them as `quercus_core::*`.

## Hard rules

1. `CHandle` only appears at the facade boundary: the crate's internal
   calls pass Rust types directly; `handle::make_owned`/`get`/`get_mut`
   are the facade entry points the quercusengine stubs call.
2. No `unsafe` outside `handle.rs`, the evaluation seam (`eval.rs`), and
   the M15 process-isolation transport (`ipc.rs` / `procpool.rs`: POSIX
   shm + SPSC rings; every block carries its own SAFETY comment). GPU
   unsafe lives in `quercus-core`'s `backend.rs`.
3. F32 + ACEScg pipeline invariants are asserted in tests, not in
   comments (see tests/pipeline_test.rs).

## Deferred (documented; tests gated with `#[ignore]`)

- **quercuscodec frame payload I/O** — disk frame-cache read/write
  (`quercusrender_frame_cache_load/save`) and footage decode go through the
  `bridge::codec` C ABI (EXR/JPEG). The quercuscodec crate is a concurrent
  wave; until it lands these fail explainably and the success-path
  tests are `#[ignore = "needs quercuscodec final"]`.
- **quercusnode C ABI** — `quercusrender_project_copier_set_project` /
  `get_copy` success paths need `quercusnode_project_deep_copy`; the
  success-path copier tests are `#[ignore = "needs quercusnode C ABI"]`.
- **Color-managed GPU blit** — the OCIO→WGSL shader generation is not
  in this pass: `GpuContext::blit` handles the plain copy and returns
  `Error::Failed` for a processor; the CPU path applies the processor
  in float. `quercusrender_color_processor_create_transform` resolves the
  destination transform against the default config's reference role
  until the quercus_core color-transform bridge lands.
- **Worker process isolation** — landed in M15: `procpool.rs`
  (`ProcessDispatcher`) + `scheduler.rs` + `ipc.rs` drive real
  quercus-worker processes (spawn, handshake, batched renders into
  main-assigned shm slots, crash restart, zero-copy completions); the
  end-to-end and crash-isolation tests live in
  `crates/quercus-worker/tests/procpool_integration.rs`. M15 S2 made the
  process backend the `RenderManager` default, removed the in-process
  thread pool (`worker::WorkerPool`) and the frozen pre-M15
  `worker::ProcessPool` facade stub, and wired the app's onscreen path
  to read the shm slots zero-copy. M15 S3 added **audio over shm**
  (`render_audio_batch` → `TicketPayload::ShmAudio`, with the
  main-process inline `InlineDispatcher` as the fallback when the
  process dispatcher is unavailable), **per-ticket slot formats**
  (forced-F32 tickets get F32 slots; the export path reads them
  directly, eliminating the BGRA8→F32 round trip; segments grow on
  demand), and **adaptive pool tuning** (`default_slots_per_worker` /
  `default_batch_size` / `default_worker_count` policies plus the
  `bench_process` example that reports 1080p throughput and
  adjacent-frame completion deltas).
- **Audio rendering** — M15 S3 migrated audio tickets onto the process
  dispatcher (`render_audio_batch`; the worker mixes via
  `eval::render_audio_samples_into` into `SLOT_FORMAT_AUDIO_F32` slots;
  the main process reads `ShmAudioRef` and releases). The app's
  real-time pull (`pull_audio_tick`) renders chunks **ahead**
  asynchronously (prefetch depth 4 ≈ 66 ms) so it never blocks on a
  busy worker; `worker::InlineDispatcher::sync` remains the fallback
  (design §3.7).
- **Borrowed caches** — `quercusrender_cache_wrap_borrowed` boxes an
  opaque marker; queries on borrowed caches return `QUERCUSRENDER_E_INVALID`
  until the C++ interop layer lands.
- **`RenderManager::global()` returns `Option<Arc<…>>`** instead of the
  draft's `Option<&'static …>` — a resettable singleton cannot hand out
  stable references safely.

## Coverage

`COVERAGE.md` maps every C++ class of `src/render/src` to its Rust
home. `cargo tarpaulin` ≥ 80% excluding the deferred areas listed
above.
