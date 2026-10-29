//! C ABI for the OFX plugin host (prefix `quercus_ofx_`).
//!
//! This is the quercus-ofx-plugin surface the app wires up in
//! `quercusui/ofx.rs` — host scan/shutdown, the instance registry,
//! progress reporters, viewer-time/project-extent sync, persistent
//! messages, interacts, and the GL bridge — plus the headless surface
//! a functional plugin (batch processing, AI-assisted editing driving
//! effects) needs: plugin enumeration, instantiation with context
//! selection, parameter introspection/editing, parametric curves, and
//! CPU frame rendering.
//!
//! Conventions (shared with the rest of the engine ABI):
//!
//! - Instance identity uses the process-global `node_factory`
//!   registry: u64 keys, 0 = invalid. Interacts live in an engine-side
//!   registry (u64 keys). Curve sets are refcounted handles
//!   (`quercus_ofx_curveset_add_ref`/`release`).
//! - String arguments are `(ptr, len)` borrows; string outputs are
//!   snprintf-style (`copy_str_out`): null/0 buffer queries the size,
//!   `ret >= buf_len` signals truncation, -1 on invalid input.
//! - All entry points are null-safe and never panic across the
//!   boundary.

pub mod curve;
pub mod gl;
pub mod host;
pub mod interact;
pub mod param;
pub mod render;
