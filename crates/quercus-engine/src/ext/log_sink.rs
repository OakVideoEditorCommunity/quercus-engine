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

//! C log-sink adapter: owns the foreign callback, `userdata` and drop
//! hook, and implements the safe [`quercus_core::debug::LogSink`] trait
//! that `quercus-core` sees.

use std::ffi::c_void;
use std::sync::Arc;

use quercus_core::debug::{self, LogSink};

/// Raw C log-sink callback (`quercus_core_log_set_sink`).
///
/// `level` is a Quercus level code (0 = DEBUG … 4 = FATAL).
/// `message`/`len` describe a borrowed, non-NUL-terminated UTF-8 line
/// formatted as `"[LEVEL] message"`, valid only for the duration of the
/// call. `userdata` is the pointer passed to the install function.
pub type QuercusLogFn =
	Option<unsafe extern "C" fn(level: i32, message: *const u8, len: usize, userdata: *mut c_void)>;

/// Optional raw C cleanup callback invoked with the sink's `userdata` when
/// that sink is replaced or cleared (exactly once per installed sink).
pub type QuercusLogDropFn = Option<unsafe extern "C" fn(userdata: *mut c_void)>;

/// The installed C sink plus its `userdata` and drop hook.
///
/// `Arc` (held by `quercus_core::debug`) keeps this alive for the duration
/// of a concurrent log call while a replacement swaps the slot; `Drop`
/// runs the C drop callback when the last reference goes away.
pub(crate) struct ForeignLogSink {
	cb: unsafe extern "C" fn(level: i32, message: *const u8, len: usize, userdata: *mut c_void),
	userdata: *mut c_void,
	drop: QuercusLogDropFn,
}

// SAFETY: `userdata` is opaque to the engine; the caller guarantees it is
// valid for every sink call and until the drop callback runs. `cb` is a C
// function pointer, valid for the process lifetime. Access is serialized
// by quercus-core's sink slot, which clones an `Arc` before calling.
unsafe impl Send for ForeignLogSink {}
unsafe impl Sync for ForeignLogSink {}

impl LogSink for ForeignLogSink {
	fn log(&self, level: i32, message: &str) {
		let bytes = message.as_bytes();
		// SAFETY: the host callback contract says `message` is readable for
		// `len` bytes and valid for the duration of the call; `userdata`
		// stays valid because this adapter is alive.
		unsafe { (self.cb)(level, bytes.as_ptr(), bytes.len(), self.userdata) };
	}
}

impl Drop for ForeignLogSink {
	fn drop(&mut self) {
		if let Some(drop) = self.drop {
			// SAFETY: the caller-installed drop callback for this userdata.
			// Rust runs `Drop` at most once, so this runs exactly once.
			unsafe { drop(self.userdata) };
		}
	}
}

/// Install a C log sink, replacing any previous one.
pub(crate) fn install(
	cb: unsafe extern "C" fn(level: i32, message: *const u8, len: usize, userdata: *mut c_void),
	userdata: *mut c_void,
	drop: QuercusLogDropFn,
) {
	debug::set_log_sink(Some(Arc::new(ForeignLogSink { cb, userdata, drop })));
}

/// Clear the installed log sink, restoring the stderr fallback.
pub(crate) fn clear() {
	debug::set_log_sink(None);
}
