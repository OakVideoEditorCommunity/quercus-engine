# Building the Windows x64 C Library

This page describes how to build, package and link the Quercus engine C ABI
for **64-bit Windows** from a Windows host. The supported target is
`x86_64-pc-windows-msvc` (x64, MSVC ABI), the same target the repository's CI
and CD workflows build on `windows-2022`.

The Windows artifact is produced by the CD workflow; nothing in this page
assumes a prebuilt archive is already in the tree. The commands below
reproduce the CI build; packaging and runtime placement are covered
separately.

> **Verification status:** the native-Windows commands below are derived from
> the CI/CD workflows and the SDK's `#[link]` attributes; they have not been
> executed on a Windows host in this revision. The **cross-compilation recipe**
> in [Cross-compiling from macOS/Linux](#cross-compiling-from-macoslinux)
> *has* been executed for `x86_64-pc-windows-msvc` in this revision, producing
> `quercus_engine.dll`, `quercus_engine.dll.lib`, `quercus_engine.lib` and a
> linked C consumer. Report discrepancies with the exact failing command and
> output.

## Target and support

| Property | Value |
|---|---|
| Rust target | `x86_64-pc-windows-msvc` |
| Architecture | x64 (64-bit) |
| ABI / toolchain | MSVC (Visual Studio 2022, x64 toolset) |
| CI runner | `windows-2022` (`.github/workflows/ci.yml`, `cd.yml`) |
| CD artifact | `quercus-engine-native-windows-x64.zip` |

32-bit Windows (`i686-pc-windows-msvc`, `i686-pc-windows-gnu`) is **not
supported** by the repository's CI or dependency matrix. Do not spend time
producing an x86 build from these instructions; the FFmpeg, OpenColorIO and
toolchain combinations used by the project are validated only for x64.

Cross-compiling to the MSVC ABI from macOS or Linux is supported with
[`cargo-xwin`](https://github.com/rust-cross/cargo-xwin): it provisions the
MSVC CRT/Windows SDK headers and libraries and drives `clang-cl` + `lld-link`.
The verified recipe is in
[Cross-compiling from macOS/Linux](#cross-compiling-from-macoslinux). Native
`cl.exe` builds on Windows remain the canonical release path (the CD artifact);
the cross build produces the same MSVC ABI, COFF import library and DLL
exports.

## Prerequisites

- **Visual Studio 2022** (Community, Professional or Build Tools) with the
  "Desktop development with C++" workload and the x64 toolset. Use the
  **x64 Native Tools Command Prompt for VS 2022**, or run
  `VC\Auxiliary\Build\vcvars64.bat` in a normal shell.
- **Rust stable** for the MSVC host:
  `rustup toolchain install stable-x86_64-pc-windows-msvc`.
- **Python 3** for the header/reference checks.
- **pkg-config**. CI installs the MSYS2 UCRT64 package:
  `C:\msys64\usr\bin\pacman -S --needed --noconfirm mingw-w64-ucrt-x86_64-pkgconf`
  and points `PKG_CONFIG` at `C:\msys64\ucrt64\bin\pkgconf.exe`.
- **FFmpeg 8.1 shared development package**, BtbN `gpl-shared` build
  (`ffmpeg-n8.1-latest-win64-gpl-shared-8.1.zip` from
  <https://github.com/BtbN/FFmpeg-Builds/releases>). CI downloads that asset
  and verifies its SHA-256 against the release's `checksums.sha256`.
- A working linker. Git for Windows ships `/usr/bin/link.exe`, which can
  shadow MSVC's `link.exe`; pin the MSVC linker explicitly (see below).

The extracted FFmpeg folder must contain:

```
ffmpeg/
├─ bin/            # runtime DLLs: avcodec-*.dll, avformat-*.dll, ...
├─ include/        # development headers
└─ lib/            # *.lib import libraries, pkgconfig/
```

`FFMPEG_DIR` points at that folder, not at its parent.

## Environment

Open an x64 MSVC shell and set the same variables the CI Windows job sets.
In PowerShell:

```powershell
$env:FFMPEG_DIR = "C:\deps\ffmpeg"
$env:PKG_CONFIG_PATH = "$env:FFMPEG_DIR\lib\pkgconfig"
$env:PKG_CONFIG = "C:\msys64\ucrt64\bin\pkgconf.exe"
$env:OCIO_RS_ENABLE_REAL = "1"
$env:OCIO_RS_LINK = "static"
$env:CC = "cl"
$env:CXX = "cl"

# Pin MSVC's linker: /usr/bin/link.exe from Git for Windows must not win.
$linker = Join-Path $env:VCToolsInstallDir "bin\Hostx64\x64\link.exe"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $linker
```

`VCToolsInstallDir` is defined by the Visual Studio developer shell. Verify it
before building:

```powershell
if (-not (Test-Path $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER)) {
    throw "MSVC link.exe not found: $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"
}
```

Why the variables matter:

| Variable | Purpose |
|---|---|
| `FFMPEG_DIR` | Development prefix with FFmpeg headers, import libraries and pkg-config files. Required by the codec crates. |
| `PKG_CONFIG_PATH` | Lets the build find FFmpeg's `.pc` files inside that prefix. |
| `PKG_CONFIG` | Explicit `pkgconf.exe`; avoids a missing or MSYS-relative pkg-config. |
| `OCIO_RS_ENABLE_REAL` | Selects the real OpenColorIO bridge instead of the stub. |
| `OCIO_RS_LINK` | `static` for the CI configuration. If you use a dynamic OpenColorIO build instead, ship its DLL with your application. |
| `CC` / `CXX` | Build scripts should use MSVC (`cl`), not a MinGW compiler. |
| `..._MSVC_LINKER` | Pins the correct `link.exe`, avoiding Git's `link.exe`. |

`tooling/ocio-env.sh` prints the OpenColorIO variables on Linux/macOS and
handles MinGW; on a native MSVC build the Windows job sets the two OCIO
variables directly, as shown above.

FFmpeg is linked **dynamically** by default. Static FFmpeg is optional via the
`ffmpeg-static` feature and a static FFmpeg development prefix; that does not
change the engine's own output layout, only whether the FFmpeg runtime DLLs are
required at run time.

## Build

Check that the checked-in header matches the Rust declarations first, then
build the engine package:

```bat
python tooling\gen_c_header.py --check
cargo build --release --locked -p quercus-engine
```

On an x64 Windows host this uses the default target
`x86_64-pc-windows-msvc` and writes to `target\release\`. To name the target
explicitly (or to build from a shell whose default target differs):

```bat
rustup target add x86_64-pc-windows-msvc
cargo build --release --locked -p quercus-engine --target x86_64-pc-windows-msvc
```

With an explicit `--target`, outputs land in
`target\x86_64-pc-windows-msvc\release\`.

Expected outputs:

| File | Crate type | Use |
|---|---|---|
| `quercus_engine.dll` | `cdylib` | Runtime library loaded by the process. |
| `quercus_engine.dll.lib` | `cdylib` import library | Link this into MSVC hosts. |
| `quercus_engine.lib` | `staticlib` | Static archive. **Not** the import library. |

The names are literal: the MSVC import library is `quercus_engine.dll.lib`,
not `quercus_engine.lib`. This is also why the Rust SDK uses
`#[link(name = "quercus_engine.dll.lib", modifiers = "+verbatim")]` on
Windows. Passing the wrong one produces `LNK1181`/`LNK2019` (see
Troubleshooting).

The header remains at
`crates\quercus-engine\include\quercus_engine.h`; it is not copied next to
the build outputs automatically.

## Cross-compiling from macOS/Linux

This recipe was executed on macOS arm64 against this revision. It produces an
**MSVC ABI** (`x86_64-pc-windows-msvc`) DLL and import library; the code is
compiled by `clang-cl` with the Microsoft CRT/Windows SDK and linked by
`lld-link`. It is not a MinGW/GNU-ABI build.

Prerequisites:

- Rust target `x86_64-pc-windows-msvc` (`rustup target add ...`).
- `cargo-xwin` (prebuilt `universal2-apple-darwin` release, or
  `cargo install cargo-xwin`). Point `XWIN_CACHE_DIR` at a writable directory
  inside the workspace so the SDK download is not blocked by a read-only
  `$HOME`.
- BtbN FFmpeg 8.1 `win64-gpl-shared` (headers, `.lib` import libraries,
  pkg-config files and runtime DLLs).
- Network access for cargo-xwin's SDK download and for the bundled OpenColorIO
  build. Set `HTTPS_PROXY`/`HTTP_PROXY` if the Microsoft CDN is slow.

One-time setup:

```bash
export XWIN_CACHE_DIR="$PWD/.cache/xwin"
export PATH="$PWD/.cache/tools/bin:$PATH"
cargo-xwin env --target x86_64-pc-windows-msvc > .cache/xwin.env
source .cache/xwin.env
```

`cargo-xwin env` downloads the MSVC CRT and Windows SDK into
`$XWIN_CACHE_DIR` and prints `CC`, `CXX`, `CFLAGS`, `LIB`, `PATH` and the
CMake toolchain path. It also rewrites
`$XWIN_CACHE_DIR/cmake/clang-cl/x86_64-pc-windows-msvc-toolchain.cmake` on
each invocation. Two workarounds are required for the bundled OpenColorIO
build:

1. **Do not leave `CMAKE_*_STANDARD_LIBRARIES` empty.** cargo-xwin's generated
   toolchain clears them. OCIO's `SystemMonitor.cpp` calls User32/GDI
   (`GetDisplayConfigBufferSizes`, `EnumDisplayDevicesW`, `CreateDCW`, ...)
   without CMake link libraries, so the DLL link fails with undefined symbols
   unless the toolchain restores the MSVC default set:

   ```cmake
   set(CMAKE_C_STANDARD_LIBRARIES "kernel32.lib user32.lib gdi32.lib winspool.lib shell32.lib ole32.lib oleaut32.lib uuid.lib comdlg32.lib advapi32.lib" CACHE STRING "" FORCE)
   set(CMAKE_CXX_STANDARD_LIBRARIES "kernel32.lib user32.lib gdi32.lib winspool.lib shell32.lib ole32.lib oleaut32.lib uuid.lib comdlg32.lib advapi32.lib" CACHE STRING "" FORCE)
   ```

   Patch the generated file with `sed`/Python, then invoke plain `cargo`
   (not `cargo-xwin`) so the file is not regenerated underneath you.

2. **Point `OCIO_SOURCE_DIR` at a writable copy of the vendored OCIO tree.**
   OCIO's external-project patch step writes `CMakeLists.txt` into the source
   directory; the git checkout under `$CARGO_HOME` is read-only for build
   scripts. Copy `ocio-sys/vendor/OpenColorIO` (or let the crate unpack it) to
   `$PWD/.cache/ocio-src/OpenColorIO` and set `OCIO_SOURCE_DIR` to it.

Build:

```bash
export FFMPEG_DIR="$PWD/.cache/win/ffmpeg-n8.1-latest-win64-gpl-shared-8.1"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$FFMPEG_DIR/lib/pkgconfig"
export OCIO_RS_ENABLE_REAL=1
export OCIO_RS_LINK=dynamic          # ship OpenColorIO_2_5.dll
export OCIO_SOURCE_DIR="$PWD/.cache/ocio-src/OpenColorIO"

# Host-side ocio-sys emits `-l c++` for the build host (macOS) even when the
# target is Windows. Satisfy the linker once with an empty COFF archive placed
# in the CRT library directory:
llvm-ar crs "$XWIN_CACHE_DIR/xwin/crt/lib/x86_64/c++.lib"

cargo build --release -p quercus-engine --target x86_64-pc-windows-msvc
```

Outputs:

```
target/x86_64-pc-windows-msvc/release/quercus_engine.dll
target/x86_64-pc-windows-msvc/release/quercus_engine.dll.lib
target/x86_64-pc-windows-msvc/release/quercus_engine.lib
```

With `OCIO_RS_LINK=dynamic`, also ship the generated
`OpenColorIO_2_5.dll` (from the `ocio-sys` `OUT_DIR`) and
`MSVCP140.dll`/`VCRUNTIME140.dll` (VC++ Redistributable). Static OCIO is the
CD default: it avoids the extra DLL but requires the static transitive
dependencies (`expat`, `yaml-cpp`, `Imath`, `pystring`, `minizip-ng`, zlib) to
be linkable under their MSVC names.

Verify the result without Windows:

```bash
llvm-readobj --file-headers --coff-imports --coff-exports \
  target/x86_64-pc-windows-msvc/release/quercus_engine.dll
llvm-readobj --coff-imports /tmp/quercus-quickstart.exe   # linked C consumer
```

Expect `Machine: IMAGE_FILE_MACHINE_AMD64 (0x8664)`, imports of
`MSVCP140.dll`/`VCRUNTIME140.dll` plus the FFmpeg and OpenColorIO DLLs, and
`quercus_engine.dll` in the consumer's import table. This checks the MSVC ABI
and symbol names but does not execute the DLL; run it on Windows (or add the
artifact to a Windows CI job) for a runtime smoke test.

## Packaging layout

A relocatable Windows package should keep runtime libraries and link-time
libraries separate:

```
quercus-engine-windows-x64/
├─ bin/
│  ├─ quercus_engine.dll
│  └─ avcodec-*.dll, avformat-*.dll, avutil-*.dll,
│     swscale-*.dll, swresample-*.dll, ...
├─ lib/
│  ├─ quercus_engine.dll.lib      # import library (dynamic hosts)
│  └─ quercus_engine.lib          # static archive
├─ include/
│  └─ quercus_engine.h
└─ docs/                          # optional
   ├─ c-api/README.md
   └─ c-api/reference.md
```

The CD workflow currently stages a flatter layout: it copies
`quercus_engine.dll`, both `.lib` files and the FFmpeg `bin\*.dll` files into
`dist\lib`, copies the header into `dist\include`, then zips `dist\*`. That
layout also works, provided the runtime DLLs end up beside the host executable
or on the loader search path. The split `bin`/`lib` layout above makes the
distinction explicit and is the recommended layout for third-party
redistribution.

At minimum, ship the DLLs your process loads at run time:

- `quercus_engine.dll`
- every FFmpeg DLL from `FFMPEG_DIR\bin` (`avcodec-*.dll`, `avformat-*.dll`,
  `avutil-*.dll`, `swscale-*.dll`, `swresample-*.dll`, `avfilter-*.dll`,
  plus any other DLLs in that folder)
- the OpenColorIO DLL if you configured a dynamic OpenColorIO instead of the
  CI's static setting

Task-based export and preview also need the `quercus-worker` helper process.
Ship `quercus-worker.exe` next to your host executable, or set the
`QUERCUS_WORKER_BIN` environment variable to its absolute path; the engine
does not search `PATH` by default. The worker is a separate executable, not a
DLL, and a missing worker makes `quercus_task_export()` and the other task
APIs fail at run time even when `quercus_engine.dll` loaded correctly.

## Runtime DLL search

Windows resolves a DLL loaded by the executable by searching, in order, the
executable's own directory, the system directory, the Windows directory, the
current directory and then `PATH` (subject to safe-DLL-search settings).

The simplest deployment is to copy `quercus_engine.dll` and all FFmpeg DLLs
next to `app.exe`:

```bat
copy /Y bin\quercus_engine.dll .
copy /Y bin\avcodec-*.dll .
copy /Y bin\avformat-*.dll .
copy /Y bin\avutil-*.dll .
copy /Y bin\swscale-*.dll .
copy /Y bin\swresample-*.dll .
app.exe
```

Alternatively, add the DLL directory to `PATH` before starting the process:

```bat
set PATH=C:\quercus-engine-windows-x64\bin;%PATH%
app.exe
```

Do not rely on the build machine's `.cache\ffmpeg` being present on the target
machine. Missing runtime DLLs surface as a loader failure
(`0xc0000135`/`0x8007007e`) before `main` runs.

## Consuming the library from MSVC

The minimal command, run in an x64 Visual Studio developer shell:

```bat
cl /I include /Fe:app.exe app.c /link quercus_engine.dll.lib
```

A more complete invocation that matches the recommended layout and the Rust
dynamic CRT:

```bat
cl /nologo /W4 /MD /I include app.c /Fe:app.exe ^
   /link /LIBPATH:lib quercus_engine.dll.lib
```

- `/MD` selects the dynamic C runtime, matching the Rust MSVC library. Do not
  mix `/MT` (static CRT) into the host: allocating and freeing across the DLL
  boundary with mismatched CRTs is undefined. A fully static CRT host is not a
  supported configuration for this library.
- `/I include` is required; the header is not on the default include path.
- The import library goes after `/link`; using `quercus_engine.lib` here links
  the static archive instead and is usually wrong.
- The resulting `app.exe` needs the runtime DLLs described above.

### CMake

An imported target keeps include directories, the import library and the
runtime DLL together:

```cmake
cmake_minimum_required(VERSION 3.20)
project(quercus_app C)

set(QUERCUS_ROOT "C:/quercus-engine-windows-x64")

add_library(quercus_engine SHARED IMPORTED)
set_target_properties(quercus_engine PROPERTIES
    IMPORTED_IMPLIB "${QUERCUS_ROOT}/lib/quercus_engine.dll.lib"
    IMPORTED_LOCATION "${QUERCUS_ROOT}/bin/quercus_engine.dll"
    INTERFACE_INCLUDE_DIRECTORIES "${QUERCUS_ROOT}/include"
)

add_executable(app app.c)
set_property(TARGET app PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
target_link_libraries(app PRIVATE quercus_engine)
```

`MSVC_RUNTIME_LIBRARY "MultiThreadedDLL"` is CMake's `/MD`. `IMPORTED_LOCATION`
is for CMake's runtime bookkeeping and copying; Windows itself still resolves
the DLL through the search path, so copy the DLLs next to `app.exe` (or add
`${QUERCUS_ROOT}/bin` to `PATH`) before running.

### Delay loading (optional)

If the engine is an optional feature and the host must start without it:

```bat
cl /nologo /MD /I include app.c /Fe:app.exe ^
   /link /DELAYLOAD:quercus_engine.dll delayimp.lib quercus_engine.dll.lib
```

Without a delay-load failure hook, a missing engine DLL raises a structured
exception (`0xc06d007e`) on first use. This is only worth the extra complexity
for genuinely optional engine loading; the default is load-time linking.

## Verify the build

Inspect the DLL's machine type and a few exported symbols:

```bat
dumpbin /headers quercus_engine.dll | findstr /C:"machine"
dumpbin /exports quercus_engine.dll | findstr /C:"quercus_editor_new"
dumpbin /exports quercus_engine.dll | findstr /C:"quercus_editor_edit"
dumpbin /exports quercus_engine.dll | findstr /C:"quercus_task_export"
```

The header should report `machine (x64)`. The export names are undecorated
because the ABI is `extern "C"` with the default x64 calling convention.

For a full comparison, count the header's declarations and the DLL's
`quercus_*` exports; the header documents the non-vector C surface (742
functions in this revision), while the engine also exports its generic vector
helpers. A missing non-vector export is a build/ABI problem; do not treat the
two raw counts as necessarily equal.

Also re-run the repository guards:

```bat
python tooling\gen_c_header.py --check
python tooling\gen_c_api_docs.py --check
```

The header check catches Rust/C drift. The reference check catches stale
`docs/c-api/reference.md`. Both are run by CI.

## Reproducing the CI Windows job

The essential steps from `.github/workflows/cd.yml` are:

1. `ilammy/msvc-dev-cmd@v1` with `arch: x64`;
2. pin `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER` to
   `%VCToolsInstallDir%\bin\Hostx64\x64\link.exe`;
3. install `stable-x86_64-pc-windows-msvc`;
4. install the UCRT64 `pkgconf` package and set `PKG_CONFIG`;
5. download and extract the BtbN `gpl-shared` FFmpeg package;
6. set `FFMPEG_DIR`, `PKG_CONFIG_PATH`, `OCIO_RS_ENABLE_REAL=1`,
   `OCIO_RS_LINK=static`, `CC=cl`, `CXX=cl`;
7. `cargo build --release --locked -p quercus-engine`;
8. stage the DLL, import library, static archive, FFmpeg DLLs and header, then
   zip the result.

The CI job uses the native MSVC target and does not pass `--target`, so its
outputs are in `target\release\`.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `LNK1181: cannot open input file 'quercus_engine.lib'` | Linked the static/import name by habit, or the SDK default library name. | Pass `quercus_engine.dll.lib` explicitly; it is the import library. |
| `LNK4272: library machine type 'x64' conflicts with target machine type 'x86'` | Mixing a 32-bit host with the x64 library. | Build the host for x64; 32-bit is unsupported. |
| `LNK2019: unresolved external symbol __imp_quercus_*` | Import library not on the link line, or wrong architecture. | Add `quercus_engine.dll.lib` after `/link`; confirm x64. |
| `0xc0000135` / `0x8007007e` at startup | `quercus_engine.dll` or an FFmpeg DLL is missing. | Copy all runtime DLLs beside the exe or add them to `PATH`. |
| `0xc000007b` at startup | 32/64-bit DLL mismatch. | Use only x64 DLLs (engine and FFmpeg). |
| `'cl' is not recognized` | Not in a Visual Studio developer shell. | Run `vcvars64.bat` or use the x64 Native Tools prompt. |
| Cargo invokes Git's `link.exe` and fails oddly | `/usr/bin/link.exe` shadows MSVC's linker. | Set `CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER` as shown above. |
| FFmpeg not found during build | `FFMPEG_DIR` points at the wrong level. | Point it at the folder containing `include\`, `lib\` and `bin\`. |
| `pkg-config` not found or returns MSYS paths | `PKG_CONFIG` unset or wrong. | Set `PKG_CONFIG` to `pkgconf.exe` and set `PKG_CONFIG_PATH`. |
| OpenColorIO stub or build errors | Missing OCIO variables. | Set `OCIO_RS_ENABLE_REAL=1` and `OCIO_RS_LINK=static`. |
| Host runs but needs `VCRUNTIME140.dll` | MSVC dynamic runtime not installed on the target. | Install the matching Visual C++ Redistributable, or deploy the runtime DLLs. |

Further reading: [README.md](README.md) for ABI conventions and the quick
start, [reference.md](reference.md) for the generated function reference,
[`docs/build.md`](../build.md) for all-platform prerequisites, and
[`crates/quercus-engine/README.md`](../../crates/quercus-engine/README.md) for
crate-level notes.

License: GPL-3.0-or-later. See [`LICENSE`](../../LICENSE).
