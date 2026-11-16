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

//! C ABI for host-provided media byte sources (prefix
//! `quercus_codec_mediaio_`).
//!
//! A host registers one process-wide [`QuercusMediaIoVTable`]; the engine
//! adapts it to quercus-codec's [`MediaIoProvider`] + [`MediaStream`] pair,
//! so FFmpeg reads a claimed URI through the host's callbacks instead of
//! the local filesystem. The codec owns the routing (probe and decoder
//! both go through it); this module only bridges the foreign table.
//!
//! # ABI
//! [`QuercusMediaIoVTable`] is the ABI: field order matters and the SDK
//! mirror in `quercus-engine-rs/src/mediaio.rs` is byte-identical.
//! [`ForeignProvider::new`] copies the table and its `name` string, so the
//! caller's vtable and name need not outlive registration. The opaque
//! `userdata` stays caller-owned and must remain alive (and thread-safe)
//! until [`quercus_codec_mediaio_unregister`] has returned.
//!
//! # Registration lifetime
//! There is exactly one provider per process. Registration fails with
//! `-2` while another provider is installed; unregistration removes the
//! provider from the codec registry, marks it closed (new
//! [`MediaIoProvider::open`]/[`MediaIoProvider::can_open`] calls fail) and
//! waits until every stream opened through it has been dropped and
//! closed. That wait uses a condition variable (no busy spinning) and may
//! block for as long as a decode in flight; only after it returns may the
//! caller free `userdata`. A callback must not call
//! [`quercus_codec_mediaio_unregister`] itself, since it would wait on
//! itself.

use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::mem;
use std::panic;
use std::ptr;
use std::str;
use std::sync::{Arc, Condvar, Mutex};
use std::sync::{MutexGuard, PoisonError};

use quercus_codec::mediaio::{
	MediaIoError, MediaIoProvider, MediaStream, clear_provider, install_provider,
};

/// The only supported `QuercusMediaIoVTable::abi_version`.
const MEDIAIO_ABI_VERSION: u32 = 1;

/// Registration returned an invalid vtable/provider definition.
const E_INVALID: i32 = -1;
/// A media I/O provider is already installed.
const E_INSTALLED: i32 = -2;

/// Failure detail a foreign media provider fills in; the engine copies the
/// message and never keeps the pointer.
///
/// `code` is provider-defined (0 means "unspecified"; negative values are
/// reserved for engine codes); `message` is best-effort UTF-8,
/// NUL-terminated and truncated to fit.
#[repr(C)]
pub struct QuercusMediaIoError {
	/// Provider-defined status code (0 = unspecified, negative = engine).
	pub code: i32,
	/// NUL-terminated UTF-8 diagnostic.
	pub message: [u8; 512],
}

impl Default for QuercusMediaIoError {
	fn default() -> Self {
		QuercusMediaIoError {
			code: 0,
			message: [0; 512],
		}
	}
}

impl QuercusMediaIoError {
	/// Replace the message with `text`, truncated to fit and
	/// NUL-terminated. Never splits a UTF-8 code point.
	#[cfg(test)]
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
		str::from_utf8(&self.message[..end]).unwrap_or("")
	}
}

/// The foreign media I/O vtable. Field order is the ABI.
///
/// All five callbacks are required. Callbacks may run from several engine
/// threads (probe and decode); the engine may call `read`/`seek` on one
/// stream concurrently with another stream's callbacks, but never
/// concurrently on the same stream.
#[repr(C)]
pub struct QuercusMediaIoVTable {
	/// `sizeof(QuercusMediaIoVTable)` as compiled by the caller; must
	/// cover every field below (the v1 layout).
	pub struct_size: u32,
	/// Must be `1`.
	pub abi_version: u32,
	/// Provider name: non-empty UTF-8, NUL-terminated; copied by the
	/// engine.
	pub name: *const c_char,
	/// Cheap, I/O-free claim check: `> 0` claims `path`, `0` declines,
	/// `< 0` is an error (ignored by the engine, treated as a decline).
	pub can_open: Option<
		unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusMediaIoError) -> i32,
	>,
	/// Open `path`: return 0 and store a non-null opaque stream in
	/// `out_stream`, or return `< 0` (and fill `err`). A failed callback
	/// must leave `out_stream` null; the engine never closes a handle it
	/// did not receive a successful open for.
	pub open: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*const u8,
			usize,
			*mut *mut c_void,
			*mut QuercusMediaIoError,
		) -> i32,
	>,
	/// Read up to `cap` bytes: `> 0` bytes read, `0` end of stream,
	/// `< 0` error (fills `err`).
	pub read: Option<
		unsafe extern "C" fn(
			*mut c_void,
			*mut c_void,
			*mut u8,
			usize,
			*mut QuercusMediaIoError,
		) -> i64,
	>,
	/// Seek: `whence` is 0 = SET, 1 = CUR, 2 = END; returns the new
	/// absolute position or `< 0` on error.
	pub seek: Option<
		unsafe extern "C" fn(*mut c_void, *mut c_void, i64, i32, *mut QuercusMediaIoError) -> i64,
	>,
	/// Close one stream opened by `open`. Exactly once per successful
	/// open, after which the handle must not be used.
	pub close: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

/// The lock-protected liveness of one installed provider.
#[derive(Default)]
struct ActivityState {
	/// Streams currently open plus callbacks currently in flight.
	active: usize,
	/// Set by unregister; no new callbacks may start afterwards.
	closed: bool,
}

/// Tracks active streams and callback calls so unregistration can wait
/// for them without polling.
struct Activity {
	state: Mutex<ActivityState>,
	idle: Condvar,
}

impl Activity {
	fn new() -> Self {
		Activity {
			state: Mutex::new(ActivityState::default()),
			idle: Condvar::new(),
		}
	}

	/// Enter a callback or open a stream; `false` once unregistered.
	fn enter(&self) -> bool {
		let mut state = self.lock();
		if state.closed {
			return false;
		}
		state.active += 1;
		true
	}

	/// Leave a callback or release a stream.
	fn leave(&self) {
		let mut state = self.lock();
		state.active = state.active.saturating_sub(1);
		if state.active == 0 {
			self.idle.notify_all();
		}
	}

	/// Refuse new callbacks and block until all active work has left.
	fn close_and_wait(&self) {
		let mut state = self.lock();
		state.closed = true;
		while state.active > 0 {
			state = self
				.idle
				.wait(state)
				.unwrap_or_else(PoisonError::into_inner);
		}
	}

	/// Lock the state, tolerating a poisoned mutex (a callback may have
	/// panicked on another thread; the counters are still consistent).
	fn lock(&self) -> MutexGuard<'_, ActivityState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// The internal [`MediaIoProvider`] adapter around a foreign vtable.
pub(crate) struct ForeignProvider {
	name: String,
	can_open: unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusMediaIoError) -> i32,
	open: unsafe extern "C" fn(
		*mut c_void,
		*const u8,
		usize,
		*mut *mut c_void,
		*mut QuercusMediaIoError,
	) -> i32,
	read: unsafe extern "C" fn(
		*mut c_void,
		*mut c_void,
		*mut u8,
		usize,
		*mut QuercusMediaIoError,
	) -> i64,
	seek: unsafe extern "C" fn(*mut c_void, *mut c_void, i64, i32, *mut QuercusMediaIoError) -> i64,
	close: unsafe extern "C" fn(*mut c_void, *mut c_void),
	userdata: *mut c_void,
	activity: Arc<Activity>,
}

// SAFETY: the vtable contract requires the provider (and its `userdata`)
// to be thread-safe; this adapter only forwards calls and holds no
// Rust-accessible state of the caller.
unsafe impl Send for ForeignProvider {}
// SAFETY: see the `Send` impl above. `can_open` may run concurrently with
// other callbacks, which the caller's contract requires to be safe.
unsafe impl Sync for ForeignProvider {}

/// One open stream: the opaque host handle plus the callbacks needed to
/// drive it after the provider itself has been removed from the registry.
struct ForeignStream {
	handle: *mut c_void,
	read: unsafe extern "C" fn(
		*mut c_void,
		*mut c_void,
		*mut u8,
		usize,
		*mut QuercusMediaIoError,
	) -> i64,
	seek: unsafe extern "C" fn(*mut c_void, *mut c_void, i64, i32, *mut QuercusMediaIoError) -> i64,
	close: unsafe extern "C" fn(*mut c_void, *mut c_void),
	userdata: *mut c_void,
	activity: Arc<Activity>,
}

// SAFETY: FFmpeg may drive one stream from a decoder/probe thread. The
// stream owns its host handle and the caller's contract makes it safe to
// move between threads.
unsafe impl Send for ForeignStream {}

impl Drop for ForeignStream {
	fn drop(&mut self) {
		if !self.handle.is_null() {
			// Exactly once per successful open; the host frees its handle
			// here. `leave` runs after `close` so unregister cannot return
			// while the host's close is still using `userdata`.
			unsafe { (self.close)(self.userdata, self.handle) };
		}
		self.activity.leave();
	}
}

impl MediaStream for ForeignStream {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let mut err = QuercusMediaIoError::default();
		let rc = unsafe {
			(self.read)(
				self.userdata,
				self.handle,
				buf.as_mut_ptr(),
				buf.len(),
				&mut err as *mut QuercusMediaIoError,
			)
		};
		if rc < 0 {
			return Err(stream_error(&err, "media stream read failed"));
		}
		let count = usize::try_from(rc)
			.map_err(|_| io::Error::other("media stream read returned a negative count"))?;
		if count > buf.len() {
			return Err(io::Error::other(
				"media stream read returned more bytes than requested",
			));
		}
		Ok(count)
	}

	fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
		let (offset, whence) = match pos {
			io::SeekFrom::Start(offset) => {
				let offset = i64::try_from(offset)
					.map_err(|_| io::Error::other("media stream seek offset too large"))?;
				(offset, 0)
			}
			io::SeekFrom::Current(offset) => (offset, 1),
			io::SeekFrom::End(offset) => (offset, 2),
		};
		let mut err = QuercusMediaIoError::default();
		let rc = unsafe {
			(self.seek)(
				self.userdata,
				self.handle,
				offset,
				whence,
				&mut err as *mut QuercusMediaIoError,
			)
		};
		if rc < 0 {
			return Err(stream_error(&err, "media stream seek failed"));
		}
		Ok(rc as u64)
	}
}

/// Map a provider error record to the codec's `MediaIoError` for `open`.
///
/// `err` is the record the foreign provider filled in (`code` plus a
/// NUL-terminated message), `fallback` the message used when it wrote no text.
/// An empty message yields `E_INVALID` with `fallback`; a message with
/// `code == 0` (provider left it unspecified) yields `E_INVALID` with that
/// message; otherwise the provider's non-zero code is preserved next to the
/// message.
fn provider_error(err: &QuercusMediaIoError, fallback: &str) -> MediaIoError {
	let message = err.message();
	if message.is_empty() {
		MediaIoError::new(E_INVALID, fallback)
	} else if err.code == 0 {
		MediaIoError::new(E_INVALID, message)
	} else {
		MediaIoError::new(err.code, message)
	}
}

/// Map a provider error record to an `io::Error` for `read`/`seek`.
///
/// `err` is the record the foreign provider filled in (`code` plus a
/// NUL-terminated message), `fallback` the message used when it wrote no text.
/// The provider code is dropped — `io::Error::other` carries only the message —
/// so callers can report the text but not the errno-style value.
fn stream_error(err: &QuercusMediaIoError, fallback: &str) -> io::Error {
	let message = err.message();
	if message.is_empty() {
		io::Error::other(fallback)
	} else {
		io::Error::other(message)
	}
}

/// Copies a NUL-terminated C string; null/invalid UTF-8 is rejected.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated C string.
unsafe fn copy_c_string(ptr: *const c_char) -> Option<String> {
	if ptr.is_null() {
		return None;
	}
	unsafe { CStr::from_ptr(ptr) }
		.to_str()
		.ok()
		.map(str::to_owned)
}

impl ForeignProvider {
	/// Wraps a caller-provided vtable as a codec [`MediaIoProvider`].
	///
	/// Validates the pointer, the v1 size/version, the name (non-empty
	/// UTF-8) and all five callbacks. The vtable and name are copied; the
	/// table need not outlive this call.
	///
	/// # Safety
	/// `vtable` must point to a readable [`QuercusMediaIoVTable`] whose
	/// `struct_size` covers the full v1 layout, and whose `name` is a
	/// valid NUL-terminated string. `userdata` is opaque to the engine,
	/// owned by the caller, and must stay alive until
	/// [`quercus_codec_mediaio_unregister`] returns.
	pub(crate) unsafe fn new(
		vtable: *const QuercusMediaIoVTable,
		userdata: *mut c_void,
	) -> Result<Self, i32> {
		if vtable.is_null() {
			return Err(E_INVALID);
		}
		// Read the size first: only the prefix the caller declares is
		// readable. The v1 layout must be complete.
		let struct_size = (unsafe { (*vtable).struct_size }) as usize;
		if struct_size < mem::size_of::<QuercusMediaIoVTable>() {
			return Err(E_INVALID);
		}
		// SAFETY: the caller guarantees `struct_size` readable bytes and
		// `struct_size` covers our whole layout.
		let vt = unsafe { ptr::read(vtable) };
		if vt.abi_version != MEDIAIO_ABI_VERSION {
			return Err(E_INVALID);
		}
		let Some(name) = (unsafe { copy_c_string(vt.name) }) else {
			return Err(E_INVALID);
		};
		if name.is_empty() {
			return Err(E_INVALID);
		}
		let (Some(can_open), Some(open), Some(read), Some(seek), Some(close)) =
			(vt.can_open, vt.open, vt.read, vt.seek, vt.close)
		else {
			return Err(E_INVALID);
		};
		Ok(ForeignProvider {
			name,
			can_open,
			open,
			read,
			seek,
			close,
			userdata,
			activity: Arc::new(Activity::new()),
		})
	}
}

impl MediaIoProvider for ForeignProvider {
	fn name(&self) -> &str {
		&self.name
	}

	fn can_open(&self, path: &str) -> bool {
		if !self.activity.enter() {
			return false;
		}
		let mut err = QuercusMediaIoError::default();
		let rc = unsafe {
			(self.can_open)(
				self.userdata,
				path.as_ptr(),
				path.len(),
				&mut err as *mut QuercusMediaIoError,
			)
		};
		self.activity.leave();
		// A negative return is an error the codec cannot act on (it only
		// asks "do you claim this path?"): treat it as a decline.
		rc > 0
	}

	fn open(&self, path: &str) -> Result<Box<dyn MediaStream>, MediaIoError> {
		// Enter before the callback: once unregister marks the provider
		// closed, no new host callback may start.
		if !self.activity.enter() {
			return Err(MediaIoError::new(
				E_INVALID,
				"media I/O provider is not registered",
			));
		}
		let mut handle: *mut c_void = ptr::null_mut();
		let mut err = QuercusMediaIoError::default();
		let rc = unsafe {
			(self.open)(
				self.userdata,
				path.as_ptr(),
				path.len(),
				&mut handle as *mut *mut c_void,
				&mut err as *mut QuercusMediaIoError,
			)
		};
		if rc != 0 || handle.is_null() {
			self.activity.leave();
			let fallback = if rc != 0 {
				"media provider failed to open the path"
			} else {
				"media provider returned a null stream"
			};
			let mut error = provider_error(&err, fallback);
			if error.code == 0 {
				error.code = E_INVALID;
			}
			return Err(error);
		}
		Ok(Box::new(ForeignStream {
			handle,
			read: self.read,
			seek: self.seek,
			close: self.close,
			userdata: self.userdata,
			activity: self.activity.clone(),
		}))
	}
}

/// The process-wide C-registered provider, kept alongside the codec
/// registry so unregister can wait on exactly the adapter it installed.
static INSTALLED: Mutex<Option<Arc<ForeignProvider>>> = Mutex::new(None);

fn installed_slot() -> MutexGuard<'static, Option<Arc<ForeignProvider>>> {
	INSTALLED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Test-only access to the currently installed adapter.
#[cfg(test)]
fn installed_provider_for_test() -> Option<Arc<ForeignProvider>> {
	installed_slot().clone()
}

/// Registers one process-wide foreign media provider.
///
/// Returns 0 on success, `-1` for an invalid vtable (null pointer, bad
/// `struct_size`/`abi_version`, missing callback, empty/invalid name) and
/// `-2` when a provider is already installed. The vtable and its `name`
/// are copied; `userdata` stays caller-owned and must outlive
/// [`quercus_codec_mediaio_unregister`].
///
/// # Safety
/// `vt` must point to a readable [`QuercusMediaIoVTable`] for the
/// duration of the call, with a valid NUL-terminated `name`. `userdata`
/// is passed through to the callbacks untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_mediaio_register(
	vt: *const QuercusMediaIoVTable,
	userdata: *mut c_void,
) -> i32 {
	let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| -> i32 {
		// Validate before touching the slot: an invalid table is always
		// -1, even when another provider is installed.
		let provider = match unsafe { ForeignProvider::new(vt, userdata) } {
			Ok(provider) => Arc::new(provider),
			Err(code) => return code,
		};
		let mut slot = installed_slot();
		if slot.is_some() {
			return E_INSTALLED;
		}
		// Publish to the codec registry. A provider installed there by
		// another path (a Rust host embedding the engine) also counts as
		// "already installed": restore it and reject.
		if let Some(previous) = install_provider(provider.clone()) {
			install_provider(previous);
			return E_INSTALLED;
		}
		*slot = Some(provider);
		0
	}));
	outcome.unwrap_or(E_INVALID)
}

/// Removes the installed foreign media provider and waits for its streams
/// to close. Returns false when no provider is installed.
///
/// Removal from the codec registry happens first, so no new input opens
/// can pick the provider, and its activity gate is closed so an already
/// resolved provider fails new `open`/`can_open` calls. The wait (a
/// condition variable, never a spin) may block for as long as a decode
/// in flight; after it returns the caller may free `userdata`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_mediaio_unregister() -> bool {
	let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| -> bool {
		let provider = match installed_slot().take() {
			Some(provider) => provider,
			None => return false,
		};
		// Stop routing new opens through the adapter, then wait for the
		// streams (and callbacks) it already handed out.
		let _previous = clear_provider();
		provider.activity.close_and_wait();
		true
	}));
	outcome.unwrap_or(false)
}

#[cfg(test)]
mod tests;
