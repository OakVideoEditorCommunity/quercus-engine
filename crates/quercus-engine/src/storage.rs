//! C ABI for quercus-storage (prefix `quercus_storage_`): project open/save
//! through the backend registry, backend enumeration, URI validation,
//! and the project library (the database backend).
//!
//! Open/save bridge to the engine's project model: a loaded project
//! arrives as a [`ProjectHandle`](crate::node) (usable with every
//! `quercus_project_*`/`quercus_node_*` entry point); saving takes one back.
//!
//! Return codes: mutations return `QUERCUSSTORAGE_OK` (0) on success or a
//! negative `QUERCUSSTORAGE_E_*` code (quercus-storage's public error codes).
//! Open returns a handle (null on failure) and reports the version
//! info code (0 = OK, positive = TOO_OLD/TOO_NEW/UNKNOWN_VERSION) via
//! an out parameter.
//!
//! Hosts can also register their own byte-transport storage backend over
//! the C ABI with [`quercus_storage_register_transport`] (e.g. S3, HTTP,
//! a database blob column): the engine keeps owning serialization and
//! schema, the transport only stores the opaque project bytes.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use quercus_node::project::Project;
use quercus_storage::backend::{LibraryProjectInfo, LibraryProjectStats, StorageBackend};
use quercus_storage::error::Error;
use crate::ext::storage_transport::{ForeignBackend, QuercusStorageTransportVTable};
use quercus_storage::handle::CHandle;
use quercus_storage::nodeutil;
use quercus_storage::registry::Registry;
use quercus_storage::uri::StorageUri;

use crate::codec::{copy_str_out, str_arg};
use crate::node::ProjectHandle;

/// Maps a quercus-storage error to the public negative `QUERCUSSTORAGE_E_*`
/// code that every C entry point in this module returns.
///
/// The mapping lives in `Error::code()`, so the C ABI and the Rust API agree on
/// one error-number space; this helper only adapts the call to `map_err`.
///
/// # Arguments
///
/// * `e` - the storage error to convert; consumed by value.
///
/// # Returns
///
/// The public error code (`QUERCUSSTORAGE_E_*`; the built-in variants are all
/// negative).
fn code(e: Error) -> i32 {
	e.code()
}

/// Parses and resolves `uri` to its claiming backend.
///
/// Wraps `str_arg` + `StorageUri::parse` + the global registry's `resolve`, and
/// returns the parsed URI alongside the backend so open/save do not parse
/// twice. A resolved backend is not guaranteed to implement the project
/// library; the `library_*` path uses `library_for`/`library_backend`.
///
/// # Errors
///
/// `QUERCUSSTORAGE_E_INVALID` for a null/non-UTF-8 argument or an unparseable
/// URI; otherwise the registry's error code (e.g. no backend claims the scheme).
fn resolve(
	uri: *const u8,
	uri_len: usize,
) -> std::result::Result<
	(
		StorageUri,
		std::sync::Arc<dyn quercus_storage::backend::StorageBackend>,
	),
	i32,
> {
	let Some(text) = (unsafe { str_arg(uri, uri_len) }) else {
		return Err(quercus_storage::error::QUERCUSSTORAGE_E_INVALID);
	};
	let parsed = StorageUri::parse(text).map_err(|_| quercus_storage::error::QUERCUSSTORAGE_E_INVALID)?;
	let backend = Registry::global().resolve(&parsed).map_err(code)?;
	Ok((parsed, backend))
}

/// The configured default write-through library.
pub(crate) fn default_library() -> Result<StorageUri, i32> {
	if !quercus_storage::writethrough::storage_enabled() {
		return Err(quercus_storage::error::QUERCUSSTORAGE_E_INVALID);
	}
	let uri = quercus_storage::writethrough::library_uri()
		.ok_or(quercus_storage::error::QUERCUSSTORAGE_E_INVALID)?;
	StorageUri::parse(&uri).map_err(|_| quercus_storage::error::QUERCUSSTORAGE_E_INVALID)
}

/// A library row URI: the library root with the row selector appended
/// (`<base>?project=<uuid>`). Backends read the `project` query parameter
/// in `load`/`save`; `library_list` keeps receiving the bare root.
pub(crate) fn library_row_uri(base: &StorageUri, uuid: &str) -> Result<StorageUri, i32> {
	StorageUri::parse(&format!("{}?project={uuid}", base.to_uri_string()))
		.map_err(|_| quercus_storage::error::QUERCUSSTORAGE_E_INVALID)
}

/// A default-library URI selecting one project row.
pub(crate) fn project_uri(uuid: &str) -> Result<StorageUri, i32> {
	library_row_uri(&default_library()?, uuid)
}

/// The configured default library URI, snprintf-style; negative when
/// write-through storage is disabled or cannot be resolved.
///
/// The URI is the write-through library root chosen for this process; it is
/// copied into `buf` (truncated and NUL-terminated when `buf_len` is too small)
/// and its full byte length is returned. Pass a null buffer with `buf_len == 0`
/// to query the required size.
///
/// # Arguments
///
/// * `buf` - destination buffer, or null to query the required length.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Returns
///
/// Byte length of the URI string, or a negative `QUERCUSSTORAGE_E_*` code when
/// write-through storage is disabled or the configured URI does not parse.
///
/// # Safety
///
/// `buf` must reference `buf_len` writable bytes (or be null with
/// `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_default_library_uri(buf: *mut u8, buf_len: usize) -> i32 {
	match default_library() {
		Ok(uri) => copy_str_out(&uri.to_uri_string(), buf, buf_len),
		Err(code) => code,
	}
}

/// Opens the project at `uri` through the claiming backend.
///
/// The result is a new engine project handle with ref count 1 (release it with
/// `quercus_project_release`), bridged from the storage layer's project box, or
/// null on failure. `version_info` reports the outcome of the load: 0 for a
/// normal open, a positive TOO_OLD/TOO_NEW/UNKNOWN_VERSION-style code when the
/// backend only reports version info (the handle is then null), or a negative
/// `QUERCUSSTORAGE_E_*` code when loading failed.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the project, resolved through the backend registry.
/// * `uri_len` - byte length of `uri`.
/// * `version_info` - optional out-parameter for the info code; null is allowed
///   and simply discards it.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes, and `version_info` when
/// non-null must be writable for one `i32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_open(
	uri: *const u8,
	uri_len: usize,
	version_info: *mut i32,
) -> *mut ProjectHandle {
	if !version_info.is_null() {
		unsafe { *version_info = 0 };
	}
	let Ok((_uri, backend)) = resolve(uri, uri_len) else {
		return std::ptr::null_mut();
	};
	let result = match backend.load(&_uri) {
		Ok(r) => r,
		Err(e) => {
			if !version_info.is_null() {
				unsafe { *version_info = code(e) };
			}
			return std::ptr::null_mut();
		}
	};
	if !version_info.is_null() {
		unsafe { *version_info = result.version_info };
	}
	if result.project.is_null() {
		return std::ptr::null_mut();
	}
	// Bridge the quercus-storage handle (a box around Arc<Mutex<Project>>)
	// into the engine's project handle: clone the Arc, then release the
	// box.
	let arc = unsafe { nodeutil::project_arc(&result.project) }.ok();
	nodeutil::release_project(result.project);
	match arc {
		Some(inner) => ProjectHandle::from_arc(inner),
		None => std::ptr::null_mut(),
	}
}

/// Saves the project to `uri` through the claiming backend.
///
/// The handle is borrowed for the call only: the engine clones the underlying
/// `Arc`, wraps it for the storage layer and releases the wrapper afterwards,
/// so the caller still owns (and must still release) `project`.
///
/// # Arguments
///
/// * `project` - engine project handle to serialize; null returns
///   `QUERCUSSTORAGE_E_INVALID`.
/// * `uri` - UTF-8 destination URI, resolved through the backend registry.
/// * `uri_len` - byte length of `uri`.
/// * `options` - quercus-storage save bitmask: bit 0 is
///   `QUERCUSSTORAGE_SAVE_COMPRESS` (accepted but not implemented by the
///   built-in file backend) and unknown bits are ignored.
///
/// # Returns
///
/// `QUERCUSSTORAGE_OK` or a negative `QUERCUSSTORAGE_E_*` code (invalid
/// arguments/URI, unresolved backend, or the backend's save error).
///
/// # Safety
///
/// `project` must be a live handle created by this engine and `uri` must
/// reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_save(
	project: *const ProjectHandle,
	uri: *const u8,
	uri_len: usize,
	options: u32,
) -> i32 {
	if project.is_null() {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	}
	let Ok((_uri, backend)) = resolve(uri, uri_len) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let inner = unsafe { &*project }.project().clone();
	let handle = nodeutil::make_project_owned(inner);
	let result = backend.save(handle, &_uri, options);
	nodeutil::release_project(handle);
	match result {
		Ok(()) => quercus_storage::error::QUERCUSSTORAGE_OK,
		Err(e) => code(e),
	}
}

/// Whether `uri` parses as a storage URI.
///
/// Lenient, parse-only check: `scheme://body` is accepted when the prefix is a
/// valid RFC 3986 scheme, and anything else (including a bare absolute or
/// relative path) is accepted as a `file://` path. Only the empty string is
/// rejected — null/non-UTF-8 bytes also return `false`, so there is no separate
/// error value. Registration and target existence are not checked; use
/// `quercus_storage_backend_for_uri` for resolvability.
///
/// # Arguments
///
/// * `uri` - UTF-8 candidate URI bytes; no NUL terminator is required.
/// * `uri_len` - byte length of `uri`.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes, or be null (which yields
/// `false`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_uri_is_valid(uri: *const u8, uri_len: usize) -> bool {
	match unsafe { str_arg(uri, uri_len) } {
		Some(text) => StorageUri::parse(text).is_ok(),
		None => false,
	}
}

/// Number of registered backends (built-ins are pre-registered).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_storage_backend_count() -> i32 {
	Registry::global().list().len() as i32
}

/// Shared body of `..._backend_name_at` and `..._backend_scheme_at`: copies
/// field 0 (name) or field 1 (scheme) of the backend at `index` out of the
/// registry's registration-order list through `copy_str_out`.
///
/// Returns -1 for a negative/out-of-range index, otherwise the full string
/// length (snprintf semantics, so a short `buf` only truncates the copy).
fn backend_string_at(index: i32, buf: *mut u8, buf_len: usize, field: usize) -> i32 {
	if index < 0 {
		return -1;
	}
	let list = Registry::global().list();
	let Some(entry) = list.get(index as usize) else {
		return -1;
	};
	let s = if field == 0 { &entry.0 } else { &entry.1 };
	copy_str_out(s, buf, buf_len)
}

/// The name of the backend at `index` (registration order), snprintf-style.
///
/// Use the name to match a user-configured backend; the paired scheme getter is
/// [`quercus_storage_backend_scheme_at`]. Returns -1 when `index` is negative or
/// out of range.
///
/// # Arguments
///
/// * `index` - zero-based position in the registration-order list.
/// * `buf` - destination buffer, or null with `buf_len == 0` to query.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Safety
///
/// `buf` must reference `buf_len` writable bytes (or be null with
/// `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_backend_name_at(
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	backend_string_at(index, buf, buf_len, 0)
}

/// The URI scheme of the backend at `index`, snprintf-style.
///
/// `index` is a position in the registry's registration-order list, so it pairs
/// with [`quercus_storage_backend_name_at`] only while the registry is not
/// modified in between. Returns -1 when `index` is negative or out of range.
///
/// # Arguments
///
/// * `index` - zero-based position in the registration-order list.
/// * `buf` - destination buffer, or null with `buf_len == 0` to query.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Safety
///
/// `buf` must reference `buf_len` writable bytes (or be null with
/// `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_backend_scheme_at(
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	backend_string_at(index, buf, buf_len, 1)
}

/// The name of the backend that claims `uri`, snprintf-style.
///
/// Copies the registered backend name into `buf` and returns its full byte
/// length; -1 when the argument is null/non-UTF-8, the URI does not parse, or
/// no backend claims the scheme. Unlike `quercus_storage_library_supported`
/// this only reports the claim, not the library capability.
///
/// # Arguments
///
/// * `uri` - UTF-8 candidate URI.
/// * `uri_len` - byte length of `uri`.
/// * `buf` - destination buffer, or null with `buf_len == 0` to query.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes and `buf` must reference
/// `buf_len` writable bytes (or be null with `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_backend_for_uri(
	uri: *const u8,
	uri_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(text) = (unsafe { str_arg(uri, uri_len) }) else {
		return -1;
	};
	let Ok(parsed) = StorageUri::parse(text) else {
		return -1;
	};
	match Registry::global().resolve(&parsed) {
		Ok(backend) => copy_str_out(backend.name(), buf, buf_len),
		Err(_) => -1,
	}
}

// ---------------------------------------------------------------------------
// Foreign transports (C vtable registration)
// ---------------------------------------------------------------------------

/// Registered foreign transports, indexed by the slot handed to the
/// caller. A slot is never reused: unregistration leaves `None` in place.
static TRANSPORTS: Mutex<Vec<Option<Arc<ForeignBackend>>>> = Mutex::new(Vec::new());

fn transports() -> MutexGuard<'static, Vec<Option<Arc<ForeignBackend>>>> {
	TRANSPORTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registers a foreign byte transport and returns its slot index (>= 0),
/// or a negative `QUERCUSSTORAGE_E_*` code.
///
/// The engine copies the vtable and its `name`/`scheme` strings, so the
/// caller's vtable and strings need not outlive the call. `userdata` stays
/// caller-owned and must remain alive and thread-safe until
/// `quercus_storage_unregister_transport(slot)` has returned. Reserved
/// schemes (`file`, `quercusdb`, `quercusdb+*`) are rejected with
/// `QUERCUSSTORAGE_E_INVALID`; a duplicate backend name with
/// `QUERCUSSTORAGE_E_STATE`.
///
/// # Safety
/// `vt` must point to a readable `QuercusStorageTransportVTable` for the
/// duration of this call (it is copied), with valid NUL-terminated
/// `name`/`scheme` strings. `userdata` is opaque to the engine and is
/// passed through to the transport callbacks untouched.
// The vtable type is deliberately crate-private: the C symbol/signature is
// the public surface, and the Rust type only exists for the adapter itself.
#[allow(private_interfaces)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_register_transport(
	vt: *const QuercusStorageTransportVTable,
	userdata: *mut c_void,
) -> i32 {
	// Validates the pointer, ABI version/size, callbacks, name/scheme and
	// reserved schemes; returns QUERCUSSTORAGE_E_INVALID on any failure.
	let backend = match unsafe { ForeignBackend::new(vt, userdata) } {
		Ok(backend) => Arc::new(backend),
		Err(e) => return e.code(),
	};
	// The registry is the arbitration point and also rejects duplicate
	// names (E_STATE), so register before publishing the slot.
	if let Err(e) = Registry::global().register(backend.clone()) {
		return e.code();
	}
	let mut slots = transports();
	slots.push(Some(backend));
	// `push` above means the length is at least one.
	let index = slots.len() - 1;
	let Ok(slot) = i32::try_from(index) else {
		// Rolling back an unreachable overflow: 2^31 registrations.
		let backend = slots.pop().flatten();
		drop(slots);
		if let Some(backend) = backend {
			let _ = Registry::global().unregister(backend.name());
		}
		return quercus_storage::error::QUERCUSSTORAGE_E_STATE;
	};
	slot
}

/// Unregisters the foreign transport at `slot` and waits for its in-flight
/// callbacks to finish. Returns false for an unknown or already
/// unregistered slot.
///
/// Resolution stops finding the backend immediately. The wait (a write
/// acquire of the transport's gate) may block for as long as one in-flight
/// network load/save takes, so after this returns the caller may free its
/// `userdata` safely.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_storage_unregister_transport(slot: i32) -> bool {
	let backend = {
		let mut slots = transports();
		let Ok(index) = usize::try_from(slot) else {
			return false;
		};
		match slots.get_mut(index) {
			Some(entry) => entry.take(),
			None => None,
		}
	};
	let Some(backend) = backend else {
		return false;
	};
	// Remove from the registry first (no new resolutions pick the
	// transport), then drain callbacks that already resolved it.
	let _ = Registry::global().unregister(backend.name());
	backend.wait_idle();
	true
}

// ---------------------------------------------------------------------------
// Project library (backend-neutral: everything resolves through the registry)
// ---------------------------------------------------------------------------

/// Parses a library URI argument and resolves the backend that provides it
/// (built-in database or a foreign transport).
///
/// The library-side peer of `resolve`: it returns the parsed root URI together
/// with the claiming backend so callers can build row URIs
/// (`<root>?project=<uuid>`) without parsing twice.
///
/// # Arguments
///
/// * `uri` - UTF-8 bytes of the library root URI.
/// * `uri_len` - byte length of `uri`.
///
/// # Errors
///
/// `QUERCUSSTORAGE_E_INVALID` for a null/non-UTF-8 argument or an unparseable
/// URI; otherwise the registry's resolution error code (no backend claims the
/// scheme).
fn library_for(
	uri: *const u8,
	uri_len: usize,
) -> std::result::Result<(StorageUri, Arc<dyn StorageBackend>), i32> {
	let Some(text) = (unsafe { str_arg(uri, uri_len) }) else {
		return Err(quercus_storage::error::QUERCUSSTORAGE_E_INVALID);
	};
	let parsed = StorageUri::parse(text).map_err(|_| quercus_storage::error::QUERCUSSTORAGE_E_INVALID)?;
	let backend = quercus_storage::writethrough::library_backend(&parsed).map_err(code)?;
	Ok((parsed, backend))
}

/// The library rows of `uri` (display name, uuid, timestamps) in the order the
/// backend returns them; the engine does not re-sort them.
///
/// Fails with the negative `QUERCUSSTORAGE_E_*` code when the URI is invalid,
/// no backend provides the library, or `library_list` fails. All index-based
/// library getters (`library_info`, `library_string_at`, `library_stats`) go
/// through this listing.
fn library_projects(
	uri: *const u8,
	uri_len: usize,
) -> std::result::Result<Vec<LibraryProjectInfo>, i32> {
	let (parsed, backend) = library_for(uri, uri_len)?;
	backend.library_list(&parsed).map_err(code)
}

/// Loads the library row `uuid` through `backend`; the returned handle is
/// owned (ref count 1) and must be released with
/// [`quercus_storage::nodeutil::release_project`].
pub(crate) fn load_library_row(
	backend: &dyn StorageBackend,
	base: &StorageUri,
	uuid: &str,
) -> std::result::Result<CHandle, Error> {
	let row = library_row_uri(base, uuid).map_err(|_| Error::Invalid)?;
	let result = backend.load(&row)?;
	if result.project.is_null() {
		return Err(Error::Format(format!(
			"library row {uuid} did not load (info code {})",
			result.version_info
		)));
	}
	Ok(result.project)
}

/// Statistics for one library row: the backend's own query when it
/// implements one, otherwise `load` + on-the-fly graph derivation
/// (`Error::Unsupported` is the capability signal, not a failure).
pub(crate) fn library_project_stats(
	backend: &dyn StorageBackend,
	base: &StorageUri,
	uuid: &str,
) -> Option<LibraryProjectStats> {
	match backend.library_stats(base, uuid) {
		Ok(stats) => Some(stats),
		Err(Error::Unsupported) => {
			let handle = load_library_row(backend, base, uuid).ok()?;
			let arc = unsafe { nodeutil::project_arc(&handle) };
			nodeutil::release_project(handle);
			let arc = arc.ok()?;
			let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
			Some(quercus_storage::backend::derive_library_stats(&guard))
		}
		Err(_) => None,
	}
}

/// A fresh uuid in the project text format. quercusnode's generator is
/// private, so a throwaway `Project` supplies one without duplicating the
/// format here.
fn fresh_project_uuid() -> String {
	let project = Project::new();
	project
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.uuid
		.clone()
}

/// Creates a blank project row in the library at `base` and returns its
/// uuid. Generic composition: a new initialized `Project` is saved through
/// the library backend at `<base>?project=<uuid>` (no backend-specific
/// "create" operation exists).
pub(crate) fn create_library_project(
	base: &StorageUri,
	name: &str,
) -> std::result::Result<String, Error> {
	let trimmed = name.trim();
	if trimmed.is_empty() {
		return Err(Error::Invalid);
	}
	let backend = quercus_storage::writethrough::library_backend(base)?;
	let project = Project::new();
	let uuid = {
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		guard.initialize().map_err(|_| Error::State)?;
		guard
			.settings
			.insert("projectname".into(), trimmed.into());
		guard.uuid.clone()
	};
	let row = library_row_uri(base, &uuid).map_err(|_| Error::Invalid)?;
	let handle = nodeutil::make_project_owned(project);
	let outcome = backend.save(handle, &row, 0);
	nodeutil::release_project(handle);
	outcome?;
	Ok(uuid)
}

/// Duplicates the library row `uuid` as a new row in `base` and returns
/// the new uuid. Generic composition: load the source, `deep_copy` it,
/// assign a fresh uuid and the default name `<name> (copy)`, then save at
/// the new row URI.
///
/// Unlike a journaled backend's native duplicate, this copy carries **no
/// history**: whole-document backends have no journal to copy, and the
/// generic path must produce the same shape for every backend.
pub(crate) fn duplicate_library_project(
	base: &StorageUri,
	uuid: &str,
) -> std::result::Result<String, Error> {
	let backend = quercus_storage::writethrough::library_backend(base)?;
	let handle = load_library_row(backend.as_ref(), base, uuid)?;
	let arc = unsafe { nodeutil::project_arc(&handle) };
	nodeutil::release_project(handle);
	let arc = arc?;
	// The library display name is authoritative (rename updates only the
	// row name); fall back to the in-project name.
	let display_name = backend
		.library_list(base)
		.ok()
		.and_then(|rows| rows.into_iter().find(|row| row.uuid == uuid))
		.map(|row| row.name)
		.unwrap_or_else(|| {
			let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
			guard
				.settings
				.get("projectname")
				.cloned()
				.unwrap_or_else(|| "Untitled Project".to_string())
		});
	let copy = {
		let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
		guard.deep_copy().map_err(|_| Error::State)?
	};
	let new_uuid = fresh_project_uuid();
	{
		let mut guard = copy.lock().unwrap_or_else(|e| e.into_inner());
		guard.uuid = new_uuid.clone();
		guard
			.settings
			.insert("projectname".into(), format!("{display_name} (copy)"));
		guard.modified = false;
	}
	let row = library_row_uri(base, &new_uuid).map_err(|_| Error::Invalid)?;
	let handle = nodeutil::make_project_owned(copy);
	let outcome = backend.save(handle, &row, 0);
	nodeutil::release_project(handle);
	outcome?;
	Ok(new_uuid)
}

/// Imports the project at `source` as a new library row in `base` and
/// returns the new uuid. Generic composition: resolve the source through
/// the registry, load it, assign a fresh uuid (the project keeps its own
/// name), then save at `<base>?project=<uuid>`.
pub(crate) fn import_library_project(
	base: &StorageUri,
	source: &StorageUri,
) -> std::result::Result<String, Error> {
	let library = quercus_storage::writethrough::library_backend(base)?;
	let source_backend = Registry::global().resolve(source)?;
	let result = source_backend.load(source)?;
	if result.project.is_null() {
		return Err(Error::Format(format!(
			"cannot import {}: backend reported info code {}",
			source.to_uri_string(),
			result.version_info
		)));
	}
	let handle = result.project;
	let uuid = fresh_project_uuid();
	let outcome = (|| {
		let arc = unsafe { nodeutil::project_arc(&handle) }?;
		{
			let mut guard = arc.lock().map_err(|_| Error::State)?;
			guard.uuid = uuid.clone();
		}
		let row = library_row_uri(base, &uuid).map_err(|_| Error::Invalid)?;
		library.save(handle, &row, 0)
	})();
	nodeutil::release_project(handle);
	outcome?;
	Ok(uuid)
}

/// Exports the library row `uuid` to `target` through whichever backend
/// claims it — the registry decides, so the target need not be `file://`.
pub(crate) fn export_library_project(
	base: &StorageUri,
	uuid: &str,
	target: &StorageUri,
) -> std::result::Result<(), Error> {
	let library = quercus_storage::writethrough::library_backend(base)?;
	let handle = load_library_row(library.as_ref(), base, uuid)?;
	let outcome = match Registry::global().resolve(target) {
		Ok(target_backend) => target_backend.save(handle, target, 0),
		Err(e) => Err(e),
	};
	nodeutil::release_project(handle);
	outcome
}

/// Whether the backend claiming `uri` implements the project library (a
/// backend may support plain project open/save without one).
///
/// Capability probe only: null/non-UTF-8 bytes, an unparseable URI, an
/// unclaimed scheme and a backend whose `supports_library()` is `false` all
/// return `false`, so callers should treat `false` as "no project library
/// here" rather than as a specific failure.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `uri_len` - byte length of `uri`.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_supported(uri: *const u8, uri_len: usize) -> bool {
	let Some(text) = (unsafe { str_arg(uri, uri_len) }) else {
		return false;
	};
	let Ok(parsed) = StorageUri::parse(text) else {
		return false;
	};
	match quercus_storage::writethrough::library_backend(&parsed) {
		Ok(backend) => backend.supports_library(),
		Err(_) => false,
	}
}

/// Number of projects in the library at `uri`.
///
/// This is the length of the backend's `library_list` result, i.e. the valid
/// index range for the `..._library_*_at` getters is `0..count`.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
///
/// # Returns
///
/// The row count, or a negative `QUERCUSSTORAGE_E_*` code (null/non-UTF-8
/// argument, unparseable URI, unresolved/unavailable library backend, or a
/// failed listing).
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_count(uri: *const u8, uri_len: usize) -> i32 {
	match library_projects(uri, uri_len) {
		Ok(projects) => projects.len() as i32,
		Err(c) => c,
	}
}

/// Shared body of `..._library_name_at` and `..._library_uuid_at`: copies
/// either the display name (`name == true`) or the uuid of the library row at
/// `index` out through `copy_str_out`.
///
/// Returns -1 for a negative/out-of-range index, an unreadable or unparseable
/// URI, an unresolved library, or a failed listing; otherwise the full string
/// length (snprintf semantics, so a short `buf` only truncates the copy).
fn library_string_at(
	uri: *const u8,
	uri_len: usize,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
	name: bool,
) -> i32 {
	if index < 0 {
		return -1;
	}
	let Ok(projects) = library_projects(uri, uri_len) else {
		return -1;
	};
	let Some(info) = projects.get(index as usize) else {
		return -1;
	};
	let s = if name { &info.name } else { &info.uuid };
	copy_str_out(s, buf, buf_len)
}

/// The uuid of the library project at `index`, snprintf-style.
///
/// Copies the backend's row uuid into `buf` and returns its full byte length;
/// `buf` may be null with `buf_len == 0` to query the required size. The uuid is
/// the stable identifier accepted by the delete/rename/duplicate entry points.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `index` - zero-based offset in the backend's listing order.
/// * `buf` - destination buffer of `buf_len` bytes, or null to query.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Returns
///
/// Byte length of the uuid, or -1 when `index` is negative/out of range, the
/// URI/library cannot be resolved, or the listing fails.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes and `buf` must reference
/// `buf_len` writable bytes (or be null with `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_uuid_at(
	uri: *const u8,
	uri_len: usize,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	library_string_at(uri, uri_len, index, buf, buf_len, false)
}

/// The display name of the library project at `index`, snprintf-style.
///
/// Copies the backend's row name into `buf` and returns its full byte length;
/// `buf` may be null with `buf_len == 0` to query the required size.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `index` - zero-based offset in the backend's listing order.
/// * `buf` - destination buffer of `buf_len` bytes, or null to query.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Returns
///
/// Byte length of the name, or -1 when `index` is negative/out of range, the
/// URI/library cannot be resolved, or the listing fails.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes and `buf` must reference
/// `buf_len` writable bytes (or be null with `buf_len == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_name_at(
	uri: *const u8,
	uri_len: usize,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	library_string_at(uri, uri_len, index, buf, buf_len, true)
}

/// Deletes the project `uuid` from the library at `uri`.
///
/// Removes the row's stored project and, on the journaled backend, its
/// journal/snapshot history; the library root itself is untouched. A uuid that
/// does not exist surfaces as the backend's not-found error.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `uuid` - uuid of the row to delete.
/// * `uri_len` / `uuid_len` - byte lengths of the arguments.
///
/// # Returns
///
/// `QUERCUSSTORAGE_OK` or a negative `QUERCUSSTORAGE_E_*` code (`E_INVALID`
/// for null/non-UTF-8 arguments or an unparseable URI, `E_NOT_FOUND` for a
/// missing row, or the backend's failure code).
///
/// # Safety
///
/// Each `(ptr, len)` pair must reference `len` readable bytes valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_delete(
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
) -> i32 {
	let (Some(uri), Some(uuid)) = (unsafe { str_arg(uri, uri_len) }, unsafe {
		str_arg(uuid, uuid_len)
	}) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let Ok(parsed) = StorageUri::parse(uri) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let backend = match quercus_storage::writethrough::library_backend(&parsed) {
		Ok(backend) => backend,
		Err(e) => return code(e),
	};
	match backend.library_delete(&parsed, uuid) {
		Ok(()) => quercus_storage::error::QUERCUSSTORAGE_OK,
		Err(e) => code(e),
	}
}

/// Renames the project `uuid` in the library at `uri`.
///
/// Only the library row metadata changes: the project payload is not
/// re-saved, so the in-project `projectname` setting keeps its old value.
/// The name is stored verbatim (no emptiness check at this layer); a uuid that
/// no longer exists surfaces as the backend's not-found error.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `uuid` - uuid of the row to rename.
/// * `new_name` - new display name, UTF-8.
/// * `uri_len` / `uuid_len` / `new_name_len` - byte lengths of the arguments.
///
/// # Returns
///
/// `QUERCUSSTORAGE_OK` or a negative `QUERCUSSTORAGE_E_*` code (`E_INVALID`
/// for null/non-UTF-8 arguments or an unparseable URI, `E_NOT_FOUND` for a
/// missing row, or the backend's failure code).
///
/// # Safety
///
/// Each `(ptr, len)` pair must reference `len` readable bytes valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_rename(
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	new_name: *const u8,
	new_name_len: usize,
) -> i32 {
	let (Some(uri), Some(uuid), Some(new_name)) = (
		unsafe { str_arg(uri, uri_len) },
		unsafe { str_arg(uuid, uuid_len) },
		unsafe { str_arg(new_name, new_name_len) },
	) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let Ok(parsed) = StorageUri::parse(uri) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let backend = match quercus_storage::writethrough::library_backend(&parsed) {
		Ok(backend) => backend,
		Err(e) => return code(e),
	};
	match backend.library_rename(&parsed, uuid, new_name) {
		Ok(()) => quercus_storage::error::QUERCUSSTORAGE_OK,
		Err(e) => code(e),
	}
}

/// Creation time of the library row at `index`, as Unix seconds (UTC); -1 when
/// the row or its metadata is unavailable.
///
/// The companion timestamp is [`quercus_storage_library_modified_at`]; both
/// read the same `library_list` entry, so one listing serves either getter.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_created_at(uri: *const u8, uri_len: usize, index: i32) -> i64 {
	library_info(uri, uri_len, index).map_or(-1, |info| info.created_at)
}

/// Last-modified time of the library row at `index`, as Unix seconds (UTC);
/// -1 when the row or its metadata is unavailable.
///
/// The value is the backend's recorded timestamp (`modified_at` column on the
/// journaled backend), not the project's in-memory dirty flag.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_modified_at(uri: *const u8, uri_len: usize, index: i32) -> i64 {
	library_info(uri, uri_len, index).map_or(-1, |info| info.modified_at)
}

/// The library row metadata at `index` (display name, uuid, created/modified
/// timestamps), or `None` when `index` is negative/out of range, the URI or
/// library cannot be resolved, or the listing fails.
///
/// This is a lookup in the backend's `library_list` result, so `index` is an
/// offset in the backend's own ordering; `quercus_storage_library_name_at`,
/// `..._uuid_at`, `..._created_at` and `..._modified_at` all go through it.
fn library_info(uri: *const u8, uri_len: usize, index: i32) -> Option<LibraryProjectInfo> {
	if index < 0 {
		return None;
	}
	library_projects(uri, uri_len)
		.ok()?
		.into_iter()
		.nth(index as usize)
}

/// Stats of the library row at `index`, or `None` when `index` is out of range,
/// the URI cannot be parsed, no backend provides the library, the row uuid is
/// not in the listing, or the backend fails a non-`Unsupported` stats query.
///
/// The uuid comes from the library listing, so a row that is listed but no
/// longer loads also yields `None`. The four stats getters
/// (`..._library_footage_count`, `..._clip_count`, `..._track_count`,
/// `..._library_duration_ms`) are thin wrappers over this.
fn library_stats(uri: *const u8, uri_len: usize, index: i32) -> Option<LibraryProjectStats> {
	let info = library_info(uri, uri_len, index)?;
	let text = unsafe { str_arg(uri, uri_len) }?;
	let parsed = StorageUri::parse(text).ok()?;
	let backend = quercus_storage::writethrough::library_backend(&parsed).ok()?;
	library_project_stats(backend.as_ref(), &parsed, &info.uuid)
}

/// Duration of the library row at `index` in milliseconds, derived from the
/// row's project graph (backend stats query or replay, see `library_stats`).
///
/// Returns 0 when the row or its stats are unavailable, which is also the
/// legitimate value for an empty project.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_duration_ms(uri: *const u8, uri_len: usize, index: i32) -> i64 {
	library_stats(uri, uri_len, index).map_or(0, |stats| stats.duration_ms)
}

/// Number of tracks in the library row at `index`, derived from the row's
/// project graph (backend stats query or replay, see `library_stats`).
///
/// Returns 0 when the row or its stats are unavailable.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_track_count(uri: *const u8, uri_len: usize, index: i32) -> i32 {
	library_stats(uri, uri_len, index).map_or(0, |stats| stats.track_count)
}

/// Number of clips in the library row at `index`, derived from the row's
/// project graph (backend stats query or replay, see `library_stats`).
///
/// Returns 0 when the row or its stats are unavailable.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_clip_count(uri: *const u8, uri_len: usize, index: i32) -> i32 {
	library_stats(uri, uri_len, index).map_or(0, |stats| stats.clip_count)
}

/// Number of footage items in the library row at `index`, derived from the
/// row's project graph (backend stats query or replay, see `library_stats`).
///
/// Returns 0 when the row or its stats are unavailable, which is also the
/// legitimate value for a project without footage.
///
/// # Safety
///
/// `uri` must reference `uri_len` readable bytes valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_footage_count(uri: *const u8, uri_len: usize, index: i32) -> i32 {
	library_stats(uri, uri_len, index).map_or(0, |stats| stats.footage_count)
}

/// Creates a blank project row in the library at `uri` and returns its uuid.
///
/// The row is a freshly initialized `Project` whose `projectname` setting is
/// the trimmed `name`; it is saved through the library backend at
/// `<uri>?project=<new uuid>`, so the library listing picks it up.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `name` - UTF-8 display name; leading/trailing whitespace is trimmed and
///   an empty or whitespace-only name is rejected with `E_INVALID`.
/// * `uuid_buf` - snprintf-style output buffer for the new uuid (see
///   [`quercus_storage_library_import`] for the truncation rules).
/// * `uuid_buf_len` - capacity of `uuid_buf` in bytes.
///
/// # Returns
///
/// The byte length of the new uuid string, or a negative `QUERCUSSTORAGE_E_*`
/// code (invalid arguments/URI, empty name, or a failed save).
///
/// # Safety
///
/// Each input `(ptr, len)` pair must reference `len` readable bytes, and
/// `uuid_buf` must reference `uuid_buf_len` writable bytes (or be null); all
/// buffers must stay valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_create(
	uri: *const u8,
	uri_len: usize,
	name: *const u8,
	name_len: usize,
	uuid_buf: *mut u8,
	uuid_buf_len: usize,
) -> i32 {
	let (Some(uri), Some(name)) = (unsafe { str_arg(uri, uri_len) }, unsafe {
		str_arg(name, name_len)
	}) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	if name.trim().is_empty() {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	}
	let Ok(uri) = StorageUri::parse(uri) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	match create_library_project(&uri, name) {
		Ok(uuid) => copy_str_out(&uuid, uuid_buf, uuid_buf_len),
		Err(e) => code(e),
	}
}

/// Duplicates library row `uuid` and reports the uuid of the new row.
///
/// Generic composition (load + deep copy + save), so the copy carries no
/// journal history; the new row is named `<display name> (copy)` and gets a
/// fresh uuid. See `duplicate_library_project` for the generic path.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root.
/// * `uuid` - uuid of the source row.
/// * `uri_len` / `uuid_len` - byte lengths of the two arguments.
/// * `new_uuid_buf` - snprintf-style output buffer for the new uuid (see
///   [`quercus_storage_library_import`] for the truncation rules).
/// * `new_uuid_buf_len` - capacity of `new_uuid_buf` in bytes.
///
/// # Returns
///
/// The byte length of the new uuid string, or a negative `QUERCUSSTORAGE_E_*`
/// code (invalid arguments, missing source row, or a failed save).
///
/// # Safety
///
/// Each input `(ptr, len)` pair must reference `len` readable bytes, and
/// `new_uuid_buf` must reference `new_uuid_buf_len` writable bytes (or be
/// null); all buffers must stay valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_duplicate(
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	new_uuid_buf: *mut u8,
	new_uuid_buf_len: usize,
) -> i32 {
	let (Some(uri), Some(uuid)) = (unsafe { str_arg(uri, uri_len) }, unsafe {
		str_arg(uuid, uuid_len)
	}) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let Ok(uri) = StorageUri::parse(uri) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	match duplicate_library_project(&uri, uuid) {
		Ok(new_uuid) => copy_str_out(&new_uuid, new_uuid_buf, new_uuid_buf_len),
		Err(e) => code(e),
	}
}

/// Imports the project at `file_uri` as a new row in the library at `uri` and
/// reports the new row uuid through `new_uuid_buf`.
///
/// The source is resolved like any project open (so it may be `file://`, a
/// foreign transport URI, ...). The loaded project keeps its own content and
/// in-project name but is assigned a fresh uuid, then saved whole to the
/// library row `<uri>?project=<uuid>`. Same composition as
/// `import_library_project`.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the target library root.
/// * `file_uri` - UTF-8 URI of the project to import.
/// * `uri_len` / `file_uri_len` - byte lengths of the two arguments.
/// * `new_uuid_buf` - snprintf-style output buffer for the new uuid (truncated
///   and NUL-terminated when `new_uuid_buf_len` is too small; null with 0 to
///   only query the required length).
/// * `new_uuid_buf_len` - capacity of `new_uuid_buf` in bytes.
///
/// # Returns
///
/// The byte length of the new uuid string, or a negative `QUERCUSSTORAGE_E_*`
/// code on any failure.
///
/// # Safety
///
/// Each input `(ptr, len)` pair must reference `len` readable bytes, and
/// `new_uuid_buf` must reference `new_uuid_buf_len` writable bytes (or be
/// null); all buffers must stay valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_import(
	uri: *const u8,
	uri_len: usize,
	file_uri: *const u8,
	file_uri_len: usize,
	new_uuid_buf: *mut u8,
	new_uuid_buf_len: usize,
) -> i32 {
	let (Some(uri), Some(file_uri)) = (unsafe { str_arg(uri, uri_len) }, unsafe {
		str_arg(file_uri, file_uri_len)
	}) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let (Ok(uri), Ok(file_uri)) = (StorageUri::parse(uri), StorageUri::parse(file_uri)) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	match import_library_project(&uri, &file_uri) {
		Ok(uuid) => copy_str_out(&uuid, new_uuid_buf, new_uuid_buf_len),
		Err(e) => code(e),
	}
}

/// Exports library row `uuid` from the library at `uri` to `file_uri`.
///
/// The destination is resolved through the backend registry, so it need not
/// be a `file://` URI: whichever backend claims `file_uri` performs the save
/// and therefore picks the format. The row is loaded in full through the
/// library backend, saved to the target with save options `0`, and released;
/// the library itself is not modified. Same composition as
/// `export_library_project`.
///
/// # Arguments
///
/// * `uri` - UTF-8 URI of the library root (not a row URI).
/// * `uuid` - uuid of the row to export.
/// * `file_uri` - UTF-8 destination URI claimed by a registered backend.
///
/// # Returns
///
/// `QUERCUSSTORAGE_OK` on success, otherwise a negative `QUERCUSSTORAGE_E_*`
/// code: null/non-UTF-8 arguments, an unparseable URI, an unresolved library
/// or target backend, a missing row, or the target backend's save error.
///
/// # Safety
///
/// Every `(ptr, len)` pair must reference `len` readable bytes valid for the
/// duration of the call; the string data is borrowed only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_storage_library_export(
	uri: *const u8,
	uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	file_uri: *const u8,
	file_uri_len: usize,
) -> i32 {
	let (Some(uri), Some(uuid), Some(file_uri)) = (
		unsafe { str_arg(uri, uri_len) },
		unsafe { str_arg(uuid, uuid_len) },
		unsafe { str_arg(file_uri, file_uri_len) },
	) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	let (Ok(uri), Ok(file_uri)) = (StorageUri::parse(uri), StorageUri::parse(file_uri)) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	match export_library_project(&uri, uuid, &file_uri) {
		Ok(()) => quercus_storage::error::QUERCUSSTORAGE_OK,
		Err(e) => code(e),
	}
}

/// Flushes all write-through bindings and stops the snapshot worker.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_storage_flush() {
	quercus_storage::writethrough::flush_all();
}

#[cfg(test)]
mod tests {
	use super::*;

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	/// Built-in backends are pre-registered and a file URI resolves to
	/// one of them; an unknown scheme resolves to nothing.
	#[test]
	fn backend_enumeration_and_resolution() {
		unsafe {
			let count = quercus_storage_backend_count();
			assert!(count >= 2, "built-ins pre-registered: {count}");
			let mut names = Vec::new();
			let mut buf = [0u8; 64];
			for i in 0..count {
				let n = quercus_storage_backend_name_at(i, buf.as_mut_ptr(), buf.len());
				assert!(n > 0);
				names.push(std::str::from_utf8(&buf[..n as usize]).unwrap().to_string());
				assert!(quercus_storage_backend_scheme_at(i, buf.as_mut_ptr(), buf.len()) > 0);
			}
			assert!(names.iter().any(|n| n == "ove-xml"), "{names:?}");
			assert!(names.iter().any(|n| n == "otio"), "{names:?}");
			assert_eq!(quercus_storage_backend_name_at(-1, buf.as_mut_ptr(), 64), -1);
			assert_eq!(quercus_storage_backend_name_at(count, buf.as_mut_ptr(), 64), -1);

			let (u, ul) = arg("file:///tmp/x.ove");
			let n = quercus_storage_backend_for_uri(u, ul, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..n as usize]).unwrap(), "ove-xml");
			let (u, ul) = arg("strange://nothing");
			assert_eq!(
				quercus_storage_backend_for_uri(u, ul, buf.as_mut_ptr(), buf.len()),
				-1
			);

			assert!(quercus_storage_uri_is_valid(u, ul));
			assert!(quercus_storage_uri_is_valid("file:///a.ove".as_ptr(), 13));
			assert!(!quercus_storage_uri_is_valid(std::ptr::null(), 0));
		}
	}

	/// Save + open round-trip through the file backend: the project
	/// crosses the C ABI as an engine handle on both directions.
	#[test]
	fn file_save_open_roundtrip() {
		unsafe {
			let path = std::env::temp_dir()
				.join(format!("quercusengine_storage_{}.ove", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let uri = format!("file://{path}");
			let (u, ul) = arg(&uri);

			let project = crate::node::quercus_project_new();
			assert!(!project.is_null());
			assert_eq!(
				quercus_storage_save(project, u, ul, 0),
				quercus_storage::error::QUERCUSSTORAGE_OK
			);

			let mut info = -1i32;
			let loaded = quercus_storage_open(u, ul, &mut info);
			assert!(!loaded.is_null());
			assert_eq!(info, quercus_storage::error::QUERCUSSTORAGE_OK);
			// The loaded project is a live engine handle.
			assert!(crate::node::quercus_node_id_is_valid(
				crate::node::quercus_project_root(loaded)
			));

			crate::node::quercus_project_release(loaded);
			crate::node::quercus_project_release(project);

			// A missing file fails closed; a non-project URI too.
			let (u, ul) = arg("file:///nonexistent/quercus.ove");
			assert!(quercus_storage_open(u, ul, std::ptr::null_mut()).is_null());
			let _ = std::fs::remove_file(&path);
		}
	}

	/// The library on a fresh database is empty; the mutations report
	/// the not-found error code.
	#[test]
	fn library_on_empty_database() {
		unsafe {
			let path = std::env::temp_dir()
				.join(format!("quercusengine_lib_{}.sqlite", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let _ = std::fs::remove_file(&path);
			// Two slashes, not three: `parse_target` requires an absolute
			// path and the platform path supplies its own root
			// (`/tmp/…` on unix, `C:\…` on Windows — a third slash would
			// make the Windows form root-relative, failing is_absolute).
			let uri = format!("quercusdb+sqlite://{path}");
			let (u, ul) = arg(&uri);
			assert_eq!(quercus_storage_library_count(u, ul), 0);
			let mut buf = [0u8; 64];
			assert_eq!(
				quercus_storage_library_uuid_at(u, ul, 0, buf.as_mut_ptr(), 64),
				-1
			);
			let (id, idl) = arg("no-such-uuid");
			assert!(quercus_storage_library_delete(u, ul, id, idl) < 0);
			let (n, nl) = arg("New Name");
			assert!(quercus_storage_library_rename(u, ul, id, idl, n, nl) < 0);
			let _ = std::fs::remove_file(&path);
		}
	}

	/// The row URI is the library root plus the `?project=` selector; the
	/// same rule applies to every scheme (the backend parses the query).
	#[test]
	fn library_row_uri_appends_project_selector() {
		let base = StorageUri::parse("quercusdb+sqlite:///tmp/lib.sqlite").unwrap();
		let row = library_row_uri(&base, "abc-123").unwrap();
		assert_eq!(
			row.to_uri_string(),
			"quercusdb+sqlite:///tmp/lib.sqlite?project=abc-123"
		);
		assert_eq!(row.scheme, base.scheme);
		let remote = StorageUri::parse("s3://bucket/projects").unwrap();
		assert_eq!(
			library_row_uri(&remote, "u").unwrap().to_uri_string(),
			"s3://bucket/projects?project=u"
		);
	}

	/// A minimal foreign-style library: it lists rows and loads/saves but
	/// reports `Unsupported` for stats, so the engine must derive them from
	/// the loaded graph.
	struct StatsFallbackBackend;

	impl StorageBackend for StatsFallbackBackend {
		fn name(&self) -> &'static str {
			"stats-fallback-test"
		}
		fn uri_scheme(&self) -> &'static str {
			"statsfallback"
		}
		fn can_handle(&self, uri: &StorageUri) -> bool {
			uri.scheme == "statsfallback"
		}
		fn load(
			&self,
			_uri: &StorageUri,
		) -> quercus_storage::error::Result<quercus_storage::backend::LoadResult> {
			let project = Project::new();
			project
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.initialize()
				.map_err(|_| Error::State)?;
			Ok(quercus_storage::backend::LoadResult::success(
				nodeutil::make_project_owned(project),
			))
		}
		fn save(
			&self,
			_project: CHandle,
			_uri: &StorageUri,
			_options: u32,
		) -> quercus_storage::error::Result<()> {
			Ok(())
		}
		fn supports_library(&self) -> bool {
			true
		}
		fn library_list(
			&self,
			_uri: &StorageUri,
		) -> quercus_storage::error::Result<Vec<LibraryProjectInfo>> {
			Ok(vec![LibraryProjectInfo {
				uuid: "fallback-uuid".into(),
				name: "Fallback".into(),
				schema_ver: 1,
				created_at: 0,
				modified_at: 0,
			}])
		}
	}

	/// `Error::Unsupported` from `library_stats` is a capability signal:
	/// the engine loads the row and derives the stats from its graph.
	#[test]
	fn library_stats_falls_back_to_graph_derivation() {
		let backend = StatsFallbackBackend;
		let base = StorageUri::parse("statsfallback://lib").unwrap();
		let info = backend.library_list(&base).unwrap();
		let stats = library_project_stats(&backend, &base, &info[0].uuid)
			.expect("Unsupported must fall back to load+derive");
		assert_eq!(stats, LibraryProjectStats::default());
	}

	/// Capability probing: a database library supports the library
	/// operations, a file interchange backend does not, and an unresolved
	/// scheme is false (never a fallback to the built-in database).
	#[test]
	fn library_supported_reflects_the_resolved_backend() {
		unsafe {
			let (bogus, n) = arg("strange://nothing");
			assert!(!quercus_storage_library_supported(bogus, n));
			assert!(!quercus_storage_library_supported(std::ptr::null(), 0));

			let (file, fl) = arg("file:///tmp/x.ove");
			assert!(!quercus_storage_library_supported(file, fl));

			let path = std::env::temp_dir()
				.join(format!("quercusengine_lib_supported_{}.sqlite", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let uri = format!("quercusdb+sqlite://{path}");
			let (db, dbl) = arg(&uri);
			assert!(quercus_storage_library_supported(db, dbl));
		}
	}

	/// Finds a row name by uuid through the public list exports.
	fn library_name_for(uri: *const u8, uri_len: usize, uuid: &str) -> Option<String> {
		let count = unsafe { quercus_storage_library_count(uri, uri_len) };
		let mut buf = [0u8; 128];
		for index in 0..count.max(0) {
			let len = unsafe { quercus_storage_library_uuid_at(uri, uri_len, index, buf.as_mut_ptr(), buf.len()) };
			if len <= 0 || std::str::from_utf8(&buf[..len as usize]).ok()? != uuid {
				continue;
			}
			let len = unsafe { quercus_storage_library_name_at(uri, uri_len, index, buf.as_mut_ptr(), buf.len()) };
			return Some(std::str::from_utf8(&buf[..len as usize]).ok()?.to_string());
		}
		None
	}

	/// End-to-end through the public C exports on a temp SQLite library:
	/// create, list (uuid/name/timestamps/stats), rename, duplicate,
	/// export, import and delete — all backend-neutral compositions.
	#[test]
	fn library_create_list_rename_duplicate_export_import_delete() {
		unsafe {
			let path = std::env::temp_dir()
				.join(format!("quercusengine_lib_crud_{}.sqlite", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let _ = std::fs::remove_file(&path);
			let uri = format!("quercusdb+sqlite://{path}");
			let (u, ul) = arg(&uri);

			let (name, name_len) = arg("First");
			let mut buf = [0u8; 128];
			let created =
				quercus_storage_library_create(u, ul, name, name_len, buf.as_mut_ptr(), buf.len());
			assert!(created > 0, "create failed: {created}");
			let uuid = std::str::from_utf8(&buf[..created as usize])
				.unwrap()
				.to_string();
			assert_eq!(quercus_storage_library_count(u, ul), 1);
			assert_eq!(library_name_for(u, ul, &uuid).as_deref(), Some("First"));
			assert!(quercus_storage_library_created_at(u, ul, 0) >= 0);
			assert!(quercus_storage_library_modified_at(u, ul, 0) >= 0);
			// An initialized but sequence-less project derives zero stats.
			assert_eq!(quercus_storage_library_duration_ms(u, ul, 0), 0);
			assert_eq!(quercus_storage_library_track_count(u, ul, 0), 0);
			assert_eq!(quercus_storage_library_clip_count(u, ul, 0), 0);
			assert_eq!(quercus_storage_library_footage_count(u, ul, 0), 0);

			let (id, idl) = arg(&uuid);
			let (renamed, renamed_len) = arg("Renamed");
			assert_eq!(
				quercus_storage_library_rename(u, ul, id, idl, renamed, renamed_len),
				quercus_storage::error::QUERCUSSTORAGE_OK
			);
			assert_eq!(library_name_for(u, ul, &uuid).as_deref(), Some("Renamed"));

			// Duplicate: fresh uuid, `<name> (copy)` default name.
			let mut copy_buf = [0u8; 128];
			let copy_len = quercus_storage_library_duplicate(
				u,
				ul,
				id,
				idl,
				copy_buf.as_mut_ptr(),
				copy_buf.len(),
			);
			assert!(copy_len > 0, "duplicate failed: {copy_len}");
			let copy_uuid = std::str::from_utf8(&copy_buf[..copy_len as usize])
				.unwrap()
				.to_string();
			assert_ne!(copy_uuid, uuid);
			assert_eq!(quercus_storage_library_count(u, ul), 2);
			assert_eq!(
				library_name_for(u, ul, &copy_uuid).as_deref(),
				Some("Renamed (copy)")
			);

			// Export through the registry: a `.ove` file target dispatches
			// to the ove-xml backend.
			let out = std::env::temp_dir()
				.join(format!("quercusengine_lib_export_{}.ove", std::process::id()));
			let out_uri = format!("file://{}", out.to_string_lossy());
			let (ou, oul) = arg(&out_uri);
			assert_eq!(
				quercus_storage_library_export(u, ul, id, idl, ou, oul),
				quercus_storage::error::QUERCUSSTORAGE_OK
			);
			assert!(out.exists(), "export target was not written");

			// Import the exported file back as a new row.
			let mut imported_buf = [0u8; 128];
			let imported_len = quercus_storage_library_import(
				u,
				ul,
				ou,
				oul,
				imported_buf.as_mut_ptr(),
				imported_buf.len(),
			);
			assert!(imported_len > 0, "import failed: {imported_len}");
			let imported_uuid = std::str::from_utf8(&imported_buf[..imported_len as usize])
				.unwrap()
				.to_string();
			assert_ne!(imported_uuid, uuid);
			assert_ne!(imported_uuid, copy_uuid);
			assert_eq!(quercus_storage_library_count(u, ul), 3);
			// Rename is library metadata only: the exported payload still
			// carries the in-project name, and import keeps it.
			assert_eq!(library_name_for(u, ul, &imported_uuid).as_deref(), Some("First"));

			assert_eq!(
				quercus_storage_library_delete(u, ul, id, idl),
				quercus_storage::error::QUERCUSSTORAGE_OK
			);
			assert_eq!(quercus_storage_library_count(u, ul), 2);
			assert!(
				quercus_storage_library_delete(u, ul, id, idl) < 0,
				"double delete must fail"
			);

			let _ = std::fs::remove_file(&path);
			let _ = std::fs::remove_file(&out);
		}
	}

	use std::ffi::CString;

	use crate::ext::storage_transport::{QuercusStorageError, QUERCUS_STORAGE_TRANSPORT_VERSION};

	/// The transport slot table is process-global; serialize these tests.
	static TRANSPORT_SERIAL: Mutex<()> = Mutex::new(());

	unsafe extern "C" fn test_load(
		_ud: *mut c_void,
		_uri: *const u8,
		_uri_len: usize,
		_out_bytes: *mut *mut u8,
		_out_len: *mut usize,
		_out_format: *mut i32,
		_err: *mut QuercusStorageError,
	) -> i32 {
		-1
	}

	unsafe extern "C" fn test_free(_ud: *mut c_void, _bytes: *mut u8, _len: usize) {}

	unsafe extern "C" fn test_save(
		_ud: *mut c_void,
		_uri: *const u8,
		_uri_len: usize,
		_bytes: *const u8,
		_bytes_len: usize,
		_options: u32,
		_err: *mut QuercusStorageError,
	) -> i32 {
		0
	}

	fn transport_vtable(name: &CString, scheme: &CString) -> QuercusStorageTransportVTable {
		QuercusStorageTransportVTable {
			struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
			abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
			name: name.as_ptr(),
			scheme: scheme.as_ptr(),
			write_policy: 0,
			// `None` = fall back to scheme equality.
			can_handle: None,
			load: Some(test_load),
			free_bytes: Some(test_free),
			save: Some(test_save),
			// No library capability on this test transport.
			library_list: None,
			library_delete: None,
			library_rename: None,
			library_snapshot: None,
		}
	}

	#[test]
	fn reserved_scheme_transports_are_rejected() {
		let _guard = TRANSPORT_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			// Null vtable is invalid.
			assert_eq!(
				quercus_storage_register_transport(std::ptr::null(), std::ptr::null_mut()),
				quercus_storage::error::QUERCUSSTORAGE_E_INVALID
			);
			for name_suffix in ["file", "quercusdb", "quercusdb+sqlite"] {
				let name = CString::new(format!("engine-reserved-{name_suffix}")).unwrap();
				let scheme = CString::new(name_suffix).unwrap();
				let vt = transport_vtable(&name, &scheme);
				assert_eq!(
					quercus_storage_register_transport(&vt, std::ptr::null_mut()),
					quercus_storage::error::QUERCUSSTORAGE_E_INVALID,
					"scheme {name_suffix:?} must be reserved"
				);
			}
			assert!(
				Registry::global()
					.list()
					.iter()
					.all(|(name, _)| !name.starts_with("engine-reserved-")),
				"rejected transports must not enter the registry"
			);
		}
	}

	#[test]
	fn register_and_unregister_slots() {
		let _guard = TRANSPORT_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let name = CString::new("engine-slot-test").unwrap();
			let scheme = CString::new("engineslottest").unwrap();
			let vt = transport_vtable(&name, &scheme);
			let slot = quercus_storage_register_transport(&vt, std::ptr::null_mut());
			assert!(slot >= 0, "register failed with {slot}");
			assert!(
				Registry::global()
					.resolve(&StorageUri::parse("engineslottest://x").unwrap())
					.is_ok(),
				"the registered transport must claim its scheme"
			);

			// Duplicate name: E_STATE and no second slot.
			assert_eq!(
				quercus_storage_register_transport(&vt, std::ptr::null_mut()),
				quercus_storage::error::QUERCUSSTORAGE_E_STATE
			);

			assert!(quercus_storage_unregister_transport(slot));
			assert!(
				Registry::global()
					.resolve(&StorageUri::parse("engineslottest://x").unwrap())
					.is_err(),
				"an unregistered transport must not resolve"
			);
			assert!(!quercus_storage_unregister_transport(slot), "double unregister");
			assert!(!quercus_storage_unregister_transport(-1));
			assert!(!quercus_storage_unregister_transport(9999));
		}
	}
}
