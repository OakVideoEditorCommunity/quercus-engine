> **Internal crate — do not depend on it directly.**
>
> This crate is an implementation detail of `quercus-engine` and is
> published only to satisfy Cargo's dependency resolution. Its API is
> **not stable** and may change in any release without notice. Use
> `quercus-engine-rs` (or the C ABI via `quercus-engine`) instead.

# quercuscore-rs — Rust value-type foundation for the quercus Rust modules

> Status: **declaration draft for review** (no implementation, not wired
> into any build). Companion to `crates/quercusnode` and `crates/quercusrender`.

Rust reimplementation of the quercuscore C++ value types that cross every
module boundary: `Rational`, `TimeRange`, `TimeRangeList`, pixel/sample
format enums. These are pure data types with value semantics — the one
place where duplicating the C++ layout discipline in plain Rust is both
safe and required (a Rust crate cannot hold C++ objects by value).

Rules:

- Bit-exact arithmetic compatibility with `olive::core::Rational`
  (reduction, overflow behavior, comparison) — the golden rule is the
  C++ test-suite semantics, not "ideal" rational math.
- `#[repr(C)]` only where a type crosses the C ABI; everything else is
  plain Rust with `Copy + Clone + Eq + Hash`.
- No I/O, no allocation in arithmetic paths, no panics on degenerate
  input (denominator zero follows the C++ sentinel semantics).
