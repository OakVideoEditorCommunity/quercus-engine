//! Safe wrapper over the engine's storage C ABI (`quercus_storage_*`):
//! project open/save through the backend registry, backend
//! enumeration, URI validation, the project library (the database
//! backend), and registration of caller-implemented byte transports.

use std::ffi::{c_char, c_void, CString};
use std::panic::{AssertUnwindSafe, catch_unwind};

use thiserror::Error;

use crate::codec::string_out;
use crate::node::Project;

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_storage_open(uri: *const u8, uri_len: usize, version_info: *mut i32) -> *mut c_void;
	fn quercus_storage_save(
		project: *const c_void,
		uri: *const u8,
		uri_len: usize,
		options: u32,
	) -> i32;
	fn quercus_storage_uri_is_valid(uri: *const u8, uri_len: usize) -> bool;
	fn quercus_storage_backend_count() -> i32;
	fn quercus_storage_backend_name_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_storage_backend_scheme_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_storage_backend_for_uri(
		uri: *const u8,
		uri_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_storage_library_count(uri: *const u8, uri_len: usize) -> i32;
	fn quercus_storage_library_uuid_at(
		uri: *const u8,
		uri_len: usize,
		index: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_storage_library_name_at(
		uri: *const u8,
		uri_len: usize,
		index: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_storage_library_delete(
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
	) -> i32;
	fn quercus_storage_library_rename(
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		new_name: *const u8,
		new_name_len: usize,
	) -> i32;
	fn quercus_storage_default_library_uri(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_storage_library_created_at(uri: *const u8, uri_len: usize, index: i32) -> i64;
	fn quercus_storage_library_modified_at(uri: *const u8, uri_len: usize, index: i32) -> i64;
	fn quercus_storage_library_duration_ms(uri: *const u8, uri_len: usize, index: i32) -> i64;
	fn quercus_storage_library_track_count(uri: *const u8, uri_len: usize, index: i32) -> i32;
	fn quercus_storage_library_clip_count(uri: *const u8, uri_len: usize, index: i32) -> i32;
	fn quercus_storage_library_footage_count(uri: *const u8, uri_len: usize, index: i32) -> i32;
	fn quercus_storage_library_create(
		uri: *const u8,
		uri_len: usize,
		name: *const u8,
		name_len: usize,
		uuid_buf: *mut u8,
		uuid_buf_len: usize,
	) -> i32;
	fn quercus_storage_library_duplicate(
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		new_uuid_buf: *mut u8,
		new_uuid_buf_len: usize,
	) -> i32;
	fn quercus_storage_library_import(
		uri: *const u8,
		uri_len: usize,
		file_uri: *const u8,
		file_uri_len: usize,
		new_uuid_buf: *mut u8,
		new_uuid_buf_len: usize,
	) -> i32;
	fn quercus_storage_library_export(
		uri: *const u8,
		uri_len: usize,
		uuid: *const u8,
		uuid_len: usize,
		file_uri: *const u8,
		file_uri_len: usize,
	) -> i32;
	fn quercus_storage_flush();
	fn quercus_storage_register_transport(
		vt: *const QuercusStorageTransportVTable,
		userdata: *mut c_void,
	) -> i32;
	fn quercus_storage_unregister_transport(slot: i32) -> bool;
	fn quercus_storage_library_supported(uri: *const u8, uri_len: usize) -> bool;
}

/// Current transport vtable ABI version (mirrors the engine constant).
const TRANSPORT_VERSION: u32 = 1;

/// Error code used when a trampoline cannot report a specific engine code
/// (invalid arguments or a panicking backend).
const E_TRANSPORT: i32 = -1;
/// Engine code for "this backend does not implement the capability".
const E_UNSUPPORTED: i32 = -100009;

/// `repr(C)` mirror of the engine's `QuercusStorageError`.
#[repr(C)]
#[allow(dead_code)] // Fields are written through the raw pointer only.
struct QuercusStorageError {
	code: i32,
	message: [u8; 512],
}

impl QuercusStorageError {
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

/// `repr(C)` mirror of the engine's `QuercusStorageProjectInfo` (a
/// library row in a caller-owned array). Field order is the ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct QuercusStorageProjectInfo {
	uuid: [u8; 40],
	name: [u8; 256],
	created_at: i64,
	modified_at: i64,
	schema_ver: i32,
	reserved: i32,
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

/// `repr(C)` mirror of the engine-side transport vtable.
/// The field order is the ABI and must stay byte-identical with the engine
/// struct.
#[repr(C)]
#[allow(dead_code)] // Fields are read by the engine through the raw pointer.
struct QuercusStorageTransportVTable {
	struct_size: u32,
	abi_version: u32,
	name: *const c_char,
	scheme: *const c_char,
	write_policy: u32,
	// ABI: order matters — byte-identical with the engine struct.
	// The header generator parses these types textually, so keep each
	// `fn(...)` on one line.
	can_handle: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageError) -> i32>,
	load: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut *mut u8, *mut usize, *mut i32, *mut QuercusStorageError) -> i32>,
	free_bytes: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize)>,
	save: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, u32, *mut QuercusStorageError) -> i32>,
	// Optional project-library capability (NULL when unsupported).
	library_list: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageProjectInfo, usize, *mut usize, *mut QuercusStorageError) -> i32>,
	library_delete: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, *mut QuercusStorageError) -> i32>,
	library_rename: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, *const u8, usize, *mut QuercusStorageError) -> i32>,
	library_snapshot: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, *mut QuercusStorageError) -> i32>,
}

/// The save succeeded / load carried no version note.
pub const OK: i32 = 0;
/// The project was written by an older build (upgraded on load).
pub const TOO_OLD: i32 = 1;
/// The project was written by a newer build (not loaded).
pub const TOO_NEW: i32 = 2;
/// The payload version is unknown (not loaded).
pub const UNKNOWN_VERSION: i32 = 3;

/// A storage operation failure: the engine's `QUERCUSSTORAGE_E_*` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("storage operation failed with code {0}")]
pub struct StorageError(pub i32);

fn check(code: i32) -> Result<(), StorageError> {
	if code == 0 {
		Ok(())
	} else {
		Err(StorageError(code))
	}
}

/// A caller-implemented byte transport for project documents.
///
/// The engine owns serialization and schema: `load` returns the opaque
/// project bytes the host stored earlier, `save` receives the
/// engine-serialized bytes. Implementations may do network I/O and must
/// be safe to call from several threads (the engine serializes
/// `load`/`save` per registration, but `can_handle` may run concurrently).
pub trait TransportBackend: Send + Sync + 'static {
	/// Backend name: unique and non-empty (copied at registration).
	fn name(&self) -> &str;

	/// Primary URI scheme (e.g. "s3"); empty means "rely on `can_handle`".
	fn scheme(&self) -> &str;

	/// Whether this transport claims `uri`. Must be cheap, I/O-free and
	/// safe to call from several threads.
	fn can_handle(&self, uri: &str) -> bool;

	/// Load the opaque project bytes stored at `uri`.
	fn load(&self, uri: &str) -> Result<Vec<u8>, StorageError>;

	/// Save engine-serialized project bytes to `uri`. `options` is the
	/// engine's save bitmask (unknown bits are ignored).
	fn save(&self, uri: &str, bytes: &[u8], options: u32) -> Result<(), StorageError>;

	/// Write policy hint: 0 = engine default (coalesced for foreign
	/// transports), 1 = immediate, 2 = coalesced, 3 = explicit flush only.
	fn write_policy(&self) -> u32 {
		0
	}

	/// Whether this transport also implements the project-library
	/// capability below. The default is `false`: a pure byte transport.
	fn supports_library(&self) -> bool {
		false
	}

	/// The library rows at `uri` (only when [`Self::supports_library`]).
	fn library_list(&self, _uri: &str) -> Result<Vec<TransportProjectInfo>, StorageError> {
		Err(StorageError(E_UNSUPPORTED))
	}

	/// Delete the row `uuid` from the library at `uri`.
	fn library_delete(&self, _uri: &str, _uuid: &str) -> Result<(), StorageError> {
		Err(StorageError(E_UNSUPPORTED))
	}

	/// Rename the row `uuid` in the library at `uri`.
	fn library_rename(&self, _uri: &str, _uuid: &str, _name: &str) -> Result<(), StorageError> {
		Err(StorageError(E_UNSUPPORTED))
	}

	/// Snapshot the row `uuid` (whole-document transports can keep the
	/// default no-op: every `save` is already a snapshot).
	fn library_snapshot(&self, _uri: &str, _uuid: &str) -> Result<(), StorageError> {
		Ok(())
	}
}

/// One project-library row returned by a [`TransportBackend`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportProjectInfo {
	/// Project uuid.
	pub uuid: String,
	/// Display name.
	pub name: String,
	/// Creation time (Unix seconds).
	pub created_at: i64,
	/// Last-write time (Unix seconds).
	pub modified_at: i64,
	/// Payload schema version.
	pub schema_ver: i32,
}

/// Heap-owned transport userdata: the backend plus its NUL-terminated
/// name/scheme strings. The engine copies both strings at registration;
/// they live here so they stay valid for the registration call.
struct TransportBox<B: TransportBackend> {
	backend: B,
	name: CString,
	scheme: CString,
}

/// A registered transport. Dropping it unregisters the backend (waiting
/// for in-flight callbacks to finish) and then frees the heap userdata.
pub struct TransportRegistration {
	slot: i32,
	userdata: *mut c_void,
	drop_userdata: unsafe fn(*mut c_void),
}

impl TransportRegistration {
	/// The engine's slot index for this registration (opaque).
	pub fn slot(&self) -> i32 {
		self.slot
	}
}

impl Drop for TransportRegistration {
	fn drop(&mut self) {
		// Unregister first: the engine removes the backend from the
		// registry, then waits for in-flight callbacks, so the userdata
		// box is unreachable when it is freed below.
		unsafe { quercus_storage_unregister_transport(self.slot) };
		unsafe { (self.drop_userdata)(self.userdata) };
	}
}

/// Registers `backend` as a process-wide transport and returns a guard
/// that unregisters it on drop.
///
/// The engine copies the vtable and the name/scheme strings. Reserved
/// schemes (`file`, `quercusdb`, `quercusdb+*`) are rejected with the
/// engine's `QUERCUSSTORAGE_E_INVALID` code and duplicate names with
/// `QUERCUSSTORAGE_E_STATE`.
///
/// The transport serves opaque project bytes; the engine keeps owning the
/// document format and schema. `write_policy` is hardcoded to 0 (engine
/// default) for now; the field exists for a future coalesced/immediate
/// choice.
pub fn register_transport<B: TransportBackend>(
	backend: B,
) -> Result<TransportRegistration, StorageError> {
	let name = CString::new(backend.name()).map_err(|_| StorageError(E_TRANSPORT))?;
	let scheme = CString::new(backend.scheme()).map_err(|_| StorageError(E_TRANSPORT))?;
	let write_policy = backend.write_policy();
	let supports_library = backend.supports_library();
	// The box is the engine's `userdata`; it is reclaimed by
	// `TransportRegistration::drop`.
	let transport = Box::into_raw(Box::new(TransportBox {
		backend,
		name,
		scheme,
	}));
	let vt = QuercusStorageTransportVTable {
		struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
		abi_version: TRANSPORT_VERSION,
		// SAFETY: `transport` is live for the whole call; the engine
		// copies the strings before returning.
		name: unsafe { (*transport).name.as_ptr() },
		scheme: unsafe { (*transport).scheme.as_ptr() },
		write_policy,
		can_handle: Some(transport_can_handle::<B>),
		load: Some(transport_load::<B>),
		free_bytes: Some(transport_free_bytes::<B>),
		save: Some(transport_save::<B>),
		library_list: supports_library.then_some(transport_library_list::<B>),
		library_delete: supports_library.then_some(transport_library_delete::<B>),
		library_rename: supports_library.then_some(transport_library_rename::<B>),
		library_snapshot: supports_library.then_some(transport_library_snapshot::<B>),
	};
	let slot = unsafe { quercus_storage_register_transport(&vt, transport as *mut c_void) };
	if slot < 0 {
		// Rejected: the engine never took ownership of the userdata.
		unsafe { drop(Box::from_raw(transport)) };
		return Err(StorageError(slot));
	}
	Ok(TransportRegistration {
		slot,
		userdata: transport as *mut c_void,
		drop_userdata: drop_transport::<B>,
	})
}

/// Type-erased drop for the registration guard.
unsafe fn drop_transport<B: TransportBackend>(userdata: *mut c_void) {
	if userdata.is_null() {
		return;
	}
	// SAFETY: `userdata` came from `Box::into_raw(Box<TransportBox<B>>)`
	// in `register_transport` and is freed exactly once, by the guard.
	unsafe { drop(Box::from_raw(userdata as *mut TransportBox<B>)) };
}

/// Borrow a `(ptr, len)` URI argument; the pointer is valid only for the
/// callback duration.
///
/// # Safety
/// `uri` must be null or point to `len` readable bytes.
unsafe fn transport_uri<'a>(uri: *const u8, len: usize) -> Result<&'a str, &'static str> {
	if uri.is_null() {
		return Err("null URI");
	}
	// SAFETY: the caller guarantees `len` readable bytes.
	let bytes = unsafe { std::slice::from_raw_parts(uri, len) };
	std::str::from_utf8(bytes).map_err(|_| "URI is not valid UTF-8")
}

/// Fill the engine's error record (a null record is ignored).
///
/// # Safety
/// `err` must be null or point to a writable `QuercusStorageError`.
unsafe fn transport_error(err: *mut QuercusStorageError, code: i32, message: &str) {
	let Some(err) = (unsafe { err.as_mut() }) else {
		return;
	};
	err.code = code;
	err.set_message(message);
}

/// C trampoline for `can_handle`: panics never cross the ABI boundary.
unsafe extern "C" fn transport_can_handle<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<i32, (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		// SAFETY: engine invariant — `userdata` is the `TransportBox<B>`
		// installed by `register_transport` while this backend is
		// registered, and unregistration waits for this callback.
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		Ok(i32::from(transport.backend.can_handle(uri)))
	}));
	match outcome {
		Ok(Ok(claimed)) => claimed,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport can_handle") };
			-1
		}
	}
}

/// C trampoline for `load`: hands a heap buffer to the engine, which
/// releases it through [`transport_free_bytes`].
unsafe extern "C" fn transport_load<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	out_bytes: *mut *mut u8,
	out_len: *mut usize,
	out_format: *mut i32,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		if out_bytes.is_null() || out_len.is_null() {
			return Err((E_TRANSPORT, "null load out-parameter".to_string()));
		}
		// SAFETY: see `transport_can_handle`.
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let bytes = transport
			.backend
			.load(uri)
			.map_err(|e| (e.0, e.to_string()))?;
		// Ownership moves to the engine; `transport_free_bytes`
		// reconstructs the boxed slice from `(ptr, len)`.
		let boxed = bytes.into_boxed_slice();
		let len = boxed.len();
		let ptr = Box::into_raw(boxed) as *mut u8;
		// SAFETY: out-parameters are non-null (checked above); the
		// engine's adapter initializes them before calling.
		unsafe {
			*out_bytes = ptr;
			*out_len = len;
			if !out_format.is_null() {
				// AUTO: the engine's document codec decides (v1: XML).
				*out_format = 0;
			}
		}
		Ok(())
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport load") };
			-1
		}
	}
}

/// C trampoline for `free_bytes`: reclaims a buffer from
/// [`transport_load`].
unsafe extern "C" fn transport_free_bytes<B: TransportBackend>(
	_userdata: *mut c_void,
	bytes: *mut u8,
	len: usize,
) {
	if bytes.is_null() {
		return;
	}
	// A `u8` slice has no destructor, so this cannot panic; still keep
	// any unwind inside the trampoline.
	let _ = catch_unwind(AssertUnwindSafe(|| {
		let slice = std::ptr::slice_from_raw_parts_mut(bytes, len);
		// SAFETY: `(bytes, len)` came from `Box::into_raw` on a boxed
		// slice of exactly this length in `transport_load`.
		unsafe { drop(Box::from_raw(slice)) };
	}));
}

/// C trampoline for `save`.
unsafe extern "C" fn transport_save<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	bytes: *const u8,
	bytes_len: usize,
	options: u32,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		if bytes.is_null() && bytes_len != 0 {
			return Err((E_TRANSPORT, "null save payload".to_string()));
		}
		// SAFETY: see `transport_can_handle`.
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		// The payload is borrowed for the call duration only.
		let payload: &[u8] = if bytes.is_null() {
			&[]
		} else {
			unsafe { std::slice::from_raw_parts(bytes, bytes_len) }
		};
		transport
			.backend
			.save(uri, payload, options)
			.map_err(|e| (e.0, e.to_string()))
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport save") };
			-1
		}
	}
}

/// Fill one C project-info POD; rejects values that do not fit the
/// fixed-size fields rather than truncating a name or uuid silently.
fn write_project_info_pod(
	out: *mut QuercusStorageProjectInfo,
	info: &TransportProjectInfo,
) -> Result<(), (i32, String)> {
	let uuid = info.uuid.as_bytes();
	let name = info.name.as_bytes();
	if uuid.len() >= 40 {
		return Err((E_TRANSPORT, "library row uuid exceeds 39 bytes".to_string()));
	}
	if name.len() >= 256 {
		return Err((E_TRANSPORT, "library row name exceeds 255 bytes".to_string()));
	}
	let mut pod = QuercusStorageProjectInfo::default();
	pod.uuid[..uuid.len()].copy_from_slice(uuid);
	pod.name[..name.len()].copy_from_slice(name);
	pod.created_at = info.created_at;
	pod.modified_at = info.modified_at;
	pod.schema_ver = info.schema_ver;
	// SAFETY: `out` points into the engine's caller-owned array.
	unsafe { out.write(pod) };
	Ok(())
}

/// C trampoline for `library_list`: two-phase (NULL/0 queries the count).
unsafe extern "C" fn transport_library_list<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	out: *mut QuercusStorageProjectInfo,
	cap: usize,
	out_count: *mut usize,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() || out_count.is_null() {
			return Err((E_TRANSPORT, "null library_list argument".to_string()));
		}
		// SAFETY: see `transport_can_handle`.
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let rows = transport
			.backend
			.library_list(uri)
			.map_err(|e| (e.0, e.to_string()))?;
		if out.is_null() || cap == 0 {
			// SAFETY: non-null (checked above).
			unsafe { *out_count = rows.len() };
			return Ok(());
		}
		let written = rows.len().min(cap);
		for (index, info) in rows.iter().take(written).enumerate() {
			// SAFETY: `out` has room for `cap` rows (engine contract).
			unsafe { write_project_info_pod(out.add(index), info)? };
		}
		// SAFETY: non-null (checked above).
		unsafe { *out_count = written };
		if rows.len() > cap {
			return Err((
				E_TRANSPORT,
				"library_list returned more rows than the caller's array".to_string(),
			));
		}
		Ok(())
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport library_list") };
			-1
		}
	}
}

/// C trampoline for `library_delete`.
unsafe extern "C" fn transport_library_delete<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let uuid = unsafe { transport_uri(uuid, uuid_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		transport
			.backend
			.library_delete(uri, uuid)
			.map_err(|e| (e.0, e.to_string()))
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport library_delete") };
			-1
		}
	}
}

/// C trampoline for `library_rename`.
unsafe extern "C" fn transport_library_rename<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	name: *const u8,
	name_len: usize,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let uuid = unsafe { transport_uri(uuid, uuid_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let name = unsafe { transport_uri(name, name_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		transport
			.backend
			.library_rename(uri, uuid, name)
			.map_err(|e| (e.0, e.to_string()))
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport library_rename") };
			-1
		}
	}
}

/// C trampoline for `library_snapshot`.
unsafe extern "C" fn transport_library_snapshot<B: TransportBackend>(
	userdata: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	err: *mut QuercusStorageError,
) -> i32 {
	let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), (i32, String)> {
		if userdata.is_null() {
			return Err((E_TRANSPORT, "null transport userdata".to_string()));
		}
		let transport = unsafe { &*(userdata as *const TransportBox<B>) };
		let uri = unsafe { transport_uri(uri, uri_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		let uuid = unsafe { transport_uri(uuid, uuid_len) }
			.map_err(|message| (E_TRANSPORT, message.to_string()))?;
		transport
			.backend
			.library_snapshot(uri, uuid)
			.map_err(|e| (e.0, e.to_string()))
	}));
	match outcome {
		Ok(Ok(())) => 0,
		Ok(Err((code, message))) => {
			unsafe { transport_error(err, code, &message) };
			-1
		}
		Err(_) => {
			unsafe { transport_error(err, E_TRANSPORT, "panic in transport library_snapshot") };
			-1
		}
	}
}

/// The outcome of [`open`]: the project plus the backend's version
/// note (0 = OK, or [`TOO_OLD`]/[`TOO_NEW`]/[`UNKNOWN_VERSION`]).
pub struct OpenedProject {
	/// The loaded project (None when the backend only reported version
	/// info — a too-new/unknown payload carries no project).
	pub project: Option<Project>,
	/// The version info code.
	pub version_info: i32,
}

/// Opens the project at `uri` through the claiming backend. Err on a
/// load failure (the code is the storage error).
pub fn open(uri: &str) -> Result<OpenedProject, StorageError> {
	let mut version_info = 0i32;
	let ptr = unsafe { quercus_storage_open(uri.as_ptr(), uri.len(), &mut version_info) };
	if ptr.is_null() {
		if version_info > 0 {
			// Info-only outcome (TOO_NEW / UNKNOWN_VERSION): not a hard
			// error, but no project either.
			return Ok(OpenedProject {
				project: None,
				version_info,
			});
		}
		return Err(StorageError(version_info));
	}
	Ok(OpenedProject {
		project: Some(Project::from_raw(ptr)),
		version_info,
	})
}

/// Saves the project to `uri` through the claiming backend.
/// `options` is the quercus-storage save bitmask (0 for defaults).
pub fn save(project: &Project, uri: &str, options: u32) -> Result<(), StorageError> {
	check(unsafe { quercus_storage_save(project.as_raw(), uri.as_ptr(), uri.len(), options) })
}

/// Whether `uri` parses as a storage URI.
pub fn uri_is_valid(uri: &str) -> bool {
	unsafe { quercus_storage_uri_is_valid(uri.as_ptr(), uri.len()) }
}

/// Whether the backend claiming `uri` implements the project-library
/// operations (`projects/create/delete/rename/duplicate/import/export`).
/// A backend may support plain project `open`/`save` without a library.
pub fn library_supported(uri: &str) -> bool {
	unsafe { quercus_storage_library_supported(uri.as_ptr(), uri.len()) }
}

/// A registered backend (name + URI scheme).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendInfo {
	/// The `name` field.
	pub name: String,
	/// The `scheme` field.
	pub scheme: String,
}

/// The registered backends, in registration order (the arbitration
/// order within a scheme).
pub fn backends() -> Vec<BackendInfo> {
	let count = unsafe { quercus_storage_backend_count() }.max(0);
	(0..count)
		.filter_map(|i| {
			Some(BackendInfo {
				name: string_out(|buf, len| unsafe { quercus_storage_backend_name_at(i, buf, len) })?,
				scheme: string_out(|buf, len| unsafe {
					quercus_storage_backend_scheme_at(i, buf, len)
				})?,
			})
		})
		.collect()
}

/// The name of the backend claiming `uri` (None when none does).
pub fn backend_for_uri(uri: &str) -> Option<String> {
	string_out(|buf, len| unsafe { quercus_storage_backend_for_uri(uri.as_ptr(), uri.len(), buf, len) })
}

/// A project in the library (uuid + display name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryProject {
	/// The `uuid` field.
	pub uuid: String,
	/// The `name` field.
	pub name: String,
	/// The `created_at` field.
	pub created_at: i64,
	/// The `modified_at` field.
	pub modified_at: i64,
	/// The `duration_ms` field.
	pub duration_ms: i64,
	/// The `track_count` field.
	pub track_count: i32,
	/// The `clip_count` field.
	pub clip_count: i32,
	/// The `footage_count` field.
	pub footage_count: i32,
}

/// The projects in the library at `uri` (an `quercusdb+sqlite:///…` URI).
pub fn library_projects(uri: &str) -> Result<Vec<LibraryProject>, StorageError> {
	let count = unsafe { quercus_storage_library_count(uri.as_ptr(), uri.len()) };
	if count < 0 {
		return Err(StorageError(count));
	}
	Ok((0..count)
		.map(|i| LibraryProject {
				uuid: string_out(|buf, len| unsafe {
					quercus_storage_library_uuid_at(uri.as_ptr(), uri.len(), i, buf, len)
				})
				.unwrap_or_default(),
				name: string_out(|buf, len| unsafe {
					quercus_storage_library_name_at(uri.as_ptr(), uri.len(), i, buf, len)
				})
				.unwrap_or_default(),
				created_at: unsafe { quercus_storage_library_created_at(uri.as_ptr(), uri.len(), i) },
				modified_at: unsafe { quercus_storage_library_modified_at(uri.as_ptr(), uri.len(), i) },
				duration_ms: unsafe { quercus_storage_library_duration_ms(uri.as_ptr(), uri.len(), i) },
				track_count: unsafe { quercus_storage_library_track_count(uri.as_ptr(), uri.len(), i) },
				clip_count: unsafe { quercus_storage_library_clip_count(uri.as_ptr(), uri.len(), i) },
				footage_count: unsafe { quercus_storage_library_footage_count(uri.as_ptr(), uri.len(), i) },
		})
		.collect())
}

/// Deletes a project from the library.
pub fn library_delete(uri: &str, uuid: &str) -> Result<(), StorageError> {
	check(unsafe { quercus_storage_library_delete(uri.as_ptr(), uri.len(), uuid.as_ptr(), uuid.len()) })
}

/// Renames a project in the library.
pub fn library_rename(uri: &str, uuid: &str, new_name: &str) -> Result<(), StorageError> {
	check(unsafe {
		quercus_storage_library_rename(
			uri.as_ptr(),
			uri.len(),
			uuid.as_ptr(),
			uuid.len(),
			new_name.as_ptr(),
			new_name.len(),
		)
	})
}

/// The configured default library URI (`None` when storage is disabled).
pub fn default_library_uri() -> Option<String> {
	string_out(|buf, len| unsafe { quercus_storage_default_library_uri(buf, len) })
}

/// Flushes every binding and stops the snapshot worker.
pub fn flush() {
	unsafe { quercus_storage_flush() }
}

fn string_result(f: impl Fn(*mut u8, usize) -> i32) -> Result<String, StorageError> {
	// Unlike read-only getters, create/duplicate/import are side-effecting:
	// never use the two-pass "query size, then call again" convention here.
	let mut buf = vec![0u8; 4096];
	let written = f(buf.as_mut_ptr(), buf.len());
	if written < 0 {
		return Err(StorageError(written));
	}
	if written as usize >= buf.len() {
		return Err(StorageError(-1));
	}
	buf.truncate(written as usize);
	String::from_utf8(buf).map_err(|_| StorageError(-1))
}

/// A typed project-library client. Session opening uses the same live
/// project instance that the library and write-through binding share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectLibrary {
	uri: String,
}

impl ProjectLibrary {
	/// Creates a client for `uri` when a registered backend claims it and
	/// implements the library capability. The backend name is irrelevant:
	/// any scheme (including a caller-registered transport) is accepted.
	pub fn new(uri: impl Into<String>) -> Result<Self, StorageError> {
		let uri = uri.into();
		if !uri_is_valid(&uri) || !library_supported(&uri) {
			return Err(StorageError(-1));
		}
		Ok(Self { uri })
	}

	/// The `configured` operation.
	pub fn configured() -> Result<Option<Self>, StorageError> {
		Ok(default_library_uri().map(|uri| Self { uri }))
	}

	/// The `uri` operation.
	pub fn uri(&self) -> &str {
		&self.uri
	}

	/// The `projects` operation.
	pub fn projects(&self) -> Result<Vec<LibraryProject>, StorageError> {
		library_projects(&self.uri)
	}

	/// The `create` operation.
	pub fn create(&self, name: &str) -> Result<String, StorageError> {
		string_result(|buf, len| unsafe {
			quercus_storage_library_create(
				self.uri.as_ptr(),
				self.uri.len(),
				name.as_ptr(),
				name.len(),
				buf,
				len,
			)
		})
	}

	/// The `delete` operation.
	pub fn delete(&self, uuid: &str) -> Result<(), StorageError> {
		library_delete(&self.uri, uuid)
	}

	/// The `rename` operation.
	pub fn rename(&self, uuid: &str, name: &str) -> Result<(), StorageError> {
		library_rename(&self.uri, uuid, name)
	}

	/// The `duplicate` operation.
	pub fn duplicate(&self, uuid: &str) -> Result<String, StorageError> {
		string_result(|buf, len| unsafe {
			quercus_storage_library_duplicate(
				self.uri.as_ptr(),
				self.uri.len(),
				uuid.as_ptr(),
				uuid.len(),
				buf,
				len,
			)
		})
	}

	/// The `import` operation.
	pub fn import(&self, file_uri: &str) -> Result<String, StorageError> {
		string_result(|buf, len| unsafe {
			quercus_storage_library_import(
				self.uri.as_ptr(),
				self.uri.len(),
				file_uri.as_ptr(),
				file_uri.len(),
				buf,
				len,
			)
		})
	}

	/// The `export` operation.
	pub fn export(&self, uuid: &str, file_uri: &str) -> Result<(), StorageError> {
		check(unsafe {
			quercus_storage_library_export(
				self.uri.as_ptr(),
				self.uri.len(),
				uuid.as_ptr(),
				uuid.len(),
				file_uri.as_ptr(),
				file_uri.len(),
			)
		})
	}

	/// Opens the row `uuid` from this client's library (any backend, not
	/// only the configured default library), then binds it for
	/// write-through persistence.
	pub fn open(&self, uuid: &str) -> Result<crate::editor::EditorSession, StorageError> {
		crate::editor::EditorSession::open_project_from_library_uri(&self.uri, uuid)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::state::Config;

	fn storage_config(backend: Option<&str>, path: Option<&str>) {
		Config::global()
			.set("Storage/Backend", backend.unwrap_or("off"))
			.expect("backend config");
		if let Some(path) = path {
			Config::global()
				.set("Storage/SqlitePath", path)
				.expect("sqlite path config");
		}
	}

	#[test]
	fn file_roundtrip_and_enumeration() {
		let names: Vec<String> = backends().iter().map(|b| b.name.clone()).collect();
		assert!(names.iter().any(|n| n == "ove-xml"), "{names:?}");
		assert!(names.iter().any(|n| n == "otio"), "{names:?}");
		assert_eq!(
			backend_for_uri("file:///tmp/x.ove").as_deref(),
			Some("ove-xml")
		);
		assert!(backend_for_uri("strange://nothing").is_none());
		assert!(uri_is_valid("file:///a.ove"));

		let path = std::env::temp_dir()
			.join(format!("quercussdk_storage_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		let uri = format!("file://{path}");
		let project = Project::new();
		save(&project, &uri, 0).expect("save");
		let opened = open(&uri).expect("open");
		assert_eq!(opened.version_info, OK);
		let loaded = opened.project.expect("project");
		assert!(loaded.root().is_valid());
		assert!(open("file:///nonexistent/quercus.ove").is_err());
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn library_on_empty_database() {
		let path = std::env::temp_dir()
			.join(format!("quercussdk_lib_{}.sqlite", std::process::id()))
			.to_string_lossy()
			.into_owned();
		let _ = std::fs::remove_file(&path);
		// Two slashes, not three: the platform path supplies its own root
		// (`/tmp/…` on unix, `C:\…` on Windows — a third slash would make
		// the Windows form root-relative, failing is_absolute).
		let uri = format!("quercusdb+sqlite://{path}");
		assert_eq!(library_projects(&uri).expect("list"), vec![]);
		assert!(library_delete(&uri, "no-such-uuid").is_err());
		assert!(library_rename(&uri, "no-such-uuid", "x").is_err());
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn library_lifecycle_and_live_session() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		flush();
		let path = std::env::temp_dir()
			.join(format!("quercussdk_library_{}.sqlite", std::process::id()))
			.to_string_lossy()
			.into_owned();
		let exported = std::env::temp_dir()
			.join(format!("quercussdk_library_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		let _ = std::fs::remove_file(&path);
		let _ = std::fs::remove_file(&exported);
		storage_config(Some("sqlite"), Some(&path));

		let library = ProjectLibrary::configured()
			.expect("configured library")
			.expect("sqlite enabled");
		assert!(ProjectLibrary::new("not a uri").is_err());
		assert!(library.create("   ").is_err());
		assert!(library.duplicate("missing").is_err());
		assert!(library.import("/definitely/missing/project.ove").is_err());
		assert!(library.export("missing", "/tmp/never.ove").is_err());
		assert!(library.projects().expect("initial rows").is_empty());
		let dropped = crate::editor::EditorSession::create_in_library("Drop Binding")
			.expect("drop-bound row");
		let dropped_uuid = dropped.project().uuid();
		assert!(dropped.storage_bound());
		drop(dropped);
		library.delete(&dropped_uuid).expect("delete after Drop unbinds");
		assert!(library.projects().expect("drop leaves no row").is_empty());

		let session = crate::editor::EditorSession::create_in_library("Library Test")
			.expect("create live row");
		let uuid = session.project().uuid();
		assert_eq!(library.projects().expect("row after create").len(), 1);
		assert!(!uuid.is_empty());
		assert!(session.storage_bound());
		let sequence = session.create_sequence("Sequence").expect("sequence");
		let project = session.project();
		let list = project.sequence_track_list_at(sequence, 0);
		let track = project.tracklist_track_at(list, 0);
		let clip = project.create_clip().expect("clip");
		assert!(project.track_append_block(track, clip));
		assert!(project.block_set_length(clip, crate::types::Rational::new(2, 1)));
		let media = crate::test_support::make_test_clip("library");
		project.import_footage(&media).expect("import footage");

		let rows = library.projects().expect("rows after edit");
		let row = rows.iter().find(|row| row.uuid == uuid).expect("created row");
		assert_eq!(row.name, "Library Test");
		assert_eq!(row.track_count, 4);
		assert_eq!(row.clip_count, 1);
		assert_eq!(row.footage_count, 1);
		assert!(row.duration_ms >= 2_000);
		assert!(row.created_at > 0 && row.modified_at > 0);

		library.rename(&uuid, "Renamed").expect("rename");
		let duplicate = library.duplicate(&uuid).expect("duplicate");
		assert_eq!(library.projects().expect("rows after duplicate").len(), 2);
		assert_ne!(duplicate, uuid);
		library.export(&uuid, &exported).expect("export");
		let imported = library.import(&exported).expect("import");
		assert_eq!(library.projects().expect("rows after import").len(), 3);
		assert_ne!(imported, uuid);

		let expected = session.snapshot();
		let opened = library.open(&duplicate).expect("open duplicate");
		let snapshot = opened.snapshot();
		assert_eq!(snapshot.sequences, expected.sequences);
		assert_eq!(snapshot.footages, expected.footages);
		opened.create_sequence("Copy Only").expect("edit duplicate");
		assert_eq!(opened.snapshot().sequences.len(), 2);
		assert_eq!(session.snapshot().sequences.len(), 1, "duplicate edits stay independent");

		let imported_session = library.open(&imported).expect("open import");
		assert_eq!(imported_session.snapshot().sequences, expected.sequences);
		assert_eq!(imported_session.snapshot().footages, expected.footages);
		let custom = ProjectLibrary::new(format!(
			"quercusdb+sqlite://{}/other.sqlite",
			std::env::temp_dir().display()
		))
		.expect("custom library URI");
		assert!(custom.open(&duplicate).is_err(), "a different library does not contain the row");

		imported_session.close();
		opened.close();
		session.close();
		assert!(!imported_session.storage_bound());
		assert!(!opened.storage_bound());
		assert!(!session.storage_bound());
		for id in [uuid, duplicate, imported] {
			library.delete(&id).expect("delete row");
		}
		let remaining = library.projects().expect("rows after delete");
		assert!(remaining.is_empty(), "remaining rows: {remaining:?}");
		storage_config(Some("off"), None);
		assert!(ProjectLibrary::configured().expect("disabled config").is_none());
		assert!(crate::editor::EditorSession::create_in_library("Disabled").is_err());
		flush();
		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(exported);
		let _ = std::fs::remove_file(media);
	}

	/// A transport that only implements the library capability: exercises
	/// the SDK's `repr(C)` project-info mirror and the library trampolines
	/// end-to-end through the engine.
	struct LibraryTransport;

	impl TransportBackend for LibraryTransport {
		fn name(&self) -> &str {
			"sdk-lib-mock"
		}

		fn scheme(&self) -> &str {
			"sdklibmock"
		}

		fn can_handle(&self, uri: &str) -> bool {
			uri.starts_with("sdklibmock://")
		}

		fn load(&self, _uri: &str) -> Result<Vec<u8>, StorageError> {
			Err(StorageError(-1))
		}

		fn save(&self, _uri: &str, _bytes: &[u8], _options: u32) -> Result<(), StorageError> {
			Ok(())
		}

		fn supports_library(&self) -> bool {
			true
		}

		fn library_list(&self, _uri: &str) -> Result<Vec<TransportProjectInfo>, StorageError> {
			Ok(vec![TransportProjectInfo {
				uuid: "{sdk-lib-row}".to_string(),
				name: "SDK Library Row".to_string(),
				created_at: 1_700_000_000,
				modified_at: 1_700_000_100,
				schema_ver: 1,
			}])
		}
	}

	#[test]
	fn transport_library_capability_round_trips() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let registration = register_transport(LibraryTransport).expect("register transport");
		assert!(library_supported("sdklibmock://library"));
		let library = ProjectLibrary::new("sdklibmock://library").expect("library client");
		let rows = library.projects().expect("library list through the transport");
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].uuid, "{sdk-lib-row}");
		assert_eq!(rows[0].name, "SDK Library Row");
		assert_eq!(rows[0].created_at, 1_700_000_000);
		assert_eq!(rows[0].modified_at, 1_700_000_100);
		drop(registration);
		assert!(
			!library_supported("sdklibmock://library"),
			"an unregistered transport stops claiming its scheme"
		);
	}
}
