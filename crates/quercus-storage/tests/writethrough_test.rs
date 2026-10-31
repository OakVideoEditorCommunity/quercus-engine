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

//! Boundary tests for the quercusstorage write-through façade
//! (`quercus_storage::writethrough`): configuration gating, binding lifecycle,
//! write-through / flush / unbind, error recording, and the null-handle
//! contract.
//!
//! The config store is process-global, so all tests serialize on a local
//! mutex and write `QUERCUS_CONFIG_DIR` to an isolated temp directory before
//! the first settings access (the tests never call `ConfigStore::save`, so
//! nothing persists).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use quercus_node::project::Project;
use quercus_storage::backend::{LoadResult, StorageBackend, WRITE_POLICY_COALESCED};
use quercus_storage::handle::CHandle;
use quercus_storage::nodeutil::{make_project_owned, project_arc, release_project};
use quercus_storage::registry::Registry;
use quercus_storage::uri::StorageUri;
use quercus_storage::writethrough;

/// Serializes the tests (global config + global bindings + the snapshot
/// thread).
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Take the lock and isolate the config store under a per-process temp
/// directory (once) before any settings access.
fn isolated_config() -> MutexGuard<'static, ()> {
	static INIT: OnceLock<()> = OnceLock::new();
	let guard = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	INIT.get_or_init(|| {
		let dir = std::env::temp_dir().join(format!(
			"quercusstorage_writethrough_config_{}",
			std::process::id()
		));
		let _ = std::fs::remove_dir_all(&dir);
		let _ = std::fs::create_dir_all(&dir);
		std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	});
	guard
}

fn set_storage_config(backend: &str, sqlite_path: Option<&std::path::Path>, pg_url: Option<&str>) {
	let store = quercus_core::configstore::ConfigStore::instance();
	store.set(Some("Storage"), "Backend", backend);
	// Every test starts from the legacy derivation unless it opts into
	// an explicit library uri.
	store.set(Some("Storage"), "LibraryUri", "");
	store.set(
		Some("Storage"),
		"SqlitePath",
		sqlite_path
			.map(|p| p.to_string_lossy().into_owned())
			.as_deref()
			.unwrap_or(""),
	);
	store.set(Some("Storage"), "PgUrl", pg_url.unwrap_or(""));
}

/// Set (or clear, with an empty string) the explicit
/// `Storage/LibraryUri` override.
fn set_library_uri(uri: &str) {
	quercus_core::configstore::ConfigStore::instance().set(Some("Storage"), "LibraryUri", uri);
}

fn work_dir(tag: &str) -> std::path::PathBuf {
	let dir = std::env::temp_dir().join(format!(
		"quercusstorage_writethrough_{tag}_{}",
		std::process::id()
	));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).expect("work dir");
	dir
}

fn project_handle() -> (CHandle, Arc<Mutex<Project>>) {
	let project = Project::new();
	let handle = make_project_owned(project.clone());
	(handle, project)
}

fn release(handle: CHandle) {
	if let Some(release) = handle.release {
		// SAFETY: the handle is an owned box (refcount 1) that this test
		// created via `make_project_owned`.
		unsafe { release(handle.ctx) };
	}
}

// ---- mock coalesced backend -------------------------------------------

/// A library backend that only counts calls: it proves the write-through
/// path stays off the edit thread and coalesces (policy hint COALESCED).
struct MockLibraryBackend {
	/// Registry name (unique per test process run).
	name: String,
	/// URI scheme this mock claims (unique per test).
	scheme: String,
	/// `save` calls observed.
	saves: AtomicUsize,
	/// `library_snapshot` calls observed.
	snapshots: AtomicUsize,
}

impl StorageBackend for MockLibraryBackend {
	fn name(&self) -> &str {
		&self.name
	}

	fn uri_scheme(&self) -> &str {
		&self.scheme
	}

	fn can_handle(&self, uri: &StorageUri) -> bool {
		uri.scheme == self.scheme
	}

	fn load(&self, _uri: &StorageUri) -> quercus_storage::error::Result<LoadResult> {
		Ok(LoadResult::success(CHandle::null()))
	}

	fn save(
		&self,
		_project: CHandle,
		_uri: &StorageUri,
		_options: u32,
	) -> quercus_storage::error::Result<()> {
		self.saves.fetch_add(1, Ordering::SeqCst);
		Ok(())
	}

	fn library_snapshot(
		&self,
		_uri: &StorageUri,
		_uuid: &str,
	) -> quercus_storage::error::Result<()> {
		self.snapshots.fetch_add(1, Ordering::SeqCst);
		Ok(())
	}

	fn write_policy(&self) -> u32 {
		WRITE_POLICY_COALESCED
	}

	fn supports_library(&self) -> bool {
		true
	}
}

/// Register a fresh mock under a unique scheme/name; returns
/// `(scheme, name, mock)`. The caller unregisters `name` at the end.
fn register_mock(tag: &str) -> (String, String, Arc<MockLibraryBackend>) {
	let scheme = format!("mockp0c{tag}");
	let name = format!("mock-p0c-{tag}-{}", std::process::id());
	let mock = Arc::new(MockLibraryBackend {
		name: name.clone(),
		scheme: scheme.clone(),
		saves: AtomicUsize::new(0),
		snapshots: AtomicUsize::new(0),
	});
	Registry::global()
		.register(mock.clone())
		.expect("register the mock library backend");
	(scheme, name, mock)
}

/// Wait (bounded) for `cond` to hold, polling at `step`; returns whether
/// it held before the deadline.
fn wait_until(timeout: Duration, step: Duration, mut cond: impl FnMut() -> bool) -> bool {
	let deadline = Instant::now() + timeout;
	loop {
		if cond() {
			return true;
		}
		if Instant::now() >= deadline {
			return false;
		}
		std::thread::sleep(step);
	}
}

// ---- null handles and status ------------------------------------------

#[test]
fn null_handles_are_safe_no_ops() {
	let _guard = isolated_config();
	assert!(!writethrough::is_bound(CHandle::null()));
	assert_eq!(writethrough::last_error(CHandle::null()), None);
	writethrough::bind_project(CHandle::null());
	writethrough::unbind_project(CHandle::null());
	// Nothing to flush is a no-op (and idempotent).
	writethrough::flush_all();
	writethrough::flush_all();
}

// ---- configuration gating ---------------------------------------------

#[test]
fn backend_disabled_by_config_leaves_projects_unbound() {
	let _guard = isolated_config();
	set_storage_config("off", None, None);
	assert!(!writethrough::storage_enabled());

	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	assert!(
		!writethrough::is_bound(handle),
		"disabled backend must not bind"
	);
	writethrough::unbind_project(handle);
	release(handle);
}

#[test]
fn backend_enabled_by_config_binds_and_round_trips() {
	let _guard = isolated_config();
	let dir = work_dir("sq");
	let db = dir.join("library.db");
	set_storage_config("sqlite", Some(&db), None);
	assert!(writethrough::storage_enabled());

	let (handle, project) = project_handle();
	assert!(!writethrough::is_bound(handle));
	writethrough::bind_project(handle);
	assert!(writethrough::is_bound(handle));
	assert_eq!(writethrough::last_error(handle), None);

	// Re-binding is idempotent (the existing binding is kept).
	writethrough::bind_project(handle);
	assert!(writethrough::is_bound(handle));

	// Add real state so the round trip carries more than an empty row.
	let original_uuid = {
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let (core, behavior) = quercus_node::folder::create("Written Through");
		guard.graph.add_node(core, behavior);
		guard.uuid.clone()
	};

	// A command-success notification writes the project through.
	writethrough::note_command();
	assert_eq!(
		writethrough::last_error(handle),
		None,
		"a write-through to a writable temp path succeeds"
	);

	// The write-through is real: the sqlite library file exists...
	assert!(
		db.is_file(),
		"the sqlite library file was created at {}",
		db.display()
	);
	assert!(
		std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0) > 0,
		"the sqlite library file is non-empty"
	);

	// Flush drains and snapshots; it is idempotent and safe afterwards.
	writethrough::flush_all();
	writethrough::flush_all();

	// ...and a fresh backend load reads the bound project's state back.
	let uri = StorageUri::parse(&writethrough::library_uri().expect("library uri"))
		.expect("parse library uri");
	let backend = writethrough::library_backend(&uri).expect("resolve the library backend");
	let loaded_handle = backend
		.load(&uri)
		.expect("load the written-through project")
		.project;
	// SAFETY: the backend returns an owned project handle.
	let loaded = unsafe { project_arc(&loaded_handle) }.expect("loaded project arc");
	release_project(loaded_handle);
	{
		let guard = loaded.lock().unwrap_or_else(|e| e.into_inner());
		assert_eq!(
			guard.uuid, original_uuid,
			"the loaded library row is the bound project"
		);
		let labels: Vec<String> = guard
			.graph
			.node_ids()
			.into_iter()
			.filter_map(|id| guard.graph.get(id))
			.map(|entry| entry.core.label.clone())
			.collect();
		assert!(
			labels.iter().any(|label| label == "Written Through"),
			"the loaded project carries the state added before note_command: {labels:?}"
		);
	}

	writethrough::unbind_project(handle);
	assert!(!writethrough::is_bound(handle), "unbind drops the binding");
	assert_eq!(writethrough::last_error(handle), None);

	release(handle);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_through_failure_is_recorded_in_last_error() {
	let _guard = isolated_config();
	// A directory cannot be opened as a SQLite database: the write-through
	// fails and the binding records the error instead of panicking.
	let dir = work_dir("bad");
	set_storage_config("sqlite", Some(&dir), None);

	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	assert!(writethrough::is_bound(handle));
	writethrough::note_command();
	assert!(
		writethrough::last_error(handle).is_some(),
		"a failing write-through must record last_error"
	);

	writethrough::flush_all();
	writethrough::unbind_project(handle);
	release(handle);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn library_uri_resolves_sqlite_and_pg_configurations() {
	let _guard = isolated_config();
	let dir = work_dir("uri");
	let db = dir.join("lib.db");
	set_storage_config("sqlite", Some(&db), None);
	let uri = writethrough::library_uri().expect("sqlite uri");
	assert!(uri.starts_with("quercusdb+sqlite://"), "{uri}");
	assert!(uri.ends_with("lib.db"), "{uri}");

	// PostgreSQL: the postgres:// scheme is stripped into the quercusdb body.
	set_storage_config(
		"pg",
		None,
		Some("postgres://user:pass@host:5432/db"),
	);
	assert_eq!(
		writethrough::library_uri().as_deref(),
		Some("quercusdb+pg://user:pass@host:5432/db")
	);
	// `postgresql://` is accepted too.
	set_storage_config("pg", None, Some("postgresql://user@host/db"));
	assert_eq!(
		writethrough::library_uri().as_deref(),
		Some("quercusdb+pg://user@host/db")
	);

	// pg selected without a URL: no library configured.
	set_storage_config("pg", None, None);
	assert_eq!(writethrough::library_uri(), None);

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_library_path_honors_the_config_directory() {
	let _guard = isolated_config();
	let path = writethrough::default_library_path();
	assert!(path.ends_with("library.db"), "{path}");
	let config_dir = std::env::var("QUERCUS_CONFIG_DIR").expect("test isolation set it");
	assert!(
		path.starts_with(&config_dir),
		"{path} must live under {config_dir}"
	);
}

#[test]
fn note_command_without_bindings_is_a_no_op() {
	let _guard = isolated_config();
	set_storage_config(
		"sqlite",
		Some(&work_dir("none").join("lib.db")),
		None,
	);
	// No project is bound in this process (the other tests' bindings are
	// dropped before they return): the notification must not panic.
	writethrough::note_command();
	writethrough::flush_all();
}

// ---- explicit LibraryUri + registry routing ---------------------------

#[test]
fn library_uri_overrides_legacy_config_and_off_still_disables() {
	let _guard = isolated_config();
	let dir = work_dir("override");
	let legacy = dir.join("legacy.db");
	let mock_uri = "mockp0coverride://library";

	// LibraryUri wins over the legacy Backend/SqlitePath derivation.
	set_storage_config("sqlite", Some(&legacy), None);
	set_library_uri(mock_uri);
	assert!(writethrough::storage_enabled());
	assert_eq!(
		writethrough::library_uri().as_deref(),
		Some(mock_uri),
		"LibraryUri overrides the legacy sqlite derivation"
	);
	// The value is trimmed and returned verbatim.
	set_library_uri(&format!("  {mock_uri}  "));
	assert_eq!(writethrough::library_uri().as_deref(), Some(mock_uri));

	// An explicit unsupported Backend still disables everything, even
	// with a LibraryUri configured.
	set_storage_config("off", Some(&legacy), None);
	set_library_uri(mock_uri);
	assert!(!writethrough::storage_enabled());
	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	assert!(
		!writethrough::is_bound(handle),
		"Backend=off disables binding even when LibraryUri is set"
	);
	writethrough::unbind_project(handle);
	release(handle);

	// Legacy derivation still applies when LibraryUri is empty.
	set_storage_config("sqlite", Some(&legacy), None);
	assert!(writethrough::library_uri()
		.expect("legacy sqlite uri")
		.starts_with("quercusdb+sqlite://"));

	// A LibraryUri no backend claims is not a fallback to the legacy
	// database: the project simply stays unbound.
	set_library_uri("mockp0cnobody://library");
	assert!(writethrough::storage_enabled());
	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	assert!(
		!writethrough::is_bound(handle),
		"an unclaimed LibraryUri must not fall back to a private database"
	);
	writethrough::unbind_project(handle);
	release(handle);

	let _ = std::fs::remove_dir_all(&dir);
}

// ---- policy: COALESCED -------------------------------------------------

#[test]
fn coalesced_backend_writes_after_debounce_not_on_the_edit_thread() {
	let _guard = isolated_config();
	let (scheme, name, mock) = register_mock("debounce");
	let dir = work_dir("debounce");
	let legacy = dir.join("legacy.db");
	let uri = format!("{scheme}://library");
	// Prove the LibraryUri override also drives the binding: the legacy
	// sqlite path must never be touched.
	set_storage_config("sqlite", Some(&legacy), None);
	set_library_uri(&uri);
	let store = quercus_core::configstore::ConfigStore::instance();
	store.set_int(Some("Storage"), "WriteDebounceMs", 200);
	store.set_int(Some("Storage"), "SnapshotIntervalSec", 600);
	assert_eq!(writethrough::library_uri().as_deref(), Some(uri.as_str()));

	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	assert!(writethrough::is_bound(handle));

	// The edit thread must return without I/O for a coalesced backend.
	writethrough::note_command();
	assert_eq!(
		mock.saves.load(Ordering::SeqCst),
		0,
		"COALESCED must not write on the edit thread"
	);

	// The background thread writes once the debounce elapsed.
	assert!(
		wait_until(Duration::from_secs(3), Duration::from_millis(20), || {
			mock.saves.load(Ordering::SeqCst) == 1
		}),
		"the coalesced write must land after the debounce"
	);
	// No duplicate writes while the project stays quiet.
	std::thread::sleep(Duration::from_millis(400));
	assert_eq!(mock.saves.load(Ordering::SeqCst), 1);

	// The exit flush snapshots the written head but must not re-save a
	// clean project.
	writethrough::flush_all();
	assert_eq!(
		mock.saves.load(Ordering::SeqCst),
		1,
		"flush must not re-save a clean binding"
	);
	assert_eq!(
		mock.snapshots.load(Ordering::SeqCst),
		1,
		"flush snapshots the written head"
	);
	assert!(
		!legacy.exists(),
		"the legacy sqlite library must not be touched"
	);

	writethrough::unbind_project(handle);
	assert!(!writethrough::is_bound(handle));
	release(handle);
	Registry::global()
		.unregister(&name)
		.expect("unregister mock");
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn coalesced_flush_and_unbind_force_a_save() {
	let _guard = isolated_config();
	let (scheme, name, mock) = register_mock("forced");
	let dir = work_dir("forced");
	let uri = format!("{scheme}://library");
	set_storage_config("sqlite", Some(&dir.join("legacy.db")), None);
	set_library_uri(&uri);
	// Keep the background thread out of the way: this test drives the
	// flush paths explicitly.
	quercus_core::configstore::ConfigStore::instance().set_int(
		Some("Storage"),
		"WriteDebounceMs",
		60_000,
	);

	let (handle, _project) = project_handle();
	writethrough::bind_project(handle);
	writethrough::note_command();
	assert_eq!(
		mock.saves.load(Ordering::SeqCst),
		0,
		"COALESCED must not write on the edit thread"
	);

	// flush_all forces the pending write, then snapshots it.
	writethrough::flush_all();
	assert_eq!(mock.saves.load(Ordering::SeqCst), 1);
	assert_eq!(mock.snapshots.load(Ordering::SeqCst), 1);
	// Flushing again is idempotent (nothing pending, nothing due).
	writethrough::flush_all();
	assert_eq!(mock.saves.load(Ordering::SeqCst), 1);
	assert_eq!(mock.snapshots.load(Ordering::SeqCst), 1);

	// A pending write at unbind time is flushed as well.
	writethrough::note_command();
	assert_eq!(mock.saves.load(Ordering::SeqCst), 1);
	writethrough::unbind_project(handle);
	assert_eq!(mock.saves.load(Ordering::SeqCst), 2);
	assert_eq!(mock.snapshots.load(Ordering::SeqCst), 2);
	assert!(!writethrough::is_bound(handle));

	release(handle);
	Registry::global()
		.unregister(&name)
		.expect("unregister mock");
	let _ = std::fs::remove_dir_all(&dir);
}
