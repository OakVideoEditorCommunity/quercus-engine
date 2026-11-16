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
	let dropped =
		crate::editor::EditorSession::create_in_library("Drop Binding").expect("drop-bound row");
	let dropped_uuid = dropped.project().uuid();
	assert!(dropped.storage_bound());
	drop(dropped);
	library
		.delete(&dropped_uuid)
		.expect("delete after Drop unbinds");
	assert!(library.projects().expect("drop leaves no row").is_empty());

	let session =
		crate::editor::EditorSession::create_in_library("Library Test").expect("create live row");
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
	let row = rows
		.iter()
		.find(|row| row.uuid == uuid)
		.expect("created row");
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
	assert_eq!(
		session.snapshot().sequences.len(),
		1,
		"duplicate edits stay independent"
	);

	let imported_session = library.open(&imported).expect("open import");
	assert_eq!(imported_session.snapshot().sequences, expected.sequences);
	assert_eq!(imported_session.snapshot().footages, expected.footages);
	let custom = ProjectLibrary::new(format!(
		"quercusdb+sqlite://{}/other.sqlite",
		std::env::temp_dir().display()
	))
	.expect("custom library URI");
	assert!(
		custom.open(&duplicate).is_err(),
		"a different library does not contain the row"
	);

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
	assert!(
		ProjectLibrary::configured()
			.expect("disabled config")
			.is_none()
	);
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
	let rows = library
		.projects()
		.expect("library list through the transport");
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
