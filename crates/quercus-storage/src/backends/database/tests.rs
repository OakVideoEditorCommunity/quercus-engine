use super::*;

fn uri(s: &str) -> StorageUri {
	StorageUri::parse(s).unwrap()
}

/// An absolute database path for the platform: `/tmp/...` is not
/// absolute on Windows (no drive prefix), and parse_target rejects
/// relative paths.
#[cfg(unix)]
const ABS_DB: &str = "/tmp/lib.db";
#[cfg(windows)]
const ABS_DB: &str = "C:/tmp/lib.db";

#[test]
fn parse_target_sqlite_absolute() {
	let t = parse_target(&uri(&format!("quercusdb+sqlite://{ABS_DB}"))).unwrap();
	assert_eq!(
		t,
		DbTarget::Sqlite {
			path: ABS_DB.to_string(),
			project: None
		}
	);
}

#[test]
fn parse_target_sqlite_project_query() {
	let t = parse_target(&uri(&format!(
		"quercusdb+sqlite://{ABS_DB}?project={{abc-123}}"
	)))
	.unwrap();
	assert_eq!(
		t,
		DbTarget::Sqlite {
			path: ABS_DB.to_string(),
			project: Some("{abc-123}".to_string())
		}
	);
}

#[test]
fn parse_target_sqlite_relative_rejected() {
	assert!(matches!(
		parse_target(&uri("quercusdb+sqlite://relative.db")),
		Err(Error::Invalid)
	));
	assert!(matches!(
		parse_target(&uri("quercusdb+sqlite://")),
		Err(Error::Invalid)
	));
}

#[test]
fn parse_target_pg_conn_string() {
	let t = parse_target(&uri("quercusdb+pg://user:pass@host:5432/db")).unwrap();
	assert_eq!(
		t,
		DbTarget::Pg {
			conn: "user:pass@host:5432/db".to_string(),
			project: None
		}
	);
	// A `?project=` query is the row selector, not a conn param.
	let t = parse_target(&uri("quercusdb+pg://user@host/db?project={abc-123}")).unwrap();
	assert_eq!(
		t,
		DbTarget::Pg {
			conn: "user@host/db".to_string(),
			project: Some("{abc-123}".to_string())
		}
	);
	// A query without a `project=` key belongs to the conn string
	// (e.g. `?sslmode=disable` passes through untouched).
	let t = parse_target(&uri("quercusdb+pg://user@host/db?sslmode=disable")).unwrap();
	assert_eq!(
		t,
		DbTarget::Pg {
			conn: "user@host/db?sslmode=disable".to_string(),
			project: None
		}
	);
}

#[test]
fn parse_target_pg_invalid_rejected() {
	// Empty / unparseable connection strings are E_INVALID (clean, no
	// network touched), matching the sqlite relative-path rule.
	assert!(matches!(
		parse_target(&uri("quercusdb+pg://")),
		Err(Error::Invalid)
	));
	assert!(matches!(
		parse_target(&uri("quercusdb+pg://?project={x}")),
		Err(Error::Invalid)
	));
	assert!(matches!(
		parse_target(&uri("quercusdb+pg://user@")),
		Err(Error::Invalid)
	));
	assert!(matches!(
		parse_target(&uri("quercusdb+pg://not a url")),
		Err(Error::Invalid)
	));
}

#[test]
fn parse_target_unknown_scheme() {
	assert!(matches!(
		parse_target(&uri("quercusdb://x")),
		Err(Error::NoBackend)
	));
	assert!(matches!(
		parse_target(&uri("file:///a.ove")),
		Err(Error::NoBackend)
	));
}

#[test]
fn node_fragment_round_trip() {
	// serialize_project_state → assemble_xml → extract_nodes must be
	// byte-stable for a real project.
	let p = Project::new();
	{
		let mut guard = p.lock().unwrap();
		guard.initialize().unwrap();
		let (core, behavior) = (quercus_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		let a = guard.graph.add_node(core, behavior);
		guard.graph.get_mut(a).unwrap().core.set_standard_value(
			"param_a_in",
			-1,
			quercus_node::value::NodeValue::Float(2.5),
		);
	}
	let guard = p.lock().unwrap();
	let (nodes, settings, _) = serialize_project_state(&guard).unwrap();
	let xml = assemble_xml(&guard.uuid, &nodes, &settings);
	let back = extract_nodes(&xml);
	assert_eq!(back, nodes, "fragment extraction is byte-stable");
	assert_eq!(extract_settings(&xml), settings);
	assert!(xml.contains(&guard.uuid));
	// And the assembled document loads.
	let loaded = quercus_node::serializer::load(&xml).unwrap();
	let loaded_guard = loaded.lock().unwrap();
	assert_eq!(loaded_guard.uuid, guard.uuid);
	assert_eq!(loaded_guard.graph.node_count(), guard.graph.node_count());
}

#[test]
fn shared_backend_is_the_builtin_singleton() {
	let a = DatabaseBackend::shared();
	let b = DatabaseBackend::shared();
	assert!(Arc::ptr_eq(a, b), "shared() must return one instance");
	let db = crate::backends::builtins()
		.into_iter()
		.find(|backend| backend.name() == "quercusdb")
		.expect("builtins must contain quercusdb");
	assert_eq!(
		Arc::as_ptr(&db) as *const (),
		Arc::as_ptr(a) as *const (),
		"the built-in quercusdb entry must be the shared singleton"
	);
}

#[test]
fn library_trait_methods_delegate_to_the_project_manager() {
	let dir = std::env::temp_dir().join(format!("quercusstorage_library_{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	let db = dir.join("lib.db");
	let uri = uri(&format!("quercusdb+sqlite://{}", db.display()));

	let backend = DatabaseBackend::shared();
	let project = Project::new();
	project
		.lock()
		.unwrap()
		.settings
		.insert("projectname".to_string(), "Shared Library".to_string());
	let handle = crate::nodeutil::make_project_owned(project);
	backend
		.save(handle, &uri, 0)
		.expect("save into the shared library");
	crate::nodeutil::release_project(handle);

	// The backend-neutral row keeps the manager's fields but replaces
	// the naive row timestamps with Unix seconds.
	let rows = backend.list_projects(&uri).expect("raw rows");
	assert_eq!(rows.len(), 1);
	let infos = backend.library_list(&uri).expect("library rows");
	assert_eq!(infos.len(), 1);
	assert_eq!(infos[0].uuid, rows[0].uuid);
	assert_eq!(infos[0].name, "Shared Library");
	assert_eq!(infos[0].schema_ver, rows[0].schema_ver);
	assert_eq!(
		infos[0].created_at,
		rows[0].created_at.and_utc().timestamp()
	);
	assert_eq!(
		infos[0].modified_at,
		rows[0].modified_at.and_utc().timestamp()
	);
	assert!(infos[0].created_at > 0, "unix seconds, not a naive epoch");

	let stats = backend
		.project_stats(&uri, &infos[0].uuid)
		.expect("raw stats");
	let lib_stats = backend
		.library_stats(&uri, &infos[0].uuid)
		.expect("library stats");
	assert_eq!(lib_stats.duration_ms, stats.duration_ms);
	assert_eq!(lib_stats.track_count, stats.track_count);
	assert_eq!(lib_stats.clip_count, stats.clip_count);
	assert_eq!(lib_stats.footage_count, stats.footage_count);

	backend
		.library_rename(&uri, &infos[0].uuid, "Renamed")
		.expect("rename");
	assert_eq!(backend.library_list(&uri).unwrap()[0].name, "Renamed");

	backend
		.library_snapshot(&uri, &infos[0].uuid)
		.expect("snapshot");

	backend
		.library_delete(&uri, &infos[0].uuid)
		.expect("delete");
	assert!(backend.library_list(&uri).unwrap().is_empty());
}
