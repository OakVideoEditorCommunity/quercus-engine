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

//! Foreign (C ABI) byte transports: adapts a caller-provided
//! [`QuercusStorageTransportVTable`] to the internal
//! [`StorageBackend`] trait, so a host can serve project bytes from its
//! own storage (S3, HTTP, a Postgres blob column, …) without touching
//! engine internals. The backend is a *transport* only: the engine owns
//! serialization and schema, and hands the backend opaque UTF-8 project
//! bytes.
//!
//! # ABI
//! [`QuercusStorageTransportVTable`] is the ABI: field order matters and
//! the SDK mirror in `quercus-engine-rs/src/storage.rs` mirrors this
//! layout field-for-field; it may trail this definition, because the
//! appended project-library slots are optional (see the prefix rule
//! below). [`ForeignBackend::new`] copies the vtable and the
//! `name`/`scheme` strings, so the caller's vtable and strings need not
//! outlive registration; the opaque `userdata` stays owned by the caller
//! and must outlive unregistration (see [`ForeignBackend::wait_idle`]).
//!
//! The project-library callbacks were appended *after* `save`, so a
//! caller compiled against the original vtable (whose `struct_size` ends
//! after `save`) still registers: [`ForeignBackend::new`] accepts any
//! `struct_size` reaching [`QuercusStorageTransportVTable::V1_PREFIX_SIZE`],
//! copies only the declared bytes into a zeroed vtable, and leaves the
//! library slots `None` (the backend then reports `supports_library() ==
//! false`).
//!
//! # Project-library ownership rules
//! `library_list` is a two-phase call. First the engine passes a NULL
//! output array with `cap == 0`; the transport stores the row count in
//! `out_count`. The engine then allocates exactly `count`
//! [`QuercusStorageProjectInfo`] PODs and calls again with that array and
//! its capacity, and the transport fills at most `cap` entries and stores
//! the number written in `out_count`. Reporting more entries than `cap`
//! is a contract violation (`E_INVALID`). The array is **caller-owned**
//! (the engine allocates and frees it); there is no free callback for it,
//! and the engine never keeps any pointer the transport hands back. All
//! strings are inline fixed-size arrays holding NUL-terminated UTF-8,
//! copied by the transport into the POD — the engine validates the
//! terminator and the encoding.
//!
//! # Threading
//! The engine may call `can_handle`/`load`/`save` from several threads
//! (the caller's thread, the undo observer, the snapshot worker). Every
//! callback runs while holding a per-backend *gate* (an `RwLock` read
//! guard); [`ForeignBackend::wait_idle`] takes the same lock in write
//! mode. `load`/`save` are additionally serialized by a per-backend op
//! mutex, so one transport never sees two transfers at once; the library
//! callbacks take the same mutex (their two phases stay one atomic
//! operation). `can_handle` is only gated: the transport contract requires
//! it to be cheap, I/O-free and thread-safe.

use std::ffi::{c_char, c_void, CStr};
use std::sync::{Mutex, RwLock};

use crate::backend::{
	LibraryProjectInfo, LoadResult, StorageBackend, WRITE_POLICY_COALESCED, WRITE_POLICY_DEFAULT,
};
use crate::error::{
	Error, QUERCUSSTORAGE_OK, QUERCUSSTORAGE_TOO_NEW, QUERCUSSTORAGE_TOO_OLD,
	QUERCUSSTORAGE_UNKNOWN_VERSION, Result,
};
use crate::handle::CHandle;
use crate::nodeutil;
use crate::uri::StorageUri;

/// Current transport vtable ABI version (`abi_version` field).
pub const QUERCUS_STORAGE_TRANSPORT_VERSION: u32 = 1;

/// `out_format` value: auto-detect (the engine's document codec decides).
pub const QUERCUS_STORAGE_FORMAT_AUTO: i32 = 0;
/// `out_format` value: the bytes are the engine's `.ove` XML document.
pub const QUERCUS_STORAGE_FORMAT_OVE_XML: i32 = 1;

/// Failure detail a foreign transport fills in; the engine copies the
/// message and never keeps the pointer.
///
/// `code` is a `QUERCUSSTORAGE_E_*` value; `message` is a best-effort
/// UTF-8, NUL-terminated diagnostic (truncated to fit).
#[repr(C)]
pub struct QuercusStorageError {
	/// `QUERCUSSTORAGE_E_*` code (0 = failed without a specific code).
	pub code: i32,
	/// NUL-terminated UTF-8 diagnostic.
	pub message: [u8; 512],
}

impl Default for QuercusStorageError {
	fn default() -> Self {
		QuercusStorageError {
			code: 0,
			message: [0; 512],
		}
	}
}

impl QuercusStorageError {
	/// Replace the message with `text`, truncated to fit and
	/// NUL-terminated. Never splits a UTF-8 code point.
	pub fn set_message(&mut self, text: &str) {
		self.message = [0; 512];
		// Reserve one byte for the NUL terminator.
		let max = self.message.len() - 1;
		let mut end = text.len().min(max);
		while !text.is_char_boundary(end) {
			end -= 1;
		}
		self.message[..end].copy_from_slice(&text.as_bytes()[..end]);
	}

	/// The message as a string slice (up to the first NUL byte).
	pub fn message(&self) -> &str {
		let end = self
			.message
			.iter()
			.position(|&b| b == 0)
			.unwrap_or(self.message.len());
		std::str::from_utf8(&self.message[..end]).unwrap_or("")
	}
}

/// One project-library row as the foreign `library_list` callback returns
/// it: a plain POD with inline, NUL-terminated UTF-8 strings.
///
/// This is the C ABI shape of [`LibraryProjectInfo`]; unix-second
/// timestamps replace the database backend's naive row times. The engine
/// allocates the array of these values ([`ForeignBackend::library_list`]),
/// the transport fills it, and the engine frees it — no allocator
/// crosses the ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QuercusStorageProjectInfo {
	/// Project uuid, NUL-terminated UTF-8 (maximum 39 bytes).
	pub uuid: [u8; 40],
	/// Display name, NUL-terminated UTF-8 (maximum 255 bytes).
	pub name: [u8; 256],
	/// Row creation time (Unix seconds, UTC).
	pub created_at: i64,
	/// Last-write time (Unix seconds, UTC).
	pub modified_at: i64,
	/// Payload schema version (the serializer's current version).
	pub schema_ver: i32,
	/// Reserved for future ABI additions; the engine ignores it and the
	/// transport must zero it.
	pub reserved: i32,
}

impl Default for QuercusStorageProjectInfo {
	fn default() -> Self {
		QuercusStorageProjectInfo {
			uuid: [0; 40],
			name: [0; 256],
			created_at: 0,
			modified_at: 0,
			schema_ver: 0,
			reserved: 0,
		}
	}
}

/// The foreign transport vtable. Field order is the ABI.
///
/// Callbacks may be invoked from several engine threads; `load`/`save`
/// for one backend are serialized by the engine. Return 0 for success,
/// a negative `QUERCUSSTORAGE_E_*` code for failure and fill `err`.
#[repr(C)]
pub struct QuercusStorageTransportVTable {
	/// `sizeof(QuercusStorageTransportVTable)` as compiled by the caller.
	pub struct_size: u32,
	/// Must be [`QUERCUS_STORAGE_TRANSPORT_VERSION`].
	pub abi_version: u32,
	/// Backend name (unique, non-empty; copied by the engine).
	pub name: *const c_char,
	/// Primary URI scheme (copied by the engine; empty = `can_handle`
	/// only).
	pub scheme: *const c_char,
	/// Write-policy hint: 0 = engine default, 1 = immediate,
	/// 2 = coalesced, 3 = explicit. Must be 0..=3; it is surfaced through
	/// [`StorageBackend::write_policy`] with 0 mapped to coalesced.
	pub write_policy: u32,
	// ABI: order matters — keep byte-identical with the SDK mirror.
	/// Claim callback: 1 = claims, 0 = declines, <0 = error (fills `err`).
	/// `None` falls back to scheme equality.
	pub can_handle:
		Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageError) -> i32>,
	/// Load callback: success (0) hands `out_bytes`/`out_len` to the
	/// engine, which frees them with `free_bytes` exactly once.
	pub load: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*mut *mut u8,
			*mut usize,
			*mut i32,
			*mut QuercusStorageError,
		) -> i32,
	>,
	/// Frees a buffer returned by `load` (cross-allocator safe). The
	/// engine calls it exactly once per `load`, even when the callback
	/// failed without handing a buffer back, so it must tolerate a null
	/// pointer.
	pub free_bytes: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize)>,
	/// Save callback: `bytes` are engine-serialized project bytes valid
	/// only for the call duration.
	pub save: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			u32,
			*mut QuercusStorageError,
		) -> i32,
	>,
	// ABI: the project-library slots were appended after `save`; an old
	// caller's `struct_size` ends before them and they stay `None`.
	/// Library list callback (two-phase: NULL/0 asks for the row count).
	pub library_list: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*mut QuercusStorageProjectInfo,
			usize,
			*mut usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	/// Library delete callback: remove the row `uuid` from the library at
	/// the URI.
	pub library_delete: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	/// Library rename callback: rename the row `uuid` to `name`.
	pub library_rename: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	/// Library snapshot callback: snapshot (compact / version) the row
	/// `uuid`.
	pub library_snapshot: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
}

impl QuercusStorageTransportVTable {
	/// Byte size of the original transport ABI: everything through the
	/// `save` callback, before the project-library slots were appended.
	///
	/// [`ForeignBackend::new`] accepts any `struct_size` reaching this
	/// prefix (an old caller's memory ends here), so it is also the
	/// compatibility floor for the compiled SDK.
	pub const V1_PREFIX_SIZE: usize = std::mem::offset_of!(QuercusStorageTransportVTable, save)
		+ std::mem::size_of::<
			Option<
				unsafe extern "C" fn(
					*mut c_void,
					*const u8,
					usize,
					*const u8,
					usize,
					u32,
					*mut QuercusStorageError,
				) -> i32,
			>,
		>();
}

/// The internal [`StorageBackend`] adapter around a foreign vtable.
pub struct ForeignBackend {
	name: String,
	scheme: String,
	write_policy: u32,
	can_handle: Option<
		unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageError) -> i32,
	>,
	load: unsafe extern "C" fn(
		*mut c_void,
		*const u8,
		usize,
		*mut *mut u8,
		*mut usize,
		*mut i32,
		*mut QuercusStorageError,
	) -> i32,
	free_bytes: unsafe extern "C" fn(*mut c_void, *mut u8, usize),
	save: unsafe extern "C" fn(
		*mut c_void,
		*const u8,
		usize,
		*const u8,
		usize,
		u32,
		*mut QuercusStorageError,
	) -> i32,
	library_list: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*mut QuercusStorageProjectInfo,
			usize,
			*mut usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	library_delete: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	library_rename: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	library_snapshot: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*const u8,
			usize,
			*mut QuercusStorageError,
		) -> i32,
	>,
	userdata: *mut c_void,
	op_lock: Mutex<()>,
	gate: RwLock<()>,
}

// SAFETY: the caller's vtable contract requires the transport (and its
// `userdata`) to be thread-safe; the engine serializes `load`/`save` with
// `op_lock` and holds the gate read lock around every callback, so no two
// callbacks touch `userdata` through those paths concurrently.
unsafe impl Send for ForeignBackend {}
// SAFETY: see the `Send` impl above; `can_handle` may run concurrently
// with other `can_handle` calls, which the contract requires to be safe.
unsafe impl Sync for ForeignBackend {}

/// Copies a NUL-terminated C string; a null pointer is the empty string
/// and invalid UTF-8 is rejected.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated C string.
unsafe fn copy_c_string(ptr: *const c_char) -> Result<String> {
	if ptr.is_null() {
		return Ok(String::new());
	}
	// SAFETY: the caller guarantees a valid NUL-terminated string.
	unsafe { CStr::from_ptr(ptr) }
		.to_str()
		.map(str::to_owned)
		.map_err(|_| Error::Invalid)
}

/// Copies a fixed-size inline C string field: it must be NUL-terminated
/// and valid UTF-8. A missing terminator means the transport overflowed or
/// mis-filled the field, so both failures are format errors.
fn pod_string<const N: usize>(bytes: &[u8; N], field: &str) -> Result<String> {
	let end = bytes.iter().position(|&b| b == 0).ok_or_else(|| {
		Error::Format(format!("foreign library row {field} is not NUL-terminated"))
	})?;
	std::str::from_utf8(&bytes[..end])
		.map(str::to_owned)
		.map_err(|_| Error::Format(format!("foreign library row {field} is not valid UTF-8")))
}

/// Map one POD row to the backend-neutral library row. `reserved` is an
/// ABI placeholder and deliberately ignored.
fn project_info_from_pod(pod: &QuercusStorageProjectInfo) -> Result<LibraryProjectInfo> {
	Ok(LibraryProjectInfo {
		uuid: pod_string(&pod.uuid, "uuid")?,
		name: pod_string(&pod.name, "name")?,
		schema_ver: pod.schema_ver,
		created_at: pod.created_at,
		modified_at: pod.modified_at,
	})
}

impl ForeignBackend {
	/// Wraps a caller-provided vtable as an engine [`StorageBackend`].
	///
	/// Validates the ABI (non-null vtable, `struct_size` reaching the
	/// original v1 prefix through `save`, `abi_version`), the required
	/// callbacks (`load`, `save`, `free_bytes`), the `write_policy`, and
	/// the name/scheme: the name must be non-empty and UTF-8, the scheme
	/// must not be reserved ([`is_reserved_scheme`]).
	/// The vtable may end after `save` (an older caller): only the
	/// declared prefix is read and the library slots default to `None`.
	/// The name and scheme are copied; the vtable need not outlive this
	/// call.
	///
	/// # Safety
	/// `vtable` must point to a readable `QuercusStorageTransportVTable`
	/// whose `struct_size` field is valid, and `name`/`scheme` (when
	/// non-null) must be valid NUL-terminated strings. `userdata` is opaque
	/// to the engine, owned by the caller, and must stay alive and
	/// thread-safe until the backend has been unregistered and
	/// [`ForeignBackend::wait_idle`] has returned.
	pub unsafe fn new(
		vtable: *const QuercusStorageTransportVTable,
		userdata: *mut c_void,
	) -> Result<Self> {
		if vtable.is_null() {
			return Err(Error::Invalid);
		}
		// Read the size field first: only the prefix the caller declares
		// is readable. The original ABI (through `save`) is the
		// compatibility floor; library slots appended later may be absent.
		let struct_size = (unsafe { (*vtable).struct_size }) as usize;
		if struct_size < QuercusStorageTransportVTable::V1_PREFIX_SIZE {
			return Err(Error::Invalid);
		}
		// Copy exactly what the caller declares, capped at our definition:
		// an old caller's memory ends after `save`, so its library slots
		// stay `None` and the backend reports no library support.
		// SAFETY: every field is an integer, raw pointer or optional `fn`
		// pointer, so an all-zero bit pattern is a valid value (null
		// pointers, `None` slots, zero versions).
		let mut vt: QuercusStorageTransportVTable = unsafe { std::mem::zeroed() };
		let copy_len = struct_size.min(std::mem::size_of::<QuercusStorageTransportVTable>());
		// SAFETY: the caller guarantees `struct_size` readable bytes at
		// `vtable`; `copy_len` never exceeds that or our own layout, and
		// the zeroed destination is large enough.
		unsafe {
			std::ptr::copy_nonoverlapping(
				vtable as *const u8,
				&mut vt as *mut QuercusStorageTransportVTable as *mut u8,
				copy_len,
			);
		}
		if vt.abi_version != QUERCUS_STORAGE_TRANSPORT_VERSION {
			return Err(Error::Invalid);
		}
		if vt.write_policy > 3 {
			return Err(Error::Invalid);
		}
		let name = unsafe { copy_c_string(vt.name) }?;
		if name.is_empty() {
			return Err(Error::Invalid);
		}
		let scheme = unsafe { copy_c_string(vt.scheme) }?;
		if !scheme.is_empty() && is_reserved_scheme(&scheme) {
			return Err(Error::Invalid);
		}
		let (Some(load), Some(free_bytes), Some(save)) = (vt.load, vt.free_bytes, vt.save) else {
			return Err(Error::Invalid);
		};
		Ok(ForeignBackend {
			name,
			scheme,
			write_policy: vt.write_policy,
			can_handle: vt.can_handle,
			load,
			free_bytes,
			save,
			library_list: vt.library_list,
			library_delete: vt.library_delete,
			library_rename: vt.library_rename,
			library_snapshot: vt.library_snapshot,
			userdata,
			op_lock: Mutex::new(()),
			gate: RwLock::new(()),
		})
	}

	/// The effective write policy of this transport: the caller's vtable
	/// hint, with 0 (engine default) mapping to
	/// [`WRITE_POLICY_COALESCED`] — a remote byte transport cannot promise
	/// synchronous local durability. Hints 1..=3 pass through unchanged
	/// (validated at registration).
	pub fn write_policy(&self) -> u32 {
		if self.write_policy == WRITE_POLICY_DEFAULT {
			WRITE_POLICY_COALESCED
		} else {
			self.write_policy
		}
	}

	/// Block until no foreign callback is in flight, then return.
	///
	/// The C ABI's `quercus_storage_unregister_transport` removes the
	/// backend from the registry (no new resolutions can pick it) and then
	/// calls this, so the caller may free its `userdata` once unregister
	/// returns. This may block for as long as one in-flight transfer
	/// takes — a slow network `load`/`save` delays unregistration. A
	/// callback must not call back into `quercus_storage_*`: the gate is
	/// already held, and a re-entrant `unregister` would deadlock.
	pub fn wait_idle(&self) {
		// A poisoned gate only means a callback panicked on another
		// thread; the transport is still callable (the contract is
		// C-facing, and the SDK trampolines catch panics themselves).
		let _write = self.gate.write().unwrap_or_else(|e| e.into_inner());
	}

	/// Acquire the callback gate (read side).
	fn enter(&self) -> std::sync::RwLockReadGuard<'_, ()> {
		self.gate.read().unwrap_or_else(|e| e.into_inner())
	}

	/// Acquire the per-backend load/save serialization lock.
	fn lock_ops(&self) -> std::sync::MutexGuard<'_, ()> {
		self.op_lock.lock().unwrap_or_else(|e| e.into_inner())
	}
}

impl StorageBackend for ForeignBackend {
	fn name(&self) -> &str {
		&self.name
	}

	fn uri_scheme(&self) -> &str {
		&self.scheme
	}

	fn can_handle(&self, uri: &StorageUri) -> bool {
		// Defence in depth: even if a backend declares a non-reserved
		// scheme, a broad callback must not shadow `file` / `quercusdb`.
		if is_reserved_scheme(&uri.scheme) {
			return false;
		}
		let Some(can_handle) = self.can_handle else {
			return !self.scheme.is_empty() && uri.scheme.eq_ignore_ascii_case(&self.scheme);
		};
		let text = uri.to_uri_string();
		let mut err = QuercusStorageError::default();
		let _gate = self.enter();
		let rc = unsafe {
			can_handle(
				self.userdata,
				text.as_ptr(),
				text.len(),
				&mut err as *mut QuercusStorageError,
			)
		};
		// A negative return is an error the engine cannot act on (the
		// registry only asks "do you claim this URI?"): treat it as a
		// decline rather than panicking or claiming the URI.
		rc > 0
	}

	fn load(&self, uri: &StorageUri) -> Result<LoadResult> {
		// Hold the gate for the whole call (callback + parse + free) so
		// `wait_idle` cannot return while this call still touches
		// `userdata`. `free_bytes` also runs under the op lock.
		let _gate = self.enter();
		let _ops = self.lock_ops();
		let text = uri.to_uri_string();
		let mut bytes: *mut u8 = std::ptr::null_mut();
		let mut len = 0usize;
		let mut format = 0i32;
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			(self.load)(
				self.userdata,
				text.as_ptr(),
				text.len(),
				&mut bytes,
				&mut len,
				&mut format,
				&mut err as *mut QuercusStorageError,
			)
		};
		// From here every exit path must release the transport buffer
		// exactly once: rc != 0, an invalid length/format, non-UTF-8
		// bytes, and a serializer/parse failure all still own `bytes`.
		let _free = FreeBytesGuard::new(self.free_bytes, self.userdata, bytes, len);
		if rc != 0 {
			return Err(map_backend_error(&err));
		}
		if bytes.is_null() {
			return Err(Error::NoMem);
		}
		if len > isize::MAX as usize {
			return Err(Error::Format(
				"foreign transport returned an oversized buffer".to_string(),
			));
		}
		if format != QUERCUS_STORAGE_FORMAT_AUTO && format != QUERCUS_STORAGE_FORMAT_OVE_XML {
			return Err(Error::Format(format!(
				"foreign transport returned unsupported payload format {format}"
			)));
		}
		// SAFETY: non-null, and the transport contract promises `len`
		// readable bytes.
		let payload = unsafe { std::slice::from_raw_parts(bytes, len) };
		let xml = std::str::from_utf8(payload)
			.map_err(|_| Error::Format("foreign transport returned non-UTF-8 bytes".to_string()))?;
		// The engine owns the version ladder (P0-A moved the probe into the
		// serializer), so a transport stays a pure byte pipe: a too-new or
		// unrecognized document is reported as an info code, not an error.
		let probe = quercus_node::serializer::probe_version(xml).ok_or_else(|| {
			Error::Format("foreign transport returned an unparseable project document".to_string())
		})?;
		use quercus_node::serializer::VersionProbe;
		match probe {
			VersionProbe::TooNew => return Ok(LoadResult::info_only(QUERCUSSTORAGE_TOO_NEW)),
			VersionProbe::Unknown => {
				return Ok(LoadResult::info_only(QUERCUSSTORAGE_UNKNOWN_VERSION));
			}
			VersionProbe::Current | VersionProbe::Old => {}
		}
		let project = nodeutil::serializer_load(xml)?;
		let version_info = if matches!(probe, VersionProbe::Old) {
			QUERCUSSTORAGE_TOO_OLD
		} else {
			QUERCUSSTORAGE_OK
		};
		Ok(LoadResult::with_info(
			nodeutil::make_project_owned(project),
			version_info,
		))
	}

	fn save(&self, project: CHandle, uri: &StorageUri, options: u32) -> Result<()> {
		// Gate first: an unregister that starts after this point must wait
		// for the whole call, not just for the callback below.
		let _gate = self.enter();
		let project = unsafe { nodeutil::project_arc(&project)? };
		let xml = {
			let guard = project.lock().map_err(|_| Error::State)?;
			nodeutil::serializer_save(&guard)?
		};
		let text = uri.to_uri_string();
		let mut err = QuercusStorageError::default();
		let rc = {
			let _ops = self.lock_ops();
			unsafe {
				(self.save)(
					self.userdata,
					text.as_ptr(),
					text.len(),
					xml.as_ptr(),
					xml.len(),
					options,
					&mut err as *mut QuercusStorageError,
				)
			}
		};
		if rc == 0 {
			Ok(())
		} else {
			Err(map_backend_error(&err))
		}
	}

	fn supports_library(&self) -> bool {
		// The list callback is the capability gate; delete/rename/snapshot
		// are individually optional (their slots default to Unsupported).
		self.library_list.is_some()
	}

	fn library_list(&self, uri: &StorageUri) -> Result<Vec<LibraryProjectInfo>> {
		let Some(list) = self.library_list else {
			return Err(Error::Unsupported);
		};
		let _gate = self.enter();
		let _ops = self.lock_ops();
		let text = uri.to_uri_string();
		// Phase 1: NULL/0 asks for the row count. The count is expected to
		// exceed `cap` here — that is the documented contract, not an
		// overflow.
		let mut count = 0usize;
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			list(
				self.userdata,
				text.as_ptr(),
				text.len(),
				std::ptr::null_mut(),
				0,
				&mut count,
				&mut err as *mut QuercusStorageError,
			)
		};
		if rc != 0 {
			return Err(map_backend_error(&err));
		}
		if count == 0 {
			return Ok(Vec::new());
		}
		if count > isize::MAX as usize / std::mem::size_of::<QuercusStorageProjectInfo>() {
			return Err(Error::Invalid);
		}
		// Phase 2: fill a caller-owned POD array and validate what came
		// back. The array is allocated here and dropped here; no free
		// callback crosses the ABI.
		let mut rows: Vec<QuercusStorageProjectInfo> = Vec::new();
		rows.try_reserve_exact(count).map_err(|_| Error::NoMem)?;
		rows.resize(count, QuercusStorageProjectInfo::default());
		let mut written = 0usize;
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			list(
				self.userdata,
				text.as_ptr(),
				text.len(),
				rows.as_mut_ptr(),
				rows.len(),
				&mut written,
				&mut err as *mut QuercusStorageError,
			)
		};
		if rc != 0 {
			return Err(map_backend_error(&err));
		}
		// A transport that reports more rows than fit in the array it was
		// given broke the two-phase contract: the engine cannot grow its
		// view of a caller-owned array after the fact.
		if written > rows.len() {
			return Err(Error::Invalid);
		}
		rows.truncate(written);
		rows.iter().map(project_info_from_pod).collect()
	}

	fn library_delete(&self, uri: &StorageUri, uuid: &str) -> Result<()> {
		let Some(delete) = self.library_delete else {
			return Err(Error::Unsupported);
		};
		let _gate = self.enter();
		let _ops = self.lock_ops();
		let text = uri.to_uri_string();
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			delete(
				self.userdata,
				text.as_ptr(),
				text.len(),
				uuid.as_ptr(),
				uuid.len(),
				&mut err as *mut QuercusStorageError,
			)
		};
		if rc == 0 {
			Ok(())
		} else {
			Err(map_backend_error(&err))
		}
	}

	fn library_rename(&self, uri: &StorageUri, uuid: &str, name: &str) -> Result<()> {
		let Some(rename) = self.library_rename else {
			return Err(Error::Unsupported);
		};
		let _gate = self.enter();
		let _ops = self.lock_ops();
		let text = uri.to_uri_string();
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			rename(
				self.userdata,
				text.as_ptr(),
				text.len(),
				uuid.as_ptr(),
				uuid.len(),
				name.as_ptr(),
				name.len(),
				&mut err as *mut QuercusStorageError,
			)
		};
		if rc == 0 {
			Ok(())
		} else {
			Err(map_backend_error(&err))
		}
	}

	fn library_snapshot(&self, uri: &StorageUri, uuid: &str) -> Result<()> {
		let Some(snapshot) = self.library_snapshot else {
			return Err(Error::Unsupported);
		};
		let _gate = self.enter();
		let _ops = self.lock_ops();
		let text = uri.to_uri_string();
		let mut err = QuercusStorageError::default();
		let rc = unsafe {
			snapshot(
				self.userdata,
				text.as_ptr(),
				text.len(),
				uuid.as_ptr(),
				uuid.len(),
				&mut err as *mut QuercusStorageError,
			)
		};
		if rc == 0 {
			Ok(())
		} else {
			Err(map_backend_error(&err))
		}
	}

	fn write_policy(&self) -> u32 {
		ForeignBackend::write_policy(self)
	}
}

/// Releases a `load` buffer exactly once (including error paths).
struct FreeBytesGuard {
	free_bytes: unsafe extern "C" fn(*mut c_void, *mut u8, usize),
	userdata: *mut c_void,
	bytes: *mut u8,
	len: usize,
}

impl FreeBytesGuard {
	fn new(
		free_bytes: unsafe extern "C" fn(*mut c_void, *mut u8, usize),
		userdata: *mut c_void,
		bytes: *mut u8,
		len: usize,
	) -> Self {
		FreeBytesGuard {
			free_bytes,
			userdata,
			bytes,
			len,
		}
	}
}

impl Drop for FreeBytesGuard {
	fn drop(&mut self) {
		// Exactly once on every exit path, including transport errors and
		// UTF-8/parse failures. A failed load may hand back a null buffer;
		// the contract requires `free_bytes` to tolerate that.
		// SAFETY: `bytes`/`len` came from this transport's `load` and the
		// contract gives ownership to the engine exactly once.
		unsafe { (self.free_bytes)(self.userdata, self.bytes, self.len) };
	}
}

/// Map a transport error record: code 0 means "failed, no code".
fn map_backend_error(err: &QuercusStorageError) -> Error {
	let message = err.message().to_string();
	if err.code == 0 {
		Error::Failed(message)
	} else {
		Error::Foreign {
			code: err.code,
			message,
		}
	}
}

/// Whether `scheme` is reserved by the built-in backends (`file`,
/// `quercusdb`, and the `quercusdb+*` family). Foreign transports may not
/// declare or claim a reserved scheme: the registry resolves first match,
/// and a broad foreign `can_handle` must never shadow local files or the
/// built-in database backend.
pub fn is_reserved_scheme(scheme: &str) -> bool {
	let scheme = scheme.to_ascii_lowercase();
	scheme == "file" || scheme == "quercusdb" || scheme.starts_with("quercusdb+")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::error::{QUERCUSSTORAGE_E_INVALID, QUERCUSSTORAGE_E_NOT_FOUND, QUERCUSSTORAGE_OK};
	use std::collections::HashMap;
	use std::ffi::CString;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::OnceLock;

	/// The mock backend's process-wide state (the extern callbacks have no
	/// way to receive a Rust context). Tests serialize on [`SERIAL`].
	struct MockState {
		store: Mutex<HashMap<String, Vec<u8>>>,
		frees: AtomicUsize,
		/// Rows the mock `library_list` serves, in order.
		library: Mutex<Vec<MockLibraryRow>>,
		/// `op:uri:uuid[:name]` log of the mock library mutation calls.
		library_calls: Mutex<Vec<String>>,
	}

	/// One row of the mock library (byte strings so a test can serve an
	/// invalid-UTF-8 payload through the POD).
	struct MockLibraryRow {
		uuid: Vec<u8>,
		name: Vec<u8>,
		created_at: i64,
		modified_at: i64,
		schema_ver: i32,
	}

	const ROW_ONE_UUID: &str = "{11111111-1111-4111-8111-111111111111}";
	const ROW_TWO_UUID: &str = "{22222222-2222-4222-8222-222222222222}";

	fn mock() -> &'static MockState {
		static STATE: OnceLock<MockState> = OnceLock::new();
		STATE.get_or_init(|| MockState {
			store: Mutex::new(HashMap::new()),
			frees: AtomicUsize::new(0),
			library: Mutex::new(Vec::new()),
			library_calls: Mutex::new(Vec::new()),
		})
	}

	/// Reset the mock library to two well-formed rows and clear the call
	/// log.
	fn reset_library() {
		let mut library = mock().library.lock().unwrap_or_else(|e| e.into_inner());
		library.clear();
		library.push(MockLibraryRow {
			uuid: ROW_ONE_UUID.as_bytes().to_vec(),
			name: b"First".to_vec(),
			created_at: 1_700_000_000,
			modified_at: 1_700_000_100,
			schema_ver: 1,
		});
		library.push(MockLibraryRow {
			uuid: ROW_TWO_UUID.as_bytes().to_vec(),
			name: b"Second".to_vec(),
			created_at: 1_700_000_200,
			modified_at: 1_700_000_300,
			schema_ver: 1,
		});
		mock()
			.library_calls
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
	}

	/// The engine is process-global and `mock()` is shared; tests in this
	/// module must not interleave.
	static SERIAL: Mutex<()> = Mutex::new(());

	fn serial() -> std::sync::MutexGuard<'static, ()> {
		SERIAL.lock().unwrap_or_else(|e| e.into_inner())
	}

	unsafe fn write_error(err: *mut QuercusStorageError, code: i32, message: &str) {
		let Some(err) = (unsafe { err.as_mut() }) else {
			return;
		};
		err.code = code;
		err.set_message(message);
	}

	unsafe extern "C" fn mock_can_handle(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		_err: *mut QuercusStorageError,
	) -> i32 {
		let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
		std::str::from_utf8(text)
			.map(|uri| i32::from(uri.starts_with("mock://")))
			.unwrap_or(0)
	}

	unsafe extern "C" fn mock_load(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		out_bytes: *mut *mut u8,
		out_len: *mut usize,
		out_format: *mut i32,
		err: *mut QuercusStorageError,
	) -> i32 {
		let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
		let Ok(uri) = std::str::from_utf8(text) else {
			unsafe { write_error(err, QUERCUSSTORAGE_E_INVALID, "non-UTF-8 URI") };
			return QUERCUSSTORAGE_E_INVALID;
		};
		let payload = if uri == "mock://invalid-utf8" {
			vec![0xff, 0xfe]
		} else {
			let store = mock().store.lock().unwrap_or_else(|e| e.into_inner());
			let Some(bytes) = store.get(uri) else {
				unsafe { write_error(err, QUERCUSSTORAGE_E_NOT_FOUND, "mock: project not found") };
				return QUERCUSSTORAGE_E_NOT_FOUND;
			};
			bytes.clone()
		};
		let boxed = payload.into_boxed_slice();
		let len = boxed.len();
		let ptr = Box::into_raw(boxed) as *mut u8;
		unsafe {
			*out_bytes = ptr;
			*out_len = len;
			*out_format = QUERCUS_STORAGE_FORMAT_OVE_XML;
		}
		0
	}

	unsafe extern "C" fn mock_free(_ud: *mut c_void, bytes: *mut u8, len: usize) {
		// The contract calls `free_bytes` exactly once per load, possibly
		// with a null buffer (a failed transport); count every call.
		mock().frees.fetch_add(1, Ordering::SeqCst);
		if bytes.is_null() {
			return;
		}
		let slice = std::ptr::slice_from_raw_parts_mut(bytes, len);
		unsafe { drop(Box::from_raw(slice)) };
	}

	unsafe extern "C" fn mock_save(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		bytes: *const u8,
		bytes_len: usize,
		_options: u32,
		_err: *mut QuercusStorageError,
	) -> i32 {
		let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
		let payload = unsafe { std::slice::from_raw_parts(bytes, bytes_len) };
		let Ok(uri) = std::str::from_utf8(text) else {
			return QUERCUSSTORAGE_E_INVALID;
		};
		mock()
			.store
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(uri.to_string(), payload.to_vec());
		0
	}

	/// Decode one borrowed C byte argument as UTF-8 (None when null or
	/// invalid).
	unsafe fn utf8_arg(ptr: *const u8, len: usize) -> Option<String> {
		if ptr.is_null() {
			return None;
		}
		let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
		std::str::from_utf8(bytes).ok().map(str::to_owned)
	}

	/// Build the POD the mock serves for one row (strings truncated to fit
	/// their NUL-terminated fields).
	fn pod_of(row: &MockLibraryRow) -> QuercusStorageProjectInfo {
		let mut pod = QuercusStorageProjectInfo::default();
		let uuid_len = row.uuid.len().min(pod.uuid.len() - 1);
		pod.uuid[..uuid_len].copy_from_slice(&row.uuid[..uuid_len]);
		let name_len = row.name.len().min(pod.name.len() - 1);
		pod.name[..name_len].copy_from_slice(&row.name[..name_len]);
		pod.created_at = row.created_at;
		pod.modified_at = row.modified_at;
		pod.schema_ver = row.schema_ver;
		pod
	}

	/// Two-phase `library_list`: NULL/0 reports the row count, a non-null
	/// array is filled with at most `cap` rows.
	unsafe extern "C" fn mock_library_list(
		_ud: *mut c_void,
		_uri: *const u8,
		_uri_len: usize,
		out: *mut QuercusStorageProjectInfo,
		cap: usize,
		out_count: *mut usize,
		err: *mut QuercusStorageError,
	) -> i32 {
		let Some(out_count) = (unsafe { out_count.as_mut() }) else {
			unsafe { write_error(err, QUERCUSSTORAGE_E_INVALID, "null out_count") };
			return QUERCUSSTORAGE_E_INVALID;
		};
		let library = mock().library.lock().unwrap_or_else(|e| e.into_inner());
		if out.is_null() || cap == 0 {
			*out_count = library.len();
			return 0;
		}
		let written = library.len().min(cap);
		for (i, row) in library.iter().take(written).enumerate() {
			unsafe { out.add(i).write(pod_of(row)) };
		}
		*out_count = written;
		0
	}

	/// A transport that violates the two-phase contract: the fill phase
	/// reports one more row than the caller's capacity.
	unsafe extern "C" fn mock_library_list_overreport(
		_ud: *mut c_void,
		_uri: *const u8,
		_uri_len: usize,
		out: *mut QuercusStorageProjectInfo,
		cap: usize,
		out_count: *mut usize,
		_err: *mut QuercusStorageError,
	) -> i32 {
		let Some(out_count) = (unsafe { out_count.as_mut() }) else {
			return QUERCUSSTORAGE_E_INVALID;
		};
		*out_count = if out.is_null() || cap == 0 {
			1
		} else {
			cap + 1
		};
		0
	}

	/// A transport that fills the name field without a NUL terminator.
	unsafe extern "C" fn mock_library_list_unterminated(
		_ud: *mut c_void,
		_uri: *const u8,
		_uri_len: usize,
		out: *mut QuercusStorageProjectInfo,
		cap: usize,
		out_count: *mut usize,
		_err: *mut QuercusStorageError,
	) -> i32 {
		let Some(out_count) = (unsafe { out_count.as_mut() }) else {
			return QUERCUSSTORAGE_E_INVALID;
		};
		if out.is_null() || cap == 0 {
			*out_count = 1;
			return 0;
		}
		if cap >= 1 {
			let pod = QuercusStorageProjectInfo {
				name: [b'x'; 256],
				..QuercusStorageProjectInfo::default()
			};
			unsafe { out.write(pod) };
		}
		*out_count = 1;
		0
	}

	unsafe extern "C" fn mock_library_delete(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		err: *mut QuercusStorageError,
	) -> i32 {
		let (Some(uri), Some(uuid)) = (
			unsafe { utf8_arg(uri, uri_len) },
			unsafe { utf8_arg(uuid, uuid_len) },
		) else {
			unsafe { write_error(err, QUERCUSSTORAGE_E_INVALID, "bad delete argument") };
			return QUERCUSSTORAGE_E_INVALID;
		};
		mock()
			.library_calls
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(format!("delete:{uri}:{uuid}"));
		mock()
			.library
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.retain(|row| row.uuid.as_slice() != uuid.as_bytes());
		0
	}

	unsafe extern "C" fn mock_library_rename(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		name: *const u8,
		name_len: usize,
		err: *mut QuercusStorageError,
	) -> i32 {
		let (Some(uri), Some(uuid), Some(name)) = (
			unsafe { utf8_arg(uri, uri_len) },
			unsafe { utf8_arg(uuid, uuid_len) },
			unsafe { utf8_arg(name, name_len) },
		) else {
			unsafe { write_error(err, QUERCUSSTORAGE_E_INVALID, "bad rename argument") };
			return QUERCUSSTORAGE_E_INVALID;
		};
		mock()
			.library_calls
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(format!("rename:{uri}:{uuid}:{name}"));
		let mut library = mock().library.lock().unwrap_or_else(|e| e.into_inner());
		for row in library.iter_mut() {
			if row.uuid.as_slice() == uuid.as_bytes() {
				row.name = name.as_bytes().to_vec();
			}
		}
		0
	}

	unsafe extern "C" fn mock_library_snapshot(
		_ud: *mut c_void,
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		err: *mut QuercusStorageError,
	) -> i32 {
		let (Some(uri), Some(uuid)) = (
			unsafe { utf8_arg(uri, uri_len) },
			unsafe { utf8_arg(uuid, uuid_len) },
		) else {
			unsafe { write_error(err, QUERCUSSTORAGE_E_INVALID, "bad snapshot argument") };
			return QUERCUSSTORAGE_E_INVALID;
		};
		mock()
			.library_calls
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(format!("snapshot:{uri}:{uuid}"));
		0
	}

	fn mock_vtable(
		name: &CString,
		scheme: &CString,
		can_handle: Option<
			unsafe extern "C" fn(
				*mut c_void,
				*const u8,
				usize,
				*mut QuercusStorageError,
			) -> i32,
		>,
	) -> QuercusStorageTransportVTable {
		QuercusStorageTransportVTable {
			struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
			abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
			name: name.as_ptr(),
			scheme: scheme.as_ptr(),
			write_policy: 0,
			can_handle,
			load: Some(mock_load),
			free_bytes: Some(mock_free),
			save: Some(mock_save),
			library_list: None,
			library_delete: None,
			library_rename: None,
			library_snapshot: None,
		}
	}

	fn backend(scheme: &str, can_handle: bool) -> ForeignBackend {
		let name = CString::new(format!("mock-{scheme}")).unwrap();
		let scheme = CString::new(scheme).unwrap();
		let vt = mock_vtable(&name, &scheme, can_handle.then_some(mock_can_handle));
		unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.expect("valid mock vtable")
	}

	/// A backend with all four library slots wired to the mock state.
	fn library_backend(scheme: &str) -> ForeignBackend {
		let name = CString::new(format!("mock-lib-{scheme}")).unwrap();
		let scheme = CString::new(scheme).unwrap();
		let mut vt = mock_vtable(&name, &scheme, Some(mock_can_handle));
		vt.library_list = Some(mock_library_list);
		vt.library_delete = Some(mock_library_delete);
		vt.library_rename = Some(mock_library_rename);
		vt.library_snapshot = Some(mock_library_snapshot);
		unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.expect("valid library vtable")
	}

	#[test]
	fn hashmap_roundtrip_through_the_trait() {
		let _serial = serial();
		let backend = backend("mock", true);
		let project = quercus_node::project::Project::new();
		project
			.lock()
			.unwrap()
			.settings
			.insert("projectname".to_string(), "Foreign Test".to_string());
		let handle = nodeutil::make_project_owned(project);
		let uri = StorageUri::parse("mock://proj/main.ove").unwrap();
		backend.save(handle, &uri, 0).expect("save through transport");
		nodeutil::release_project(handle);

		let result = backend.load(&uri).expect("load through transport");
		assert_eq!(result.version_info, QUERCUSSTORAGE_OK);
		let loaded = unsafe { nodeutil::project_arc(&result.project) }.unwrap();
		nodeutil::release_project(result.project);
		assert_eq!(
			loaded
				.lock()
				.unwrap()
				.settings
				.get("projectname")
				.map(String::as_str),
			Some("Foreign Test")
		);
	}

	#[test]
	fn version_info_is_reported_without_an_error() {
		let _serial = serial();
		let backend = backend("mock", true);
		mock()
			.store
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(
				"mock://too-new.ove".to_string(),
				b"<olive version=\"999999\"></olive>".to_vec(),
			);
		let result = backend
			.load(&StorageUri::parse("mock://too-new.ove").unwrap())
			.expect("too-new is an info outcome, not a failure");
		assert_eq!(result.version_info, QUERCUSSTORAGE_TOO_NEW);
		assert!(result.project.is_null());

		mock()
			.store
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(
				"mock://unknown.ove".to_string(),
				b"<thirdparty></thirdparty>".to_vec(),
			);
		let result = backend
			.load(&StorageUri::parse("mock://unknown.ove").unwrap())
			.expect("unknown root is an info outcome");
		assert_eq!(result.version_info, QUERCUSSTORAGE_UNKNOWN_VERSION);
		assert!(result.project.is_null());
	}

	#[test]
	fn backend_error_message_propagates() {
		let _serial = serial();
		let backend = backend("mock", true);
		let uri = StorageUri::parse("mock://definitely-missing.ove").unwrap();
		let err = backend.load(&uri).expect_err("missing project must fail");
		match err {
			Error::Foreign { code, ref message } => {
				assert_eq!(code, QUERCUSSTORAGE_E_NOT_FOUND);
				assert!(message.contains("not found"), "message: {message}");
			}
			other => panic!("expected Error::Foreign, got {other:?}"),
		}
		assert_eq!(
			backend
				.load(&uri)
				.expect_err("still missing")
				.code(),
			QUERCUSSTORAGE_E_NOT_FOUND
		);
	}

	#[test]
	fn free_bytes_runs_exactly_once_on_utf8_error() {
		let _serial = serial();
		let backend = backend("mock", true);
		let before = mock().frees.load(Ordering::SeqCst);
		let uri = StorageUri::parse("mock://invalid-utf8").unwrap();
		let err = backend.load(&uri).expect_err("invalid UTF-8 must fail");
		assert!(matches!(err, Error::Format(_)), "got {err:?}");
		assert_eq!(
			mock().frees.load(Ordering::SeqCst) - before,
			1,
			"the transport buffer must be released exactly once"
		);
	}

	#[test]
	fn free_bytes_runs_once_on_error_path() {
		let _serial = serial();
		let backend = backend("mock", true);
		let before = mock().frees.load(Ordering::SeqCst);
		let uri = StorageUri::parse("mock://missing-on-error").unwrap();
		assert!(matches!(
			backend.load(&uri),
			Err(Error::Foreign { .. })
		));
		assert_eq!(
			mock().frees.load(Ordering::SeqCst) - before,
			1,
			"free_bytes is called exactly once even when the transport failed"
		);
	}

	#[test]
	fn free_bytes_runs_on_parse_error() {
		let _serial = serial();
		let backend = backend("mock", true);
		// Not a project document at all: `probe_version` has no parseable
		// root, so `load` reports a format error (an unrecognized *root*
		// such as `<nope/>` is an UNKNOWN_VERSION info outcome instead).
		mock()
			.store
			.lock()
			.unwrap()
			.insert("mock://not-a-project".to_string(), b"not xml".to_vec());
		let before = mock().frees.load(Ordering::SeqCst);
		let uri = StorageUri::parse("mock://not-a-project").unwrap();
		assert!(backend.load(&uri).is_err());
		assert_eq!(mock().frees.load(Ordering::SeqCst) - before, 1);
	}

	#[test]
	fn reserved_schemes_are_rejected_and_never_claimed() {
		let _serial = serial();
		assert!(is_reserved_scheme("file"));
		assert!(is_reserved_scheme("quercusdb"));
		assert!(is_reserved_scheme("quercusdb+sqlite"));
		assert!(is_reserved_scheme("QuercusDB+PG"));
		assert!(!is_reserved_scheme("s3"));
		assert!(!is_reserved_scheme("quercusdb2"));

		for scheme in ["file", "quercusdb", "quercusdb+sqlite"] {
			let name = CString::new(format!("reserved-{scheme}")).unwrap();
			let scheme_c = CString::new(scheme).unwrap();
			let vt = mock_vtable(&name, &scheme_c, Some(mock_can_handle));
			assert!(
				matches!(
					unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) },
					Err(Error::Invalid)
				),
				"declared scheme {scheme} must be rejected"
			);
		}

		// Defence in depth: even a valid backend whose broad callback
		// claims everything refuses reserved URIs at resolution time.
		unsafe extern "C" fn claim_everything(
			_ud: *mut c_void,
			_uri: *const u8,
			_uri_len: usize,
			_err: *mut QuercusStorageError,
		) -> i32 {
			1
		}
		let name = CString::new("broad").unwrap();
		let scheme = CString::new("s3").unwrap();
		let vt = mock_vtable(&name, &scheme, Some(claim_everything));
		let backend = unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.unwrap();
		assert!(backend.can_handle(&StorageUri::parse("s3://bucket/x.ove").unwrap()));
		assert!(!backend.can_handle(&StorageUri::parse("file:///tmp/x.ove").unwrap()));
		assert!(!backend.can_handle(&StorageUri::parse("quercusdb+sqlite:///tmp/db").unwrap()));
	}

	#[test]
	fn scheme_fallback_without_can_handle_and_abi_validation() {
		let _serial = serial();
		let backend = backend("mock", false);
		assert!(backend.can_handle(&StorageUri::parse("mock://a").unwrap()));
		assert!(!backend.can_handle(&StorageUri::parse("other://a").unwrap()));

		let name = CString::new("bad-abi").unwrap();
		let scheme = CString::new("mock2").unwrap();
		let mut vt = mock_vtable(&name, &scheme, None);
		vt.abi_version = 99;
		assert!(matches!(
			unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) },
			Err(Error::Invalid)
		));
		vt.abi_version = QUERCUS_STORAGE_TRANSPORT_VERSION;
		// The compatibility floor is the original prefix (through `save`):
		// a vtable that does not even reach it is rejected, while a smaller
		// than current but complete v1 table is accepted (see the prefix
		// test below).
		vt.struct_size = QuercusStorageTransportVTable::V1_PREFIX_SIZE as u32 - 1;
		assert!(matches!(
			unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) },
			Err(Error::Invalid)
		));
	}

	/// The original vtable layout (v1, before the library slots): what a
	/// caller compiled against the old SDK passes. `struct_size` ends
	/// right after `save`; the engine must not read beyond it.
	#[repr(C)]
	struct V1VTable {
		struct_size: u32,
		abi_version: u32,
		name: *const c_char,
		scheme: *const c_char,
		write_policy: u32,
		can_handle: Option<
			unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageError) -> i32,
		>,
		load: Option<
			unsafe extern "C" fn(
				*mut c_void,
				*const u8,
				usize,
				*mut *mut u8,
				*mut usize,
				*mut i32,
				*mut QuercusStorageError,
			) -> i32,
		>,
		free_bytes: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize)>,
		save: Option<
			unsafe extern "C" fn(
				*mut c_void,
				*const u8,
				usize,
				*const u8,
				usize,
				u32,
				*mut QuercusStorageError,
			) -> i32,
		>,
	}

	#[test]
	fn v1_prefix_vtable_registers_without_library_slots() {
		let _serial = serial();
		assert_eq!(
			std::mem::size_of::<V1VTable>(),
			QuercusStorageTransportVTable::V1_PREFIX_SIZE,
			"the v1 struct must end exactly at the library-slot boundary"
		);
		let name = CString::new("mock-v1").unwrap();
		let scheme = CString::new("mockv1").unwrap();
		let vt = V1VTable {
			struct_size: std::mem::size_of::<V1VTable>() as u32,
			abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
			name: name.as_ptr(),
			scheme: scheme.as_ptr(),
			write_policy: 0,
			can_handle: Some(mock_can_handle),
			load: Some(mock_load),
			free_bytes: Some(mock_free),
			save: Some(mock_save),
		};
		// The cast is the ABI boundary: the caller's struct is shorter
		// than the engine's and only `struct_size` bytes are readable.
		let backend = unsafe {
			ForeignBackend::new(
				&vt as *const V1VTable as *const QuercusStorageTransportVTable,
				std::ptr::null_mut(),
			)
		}
		.expect("a v1 vtable must still register");

		assert!(!backend.supports_library());
		assert_eq!(
			StorageBackend::write_policy(&backend),
			WRITE_POLICY_COALESCED,
			"hint 0 maps to coalesced"
		);

		// The copied prefix is still a working load/save transport.
		let project = quercus_node::project::Project::new();
		let handle = nodeutil::make_project_owned(project);
		let uri = StorageUri::parse("mockv1://prefix.ove").unwrap();
		backend.save(handle, &uri, 0).expect("save via v1 vtable");
		nodeutil::release_project(handle);
		let result = backend.load(&uri).expect("load via v1 vtable");
		assert_eq!(result.version_info, QUERCUSSTORAGE_OK);
		nodeutil::release_project(result.project);

		// Library operations are explicitly unsupported, not defaulted.
		assert!(matches!(backend.library_list(&uri), Err(Error::Unsupported)));
		assert!(matches!(
			backend.library_delete(&uri, "{x}"),
			Err(Error::Unsupported)
		));
		assert!(matches!(
			backend.library_rename(&uri, "{x}", "n"),
			Err(Error::Unsupported)
		));
		assert!(matches!(
			backend.library_snapshot(&uri, "{x}"),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn library_slots_roundtrip_through_the_trait() {
		let _serial = serial();
		reset_library();
		let backend = library_backend("mocklib");
		assert!(backend.supports_library());
		let uri = StorageUri::parse("mocklib://library").unwrap();

		let rows = backend.library_list(&uri).expect("library list");
		assert_eq!(rows.len(), 2);
		assert_eq!(
			rows[0],
			LibraryProjectInfo {
				uuid: ROW_ONE_UUID.to_string(),
				name: "First".to_string(),
				schema_ver: 1,
				created_at: 1_700_000_000,
				modified_at: 1_700_000_100,
			}
		);
		assert_eq!(rows[1].uuid, ROW_TWO_UUID);
		assert_eq!(rows[1].name, "Second");

		backend
			.library_delete(&uri, ROW_ONE_UUID)
			.expect("delete through the slot");
		let rows = backend.library_list(&uri).expect("list after delete");
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].uuid, ROW_TWO_UUID);

		backend
			.library_rename(&uri, ROW_TWO_UUID, "Renamed")
			.expect("rename through the slot");
		let rows = backend.library_list(&uri).expect("list after rename");
		assert_eq!(rows[0].name, "Renamed");

		backend
			.library_snapshot(&uri, ROW_TWO_UUID)
			.expect("snapshot through the slot");
		let calls = mock()
			.library_calls
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone();
		assert!(
			calls.iter().any(|c| c == &format!("delete:mocklib://library:{ROW_ONE_UUID}")),
			"delete call log: {calls:?}"
		);
		assert!(
			calls
				.iter()
				.any(|c| c == &format!("rename:mocklib://library:{ROW_TWO_UUID}:Renamed")),
			"rename call log: {calls:?}"
		);
		assert!(
			calls
				.iter()
				.any(|c| c == &format!("snapshot:mocklib://library:{ROW_TWO_UUID}")),
			"snapshot call log: {calls:?}"
		);
	}

	#[test]
	fn library_delete_rename_and_snapshot_are_individually_optional() {
		let _serial = serial();
		reset_library();
		let name = CString::new("mock-list-only").unwrap();
		let scheme = CString::new("mocklistonly").unwrap();
		let mut vt = mock_vtable(&name, &scheme, Some(mock_can_handle));
		vt.library_list = Some(mock_library_list);
		let backend = unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.unwrap();
		let uri = StorageUri::parse("mocklistonly://library").unwrap();
		assert!(backend.supports_library());
		assert_eq!(backend.library_list(&uri).unwrap().len(), 2);
		assert!(matches!(
			backend.library_delete(&uri, "{x}"),
			Err(Error::Unsupported)
		));
		assert!(matches!(
			backend.library_rename(&uri, "{x}", "n"),
			Err(Error::Unsupported)
		));
		assert!(matches!(
			backend.library_snapshot(&uri, "{x}"),
			Err(Error::Unsupported)
		));
	}

	#[test]
	fn library_list_rejects_malformed_pod_strings() {
		let _serial = serial();
		reset_library();
		let backend = library_backend("mockutf8");
		let uri = StorageUri::parse("mockutf8://library").unwrap();
		{
			let mut library = mock().library.lock().unwrap_or_else(|e| e.into_inner());
			library.clear();
			// Invalid UTF-8 in a NUL-terminated name field.
			library.push(MockLibraryRow {
				uuid: b"{bad-utf8}".to_vec(),
				name: vec![0xff, 0x00],
				created_at: 0,
				modified_at: 0,
				schema_ver: 1,
			});
		}
		assert!(
			matches!(backend.library_list(&uri), Err(Error::Format(_))),
			"an invalid UTF-8 name must be a format error"
		);

		// A full-width field without a NUL terminator is malformed too
		// (the transport overflowed or mis-filled it).
		let name = CString::new("mock-unterminated").unwrap();
		let scheme = CString::new("mockunterminated").unwrap();
		let mut vt = mock_vtable(&name, &scheme, Some(mock_can_handle));
		vt.library_list = Some(mock_library_list_unterminated);
		let backend = unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.unwrap();
		let uri = StorageUri::parse("mockunterminated://library").unwrap();
		assert!(
			matches!(backend.library_list(&uri), Err(Error::Format(_))),
			"a name without a NUL terminator must be a format error"
		);
	}

	#[test]
	fn library_list_rejects_a_count_above_capacity() {
		let _serial = serial();
		let name = CString::new("mock-over").unwrap();
		let scheme = CString::new("mockover").unwrap();
		let mut vt = mock_vtable(&name, &scheme, Some(mock_can_handle));
		vt.library_list = Some(mock_library_list_overreport);
		let backend = unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.unwrap();
		let uri = StorageUri::parse("mockover://library").unwrap();
		assert!(
			matches!(backend.library_list(&uri), Err(Error::Invalid)),
			"a transport reporting more rows than cap must be rejected"
		);
	}
}
