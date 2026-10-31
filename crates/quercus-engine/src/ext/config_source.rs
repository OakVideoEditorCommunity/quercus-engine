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

//! C config-source adapter: owns the foreign vtable, `userdata` and drop
//! hook, performs the snprintf-style load protocol, and implements the
//! safe [`quercus_core::configstore::ConfigSource`] trait that
//! `quercus-core` sees.

use std::ffi::c_void;
use std::sync::Arc;

use quercus_core::configstore::{self, ConfigSource, ConfigStore};

/// The only supported `QuercusConfigSourceVTable::abi_version`.
pub(crate) const CONFIG_SOURCE_ABI_VERSION: u32 = 1;

/// Host config loader: snprintf-style. Writes at most `cap` bytes of TOML
/// into `buf` (NUL-terminated when `cap > 0`) and returns the full byte
/// length excluding the NUL; a null `buf` with `cap == 0` is a size query,
/// and a negative return is an error.
pub type QuercusConfigLoadFn =
	unsafe extern "C" fn(userdata: *mut c_void, buf: *mut u8, cap: usize) -> i32;

/// Host config saver: receives the TOML text as a borrowed, non-NUL-
/// terminated UTF-8 buffer and returns false on rejection/error.
pub type QuercusConfigSaveFn =
	unsafe extern "C" fn(userdata: *mut c_void, data: *const u8, len: usize) -> bool;

/// Optional host cleanup callback for a config source's `userdata`; runs
/// exactly once when that source is replaced or cleared.
pub type QuercusConfigDropFn = unsafe extern "C" fn(userdata: *mut c_void);

/// The foreign config-source vtable. Field order is the ABI; the SDK's
/// `QuercusConfigSourceVTable` mirror in
/// `quercus-engine-rs/src/state.rs` is byte-identical.
#[repr(C)]
pub struct QuercusConfigSourceVTable {
	/// `sizeof(QuercusConfigSourceVTable)` as compiled by the caller; must
	/// cover the full v1 layout.
	pub struct_size: u32,
	/// Must be `1`.
	pub abi_version: u32,
	/// Loader reading TOML from the host (required).
	pub load: Option<QuercusConfigLoadFn>,
	/// Saver writing TOML back to the host (required).
	pub save: Option<QuercusConfigSaveFn>,
}

/// The installed C config source plus its `userdata` and drop hook.
///
/// `Arc` (held by `quercus_core::configstore`) keeps this alive for the
/// duration of a concurrent load/save while a replacement swaps the slot;
/// `Drop` runs the C drop callback when the last reference goes away.
pub(crate) struct ForeignConfigSource {
	load: QuercusConfigLoadFn,
	save: QuercusConfigSaveFn,
	userdata: *mut c_void,
	drop: Option<QuercusConfigDropFn>,
}

// SAFETY: `userdata` is opaque to the engine; the caller guarantees it is
// valid for every callback and until the drop callback runs. The function
// pointers are C callbacks. Access is serialized by quercus-core's source
// slot, which clones an `Arc` before calling.
unsafe impl Send for ForeignConfigSource {}
unsafe impl Sync for ForeignConfigSource {}

impl ConfigSource for ForeignConfigSource {
	fn load(&self) -> Option<String> {
		// SAFETY: the callback/userdata pair is kept alive by the `Arc`
		// quercus-core holds for this call.
		let mut query = unsafe { (self.load)(self.userdata, std::ptr::null_mut(), 0) };
		for _ in 0..3 {
			if query < 0 {
				return None;
			}
			let size = query as usize;
			// snprintf-style: room for `size` bytes plus a NUL.
			let cap = size.checked_add(1)?;
			if cap > isize::MAX as usize {
				return None;
			}

			let mut buf = vec![0u8; cap];
			// SAFETY: `buf` is writable for `cap` bytes; same liveness as
			// above.
			let read = unsafe { (self.load)(self.userdata, buf.as_mut_ptr(), buf.len()) };
			if read < 0 {
				return None;
			}
			let len = read as usize;
			if len >= cap {
				// The text grew between the query and the buffer call (the
				// buffer keeps one byte for the NUL).
				query = read;
				continue;
			}
			return String::from_utf8(buf[..len].to_vec()).ok();
		}
		None
	}

	fn save(&self, toml: &str) -> bool {
		// SAFETY: `toml` is live for the call and `userdata` stays valid
		// because this adapter is alive.
		unsafe { (self.save)(self.userdata, toml.as_ptr(), toml.len()) }
	}
}

impl Drop for ForeignConfigSource {
	fn drop(&mut self) {
		if let Some(drop) = self.drop {
			// SAFETY: the caller-installed drop callback for this userdata.
			// Rust runs `Drop` at most once, so this runs exactly once.
			unsafe { drop(self.userdata) };
		}
	}
}

/// Install (or replace) the process-wide host config persistence source.
///
/// Returns false for a null vtable, a `struct_size` smaller than the v1
/// layout, an unsupported `abi_version`, or a null `load`/`save` callback;
/// nothing is installed then.
///
/// # Safety
///
/// `vt` must point to a readable [`QuercusConfigSourceVTable`] whose
/// declared `struct_size` covers the full v1 layout. `userdata` is opaque
/// to the engine, owned by the caller, and must stay valid until `drop`
/// runs. All callbacks are `extern "C"` and must not unwind.
pub(crate) unsafe fn install(
	vt: *const QuercusConfigSourceVTable,
	userdata: *mut c_void,
	drop: Option<QuercusConfigDropFn>,
) -> bool {
	if vt.is_null() {
		return false;
	}
	// Read the declared size first: only the prefix the caller declares is
	// guaranteed readable, and the v1 layout must be complete.
	let declared = unsafe { (*vt).struct_size } as usize;
	if declared < std::mem::size_of::<QuercusConfigSourceVTable>() {
		return false;
	}
	// SAFETY: the caller guarantees `declared` readable bytes, which cover
	// our whole layout.
	let vt = unsafe { std::ptr::read(vt) };
	if vt.abi_version != CONFIG_SOURCE_ABI_VERSION {
		return false;
	}
	let (Some(load), Some(save)) = (vt.load, vt.save) else {
		return false;
	};
	configstore::set_config_source(Some(Arc::new(ForeignConfigSource {
		load,
		save,
		userdata,
		drop,
	})));
	true
}

/// Clear the process-wide host config persistence source, restoring the
/// file behavior. Returns true when one was installed; its drop callback
/// runs exactly once.
pub(crate) fn clear() -> bool {
	ConfigStore::instance().remove_source()
}
