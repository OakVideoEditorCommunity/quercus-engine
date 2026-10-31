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

//! Integration tests for foreign (C ABI vtable) storage transports,
//! exercising them through the process-wide [`Registry`]: registration,
//! save/load arbitration, unregister waiting for in-flight callbacks, and
//! per-backend load/save serialization. The unit tests in
//! `quercus_storage::foreign` cover the vtable validation and the
//! `free_bytes` ownership rules.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::time::Duration;

use quercus_node::project::Project;
use quercus_storage::backend::{LibraryProjectInfo, StorageBackend, WRITE_POLICY_COALESCED};
use quercus_storage::error::{Error, QUERCUSSTORAGE_E_INVALID, QUERCUSSTORAGE_OK};
use quercus_storage::foreign::{
	ForeignBackend, QUERCUS_STORAGE_FORMAT_OVE_XML, QUERCUS_STORAGE_TRANSPORT_VERSION,
	QuercusStorageError, QuercusStorageProjectInfo, QuercusStorageTransportVTable,
};
use quercus_storage::nodeutil::{make_project_owned, project_arc, release_project};
use quercus_storage::registry::Registry;
use quercus_storage::uri::StorageUri;

/// The process-wide registry and the mock's statics are shared; tests in
/// this binary serialize on this lock.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
	SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn store() -> &'static Mutex<HashMap<String, Vec<u8>>> {
	static STORE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
	STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One row the mock library transport serves (byte strings so a test can
/// serve an invalid-UTF-8 name through the POD).
struct MockLibraryRow {
	uuid: Vec<u8>,
	name: Vec<u8>,
	created_at: i64,
	modified_at: i64,
	schema_ver: i32,
}

const ROW_ONE_UUID: &str = "{11111111-1111-4111-8111-111111111111}";
const ROW_TWO_UUID: &str = "{22222222-2222-4222-8222-222222222222}";

fn library() -> &'static Mutex<Vec<MockLibraryRow>> {
	static LIBRARY: OnceLock<Mutex<Vec<MockLibraryRow>>> = OnceLock::new();
	LIBRARY.get_or_init(|| Mutex::new(Vec::new()))
}

fn library_calls() -> &'static Mutex<Vec<String>> {
	static CALLS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
	CALLS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Reset the mock library to two well-formed rows and clear the call log.
fn reset_library() {
	let mut rows = library().lock().unwrap_or_else(|e| e.into_inner());
	rows.clear();
	rows.push(MockLibraryRow {
		uuid: ROW_ONE_UUID.as_bytes().to_vec(),
		name: b"First".to_vec(),
		created_at: 1_700_000_000,
		modified_at: 1_700_000_100,
		schema_ver: 1,
	});
	rows.push(MockLibraryRow {
		uuid: ROW_TWO_UUID.as_bytes().to_vec(),
		name: b"Second".to_vec(),
		created_at: 1_700_000_200,
		modified_at: 1_700_000_300,
		schema_ver: 1,
	});
	library_calls()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
}

/// Decode one borrowed C byte argument as UTF-8 (None when null/invalid).
unsafe fn utf8_arg(ptr: *const u8, len: usize) -> Option<String> {
	if ptr.is_null() {
		return None;
	}
	let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
	std::str::from_utf8(bytes).ok().map(str::to_owned)
}

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
	let rows = library().lock().unwrap_or_else(|e| e.into_inner());
	if out.is_null() || cap == 0 {
		*out_count = rows.len();
		return 0;
	}
	let written = rows.len().min(cap);
	for (i, row) in rows.iter().take(written).enumerate() {
		unsafe { out.add(i).write(pod_of(row)) };
	}
	*out_count = written;
	0
}

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
	library_calls()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(format!("delete:{uri}:{uuid}"));
	library()
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
	library_calls()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(format!("rename:{uri}:{uuid}:{name}"));
	for row in library()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.iter_mut()
	{
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
	library_calls()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(format!("snapshot:{uri}:{uuid}"));
	0
}

/// Fill `QuercusStorageError` from a mock callback (ignores a null pointer).
unsafe fn write_error(err: *mut QuercusStorageError, code: i32, message: &str) {
	let Some(err) = (unsafe { err.as_mut() }) else {
		return;
	};
	err.code = code;
	err.set_message(message);
}

/// Set while the gated `load` callback is sleeping (waits for the gate).
static LOAD_STARTED: AtomicBool = AtomicBool::new(false);
/// Set by the test to let the gated `load` callback return.
static LOAD_RELEASED: AtomicBool = AtomicBool::new(false);
/// Current/maximum number of concurrent `save` callbacks.
static SAVE_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static SAVE_MAX_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

const EMPTY_PROJECT_XML: &[u8] = b"<project version=\"1\"></project>";

unsafe extern "C" fn mock_can_handle(
	_ud: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	_err: *mut QuercusStorageError,
) -> i32 {
	let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
	std::str::from_utf8(text)
		.map(|uri| i32::from(uri.starts_with("fstore://") || uri.starts_with("gate://")))
		.unwrap_or(0)
}

unsafe extern "C" fn mock_load(
	_ud: *mut c_void,
	uri: *const u8,
	uri_len: usize,
	out_bytes: *mut *mut u8,
	out_len: *mut usize,
	out_format: *mut i32,
	_err: *mut QuercusStorageError,
) -> i32 {
	let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
	let uri = std::str::from_utf8(text).expect("mock URI is UTF-8");
	let payload = if uri.starts_with("gate://") {
		LOAD_STARTED.store(true, Ordering::SeqCst);
		while !LOAD_RELEASED.load(Ordering::SeqCst) {
			std::thread::sleep(Duration::from_millis(1));
		}
		EMPTY_PROJECT_XML.to_vec()
	} else {
		store()
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(uri)
			.cloned()
			.unwrap_or_else(|| EMPTY_PROJECT_XML.to_vec())
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
	let in_flight = SAVE_IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
	SAVE_MAX_IN_FLIGHT.fetch_max(in_flight, Ordering::SeqCst);
	// Hold the callback long enough that an unserialized pair overlaps.
	std::thread::sleep(Duration::from_millis(40));
	let text = unsafe { std::slice::from_raw_parts(uri, uri_len) };
	let payload = unsafe { std::slice::from_raw_parts(bytes, bytes_len) };
	let uri = std::str::from_utf8(text).expect("mock URI is UTF-8");
	store()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(uri.to_string(), payload.to_vec());
	SAVE_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
	0
}

fn make_backend(
	name: &str,
	scheme: &str,
	with_can_handle: bool,
) -> Result<ForeignBackend, quercus_storage::error::Error> {
	let name = CString::new(name).unwrap();
	let scheme = CString::new(scheme).unwrap();
	let vt = QuercusStorageTransportVTable {
		struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
		abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
		name: name.as_ptr(),
		scheme: scheme.as_ptr(),
		write_policy: 0,
		can_handle: with_can_handle.then_some(mock_can_handle),
		load: Some(mock_load),
		free_bytes: Some(mock_free),
		save: Some(mock_save),
		library_list: None,
		library_delete: None,
		library_rename: None,
		library_snapshot: None,
	};
	unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }
}

/// A backend whose four library slots are wired to the mock library.
fn make_library_backend(
	name: &str,
	scheme: &str,
) -> Result<ForeignBackend, quercus_storage::error::Error> {
	let name = CString::new(name).unwrap();
	let scheme = CString::new(scheme).unwrap();
	let vt = QuercusStorageTransportVTable {
		struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
		abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
		name: name.as_ptr(),
		scheme: scheme.as_ptr(),
		write_policy: 0,
		can_handle: None,
		load: Some(mock_load),
		free_bytes: Some(mock_free),
		save: Some(mock_save),
		library_list: Some(mock_library_list),
		library_delete: Some(mock_library_delete),
		library_rename: Some(mock_library_rename),
		library_snapshot: Some(mock_library_snapshot),
	};
	unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }
}

/// The original vtable layout (v1, through `save`): an SDK/caller built
/// before the library slots existed. Its `struct_size` ends before the
/// appended fields, which the engine must never read.
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
fn registry_save_load_roundtrip_and_removal() {
	let _serial = serial();
	let backend = Arc::new(make_backend("foreign-it-roundtrip", "fstore", true).expect("backend"));
	Registry::global().register(backend).expect("register");

	let uri = StorageUri::parse("fstore://bucket/project.ove").unwrap();
	let project = Project::new();
	project
		.lock()
		.unwrap()
		.settings
		.insert("projectname".to_string(), "Registry Transport".to_string());
	let handle = make_project_owned(project);
	Registry::global()
		.resolve(&uri)
		.expect("resolve")
		.save(handle, &uri, 0)
		.expect("save through transport");
	release_project(handle);

	let result = Registry::global()
		.resolve(&uri)
		.expect("resolve")
		.load(&uri)
		.expect("load through transport");
	assert_eq!(result.version_info, QUERCUSSTORAGE_OK);
	let loaded = unsafe { project_arc(&result.project) }.expect("project arc");
	release_project(result.project);
	assert_eq!(
		loaded
			.lock()
			.unwrap()
			.settings
			.get("projectname")
			.map(String::as_str),
		Some("Registry Transport")
	);

	Registry::global()
		.unregister("foreign-it-roundtrip")
		.expect("unregister");
	assert!(
		Registry::global().resolve(&uri).is_err(),
		"an unregistered transport must not resolve"
	);
}

#[test]
fn unregister_waits_for_in_flight_callback() {
	let _serial = serial();
	LOAD_STARTED.store(false, Ordering::SeqCst);
	LOAD_RELEASED.store(false, Ordering::SeqCst);
	let backend = Arc::new(make_backend("foreign-it-wait", "gate", true).expect("backend"));
	Registry::global().register(backend.clone()).expect("register");

	let loader = {
		let backend = backend.clone();
		let uri = StorageUri::parse("gate://project.ove").unwrap();
		std::thread::spawn(move || backend.load(&uri).map(|_| ()))
	};
	while !LOAD_STARTED.load(Ordering::SeqCst) {
		std::thread::yield_now();
	}
	let releaser = std::thread::spawn(|| {
		std::thread::sleep(Duration::from_millis(30));
		LOAD_RELEASED.store(true, Ordering::SeqCst);
	});

	// `wait_idle` is what the C ABI's unregister calls: it must block
	// until the load callback returns.
	backend.wait_idle();
	assert!(
		LOAD_RELEASED.load(Ordering::SeqCst),
		"wait_idle returned while the transport callback was still running"
	);
	// The gated load's own result is irrelevant; it must simply have
	// finished by now.
	let _ = loader.join().expect("loader thread");
	releaser.join().expect("releaser thread");

	Registry::global().unregister("foreign-it-wait").expect("unregister");
}

#[test]
fn save_callbacks_are_serialized_by_the_op_lock() {
	let _serial = serial();
	SAVE_MAX_IN_FLIGHT.store(0, Ordering::SeqCst);
	let backend = Arc::new(make_backend("foreign-it-serial", "fstore", true).expect("backend"));
	Registry::global().register(backend.clone()).expect("register");
	let uri = StorageUri::parse("fstore://serial/project.ove").unwrap();
	let project = Project::new();
	let barrier = Arc::new(Barrier::new(3));

	let workers: Vec<_> = (0..2)
		.map(|_| {
			let backend = backend.clone();
			let uri = uri.clone();
			let handle = make_project_owned(project.clone());
			let barrier = barrier.clone();
			std::thread::spawn(move || {
				barrier.wait();
				let result = backend.save(handle, &uri, 0);
				release_project(handle);
				result
			})
		})
		.collect();
	barrier.wait();
	for worker in workers {
		worker.join().expect("save thread").expect("save");
	}
	assert_eq!(
		SAVE_MAX_IN_FLIGHT.load(Ordering::SeqCst),
		1,
		"the transport must never see two concurrent save callbacks"
	);
	Registry::global().unregister("foreign-it-serial").expect("unregister");
}

#[test]
fn reserved_scheme_registration_is_rejected() {
	let _serial = serial();
	for scheme in ["file", "quercusdb", "quercusdb+sqlite"] {
		match make_backend("foreign-it-reserved", scheme, false) {
			Err(Error::Invalid) => {}
			Err(other) => panic!("scheme {scheme:?}: unexpected error {other:?}"),
			Ok(_) => panic!("scheme {scheme:?} must be reserved"),
		}
	}
	assert_eq!(Error::Invalid.code(), QUERCUSSTORAGE_E_INVALID);
	// Sanity: a non-reserved scheme is accepted.
	let backend = make_backend("foreign-it-ok", "s3", false).expect("s3 backend");
	assert_eq!(backend.uri_scheme(), "s3");
}

#[test]
fn scheme_fallback_claims_only_its_own_scheme() {
	let _serial = serial();
	let backend = make_backend("foreign-it-fallback", "s3", false).expect("backend");
	assert!(backend.can_handle(&StorageUri::parse("s3://bucket/project.ove").unwrap()));
	assert!(!backend.can_handle(&StorageUri::parse("https://example.com/project.ove").unwrap()));
}

// ---------------------------------------------------------------------------
// Project library (appended vtable slots) and v1 prefix compatibility
// ---------------------------------------------------------------------------

#[test]
fn v1_prefix_vtable_registers_and_library_ops_are_unsupported() {
	let _serial = serial();
	assert_eq!(
		std::mem::size_of::<V1VTable>(),
		QuercusStorageTransportVTable::V1_PREFIX_SIZE,
		"the v1 struct must end exactly at the library-slot boundary"
	);
	let name = CString::new("foreign-it-v1").unwrap();
	let scheme = CString::new("v1store").unwrap();
	let vt = V1VTable {
		struct_size: std::mem::size_of::<V1VTable>() as u32,
		abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
		name: name.as_ptr(),
		scheme: scheme.as_ptr(),
		write_policy: 0,
		can_handle: None,
		load: Some(mock_load),
		free_bytes: Some(mock_free),
		save: Some(mock_save),
	};
	// The cast crosses the ABI boundary: only `struct_size` bytes of the
	// caller's v1 struct are readable.
	let backend = Arc::new(
		unsafe {
			ForeignBackend::new(
				&vt as *const V1VTable as *const QuercusStorageTransportVTable,
				std::ptr::null_mut(),
			)
		}
		.expect("a v1 vtable must still register"),
	);
	assert!(!backend.supports_library());
	assert_eq!(
		backend.write_policy(),
		WRITE_POLICY_COALESCED,
		"vtable hint 0 maps to coalesced"
	);
	Registry::global().register(backend.clone()).expect("register");

	let uri = StorageUri::parse("v1store://bucket/project.ove").unwrap();
	let project = Project::new();
	let handle = make_project_owned(project);
	Registry::global()
		.resolve(&uri)
		.expect("resolve v1 transport")
		.save(handle, &uri, 0)
		.expect("save through a v1 transport");
	release_project(handle);
	let result = Registry::global()
		.resolve(&uri)
		.expect("resolve v1 transport")
		.load(&uri)
		.expect("load through a v1 transport");
	assert_eq!(result.version_info, QUERCUSSTORAGE_OK);
	release_project(result.project);

	let resolved = Registry::global().resolve(&uri).expect("resolve v1 transport");
	assert!(matches!(resolved.library_list(&uri), Err(Error::Unsupported)));
	assert!(matches!(
		resolved.library_delete(&uri, "{x}"),
		Err(Error::Unsupported)
	));
	assert!(matches!(
		resolved.library_rename(&uri, "{x}", "n"),
		Err(Error::Unsupported)
	));
	assert!(matches!(
		resolved.library_snapshot(&uri, "{x}"),
		Err(Error::Unsupported)
	));

	Registry::global().unregister("foreign-it-v1").expect("unregister");
}

#[test]
fn library_slots_roundtrip_through_the_registry() {
	let _serial = serial();
	reset_library();
	let backend = Arc::new(
		make_library_backend("foreign-it-library", "libstore").expect("library backend"),
	);
	Registry::global().register(backend.clone()).expect("register");

	let uri = StorageUri::parse("libstore://library").unwrap();
	let resolved = Registry::global().resolve(&uri).expect("resolve");
	assert!(resolved.supports_library());

	let rows = resolved.library_list(&uri).expect("two-phase library list");
	assert_eq!(
		rows,
		vec![
			LibraryProjectInfo {
				uuid: ROW_ONE_UUID.to_string(),
				name: "First".to_string(),
				schema_ver: 1,
				created_at: 1_700_000_000,
				modified_at: 1_700_000_100,
			},
			LibraryProjectInfo {
				uuid: ROW_TWO_UUID.to_string(),
				name: "Second".to_string(),
				schema_ver: 1,
				created_at: 1_700_000_200,
				modified_at: 1_700_000_300,
			},
		]
	);

	resolved
		.library_delete(&uri, ROW_ONE_UUID)
		.expect("delete through the trait");
	let rows = resolved.library_list(&uri).expect("list after delete");
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].uuid, ROW_TWO_UUID);

	resolved
		.library_rename(&uri, ROW_TWO_UUID, "Renamed")
		.expect("rename through the trait");
	let rows = resolved.library_list(&uri).expect("list after rename");
	assert_eq!(rows[0].name, "Renamed");

	resolved
		.library_snapshot(&uri, ROW_TWO_UUID)
		.expect("snapshot through the trait");
	let calls = library_calls()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone();
	assert!(
		calls
			.iter()
			.any(|c| c == &format!("delete:libstore://library:{ROW_ONE_UUID}")),
		"delete call log: {calls:?}"
	);
	assert!(
		calls
			.iter()
			.any(|c| c == &format!("rename:libstore://library:{ROW_TWO_UUID}:Renamed")),
		"rename call log: {calls:?}"
	);
	assert!(
		calls
			.iter()
			.any(|c| c == &format!("snapshot:libstore://library:{ROW_TWO_UUID}")),
		"snapshot call log: {calls:?}"
	);

	Registry::global().unregister("foreign-it-library").expect("unregister");
}

#[test]
fn library_pod_validation_rejects_bad_utf8_and_over_capacity() {
	let _serial = serial();
	reset_library();
	let backend = make_library_backend("foreign-it-bad-pod", "badpod").expect("backend");
	let uri = StorageUri::parse("badpod://library").unwrap();
	library()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(MockLibraryRow {
			uuid: b"{bad-utf8}".to_vec(),
			name: vec![0xff, 0x00],
			created_at: 0,
			modified_at: 0,
			schema_ver: 1,
		});
	assert!(
		matches!(backend.library_list(&uri), Err(Error::Format(_))),
		"an invalid UTF-8 name must be a format error"
	);

	let name = CString::new("foreign-it-over").unwrap();
	let scheme = CString::new("overcap").unwrap();
	let vt = QuercusStorageTransportVTable {
		struct_size: std::mem::size_of::<QuercusStorageTransportVTable>() as u32,
		abi_version: QUERCUS_STORAGE_TRANSPORT_VERSION,
		name: name.as_ptr(),
		scheme: scheme.as_ptr(),
		write_policy: 0,
		can_handle: None,
		load: Some(mock_load),
		free_bytes: Some(mock_free),
		save: Some(mock_save),
		library_list: Some(mock_library_list_overreport),
		library_delete: None,
		library_rename: None,
		library_snapshot: None,
	};
	let backend = unsafe { ForeignBackend::new(&vt, std::ptr::null_mut()) }.expect("backend");
	let uri = StorageUri::parse("overcap://library").unwrap();
	assert!(
		matches!(backend.library_list(&uri), Err(Error::Invalid)),
		"a transport reporting more rows than cap must be rejected"
	);
}
