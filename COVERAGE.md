# Test coverage audit

Date: 2026-10-30. Workspace: `quercus-engine` at `0.5.2`.
FFmpeg cache copied from the Oak checkout into `.cache` (git-ignored) before
the run.

## Primary result (cargo-llvm-cov)

Command:

```sh
cargo llvm-cov --workspace --summary-only -- --test-threads=1
```

All 2,782 tests pass. Overall **line coverage 89.09%** (107,939 / 121,157).
Excluding test-only files (integration test binaries and
`worker/procpool_integration.rs`) the production line coverage is **89.73%**
(107,875 / 120,218).

| crate | lines covered / total | line % |
|---|---:|---:|
| quercus-undo | 532 / 555 | 95.9% |
| quercus-core | 13,188 / 13,909 | 94.8% |
| quercus-codec | 6,893 / 7,382 | 93.4% |
| quercus-node | 22,937 / 24,788 | 92.5% |
| quercus-render | 14,194 / 15,352 | 92.5% |
| quercus-otio | 1,643 / 1,781 | 92.3% |
| quercus-storage | 2,155 / 2,362 | 91.2% |
| quercus-ofx-plugin | 12,143 / 13,514 | 89.9% |
| quercus-timeline | 3,714 / 4,160 | 89.3% |
| quercus-task | 4,176 / 4,911 | 85.0% |
| quercus-audio | 1,493 / 1,757 | 85.0% |
| quercus-engine | 17,914 / 22,486 | 79.7% |
| quercus-engine-rs | 6,957 / 8,199 | 84.9% |
| quercus-ffmpeg-link | 0 / 1 | 0.0% (link-only crate) |

Target from the project coverage plan: **line ≥90%, branch ≥80%**.
Line coverage misses the line target by **0.27 points** on production code
(about 325 lines), or 0.91 points on the raw all-files number.

## Lowest-coverage files

| file | coverage |
|---|---:|
| `quercus-engine/src/editor_commands/library.rs` | 0 / 77 (0.0%) |
| `quercus-engine/src/ofx/gl.rs` | 16 / 98 (16.3%) |
| `quercus-engine/src/timeline.rs` | 141 / 310 (45.5%) |
| `quercus-ofx-plugin/src/gl_bridge.rs` | 139 / 543 (25.6%) |
| `quercus-engine-rs/src/timeline.rs` | 52 / 130 (40.0%) |
| `quercus-codec/src/gpuinterop.rs` | 82 / 181 (45.3%) |
| `quercus-engine/src/ofx/param.rs` | 204 / 376 (54.3%) |
| `quercus-task/src/project/import.rs` | 156 / 274 (56.9%) |
| `quercus-engine/src/colorproc.rs` | 127 / 209 (60.8%) |
| `quercus-engine/src/preview_video/montage.rs` | 229 / 363 (63.1%) |
| `quercus-engine/src/codec/decoder.rs` | 357 / 576 (62.0%) |
| `quercus-engine/src/preview_video.rs` | 221 / 331 (66.8%) |
| `quercus-task/src/project/saveotio.rs` | 190 / 307 (61.9%) |
| `quercus-audio/src/outputdevice.rs` | 98 / 147 (66.7%) |

Notable: `worker/procpool_integration.rs` is a test-only module and shows
6.8%; excluding it raises the production number by ~0.6 points.

## Alternate measurement (cargo-tarpaulin)

```sh
cargo tarpaulin --workspace --out Lcov --output-dir target/coverage -- --test-threads=1
```

Result: **80.21% line coverage** (42,213 / 52,626). This number undercounts:
the engine is exercised through the SDK-linked cdylib, and tarpaulin does not
merge coverage from that dylib into the engine source files, so the entire
ABI dispatch path appears largely uncovered. Use tarpaulin for per-crate
unit-test views, and llvm-cov for the end-to-end number.

## Test isolation finding

Running tarpaulin without serial tests produced three failures in the engine
undo tests:

- `editor_commands::tests::tracks::track_flag_setters_toggle_and_undo`
- `editor_commands::tests::tracks::undo_redo_cycles_converge_to_the_same_state`
- `node::edit_tests::undo_groups_batch_edits`

The process-global undo stack is shared by these tests, so they are only
valid with `--test-threads=1`. The workspace test command and coverage
commands must keep the serial flag. A follow-up should add a test lock (or a
`serial_test`-style guard) so parallel runs are safe.

## Branch coverage

`cargo +nightly llvm-cov --branch` could not complete in this environment:
the engine cdylib is linked by the SDK test binary, and the mixed
stable/nightly/instrumented profile artifacts in `target/llvm-cov-target`
failed `llvm-profdata` merging ("raw profile version mismatch"). This is a
tooling/layout issue, not a test failure. Branch coverage still needs a clean
nightly run (e.g. `cargo llvm-cov clean --workspace` then
`cargo +nightly llvm-cov --branch ...`) on a machine where the cdylib
artifact layout is stable. Historical Oak baseline was ~55% branch; assume
this workspace is in the same range until measured.

## Recommended next steps

1. Raise production line coverage from 89.73% to ≥90% (~325 lines):
   cover `editor_commands/library.rs`, `timeline.rs`, OFX GL/param paths,
   task project import/saveotio, and audio device selection.
2. Re-run branch coverage on a clean nightly target; if below 80%, prioritize
   error branches in the ABI dispatch and codec/decoder paths.
3. Add a shared test lock for the undo tests so parallel `cargo test` is safe.
4. Put the llvm-cov line gate in CI at the current baseline (89%) and ratchet
   toward 90/80; use `--test-threads=1`.
