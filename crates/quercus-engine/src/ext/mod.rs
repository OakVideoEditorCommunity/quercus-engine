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

//! Caller-supplied extension adapters: the single place in the engine where
//! raw C vtables, `userdata` pointers and callback lifetimes are marshalled.
//!
//! Each submodule owns one extension point. The implementation crates
//! (`quercus-storage`, `quercus-core`, `quercus-codec`, `quercus-render`)
//! only expose safe Rust traits; the `unsafe` translation from the C ABI
//! lives here, next to the C exports that use it. The safe SDK
//! (`quercus-engine-rs`) is the mirror-image boundary on the caller's side.

pub(crate) mod config_source;
pub(crate) mod gpu;
pub(crate) mod log_sink;
pub(crate) mod media_io;
pub(crate) mod storage_transport;
