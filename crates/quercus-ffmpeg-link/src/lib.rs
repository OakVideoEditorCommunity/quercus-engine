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

//! Empty crate whose build script emits the transitive link flags of a
//! static FFmpeg (see build.rs). Depend on it from every crate that uses
//! `ffmpeg-next` so the final binary/test links.

/// Referenced by dependents (`#[used] static … = force_link`) so this
/// rlib is never pruned as unused — the build script's native link flags
/// only reach the final link while the rlib is linked.
pub fn force_link() {}
