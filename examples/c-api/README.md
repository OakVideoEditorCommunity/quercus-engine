# Quercus engine C ABI examples

Self-contained C11 examples that use only the generated public header
`quercus_engine.h` and the engine shared library. They prove the C ABI is
usable end to end and document the parts a C caller cannot guess from the
header alone.

| File | Purpose |
| --- | --- |
| `quickstart.c` | Create an editor session and sequence, import optional footage, set the work area, save a `.ove` project. |
| `export.c` | Create a session and sequence, import footage, place a video clip, export through `quercus_task_export`, wait for the task. |
| `worker.c` | Thin `quercus-worker` launcher for export/preview rendering when the install does not ship that binary (see below). |
| `Makefile` | Local macOS build and run targets. |

All examples use 4-space indentation and compile without warnings under
`-Wall -Wextra`.

## Requirements

- A built engine dylib: `target/debug/libquercus_engine.dylib` (macOS) or
  `quercus_engine.dll` plus its import library (Windows).
- The public header directory: `crates/quercus-engine/include`.
- For export: a `quercus-worker` executable. See
  [Export needs quercus-worker](#export-needs-quercus-worker).

This checkout's dylib links FFmpeg statically (`otool -L` shows no
`libav*.dylib`), so no FFmpeg loader path is required here. A dynamically
linked build may need its FFmpeg dylibs on `DYLD_LIBRARY_PATH`; the commands
below keep `.cache/ffmpeg/lib` there for parity.

## macOS: build and run

The link line needs an rpath (or the loader path must contain
`target/debug`) because the dylib's install name is
`@rpath/libquercus_engine.dylib`. Without it the binary aborts with
`Library not loaded: @rpath/libquercus_engine.dylib`.

```sh
make -C examples/c-api all
make -C examples/c-api run-quickstart
make -C examples/c-api run-export
```

Equivalent manual commands from the repository root:

```sh
clang -std=c11 -Wall -Wextra \
    -I crates/quercus-engine/include \
    examples/c-api/quickstart.c \
    -L target/debug -lquercus_engine \
    -Wl,-rpath,"$PWD/target/debug" \
    -o /tmp/quercus-quickstart

/tmp/quercus-quickstart testdata/demo.mp4 /tmp/quercus-quickstart.ove
```

`quickstart` takes `[media-file] [output.ove]`. With no arguments it skips
footage import and saves `quercus-quickstart.ove` in the current directory.

Observed output with `testdata/demo.mp4` (macOS arm64, 2026-11):

```text
Quercus engine C ABI quickstart
  editor session created
  project name       (untitled)
  sequence id       3 (current: 3)
  sequence geometry 1920x1080
  sequence fps      30000/1001
  sequence length    0.000000 s
  footage id        10
  footage filename   ../../testdata/demo.mp4
  footage duration   17.000000 s
  video streams     1
  audio streams     1
  work area         [0.000000, 2.000000) s
  saved project     /tmp/quercus-quickstart.ove
done
exit 0
```

## Export

```sh
clang -std=c11 -Wall -Wextra \
    -I crates/quercus-engine/include \
    examples/c-api/export.c \
    -L target/debug -lquercus_engine \
    -Wl,-rpath,"$PWD/target/debug" \
    -o /tmp/quercus-export

clang -std=c11 -Wall -Wextra \
    -I crates/quercus-engine/include examples/c-api/worker.c \
    -L target/debug -lquercus_engine \
    -Wl,-rpath,"$PWD/target/debug" \
    -o /tmp/quercus-worker

QUERCUS_WORKER_BIN=/tmp/quercus-worker \
    /tmp/quercus-export testdata/demo.mp4 /tmp/quercus-export.mp4 1
```

`make -C examples/c-api run-export` builds and runs all of the above.

`export` takes `<media-file> [output.mp4] [seconds]`. The sequence node id
itself is the export "viewer"; no separate viewer handle exists in the C
ABI. Observed stdout:

```text
Quercus engine C ABI export example
  sequence          3 at 30000/1001 fps
  footage           10 (../../testdata/demo.mp4)
  clip              11
  export task       1
  task state        1 (succeeded)
  wrote             /tmp/quercus-export.mp4
done
exit 0
```

`ffprobe` verification of `/tmp/quercus-export.mp4`:

```text
codec_name=h264  codec_type=video  width=640  height=360  r_frame_rate=30000/1001
codec_name=aac   codec_type=audio  sample_rate=48000  channels=2
format_name=mov,mp4,m4a,3gp,3g2,mj2
duration=0.834167
```

The output is shorter than the requested 1 s because the placed clip is 25
frames; an export cannot be longer than the viewer's content.

### Export needs quercus-worker

Rendering runs in child processes. On a checkout without a packaged
`quercus-worker`, `quercus_task_export` starts correctly but the task fails:

```text
task error        task: operation failed: render worker pool: render: operation
failed: quercus-worker binary not found at /tmp/quercus-worker; set
DispatcherConfig::worker_bin or QUERCUS_WORKER_BIN
```

The engine looks for the worker through its dispatcher config,
`QUERCUS_WORKER_BIN`, or a `quercus-worker` file beside the current
executable. `worker.c` is a working launcher built from the public ABI:

```sh
clang -std=c11 -Wall -Wextra \
    -I crates/quercus-engine/include examples/c-api/worker.c \
    -L target/debug -lquercus_engine \
    -Wl,-rpath,"$PWD/target/debug" \
    -o /tmp/quercus-worker
```

It forwards the engine's `--backend <name>` argument to
`quercus_worker_run` and runs the NDJSON worker loop on stdin/stdout. With
`QUERCUS_WORKER_BIN` pointing at it, the export above succeeds. Use the
official `quercus-worker` binary when your install ships one; the launcher is
a verified fallback, not a replacement for packaging.

## Windows x64 MSVC

The header and examples are C11; build from a Developer Command Prompt with
`/std:c11` (VS 2019 16.7+). The import library name expected by the Rust
cdylib build is `quercus_engine.dll.lib`.

From the repository root:

```bat
cl /nologo /std:c11 /W4 ^
   /I crates\quercus-engine\include ^
   examples\c-api\quickstart.c ^
   /Fe:quickstart.exe ^
   /link /LIBPATH:target\debug quercus_engine.dll.lib
```

If headers and the import library are already staged in `include\` and the
current directory, the short form is:

```bat
cl /I include /Fe:quickstart.exe quickstart.c /link quercus_engine.dll.lib
```

Build the export example and the worker launcher the same way:

```bat
cl /nologo /std:c11 /W4 /I crates\quercus-engine\include ^
   examples\c-api\export.c /Fe:export.exe ^
   /link /LIBPATH:target\debug quercus_engine.dll.lib

cl /nologo /std:c11 /W4 /I crates\quercus-engine\include ^
   examples\c-api\worker.c /Fe:quercus-worker.exe ^
   /link /LIBPATH:target\debug quercus_engine.dll.lib
```

DLL placement: copy `quercus_engine.dll` next to the `.exe`, or put its
directory on `PATH` before running. For export, also set:

```bat
set QUERCUS_WORKER_BIN=%CD%\quercus-worker.exe
quickstart.exe testdata\demo.mp4 out.ove
export.exe testdata\demo.mp4 out.mp4 1
```

If the Windows build links FFmpeg dynamically, the FFmpeg DLLs must be next
to the executable or on `PATH` as well.

**Verification status:** the Windows instructions are not executed on this
macOS host. Windows coverage here is a syntax check only (next section). The
MSVC ABI, import-library layout, and DLL discovery still need a real Windows
x64 run.

## Header syntax checks

macOS / clang:

```sh
for f in examples/c-api/quickstart.c examples/c-api/export.c examples/c-api/worker.c; do
    clang -std=c11 -Wall -Wextra -fsyntax-only \
        -I crates/quercus-engine/include "$f"
done
```

Windows GNU cross-check (no run, no link):

```sh
for f in examples/c-api/quickstart.c examples/c-api/export.c examples/c-api/worker.c; do
    x86_64-w64-mingw32-gcc -std=c11 -Wall -Wextra -fsyntax-only \
        -I crates/quercus-engine/include "$f"
done
```

`x86_64-w64-mingw32-gcc -fsyntax-only` validates that the header and sources
parse for a Windows GNU target. It does **not** validate MSVC ABI details
(struct packing, import-library naming, `/std:c11` feature set); only a build
and run with MSVC does that.

## API notes for C callers

These are verified against the header and runtime, and they matter for C
documentation:

1. **There is no `quercus_engine_last_error`.** Fallible calls report
   failure through their return value; some take an error buffer
   (`quercus_editor_edit`), tasks have `quercus_task_error`, and
   `quercus_editor_storage_last_error` covers only the write-through storage
   backend. `quercus_footage_import` just returns an invalid node id.
2. **There is no `quercus_editor_workarea_set`.** Read the work area with
   `quercus_editor_workarea`; write it through the generic edit dispatcher:
   `quercus_editor_edit(session, QUERCUS_EDITOP_WORKAREASET, &params,
   &result, error, error_len)` with `params.enabled` and `params.range` set.
   The work area lives in the session, not the node graph, so save through
   `quercus_editor_save` for it to be serialized.
3. **No `QUERCUS_NODE_ID_INVALID` constant is exported.** Use
   `quercus_node_id_invalid()`.
4. **Task states are bare integers in the header.** `quercus_task_state`
   returns `0` running, `1` succeeded, `2` failed, `3` cancelled, `-1` for an
   unknown id. These names are not generated as constants.
5. **`EncodingParams` is ambiguous in the header.** The generated struct
   matches the export task POD (`QuercusTaskEncodingParams`, no filename
   field). The header also declares `quercus_codec_encoding_params_default()`
   as returning that same `EncodingParams`, but at runtime it returns
   `quercus-codec`'s separate, much larger encoder struct (it starts with a
   1024-byte filename). Calling that function through this header is
   ABI-unsafe. `export.c` therefore zero-initializes the task struct and sets
   fields directly. The header generator needs distinct C names for the two
   structs before C callers can use the codec encoder API.
6. **String getters are snprintf-style.** They NUL-terminate, return the full
   length excluding the NUL, and return `-1` for a null/missing handle. A
   return value `>= buf_len` means truncation.
7. **The link line needs an rpath** (or a loader path containing
   `target/debug`). The header/lib flag pair alone fails at load time on
   macOS with the install name `@rpath/libquercus_engine.dylib`.
