//! **Internal implementation detail — do not depend on this crate directly.**
//!
//! It is published only for `quercus-engine`'s dependency graph. Its API is
//! not stable and may change without notice. Use `quercus-engine-rs`, or the
//! C ABI via `quercus-engine`, instead.

// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! # quercusaudio — the audio I/O, processing and synchronization engine (Rust)
//!
//! Reimplements the C++ quercusaudio module behind its frozen C ABI
//! (`include/audio/*.h`). See README.md for the architectural mapping
//! (singleton manager, stateless sync helpers, local value types).
//!
//! ## Structure
//!
//! Single-lib unification: the module crates are called directly from the
//! quercusengine facade (`crates/quercusengine`), which owns the C-ABI shims and
//! boxes the singleton [`manager::ManagerInner`] behind its own `CHandle`.
//! Shared state lives behind `Mutex`.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod levelmeter;
pub mod manager;
pub mod params;
pub mod outputdevice;
pub mod previewdevice;
pub mod processor;
pub mod synchronizer;
pub mod waveform;
pub mod waveformsync;

// Keep the quercusffmpeg-link rlib referenced so its build script's native
// link flags (the static FFmpeg's transitive dependencies) reach the
// final link — rustc prunes the flags of an unreferenced rlib.
#[used]
static FORCE_FFMPEG_LINK: fn() = quercus_ffmpeg_link::force_link;
