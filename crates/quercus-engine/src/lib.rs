//! # Quercus Nonlinear Video Editing Engine
//!
//! The Quercus engine is a headless nonlinear video editing engine. This crate
//! builds the C ABI dynamic library (`libquercus_engine`) consumed by the
//! [`quercus-engine-rs`](https://crates.io/crates/quercus-engine-rs) SDK and by
//! C/C++ frontends through `include/quercus_engine.h`.
//!
//! The Rust modules in this crate are implementation details: the stable
//! surface is the C ABI. On docs.rs the crate is built with the `docs-only`
//! feature, so this page is intentionally limited; see the SDK documentation
//! for the supported Rust API and the C header for the native API.
//!
//! ```
//! // Rust users should depend on `quercus-engine-rs`, not on this crate.
//! ```

#[cfg(not(feature = "docs-only"))]
pub mod audio;
#[cfg(not(feature = "docs-only"))]
pub mod audio_analysis;
#[cfg(not(feature = "docs-only"))]
pub mod codec;
#[cfg(not(feature = "docs-only"))]
pub mod colorproc;
#[cfg(not(feature = "docs-only"))]
pub mod corestate;
#[cfg(not(feature = "docs-only"))]
pub mod coretypes;
#[cfg(not(feature = "docs-only"))]
pub mod coreutil;
#[cfg(not(feature = "docs-only"))]
pub mod effect;
#[cfg(not(feature = "docs-only"))]
pub mod editor;
#[cfg(not(feature = "docs-only"))]
mod ext;
#[cfg(not(feature = "docs-only"))]
pub mod editor_commands;
#[cfg(not(feature = "docs-only"))]
pub mod editor_dispatch;
#[cfg(not(feature = "docs-only"))]
pub mod editor_graph;
#[cfg(not(feature = "docs-only"))]
pub mod editor_params;
#[cfg(not(feature = "docs-only"))]
pub mod handle;
#[cfg(not(feature = "docs-only"))]
pub mod node;
#[cfg(not(feature = "docs-only"))]
pub mod ofx;
#[cfg(not(feature = "docs-only"))]
pub mod otio;
#[cfg(not(feature = "docs-only"))]
pub mod plugin;
#[cfg(not(feature = "docs-only"))]
pub mod preview_audio;
#[cfg(not(feature = "docs-only"))]
pub mod preview_video;
#[cfg(not(feature = "docs-only"))]
pub mod render;
#[cfg(not(feature = "docs-only"))]
pub mod thumbnail;
#[cfg(not(feature = "docs-only"))]
pub mod storage;
#[cfg(not(feature = "docs-only"))]
pub mod task;
#[cfg(not(feature = "docs-only"))]
pub mod timeline;
#[cfg(not(feature = "docs-only"))]
pub mod undo;
#[cfg(not(feature = "docs-only"))]
pub mod vecs;

#[cfg(not(feature = "docs-only"))]
mod worker;
