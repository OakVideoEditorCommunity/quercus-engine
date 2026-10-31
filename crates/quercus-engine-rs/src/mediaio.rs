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

//! Host-provided media byte sources (`quercus_codec_mediaio_`).
//!
//! A host can serve media bytes itself (object store, proxy cache, vendor
//! SDK) by registering one [`MediaIoProvider`]. The engine asks the
//! provider whether it claims a URI ([`MediaIoProvider::can_open`]) and,
//! if so, reads the file through a [`MediaStream`] instead of the local
//! filesystem, for both probing and decoding.
//!
//! Registration is process-wide and returns a [`MediaIoRegistration`]
//! guard. Dropping the guard unregisters the provider; that call waits
//! until every stream already handed out has been closed, so a decode in
//! flight can delay it. Keep the guard alive for as long as custom URIs
//! should be routed.

use std::ffi::{CString, c_char, c_void};
use std::io::{self, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Failure returned by a [`MediaIoProvider`] or [`MediaStream`].
///
/// `code` is provider-defined (0 means "unspecified"); `message` is the
/// human-readable diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaIoError {
	/// Provider-defined status code (0 = unspecified).
	pub code: i32,
	/// Human-readable diagnostic.
	pub message: String,
}

impl std::fmt::Display for MediaIoError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{} (code {})", self.message, self.code)
	}
}

impl std::error::Error for MediaIoError {}

/// Readable and seekable byte source for one media resource.
///
/// Implementations are driven from decoder/probe threads and must be
/// `Send`. `read`/`seek` follow [`std::io::Read`] / [`std::io::Seek`]
/// semantics: short reads are allowed, `seek` returns the new absolute
/// position and unsupported operations report
/// [`io::ErrorKind::Unsupported`].
pub trait MediaStream: Send + 'static {
	/// Read up to `buf.len()` bytes, returning 0 only at end of stream.
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

	/// Seek relative to the start, current position or end of the stream.
	fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
}

/// Host-supplied resolver for custom media URIs.
///
/// One provider is registered per process with [`register_media_io`].
/// `can_open` must be cheap and I/O-free (it is asked for every input
/// open, including ordinary local files); `open` performs the actual
/// lookup and returns a fresh seekable stream per call. Implementations
/// must be `Send + Sync`: the registry is read from decoder and probe
/// threads.
pub trait MediaIoProvider: Send + Sync + 'static {
	/// Short provider name, used in diagnostics (copied at registration).
	fn name(&self) -> &str;

	/// Cheap, I/O-free claim check on the URI/path text.
	fn can_open(&self, path: &str) -> bool;

	/// Open `path`, returning a seekable byte stream.
	fn open(&self, path: &str) -> Result<Box<dyn MediaStream>, MediaIoError>;
}

/// The only supported vtable ABI version.
const MEDIAIO_ABI_VERSION: u32 = 1;

/// Registration rejected as invalid (bad vtable or provider definition).
const E_INVALID: i32 = -1;
/// A media I/O provider is already installed.
const E_INSTALLED: i32 = -2;

/// `repr(C)` mirror of the engine's `QuercusMediaIoError`.
#[repr(C)]
#[allow(dead_code)] // Fields are written through the raw pointer only.
struct QuercusMediaIoError {
	code: i32,
	message: [u8; 512],
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
	fn set_message(&mut self, text: &str) {
		self.message = [0; 512];
		let max = self.message.len() - 1;
		let mut end = text.len().min(max);
		while !text.is_char_boundary(end) {
			end -= 1;
		}
		self.message[..end].copy_from_slice(&text.as_bytes()[..end]);
	}
}

/// `repr(C)` mirror of the engine's `QuercusMediaIoVTable`. The field
/// order is the ABI and must stay byte-identical with the engine struct.
#[repr(C)]
#[allow(dead_code)] // Fields are read by the engine through the raw pointer.
struct QuercusMediaIoVTable {
	struct_size: u32,
	abi_version: u32,
	name: *const c_char,
	// ABI: order matters — byte-identical with the engine struct.
	// The header generator parses these types textually, so keep each
	// `fn(...)` on one line.
	can_open: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusMediaIoError) -> i32>,
	open: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut *mut c_void, *mut QuercusMediaIoError) -> i32>,
	read: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u8, usize, *mut QuercusMediaIoError) -> i64>,
	seek: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, i64, i32, *mut QuercusMediaIoError) -> i64>,
	close: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

#[cfg_attr(windows, link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim"))]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_codec_mediaio_register(
		vt: *const QuercusMediaIoVTable,
		userdata: *mut c_void,
	) -> i32;
	fn quercus_codec_mediaio_unregister() -> bool;
}

/// Heap-owned provider userdata: the boxed provider plus its
/// NUL-terminated name. The engine copies the name at registration; the
/// box itself lives until the registration guard is dropped.
struct ProviderBox {
	provider: Box<dyn MediaIoProvider>,
	name: CString,
}

/// Heap-owned stream userdata: the boxed stream the host handed to one
/// `open`. The engine reclaims it through the `close` trampoline.
struct StreamBox {
	stream: Box<dyn MediaStream>,
}

/// A live media provider registration.
///
/// Dropping it unregisters the provider: the engine removes it from the
/// codec registry first (new opens fail), then waits until every stream
/// opened through it has been closed before returning, so it is safe to
/// free host state afterwards. The wait can block for as long as a decode
/// in flight.
#[must_use = "dropping the registration immediately unregisters the provider"]
pub struct MediaIoRegistration {
	userdata: *mut c_void,
}

impl Drop for MediaIoRegistration {
	fn drop(&mut self) {
		// Unregister first: it blocks until the engine no longer holds
		// any stream or callback that could touch the userdata box.
		unsafe { quercus_codec_mediaio_unregister() };
		if !self.userdata.is_null() {
			// SAFETY: `userdata` came from `Box::into_raw(Box<ProviderBox>)`
			// in `register_media_io` and is freed exactly once, here.
			unsafe { drop(Box::from_raw(self.userdata as *mut ProviderBox)) };
		}
	}
}

/// Fill the engine's error record (a null record is ignored).
///
/// # Safety
/// `err` must be null or point to a writable `QuercusMediaIoError`.
unsafe fn set_error(err: *mut QuercusMediaIoError, code: i32, message: &str) {
	let Some(err) = (unsafe { err.as_mut() }) else {
		return;
	};
	err.code = code;
	err.set_message(message);
}

/// Borrow a `(ptr, len)` path argument as UTF-8.
///
/// # Safety
/// `ptr` must be null or point to `len` readable bytes.
unsafe fn path_arg<'a>(ptr: *const u8, len: usize) -> Result<&'a str, (i32, String)> {
	if ptr.is_null() {
		return if len == 0 {
			Ok("")
		} else {
			Err((E_INVALID, "null media path".to_string()))
		};
	}
	let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
	std::str::from_utf8(bytes).map_err(|_| (E_INVALID, "media path is not valid UTF-8".to_string()))
}

/// Map an `io::Error` to the error-record pair (code 0 = generic).
fn io_failure(error: io::Error) -> (i32, String) {
	(0, error.to_string())
}

/// Map a registration return code to the SDK error.
fn registration_error(code: i32) -> MediaIoError {
	let message = match code {
		E_INSTALLED => "a media I/O provider is already installed".to_string(),
		E_INVALID => "invalid media I/O provider or vtable".to_string(),
		other => format!("media I/O registration failed with code {other}"),
	};
	MediaIoError { code, message }
}

/// C trampoline for `can_open`: panics never cross the ABI boundary.
unsafe extern "C" fn mediaio_can_open(
	userdata: *mut c_void,
	path: *const u8,
	path_len: usize,
	err: *mut QuercusMediaIoError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<i32, (i32, String)> {
		if userdata.is_null() {
			return Err((E_INVALID, "null media provider userdata".to_string()));
		}
		let path = unsafe { path_arg(path, path_len) }?;
		// SAFETY: engine invariant — `userdata` is the `ProviderBox`
		// installed by `register_media_io` while this provider is
		// registered, and unregistration waits for this callback.
		let provider = unsafe { &*(userdata as *const ProviderBox) };
		Ok(i32::from(provider.provider.can_open(path)))
	}));
	match outcome {
		Ok(Ok(claimed)) => claimed,
		Ok(Err((code, message))) => {
			unsafe { set_error(err, code, &message) };
			0
		}
		Err(_) => {
			unsafe { set_error(err, E_INVALID, "panic in media I/O can_open") };
			0
		}
	}
}

/// C trampoline for `open`: boxes the stream and hands its raw pointer to
/// the engine, which returns it through [`mediaio_close`].
unsafe extern "C" fn mediaio_open(
	userdata: *mut c_void,
	path: *const u8,
	path_len: usize,
	out_stream: *mut *mut c_void,
	err: *mut QuercusMediaIoError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_INVALID, "null media provider userdata".to_string()));
		}
		let Some(out_stream) = (unsafe { out_stream.as_mut() }) else {
			return Err((E_INVALID, "null media stream out-parameter".to_string()));
		};
		*out_stream = std::ptr::null_mut();
		let path = unsafe { path_arg(path, path_len) }?;
		// SAFETY: see `mediaio_can_open`.
		let provider = unsafe { &*(userdata as *const ProviderBox) };
		let stream = provider.provider.open(path).map_err(|error| {
			let code = if error.code == 0 {
				E_INVALID
			} else {
				error.code
			};
			(code, error.message)
		})?;
		let boxed = Box::into_raw(Box::new(StreamBox { stream }));
		*out_stream = boxed as *mut c_void;
		Ok(())
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { set_error(err, code, &message) };
			if code < 0 { code } else { E_INVALID }
		}
		Err(_) => {
			unsafe { set_error(err, E_INVALID, "panic in media I/O open") };
			E_INVALID
		}
	}
}

/// C trampoline for `read`: recovers the boxed [`MediaStream`].
unsafe extern "C" fn mediaio_read(
	_userdata: *mut c_void,
	stream: *mut c_void,
	buf: *mut u8,
	cap: usize,
	err: *mut QuercusMediaIoError,
) -> i64 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<i64, (i32, String)> {
		if stream.is_null() {
			return Err((E_INVALID, "null media stream handle".to_string()));
		}
		if buf.is_null() && cap != 0 {
			return Err((E_INVALID, "null media read buffer".to_string()));
		}
		// SAFETY: `stream` is the `StreamBox` handed to the engine by
		// `mediaio_open`, valid until `mediaio_close`; one stream is not
		// driven concurrently with itself.
		let stream = unsafe { &mut *(stream as *mut StreamBox) };
		let buffer: &mut [u8] = if cap == 0 {
			&mut []
		} else {
			unsafe { std::slice::from_raw_parts_mut(buf, cap) }
		};
		let count = stream.stream.read(buffer).map_err(io_failure)?;
		if count > cap {
			return Err((
				E_INVALID,
				format!("media stream read returned {count} bytes for a {cap}-byte buffer"),
			));
		}
		i64::try_from(count)
			.map_err(|_| (E_INVALID, "media stream read count overflow".to_string()))
	}));
	match outcome {
		Ok(Ok(count)) => count,
		Ok(Err((code, message))) => {
			unsafe { set_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { set_error(err, E_INVALID, "panic in media I/O read") };
			-1
		}
	}
}

/// C trampoline for `seek`: maps the C whence value to [`SeekFrom`].
unsafe extern "C" fn mediaio_seek(
	_userdata: *mut c_void,
	stream: *mut c_void,
	offset: i64,
	whence: i32,
	err: *mut QuercusMediaIoError,
) -> i64 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<i64, (i32, String)> {
		if stream.is_null() {
			return Err((E_INVALID, "null media stream handle".to_string()));
		}
		let from = match whence {
			0 => {
				let offset = u64::try_from(offset).map_err(|_| {
					(E_INVALID, "negative media seek offset".to_string())
				})?;
				SeekFrom::Start(offset)
			}
			1 => SeekFrom::Current(offset),
			2 => SeekFrom::End(offset),
			other => {
				return Err((E_INVALID, format!("invalid media seek whence {other}")));
			}
		};
		// SAFETY: see `mediaio_read`.
		let stream = unsafe { &mut *(stream as *mut StreamBox) };
		let position = stream.stream.seek(from).map_err(io_failure)?;
		i64::try_from(position)
			.map_err(|_| (E_INVALID, "media seek position overflow".to_string()))
	}));
	match outcome {
		Ok(Ok(position)) => position,
		Ok(Err((code, message))) => {
			unsafe { set_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { set_error(err, E_INVALID, "panic in media I/O seek") };
			-1
		}
	}
}

/// C trampoline for `close`: reclaims the boxed stream exactly once.
unsafe extern "C" fn mediaio_close(_userdata: *mut c_void, stream: *mut c_void) {
	// The stream's destructor may panic; keep any unwind inside the
	// trampoline. A null handle is tolerated (nothing to reclaim).
	let _ = catch_unwind(AssertUnwindSafe(|| {
		if stream.is_null() {
			return;
		}
		// SAFETY: `stream` came from `Box::into_raw(Box::new(StreamBox))`
		// in `mediaio_open`; the engine calls close exactly once.
		unsafe { drop(Box::from_raw(stream as *mut StreamBox)) };
	}));
}

/// Registers `provider` as the process-wide media byte source.
///
/// Returns a [`MediaIoRegistration`] guard; dropping it unregisters the
/// provider (blocking until streams already opened through it close).
/// Registration fails with code `-2` when a provider is already
/// installed, or `-1` when the provider definition is invalid (for
/// example a name containing a NUL byte).
pub fn register_media_io(
	provider: Box<dyn MediaIoProvider>,
) -> Result<MediaIoRegistration, MediaIoError> {
	let name = CString::new(provider.name()).map_err(|_| MediaIoError {
		code: E_INVALID,
		message: "media I/O provider name contains a NUL byte".to_string(),
	})?;
	// The box is the engine's `userdata`; it is reclaimed by
	// `MediaIoRegistration::drop`.
	let userdata = Box::into_raw(Box::new(ProviderBox { provider, name }));
	let vt = QuercusMediaIoVTable {
		struct_size: std::mem::size_of::<QuercusMediaIoVTable>() as u32,
		abi_version: MEDIAIO_ABI_VERSION,
		// SAFETY: `userdata` is live for the whole call; the engine copies
		// the name before returning.
		name: unsafe { (*userdata).name.as_ptr() },
		can_open: Some(mediaio_can_open),
		open: Some(mediaio_open),
		read: Some(mediaio_read),
		seek: Some(mediaio_seek),
		close: Some(mediaio_close),
	};
	let code = unsafe { quercus_codec_mediaio_register(&vt, userdata as *mut c_void) };
	if code != 0 {
		// The engine never took ownership of the userdata on failure.
		// SAFETY: `userdata` is the box created above and not registered.
		unsafe { drop(Box::from_raw(userdata)) };
		return Err(registration_error(code));
	}
	Ok(MediaIoRegistration {
		userdata: userdata as *mut c_void,
	})
}
