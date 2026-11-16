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
		assert_eq!(
			quercus_storage_backend_name_at(-1, buf.as_mut_ptr(), 64),
			-1
		);
		assert_eq!(
			quercus_storage_backend_name_at(count, buf.as_mut_ptr(), 64),
			-1
		);

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
			.join(format!(
				"quercusengine_lib_supported_{}.sqlite",
				std::process::id()
			))
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
		let len = unsafe {
			quercus_storage_library_uuid_at(uri, uri_len, index, buf.as_mut_ptr(), buf.len())
		};
		if len <= 0 || std::str::from_utf8(&buf[..len as usize]).ok()? != uuid {
			continue;
		}
		let len = unsafe {
			quercus_storage_library_name_at(uri, uri_len, index, buf.as_mut_ptr(), buf.len())
		};
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
			.join(format!(
				"quercusengine_lib_crud_{}.sqlite",
				std::process::id()
			))
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
		let out = std::env::temp_dir().join(format!(
			"quercusengine_lib_export_{}.ove",
			std::process::id()
		));
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
		assert_eq!(
			library_name_for(u, ul, &imported_uuid).as_deref(),
			Some("First")
		);

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

use crate::ext::storage_transport::{QUERCUS_STORAGE_TRANSPORT_VERSION, QuercusStorageError};

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
		assert!(
			!quercus_storage_unregister_transport(slot),
			"double unregister"
		);
		assert!(!quercus_storage_unregister_transport(-1));
		assert!(!quercus_storage_unregister_transport(9999));
	}
}
