> **Internal crate — do not depend on it directly.**
>
> This crate is an implementation detail of `quercus-engine` and is
> published only to satisfy Cargo's dependency resolution. Its API is
> **not stable** and may change in any release without notice. Use
> `quercus-engine-rs` (or the C ABI via `quercus-engine`) instead.

# quercusstorage Rust crate — project persistence

> Status: **implemented** (file backends). Manual:
> docs/zh/plans/riir/M10-quercusstorage.md.

## Scope

Project persistence — the single module that knows where projects come
from and where they are saved to. Backends are pluggable via a manual
vtable; shipping in this pass: `ove-xml` (the XML project format) and
`otio` (the `.otio` / `.fcpxml` interchange, via the native quercusotio
crate). The **database** backend (PostgreSQL + SQLite, SeaORM) is
implemented and pre-registered with the other built-ins; it backs the
project library / write-through path. Consumers never branch on backend.

## Architectural decisions

1. **URI dispatch, not file paths.** Every entry point takes a URI:
   `file:///…proj.ove` / `file:///…proj.otio` / `quercusdb://…`. Bare
   paths are normalized to `file://`. The core resolves scheme +
   backend `can_handle` arbitration (M10 §2.3).
2. **Two backend layers.** In-crate backends implement the Rust
   `StorageBackend` trait (`backend.rs`) directly; this crate exposes only
   that safe trait plus the registry. Hosts register their own *byte
   transport* through the C ABI (`quercus_storage_register_transport` /
   `quercus_storage_unregister_transport`); the unsafe vtable adapter that
   marshals the caller's callbacks into the safe trait lives in
   `quercus-engine` (`src/ext/storage_transport.rs`), not here. The
   transport stores opaque engine-serialized project bytes and never
   sees the project graph. Version probing and schema stay in the engine.
   A transport may optionally implement the project-library capability
   (list/delete/rename/snapshot) and a write-policy hint; the engine then
   drives the library and write-through paths through the registry.
3. **The graph (de)serialization itself stays in quercusnode** — every
   backend calls the quercusnode serializer (`quercusnode::serializer::load` /
   `save`) through `bridge::node` (direct Rust calls, single-lib
   unification) to fetch/rebuild the in-memory graph; backends own
   framing: container bytes, schema, versioning (TOO_OLD/TOO_NEW/
   UNKNOWN_VERSION), sessions. `QUERCUSSTORAGE_SAVE_COMPRESS` is accepted
   but not implemented (the quercusnode serializer emits plain XML only).
4. **Database backend shares one logical schema** across PostgreSQL
   and SQLite via **SeaORM**: a private current-thread tokio runtime
   drives the async API behind the synchronous C ABI; the entity set is
   minimal (projects table: id, name, payload blob, version,
   timestamps). The graph payload is the same serialized form the
   ove-xml backend uses — one serialization truth, two containers.
5. **No callbacks/events** (M10: synchronous commands only; the caller
   — quercustask/facade — owns progress reporting).
6. **Errors** follow the project -MMCCCC scheme, module 10
   (`-100001` …); the M10 positive info codes (TOO_OLD/TOO_NEW/…) are
   kept verbatim.
7. **Interchange is lossy.** The otio backend's export/import mapping
   preserves sequences/tracks/clips/gaps/transitions; effect chains,
   keyframes, project bins/settings and exact rational timebases are
   not carried (see the module docs in `backends/otio.rs`).

## Layout

```
src/
  lib.rs          crate doc + module map
  error.rs        error/info codes (M10 §2.1, -MMCCCC module 10)
  handle.rs       shared CHandle re-export (boxes live in quercusnode's handle.rs)
  uri.rs          URI parsing/classification
  session.rs      StorageProject session (open/take/uri)
  registry.rs     backend registry (register/unregister/arbitrate)
  backend.rs      StorageBackend trait + LoadResult
  backends/
    ove_xml.rs    built-in .ove XML backend (via nodeutil)
    otio.rs       built-in .otio/.fcpxml backend (via quercusotio)
    database/     built-in SQLite/PostgreSQL library backend (SeaORM)
  nodeutil.rs     quercusnode calls (project + serializer + sequence builder)
  writethrough.rs library binding, write-through and snapshot thread
tests/            contract tests for the built-in and database backends
```

The caller-supplied C ABI transport adapter (`ForeignBackend` and the
`QuercusStorageTransportVTable` family) lives in `quercus-engine`'s
`ext::storage_transport` module; this crate contains no raw-callback
adapter, only safe traits and backends.

## Build / test

A workspace member (not a default member); build and test explicitly:

```
cargo test -p quercus-storage
```
