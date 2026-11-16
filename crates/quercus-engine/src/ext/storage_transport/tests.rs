use super::*;
use quercus_storage::error::{
	QUERCUSSTORAGE_E_INVALID, QUERCUSSTORAGE_E_NOT_FOUND, QUERCUSSTORAGE_OK,
};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

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
	let (Some(uri), Some(uuid)) = (unsafe { utf8_arg(uri, uri_len) }, unsafe {
		utf8_arg(uuid, uuid_len)
	}) else {
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
	let (Some(uri), Some(uuid)) = (unsafe { utf8_arg(uri, uri_len) }, unsafe {
		utf8_arg(uuid, uuid_len)
	}) else {
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
		unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut QuercusStorageError) -> i32,
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
	backend
		.save(handle, &uri, 0)
		.expect("save through transport");
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
		backend.load(&uri).expect_err("still missing").code(),
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
	assert!(matches!(backend.load(&uri), Err(Error::Foreign { .. })));
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
	assert!(matches!(
		backend.library_list(&uri),
		Err(Error::Unsupported)
	));
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
		calls
			.iter()
			.any(|c| c == &format!("delete:mocklib://library:{ROW_ONE_UUID}")),
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
