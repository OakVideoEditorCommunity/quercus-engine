# Quercus Engine C API

The Quercus engine is a headless nonlinear video editing engine: project and
timeline graph, codecs, audio, effects and OpenFX, rendering, storage, export
and a render-worker runtime. This repository exposes it as a stable C ABI
dynamic library (`quercus_engine`) plus the safe Rust SDK
[`quercus-engine-rs`](../../crates/quercus-engine-rs).

This directory is the C/C++ host documentation:

- this page — orientation, conventions and a working quick start;
- [reference.md](reference.md) — generated per-function reference for the
  entire header;
- [build-windows.md](build-windows.md) — building and shipping the Windows
  x64 MSVC library;
- [`examples/c-api/quickstart.c`](../../examples/c-api/quickstart.c) — a
  complete, compilable C program using only the public header.

The public header is
[`crates/quercus-engine/include/quercus_engine.h`](../../crates/quercus-engine/include/quercus_engine.h):
**60 POD structs, 14 enums and 742 functions**, plus 3 type aliases and 10
callback typedefs. It is generated from the Rust SDK's `extern "C"`
declarations, so the two surfaces cannot drift. Platform-specific entry points
are wrapped in C preprocessor guards: a Windows build sees **741** functions
(the X11 display-ICC probe is Linux-only), while all 741 are exported by
`quercus_engine.dll`. The guide below matches workspace version 0.7.1.

## What you get

The engine crate (`crates/quercus-engine`) has crate types `rlib`, `cdylib`
and `staticlib`. A relocatable Windows x64 package should contain:

| File | Role |
|---|---|
| `bin/quercus_engine.dll` | Runtime dynamic library. Loaded by the process. |
| `lib/quercus_engine.dll.lib` | MSVC import library. Link this into the host executable. |
| `lib/quercus_engine.lib` | Static archive (`staticlib`). Not the import library. |
| `include/quercus_engine.h` | Public C/C++ header. Self-contained; only C standard headers. |
| `bin/*.dll` | FFmpeg runtime DLLs (BtbN `gpl-shared` build). |
| `bin/OpenColorIO_2_5.dll` | Only for packages built with dynamic OpenColorIO (`OCIO_RS_LINK=dynamic`); the CD build statically links OCIO. |

The current CD workflow stages the DLLs together with the link libraries in
`lib/` inside `quercus-engine-native-windows-x64.zip`. Either layout works as
long as the runtime DLLs are discoverable; see
[build-windows.md](build-windows.md) for the packaging and runtime search
rules.

The artifact is produced by the CD workflow on `windows-2022` with MSVC. The
page below describes how to reproduce it from a Windows x64 host; it does not
assume a prebuilt artifact is present in the source tree.

## Quick start

A C host only needs the header, the import library and the runtime DLLs.
The minimal lifecycle is:

1. create an editor session with `quercus_editor_new()`;
2. create a sequence with `quercus_editor_create_sequence()`, which also
   installs the default V1/V2/A1/A2 track layout and makes the sequence
   current;
3. take a project handle with `quercus_editor_project()` so the lower-level
   graph queries can be used;
4. import footage and place a clip through `quercus_editor_edit()`;
5. save with `quercus_editor_save()`;
6. release the project handle and the session.

```c
/* quickstart.c — create, import, place one clip, save, release. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "quercus_engine.h"

static int32_t run_edit(void *session, uint32_t op,
                        const QuercusEditParams *params,
                        QuercusEditResult *result)
{
    uint8_t error[512] = {0};
    int32_t code = quercus_editor_edit(session, op, params, result,
                                       error, sizeof error);

    if (code != 0) {
        fprintf(stderr, "edit %u failed (%d): %s\n", op, code,
                (const char *)error);
    }
    return code;
}

int main(void)
{
    const uint8_t media[] = "clip.mp4";
    const uint8_t out[] = "demo.ove";
    const uint64_t invalid = quercus_node_id_invalid();
    QuercusEditParams params = {0};
    QuercusEditResult result = {0};
    void *session = quercus_editor_new();
    void *project = NULL;
    uint64_t sequence;
    uint64_t footage;
    Rational fps;
    Rational duration;
    Rational frame_duration;

    if (session == NULL) {
        fprintf(stderr, "quercus_editor_new failed\n");
        return 1;
    }

    sequence = quercus_editor_create_sequence(
        session, (const uint8_t *)"Demo", 4);
    project = quercus_editor_project(session);
    if (sequence == invalid || project == NULL) {
        fprintf(stderr, "sequence/project creation failed\n");
        goto fail;
    }

    /* Import: semantic edits are undoable and report an error string. */
    params.path = media;
    params.path_len = sizeof media - 1;
    if (run_edit(session, QUERCUS_EDITOP_IMPORTFOOTAGE,
                 &params, &result) != 0) {
        goto fail;
    }
    footage = result.primary;
    if (footage == invalid) {
        goto fail;
    }

    /* Edit timestamp slots are integer frames in the sequence timebase. */
    fps = quercus_sequence_frame_rate(project, sequence);
    duration = quercus_footage_duration(project, footage);
    frame_duration = quercus_core_rational_new(fps.den, fps.num);

    memset(&params, 0, sizeof params);
    params.a = footage;
    params.kind = 0; /* 0 = video, 1 = audio, 2 = subtitle */
    params.track_index = 0;
    params.in_ts = 0;
    params.out_ts = quercus_core_rational_time_to_timestamp(
        frame_duration, duration);
    params.media_in_ts = 0;

    if (params.out_ts <= 0 ||
        run_edit(session, QUERCUS_EDITOP_PLACEFOOTAGECLIP,
                 &params, &result) != 0) {
        goto fail;
    }

    if (quercus_editor_save(session, out, sizeof out - 1, 0) != 0) {
        fprintf(stderr, "save failed\n");
        goto fail;
    }

    quercus_project_release(project);
    quercus_editor_release(session);
    return 0;

fail:
    quercus_project_release(project);
    quercus_editor_release(session);
    return 1;
}
```

Points worth noticing:

- The edit timestamps (`in_ts`, `out_ts`, `media_in_ts`) are **frame
  timestamps in the current sequence timebase**, not seconds. Calculate the
  frame duration as `Rational{den, num}` from the frame rate, then convert
  with `quercus_core_rational_time_to_timestamp()`.
- `params.a` carries the footage id for `PLACEFOOTAGECLIP`; the semantic
  dispatcher documents the slot mapping per opcode. Use
  `QUERCUS_EDITOP_PLACEFOOTAGECLIPSLINKED` (or the
  `placements`/`placements_len` fields) when the video and audio should be
  placed as linked clips in one operation.
- `quercus_footage_import()` is the lower-level, direct graph operation. It
  is convenient for probing, but it bypasses the undoable semantic command
  path. `QUERCUS_EDITOP_IMPORTFOOTAGE` (used above) is the undoable
  variant.
- Resolve every failure instead of assuming success: handle-returning calls
  yield `NULL`, id-returning calls yield `quercus_node_id_invalid()`, and
  `quercus_editor_edit()` returns `0`/`-1`/`-2` with a NUL-terminated error
  string when a buffer is supplied.
- The work area has no dedicated setter. Read it with
  `quercus_editor_workarea()`; write it with
  `quercus_editor_edit(session, QUERCUS_EDITOP_WORKAREASET, ...)`, setting
  `params.enabled` and `params.range` (a `TimeRange` in seconds).

The complete example also demonstrates save/reopen checks and string
readback: [`examples/c-api/quickstart.c`](../../examples/c-api/quickstart.c).
[`examples/c-api/export.c`](../../examples/c-api/export.c) shows the task
pipeline (export, progress, result retrieval).

## Core conventions

These conventions hold across the whole header. Each is enforced by the
engine implementation, not just by documentation style.

### Opaque handles and reference counting

Objects such as the editor session, project, decoder, encoder, color
processor, OTIO document and audio/video buffers are exposed as opaque
`void *` handles (and a few dedicated `struct` types with the same opaque
identity). Never dereference them in C.

- Most handle families export `quercus_<thing>_add_ref()` and
  `quercus_<thing>_release()`. `NULL` is accepted by both and is a no-op.
- `*_add_ref()` retains an additional reference; `*_release()` drops exactly
  one. Release every reference you own exactly once.
- Functions that return a handle transfer one reference to the caller.
  `quercus_editor_project()` is the important special case: it returns a
  **new** reference on the session's live project, which the caller must
  release with `quercus_project_release()`, even though the session also
  keeps the project alive.
- `quercus_editor_from_project_handle()` and
  `quercus_editor_adopt_storage_project_handle()` **consume** a reference:
  do not release the input handle afterwards, and do not use it again.
- Snapshot handles (`quercus_editor_graph_snapshot()`,
  `quercus_editor_params_snapshot()`) are immutable views with only
  `*_release()`. They stay valid until released; they do not track later
  edits. Take a new snapshot after mutating the graph.
- Audio buffers, video buffers, thumbnails and waveforms are refcounted and
  may outlive the task that produced them. Their `*_info()`/`*_copy_*()`
  accessors are read-only.

### Node ids are opaque `uint64_t` tokens

`uint64_t` node ids are not indices. They encode an arena slot plus a
generation counter, so a stale token is not silently treated as a live node
with the same slot.

- The null token is returned by `quercus_node_id_invalid()`
  (`u32::MAX`, i.e. `0xFFFFFFFF`).
- `quercus_node_id_is_valid()` validates the **encoding** only. A stale token
  whose node was removed can still pass. Use `quercus_node_kind()` or the
  traversal getters to test liveness.
- Do not derive ids by arithmetic. Store them, pass them, and query the graph
  to navigate (root, folder children, sequence track lists, tracks, blocks).

### Rational and time ranges

`Rational` is `{ int64_t num; int64_t den; }` (16 bytes). Values are
normalized: reduced, with a non-negative denominator, and expressed in
**seconds** when used as a time.

- `0/0` is the null/invalid sentinel. It is produced by invalid parsing,
  division by zero and sentinel arithmetic.
- `quercus_core_rational_is_null()` tests the numerator;
  `quercus_core_rational_is_nan()` tests the denominator. `to_f64` maps the
  sentinel to `NaN`, so check before formatting or comparing.
- Use the constructors and operators rather than hand-reducing fields:
  `quercus_core_rational_new()`, `quercus_core_rational_add()`,
  `quercus_core_rational_from_f64()`, `quercus_core_rational_to_string()`.
- `TimeRange` is `{ Rational in_; Rational out; }` (32 bytes), a normalized
  half-open interval `[in_, out)`. It is used for clip ranges, work areas,
  markers and export ranges. `quercus_core_timerange_new()` normalizes the
  endpoints; `quercus_core_timerange_length()` subtracts them.
- OpenTimelineIO calls use `QuercusOtioTimeRange`, which is two `RationalTime`
  values (`start_time`, `duration`), not the core `TimeRange`. Keep the two
  types apart when calling `quercus_otio_*`.
- Frame timestamps are separate: they are plain `int64_t` values in the
  sequence's frame timebase. Convert with
  `quercus_core_rational_time_to_timestamp()` and
  `quercus_core_rational_timestamp_to_time()`.

### Strings and arrays

Two distinct conventions exist, and mixing them up is the most common host
bug.

**Input strings** are UTF-8 pointer + explicit byte length, not
NUL-terminated C strings:

```c
const uint8_t *path = (const uint8_t *)"demo.ove";
quercus_editor_save(session, path, 8, 0);
```

Exactly `len` bytes are read. The buffer may contain an embedded NUL (it is
just another byte), and it need not be NUL-terminated. A `NULL` pointer or
invalid UTF-8 is rejected with a `false`/`NULL`/`-1`/invalid-id result,
depending on the function. The engine never takes ownership of input
buffers; they only need to stay alive for the duration of the call.

**Output strings** use the `snprintf` convention:

- pass `buf` and `buf_len`; at most `buf_len - 1` bytes plus a NUL are
  written;
- the return value is the **full required length in bytes excluding the NUL**,
  even when the output was truncated;
- a negative return means an error (commonly `-1` for a null/missing handle);
- passing `NULL`/`0` queries the required length without writing.

The engine writes a NUL terminator whenever `buf != NULL && buf_len > 0`.
A returned length greater than or equal to `buf_len` means truncation. A
single call with a sufficiently large buffer is enough; side-effecting calls
never require a two-step "size then execute" dance.

**Arrays** are pointer + element/byte count, with the same borrowed-for-the-
call lifetime. Output arrays are caller-allocated with an explicit capacity;
the function reports how many elements were written (or the required count
for query-style calls).

Some fixed-layout structs embed fixed-size byte arrays (plugin descriptors,
subtitle styles, proxy presets). Those are raw ABI fields, not C strings;
respect the declared size and do not assume NUL termination.

### Two encoding parameter structs

The header has two similarly named parameter structures; they are not
interchangeable:

- `QuercusCodecEncodingParams` is used by `quercus_codec_encoder_create()`
  and the codec encoder API. It includes a `filename[1024]` output path and
  uses integer flags/discriminants.
- `EncodingParams` is used by task/export submission, for example
  `quercus_task_export()`. It has no output filename field (the task takes
  the filename separately) and uses `bool` for `video_enabled` and
  `audio_enabled`.

Initialize the whole struct (for example with `memset` to zero) before
filling the fields you need, and pass the type the function signature
declares.

### Errors, callbacks and panics

There is no exception mechanism and no global error object. Failure is
reported through the return value, with optional detail channels:

- `bool` setters/queries return `false`.
- Handle constructors return `NULL`; id constructors and lookups return
  `quercus_node_id_invalid()`.
- Length/getter functions return `-1` or another negative value.
- `quercus_editor_edit()` returns `0` on success, `-1` for an invalid request
  or domain error, and `-2` when the fixed result output cannot be written.
  Pass `error`/`error_len` to receive a bounded,
  NUL-terminated UTF-8 message; the buffer is left untouched on success.
- `quercus_task_error(id, buf, len)` reads a task's error text
  (snprintf-style, `-1` for an unknown task id).
- `quercus_editor_storage_last_error(session, buf, len)` reads the write-
  through storage diagnostic for a bound session.
- `quercus_task_state()` returns a number: `0` running, `1`
  finished/succeeded, `2` failed, `3` cancelled, and `-1` for an unknown task
  id. These states are not exported as named enum constants in the header;
  compare the numeric values. `quercus_task_wait()` returns `false` for
  failure, cancellation or an unknown id.

There is no engine-wide error string accessor. Error detail is per domain:
`quercus_editor_edit()` accepts a result code and an error buffer,
`quercus_editor_storage_last_error()` reports write-through storage,
`quercus_task_error()` reports one task, and
`quercus_audio_manager_push_to_output()` and
`quercus_editor_proxy_generate()` also accept caller-owned error buffers. For
every other call, the return value is the complete diagnostic. To capture
engine logs, install a sink with `quercus_core_log_set_sink()`.

The ABI is `extern "C"`: Rust unwinding does not cross the boundary. A panic
that escapes an engine function aborts the process instead of propagating
into the C caller, so treat crashes as engine bugs rather than recoverable
errors. The engine's exported entry points are written to be null-safe and
return documented error values instead of panicking.

### Threading and callback lifetime

The engine serializes access to a session's project behind internal mutexes,
but individual ABI entry points document that the same session must not be
used concurrently for the duration of a call. Practical rules:

- Serialize calls per session/project from the host side (one thread at a
  time). Different sessions may be used from different threads.
- Do not hold a session lock or call back into the same session from a
  callback.
- Process-wide facilities are singletons: the audio manager, the global undo
  stack, the log sink, the OpenFX host and the storage registry. Initialize
  once and shut down once.
- `quercus_editor_edit_subscribe()` callbacks run on whichever thread
  executes the edit. The registration takes ownership of `user_data` and
  calls the optional `drop_callback` exactly once when the registration is
  destroyed (after any in-flight notification). Do not free `user_data`
  yourself.
- `quercus_undo_add_observer()` adds a process-wide observer with the same
  "engine owns the registration, host owns the data lifetime" contract.
- `quercus_ofx_set_progress_handler()` progress callbacks must be
  thread-safe and must not block; return `false` from the progress callback
  to request cancellation where the API supports it.

### ABI and versioning

The ABI is append-only within a major version:

- Existing struct layouts, callback signatures and enum discriminants never
  change. New enum values are appended; the generated header is the canonical
  list.
- Semantic edit opcodes (`QUERCUS_EDITOP_*`) are contiguous and
  append-only. Never renumber them; new operations are added after
  `QUERCUS_EDITOP_CREATENODE` (74 at the time of writing).
- New functions are added; existing function signatures are not rewritten.
  Removed functionality stays in the header and reports a domain error.
- Extensibility structs carry `struct_size` + `abi_version` and are validated
  on registration (`QuercusConfigSourceVTable`, `QuercusMediaIoVTable`,
  `QuercusStorageTransportVTable`); `StorageProjectHandle` carries an
  `abi_version` with a fixed layout. Fill these fields with the values from
  the header.
- Persisted-project compatibility is separate from the binary ABI:
  `quercus_editor_open()` and `quercus_project_load_from_bytes()` report a
  `version_info` code (`0` success, positive too-old/too-new/unknown-version
  codes, negative storage errors).

There is no runtime "ABI version" query function. Ship the header and the
library from the same release, and rebuild the host when either changes.
Enum values and `#define` opcodes are compile-time constants; do not
reconstruct them from another version.

### Dynamic vs static linking

The supported C host path is the dynamic library: link the import library on
Windows (`quercus_engine.dll.lib`) or the shared object on other platforms,
and ship the runtime library. The `staticlib` archive is produced for
embedding scenarios, but it carries no transitive link metadata for C
toolchains. Linking it into an MSVC executable requires supplying Rust's
platform dependencies yourself; prefer the dynamic library unless you have a
specific embedding requirement. The Rust SDK's `#[link]` attributes show the
exact per-platform library names.

On macOS, `-L` alone is not enough at run time: the dylib's install name must
be resolvable, so pass an rpath that points at the directory containing
`libquercus_engine.dylib`:

```sh
cc app.c -I crates/quercus-engine/include \
   -L target/release -lquercus_engine \
   -Wl,-rpath,"$PWD/target/release"
```

For a relocatable bundle, use `-Wl,-rpath,@loader_path` (or
`@loader_path/../lib`) and keep the dylib next to the executable. Linux hosts
should do the equivalent with `-Wl,-rpath,'$ORIGIN'`.

### Worker helper process

Task-based work — `quercus_task_export()`, `quercus_task_video_render()` and
the other render tasks, plus the process-isolated render backend — needs the
`quercus-worker` helper executable. The engine resolves it from an explicit
host configuration (where the API exposes one), then the
`QUERCUS_WORKER_BIN` environment variable, then a sibling binary named
`quercus-worker` next to the host executable. It does not search `PATH` by
default. Ship the worker in the same directory as the host executable, or set
`QUERCUS_WORKER_BIN` to its absolute path before starting the process. A
missing worker makes task submission fail at run time even when the engine
library itself loaded correctly.

## Module map

Functions are named `quercus_<area>_<verb>`. The generated
[reference.md](reference.md) groups the full list; this table is a map, not a
substitute for it.

| Area | Prefix | Purpose | Representative calls |
|---|---|---|---|
| Core | `quercus_core_` | Rational/TimeRange math, platform paths, config, logging, color processing and display ICC. | `quercus_core_rational_new()`, `quercus_core_config_set()`, `quercus_core_colorproc_create()` |
| Audio | `quercus_audio_` | Audio buffers, device manager, channel/format conversions, waveform extraction. | `quercus_audio_manager_create_instance()`, `quercus_audio_buffer_copy_samples()`, `quercus_audio_waveform_extract()` |
| Audio analysis | `quercus_audio_` | Waveform envelope/stretch estimation, sync placement, loudness/level metering. | `quercus_audio_estimate_envelope_offset()`, `quercus_audio_levelmeter_analyze()`, `quercus_audio_place_by_source_time()` |
| Codec | `quercus_codec_` | Probe, decode, encode, frames, formats, proxies, Media I/O and codec task submission. | `quercus_codec_decoder_open()`, `quercus_codec_encoder_create()`, `quercus_codec_proxy_get_or_start()` |
| Editor | `quercus_editor_` | Session lifecycle, semantic edit dispatch, markers, multicam, clip audio, graph/parameter snapshots, save. | `quercus_editor_new()`, `quercus_editor_edit()`, `quercus_editor_save()`, `quercus_editor_edit_subscribe()` |
| Project | `quercus_project_` | Open/save document handles, settings, cache location, active project, byte-level load/save. | `quercus_project_new()`, `quercus_project_load()`, `quercus_project_save()` |
| Footage | `quercus_footage_` | Media-bin entries: import, filename, streams, duration, proxy parameters. | `quercus_footage_import()`, `quercus_footage_duration()`, `quercus_footage_video_params()` |
| Sequence | `quercus_sequence_` | Sequence properties: geometry, frame rate, interlacing, length, playhead, track lists. | `quercus_sequence_create()`, `quercus_sequence_frame_rate()`, `quercus_sequence_geometry()` |
| Track / track list | `quercus_track_`, `quercus_tracklist_` | Track creation and properties; block ordering; track-list traversal. | `quercus_track_add()`, `quercus_track_block_at()`, `quercus_tracklist_track_at()` |
| Block (clip/gap/transition) | `quercus_block_` | Timeline block accessors and low-level setters for in/out/media range, speed, links. | `quercus_block_create_clip()`, `quercus_block_in()`, `quercus_block_set_media_in()` |
| Effect | `quercus_effect_` | Effect factory enumeration, effect chains, enabled state, plugin instance handles. | `quercus_effect_chain_insert()`, `quercus_effect_set_enabled()`, `quercus_effect_factory_count()` |
| Timeline edits | `quercus_timeline_` | Low-level timeline operations: place/move/resize/split/trim blocks, ripple delete, transitions. | `quercus_timeline_block_place()`, `quercus_timeline_block_split()`, `quercus_timeline_transition_set_offsets()` |
| Node graph | `quercus_node_` | Generic graph nodes: connect/disconnect, kind/type, labels, standard values and time-varying values. | `quercus_node_connect()`, `quercus_node_kind()`, `quercus_node_standard_value()` |
| Undo | `quercus_undo_` | Process-wide undo stack: undo/redo, history queries, groups, observers. | `quercus_undo_undo()`, `quercus_undo_group_begin()`, `quercus_undo_add_observer()` |
| OpenTimelineIO | `quercus_otio_` | Build/parse OTIO documents and FCPXML, timeline/track/clip/transition objects. | `quercus_otio_timelines_from_fcpxml()`, `quercus_otio_document_to_json()`, `quercus_otio_timeline_new()` |
| OpenFX | `quercus_ofx_` | Plugin scan, instances, parameters, interaction events, curves, GL bridge, progress/cancel. | `quercus_ofx_scan()`, `quercus_ofx_instance_create()`, `quercus_ofx_param_set_double()` |
| App plugins | `quercus_plugin_` | Registry of host plugins and their menu commands. | `quercus_plugin_register()`, `quercus_plugin_add_command()`, `quercus_plugin_invoke_command()` |
| Render | `quercus_render_` | Render manager, engine-owned or host-adopted GPU device, CPU frame rendering, disk cache, text engine/fonts. | `quercus_render_frame_f32()`, `quercus_render_manager_init()`, `quercus_render_gpu_init()`, `quercus_render_gpu_install()` |
| Preview buffers | `quercus_video_`, `quercus_audio_` | Refcounted preview video/audio buffers returned by tasks and preview calls. | `quercus_video_buffer_info()`, `quercus_video_buffer_copy_bytes()`, `quercus_audio_buffer_info()` |
| Storage | `quercus_storage_` | Storage backends, project library operations, URI validation, transport registration. | `quercus_storage_open()`, `quercus_storage_save()`, `quercus_storage_register_transport()` |
| Tasks | `quercus_task_` | Asynchronous export, preview/thumbnail/waveform/audio renders, progress, cancellation, results. | `quercus_task_export()`, `quercus_task_wait()`, `quercus_task_take_thumbnail()`, `quercus_task_error()` |
| Thumbnails | `quercus_thumbnail_` | Refcounted thumbnail buffers and pixel access. | `quercus_thumbnail_info()`, `quercus_thumbnail_copy_pixels()` |
| Waveforms | `quercus_waveform_` | Refcounted waveform summaries and min/max point access. | `quercus_waveform_info()`, `quercus_waveform_point_at()` |
| Workers | `quercus_worker_` | Standalone worker entry points for render and OpenFX hosting. | `quercus_worker_run()`, `quercus_worker_ofx_host()` |
| Generators | `quercus_generator_` | Create generator nodes from a type id. | `quercus_generator_create()` |

Two naming notes:

- The Rust SDK calls timeline blocks "clips"; the C ABI uses the historical
  `quercus_block_` prefix for the same objects. Gap, clip, transition and
  adjustment blocks share it.
- `clip` is not a header prefix. Use `quercus_block_*` for block data and
  `quercus_timeline_*` or `quercus_editor_edit()` for edits.

A small number of functions return opaque vector handles
(`quercus_audio_manager_input_device_names()`,
`quercus_codec_decoder_ids()`,
`quercus_codec_format_get_audio_codecs()`). Their vector accessor ABI is
exported by the engine and wrapped by the Rust SDK, but is not declared in
the generated header. C hosts should prefer the scalar queries
(`quercus_audio_manager_device_index_by_name()`,
`quercus_codec_codec_count()`, …) unless the accessors are present in their
header.

## Documentation pipeline

### Can I just run `cargo doc`?

**No — `cargo doc` does not document the C ABI.** `rustdoc` documents Rust
items, not C declarations. The engine's C functions exist in the SDK as
`extern "C"` declarations inside `unsafe extern` blocks; they carry no
rustdoc pages. `cargo doc -p quercus-engine` with the `docs-only` feature (as
docs.rs builds it) deliberately documents the crate's role and links to the
header instead of the full API; a normal build documents internal Rust
modules, not the C surface.

Use the right tool for each surface:

```sh
# 1. Rust SDK reference (Rust types and safe wrappers)
cargo doc -p quercus-engine-rs --no-deps
# open target/doc/quercus_engine_rs/index.html

# 2. C API reference (generated from the checked-in header)
python3 tooling/gen_c_api_docs.py --write   # writes docs/c-api/reference.md
python3 tooling/gen_c_api_docs.py --check   # CI drift guard

# 3. Regenerate / verify the C header itself
python3 tooling/gen_c_header.py --write
python3 tooling/gen_c_header.py --check
```

The pipeline is one-way and header-centric:

```
Rust SDK extern "C" declarations
        │  tooling/gen_c_header.py
        ▼
crates/quercus-engine/include/quercus_engine.h   (checked in)
        │  tooling/gen_c_api_docs.py
        ▼
docs/c-api/reference.md                           (checked in)
```

The header is the single source of truth for C hosts: if a symbol is not in
the header, it is not part of the documented C ABI. CI runs both `--check`
commands so neither generated file can go stale. Editing the header or
`reference.md` by hand is always wrong; change the Rust declarations or the
generators instead.

For a host build that consumes the SDK dynamically, `cargo doc` is still
useful for the Rust-side ownership and error types, but it is not a C
reference and will not list the 742 functions by name.

## Related documentation

- [reference.md](reference.md) — generated function-by-function C reference.
- [build-windows.md](build-windows.md) — Windows x64 MSVC build, packaging,
  linking, runtime DLLs and troubleshooting.
- [`examples/c-api/quickstart.c`](../../examples/c-api/quickstart.c) — full
  minimal session/import/place/save program.
- [`examples/c-api/export.c`](../../examples/c-api/export.c) — task export
  example.
- [`docs/build.md`](../build.md) — all-platform build prerequisites.
- [`crates/quercus-engine/README.md`](../../crates/quercus-engine/README.md)
  — crate-level ABI notes.
- [`crates/quercus-engine-rs/README.md`](../../crates/quercus-engine-rs/README.md)
  — Rust SDK guide.

License: GPL-3.0-or-later. See [`LICENSE`](../../LICENSE).
