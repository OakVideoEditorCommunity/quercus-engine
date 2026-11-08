//! High-level editor sessions: one live project, its write-through
//! binding, current sequence and sequence-local auxiliary state.
//!
//! The C ABI exposes only opaque session/project handles and POD data.
//! In particular, consumers never recover the `Arc<Mutex<Project>>` that
//! the engine and write-through backend share.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
pub mod auxiliary;
pub mod notify;
pub mod proxy;
pub mod sync;
pub mod multicam;
pub mod audio;
pub mod subtitles;

use quercus_core::handle::CHandle;
use quercus_node::id::NodeId;
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use quercus_storage::nodeutil;
use quercus_storage::uri::StorageUri;
use quercus_timeline::marker::TimelineMarkerList;
use quercus_timeline::workarea::TimelineWorkArea;

use crate::codec::{copy_str_out, str_arg};
use crate::node::{QUERCUS_NODE_ID_INVALID, ProjectHandle};

/// Mutable runtime context owned by one [`EditorSessionHandle`].
///
/// A session wraps one live project together with the editing state that should
/// not be stored inside the project graph: the selected sequence, sequence-local
/// auxiliary objects, clipboard and write-through binding. Commands receive this
/// structure so they all operate on the same project and session context.
///
/// This type is engine-internal, not a C ABI value and not a serializable project
/// format. The handle's outer mutex serializes session access; fields that can
/// also be shared independently have their own `Arc<Mutex<_>>` ownership.
pub(crate) struct SessionState {
	/// The sole live project shared with project handles and write-through storage.
	pub(crate) project: Arc<Mutex<Project>>,
	/// Sequence used by commands that do not take an explicit sequence ID.
	pub(crate) current_sequence: NodeId,
	// Arc-backed marker/work-area objects can be shared with CHandle-compatible
	// commands without creating short-lived native handles.
	pub(crate) markers: Arc<Mutex<TimelineMarkerList>>,
	pub(crate) workarea: Arc<Mutex<TimelineWorkArea>>,
	/// Owned clips captured by the most recent copy operation.
	pub(crate) clipboard: Vec<crate::editor_commands::ClipboardClip>,
	/// Handle used to flush and detach write-through storage on unbind/session drop.
	pub(crate) storage: Option<CHandle>,
	/// Session-local edit callbacks cloned before notification.
	pub(crate) observers: Vec<Arc<notify::SessionObserver>>,
	/// Next nonzero ID returned by the edit subscription API.
	pub(crate) next_observer_id: u64,
	/// Active proxy tasks keyed by footage identity. Values are engine task IDs;
	/// keeping them in session state prevents duplicate generation and lets
	/// status queries distinguish a live worker from a stale working file.
	pub(crate) proxy_tasks: HashMap<u64, u64>,
}

impl SessionState {
	/// Creates auxiliary state for a project without rebinding or copying it.
	///
	/// The project `Arc` is stored as-is (shared, not cloned into a fresh
	/// project), no sequence is selected (`current_sequence` starts invalid),
	/// the marker list and work area start empty, the clipboard is empty and
	/// there is no write-through storage binding. Used by
	/// [`session_from_project_arc`] for both fresh and loaded projects.
	fn new(project: Arc<Mutex<Project>>) -> Self {
		Self {
			project,
			current_sequence: NodeId::INVALID,
			markers: Arc::new(Mutex::new(TimelineMarkerList::new())),
			workarea: Arc::new(Mutex::new(TimelineWorkArea::new())),
			clipboard: Vec::new(),
			storage: None,
			observers: Vec::new(),
			next_observer_id: 1,
			proxy_tasks: HashMap::new(),
		}
	}

	/// Takes the handle first so cleanup is idempotent even if called explicitly
	/// before `EditorSessionHandle::drop`.
	fn unbind(&mut self) {
		if let Some(handle) = self.storage.take() {
			quercus_storage::writethrough::unbind_project(handle);
			nodeutil::release_project(handle);
		}
	}
}

/// Project settings keys used to persist sequence-local auxiliary state
/// through the normal `.ove` serializer (the node crate cannot depend on
/// quercus-timeline, so the engine serializes these JSON blobs).
const MARKERS_SETTING: &str = "quercus.markers";
const WORKAREA_SETTING: &str = "quercus.workarea";

fn rational_json(value: quercus_core::Rational) -> serde_json::Value {
	serde_json::json!([value.numerator(), value.denominator()])
}

fn rational_from_json(value: &serde_json::Value) -> Option<quercus_core::Rational> {
	let array = value.as_array()?;
	let num = array.first()?.as_i64()?;
	let den = array.get(1)?.as_i64()?;
	Some(quercus_core::Rational::new(num, den))
}

/// Write the session's markers/work area into the project's settings map so
/// the storage serializer persists them. The `.ove` writer escapes values.
fn sync_auxiliary_to_settings(session: &EditorSessionHandle) {
	let (project, markers, workarea) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(
			state.project.clone(),
			state.markers.clone(),
			state.workarea.clone(),
		)
	};
	let markers = {
		let list = markers.lock().unwrap_or_else(|e| e.into_inner());
		let mut out = Vec::with_capacity(list.size());
		for index in 0..list.size() {
			if let Some(marker) = list.at(index) {
				out.push(serde_json::json!({
					"in": rational_json(marker.time().in_()),
					"out": rational_json(marker.time().out()),
					"name": marker.name(),
					"color": marker.color(),
				}));
			}
		}
		serde_json::Value::Array(out).to_string()
	};
	let workarea = {
		let area = workarea.lock().unwrap_or_else(|e| e.into_inner());
		serde_json::json!({
			"enabled": area.enabled(),
			"in": rational_json(area.in_()),
			"out": rational_json(area.out()),
		})
		.to_string()
	};
	let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
	guard.settings.insert(MARKERS_SETTING.to_string(), markers);
	guard.settings.insert(WORKAREA_SETTING.to_string(), workarea);
}

/// Restores markers/work area from a project's settings after load.
///
/// Reads the `quercus.markers` and `quercus.workarea` JSON blobs written by
/// [`sync_auxiliary_to_settings`] and replaces the session's live marker list
/// and work area. Decoding is tolerant: malformed JSON, a non-array markers
/// blob, or a marker/work-area entry without valid `in`/`out` rationals is
/// skipped (the whole work-area entry is skipped, but markers already added
/// are kept). A marker entry with a missing `name` or `color` falls back to
/// `""`/`0`. Each blob replaces the session's live object only when it
/// decodes; an absent or undecodable blob leaves that object untouched.
fn restore_auxiliary_from_settings(session: &EditorSessionHandle) {
	let (project, markers, workarea) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(
			state.project.clone(),
			state.markers.clone(),
			state.workarea.clone(),
		)
	};
	let (marker_json, workarea_json) = {
		let guard = project.lock().unwrap_or_else(|e| e.into_inner());
		(
			guard.settings.get(MARKERS_SETTING).cloned(),
			guard.settings.get(WORKAREA_SETTING).cloned(),
		)
	};
	if let Some(text) = marker_json {
		if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&text) {
			let mut list = markers.lock().unwrap_or_else(|e| e.into_inner());
			*list = TimelineMarkerList::new();
			for item in items {
				let (Some(in_), Some(out)) = (
					item.get("in").and_then(rational_from_json),
					item.get("out").and_then(rational_from_json),
				) else {
					continue;
				};
				let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
				let color = item.get("color").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
				list.add_marker(quercus_timeline::marker::TimelineMarker::with_time(
					color,
					quercus_core::TimeRange::new(in_, out),
					name,
				));
			}
		}
	}
	if let Some(text) = workarea_json {
		if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
			let (Some(in_), Some(out)) = (
				value.get("in").and_then(rational_from_json),
				value.get("out").and_then(rational_from_json),
			) else {
				return;
			};
			let enabled = value.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
			let mut area = workarea.lock().unwrap_or_else(|e| e.into_inner());
			area.set_enabled(enabled);
			area.set_range(quercus_core::TimeRange::new(in_, out));
		}
	}
}

/// An opaque editor session. The mutex serializes session mutations;
/// project access additionally uses the project's own lock.
pub struct EditorSessionHandle {
	pub(crate) inner: Mutex<SessionState>,
}

impl EditorSessionHandle {
	fn project(&self) -> Arc<Mutex<Project>> {
		self.inner
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.project
			.clone()
	}
}

impl Drop for EditorSessionHandle {
	fn drop(&mut self) {
		self.inner
			.get_mut()
			.unwrap_or_else(|e| e.into_inner())
			.unbind();
	}
}

crate::export_handle!(EditorSessionHandle, quercus_editor_add_ref, quercus_editor_release);

/// Builds a new opaque session directly from a shared project Arc.
///
/// This is the low-level constructor: it creates fresh `SessionState` and turns
/// it into an FFI handle without copying the project or selecting a sequence.
fn session_from_project_arc(project: Arc<Mutex<Project>>) -> *mut EditorSessionHandle {
	crate::handle::into_ffi(EditorSessionHandle {
		inner: Mutex::new(SessionState::new(project)),
	}) as *mut EditorSessionHandle
}

/// Adopts a project handle into a new editor session.
///
/// The session retains the handle's underlying project Arc, then releases the
/// supplied handle reference. After construction through
/// [`session_from_project_arc`], the first sequence is selected as the default
/// editing target. A null project handle produces a null session handle.
fn session_from_project_handle(project: *mut ProjectHandle) -> *mut EditorSessionHandle {
	if project.is_null() {
		return std::ptr::null_mut();
	}
	let inner = unsafe { &*project }.project().clone();
	unsafe { crate::node::quercus_project_release(project) };
	let session = session_from_project_arc(inner);
	if session.is_null() {
		return session;
	}
	// The first sequence is a useful default, matching the app adopt path.
	let first = {
		let guard = unsafe { &*session }
			.inner
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let project = guard.project.lock().unwrap_or_else(|e| e.into_inner());
		project.graph.node_ids().into_iter().find(|&id| {
			project
				.graph
				.get(id)
				.and_then(|entry| entry.behavior.as_any())
				.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
				.is_some()
		})
	};
	if let Some(id) = first {
		unsafe { quercus_editor_set_current_sequence(session, id.identity()) };
	}
	restore_auxiliary_from_settings(unsafe { &*session });
	session
}

/// Creates a fresh session around an existing engine project handle.
///
/// The handle is consumed exactly once. The new session has independent
/// transport/selection state but shares the same live project Arc.
///
/// # Safety
///
/// `project` must be null or a valid handle from the project ABI, owned by
/// the caller; on success that reference is released and the project Arc is
/// retained by the new session.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_from_project_handle(
	project: *mut ProjectHandle,
) -> *mut EditorSessionHandle {
	if project.is_null() {
		return std::ptr::null_mut();
	}
	session_from_project_handle(project)
}

/// Creates an uninitialized-session around a new, initialized project.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_editor_new() -> *mut EditorSessionHandle {
	let project = Project::new();
	if project
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.initialize()
		.is_err()
	{
		return std::ptr::null_mut();
	}
	session_from_project_arc(project)
}

/// Opens any URI supported by the storage registry and wraps the loaded
/// project in a session, recording the URI as the project's file name.
///
/// The returned project is marked unmodified. The default sequence and the
/// project's markers/work area are established by the session constructor, so
/// a project that contains a sequence starts with that sequence selected.
///
/// # Arguments
///
/// * `uri` - UTF-8 storage URI/path, resolved by the registry's registered
///   backends.
/// * `version_info` - optional out-parameter; when non-null it starts at 0
///   and receives the backend's version-info code (a positive value means
///   the file is too old/new/unknown, which yields a null result).
///
/// # Returns
///
/// An editor session for the loaded project, or null when the URI cannot be
/// resolved, the backend fails, the file version is unsupported, or the URI
/// is null/non-UTF-8. A null result leaves the `version_info` value telling
/// those cases apart.
///
/// # Safety
///
/// `uri` must be null or point to `uri_len` readable bytes of valid UTF-8 and
/// `version_info` must be null or point to one writable `i32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_open(
	uri: *const u8,
	uri_len: usize,
	version_info: *mut i32,
) -> *mut EditorSessionHandle {
	let project = unsafe { crate::storage::quercus_storage_open(uri, uri_len, version_info) };
	let session = session_from_project_handle(project);
	if !session.is_null() {
		if let Some(path) = unsafe { str_arg(uri, uri_len) } {
			let project = unsafe { &*session }.project();
			let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
			guard.set_filename(path);
			guard.set_modified(false);
		}
	}
	session
}

/// Opens a library row URI through the storage registry and wraps the
/// loaded project in a session.
///
/// Session bookkeeping mirrors [`quercus_editor_open_project_from_library`]:
/// the project starts unmodified, the error out-parameter is 0 on success
/// and a negative `QUERCUSSTORAGE_*` code on failure, and a version-info
/// outcome (no project) reports `E_INVALID`.
fn open_library_row_uri(uri: &StorageUri, error: *mut i32) -> *mut EditorSessionHandle {
	let text = uri.to_uri_string();
	let project = unsafe { crate::storage::quercus_storage_open(text.as_ptr(), text.len(), error) };
	if project.is_null() {
		// A positive version-info code (too old/new/unknown) still means
		// there is no project to open.
		if !error.is_null() && unsafe { *error } >= 0 {
			unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_E_INVALID };
		}
		return std::ptr::null_mut();
	}
	{
		let inner = unsafe { &*project }.project().clone();
		inner
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.set_modified(false);
	}
	if !error.is_null() {
		unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_OK };
	}
	session_from_project_handle(project)
}

/// Opens the project identified by `uuid` from the configured default library.
///
/// The library is the project's catalog and storage backend; this function does
/// not open a library as an asset inside another project. On success it returns
/// an editor session for the loaded project and reports `QUERCUSSTORAGE_OK` through
/// `error` when that pointer is non-null.
///
/// The row URI (`<library>?project=<uuid>`) is opened through the storage
/// registry, so any backend that claims the library can serve it.
///
/// # Safety
///
/// `uuid` must be null or point to `uuid_len` readable bytes of valid UTF-8,
/// and `error` must be null or point to one writable `i32`. The configured
/// default library must not be mutated concurrently by another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_open_project_from_library(
	uuid: *const u8,
	uuid_len: usize,
	error: *mut i32,
) -> *mut EditorSessionHandle {
	if !error.is_null() {
		unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_E_INVALID };
	}
	let Some(uuid) = (unsafe { str_arg(uuid, uuid_len) }) else {
		return std::ptr::null_mut();
	};
	if uuid.is_empty() {
		return std::ptr::null_mut();
	}
	let uri = match crate::storage::project_uri(uuid) {
		Ok(uri) => uri,
		Err(code) => {
			if !error.is_null() {
				unsafe { *error = code };
			}
			return std::ptr::null_mut();
		}
	};
	open_library_row_uri(&uri, error)
}

/// Opens the project `uuid` from an explicit library URI.
///
/// Unlike [`quercus_editor_open_project_from_library`], the library need not
/// be the configured default: the URI is passed to the storage registry as-is
/// and the row is opened as `<library_uri>?project=<uuid>`.
///
/// `error` (when non-null) starts at `QUERCUSSTORAGE_E_INVALID` and receives
/// `QUERCUSSTORAGE_OK` or the negative failure code. A null/non-UTF-8 URI, a
/// URI that does not parse, or an empty `uuid` leaves it at `E_INVALID`.
///
/// # Returns
///
/// An editor session for the loaded project, or null on any of the failures
/// above or when the row cannot be loaded.
///
/// # Safety
///
/// `library_uri` must be null or point to `library_uri_len` readable bytes
/// and `uuid` must be null or point to `uuid_len` readable bytes, both valid
/// UTF-8; `error` must be null or point to one writable `i32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_open_project_from_library_uri(
	library_uri: *const u8,
	library_uri_len: usize,
	uuid: *const u8,
	uuid_len: usize,
	error: *mut i32,
) -> *mut EditorSessionHandle {
	if !error.is_null() {
		unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_E_INVALID };
	}
	let (Some(library_uri), Some(uuid)) = (
		unsafe { str_arg(library_uri, library_uri_len) },
		unsafe { str_arg(uuid, uuid_len) },
	) else {
		return std::ptr::null_mut();
	};
	if uuid.is_empty() {
		return std::ptr::null_mut();
	}
	let Ok(base) = StorageUri::parse(library_uri) else {
		return std::ptr::null_mut();
	};
	let Ok(row) = crate::storage::library_row_uri(&base, uuid) else {
		return std::ptr::null_mut();
	};
	open_library_row_uri(&row, error)
}

/// Adopts one owned storage project handle without reopening or copying it.
///
/// This is the migration path for a host that already owns a `ProjectRef`: the
/// handle's Arc is retained by the new session and the supplied handle reference
/// is released. Both sides therefore observe the same live project. Null or
/// invalid handles return null.
///
/// # Safety
///
/// `project` must be null or a valid `ProjectRef`-compatible handle owned by
/// the caller; ownership is consumed on success and on the invalid-handle
/// error path (the handle is released either way).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_adopt_storage_project_handle(
	project: quercus_core::handle::CHandle,
) -> *mut EditorSessionHandle {
	if project.is_null() {
		return std::ptr::null_mut();
	}
	let project = match unsafe { nodeutil::project_arc(&project) } {
		Ok(project) => project,
		Err(_) => {
			nodeutil::release_project(project);
			return std::ptr::null_mut();
		}
	};
	session_from_project_handle(ProjectHandle::from_arc(project))
}

/// Creates a blank row in the configured default library and returns a
/// session for it.
///
/// The new project is initialized, its `projectname` setting is set to
/// trimmed `name`, and it is saved once through the storage registry so the
/// library row exists. The returned session owns the same project instance
/// that was saved, not a reloaded copy; the project starts unmodified.
///
/// `error` (when non-null) is set to `QUERCUSSTORAGE_E_INVALID` on entry and
/// holds `QUERCUSSTORAGE_OK` on success or the negative failure code
/// otherwise; it is also left at `E_INVALID` for a null/non-UTF-8 or blank
/// `name`.
///
/// # Returns
///
/// A new session handle, or null when the name is blank, no default library
/// is configured/usable, the project cannot be initialized, or the save
/// fails.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// `error` must be null or point to one writable `i32`, and the default
/// library configuration must not be mutated concurrently by another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_create_library(
	name: *const u8,
	name_len: usize,
	error: *mut i32,
) -> *mut EditorSessionHandle {
	if !error.is_null() {
		unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_E_INVALID };
	}
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return std::ptr::null_mut();
	};
	if name.trim().is_empty() {
		return std::ptr::null_mut();
	}
	let uri = match crate::storage::default_library() {
		Ok(uri) => uri,
		Err(code) => {
			if !error.is_null() {
				unsafe { *error = code };
			}
			return std::ptr::null_mut();
		}
	};
	let project = Project::new();
	let uuid = {
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		if guard.initialize().is_err() {
			return std::ptr::null_mut();
		}
		guard
			.settings
			.insert("projectname".into(), name.trim().into());
		guard.uuid.clone()
	};
	let row = match crate::storage::library_row_uri(&uri, &uuid) {
		Ok(row) => row,
		Err(code) => {
			if !error.is_null() {
				unsafe { *error = code };
			}
			return std::ptr::null_mut();
		}
	};
	let text = row.to_uri_string();
	let handle = ProjectHandle::from_arc(project.clone());
	let saved =
		unsafe { crate::storage::quercus_storage_save(handle, text.as_ptr(), text.len(), 0) };
	unsafe { crate::node::quercus_project_release(handle) };
	if saved != quercus_storage::error::QUERCUSSTORAGE_OK {
		if !error.is_null() {
			unsafe { *error = saved };
		}
		return std::ptr::null_mut();
	}
	if !error.is_null() {
		unsafe { *error = quercus_storage::error::QUERCUSSTORAGE_OK };
	}
	session_from_project_arc(project)
}

/// Returns a new reference on the session's live project.
///
/// The reference is a fresh [`ProjectHandle`] that shares the session's
/// project `Arc`; the session keeps its own reference, so the caller must
/// release the returned handle with `quercus_project_release` when done.
///
/// # Returns
///
/// A null pointer for a null session; otherwise an owned project handle.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI; it is dereferenced
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_project(
	session: *const EditorSessionHandle,
) -> *mut ProjectHandle {
	if session.is_null() {
		return std::ptr::null_mut();
	}
	ProjectHandle::from_arc(unsafe { &*session }.project())
}

/// Saves the session's project to `path` through the storage registry.
///
/// Before saving, the session's markers and work area are copied into the
/// project's settings map (`quercus.markers`, `quercus.workarea`) because the
/// `.ove` serializer only persists project state. On success the project's
/// file name is set to `path` and its modified flag is cleared; on failure
/// both are left unchanged.
///
/// # Arguments
///
/// * `path` - UTF-8 destination understood by the storage registry (a file
///   path or a backend URI such as `quercusdb+sqlite://…`).
/// * `options` - quercus-storage save bitmask; unknown bits are ignored.
///
/// # Returns
///
/// `QUERCUSSTORAGE_OK` (0) on success, otherwise a negative
/// `QUERCUSSTORAGE_*` code; `QUERCUSSTORAGE_E_INVALID` for a null session or
/// a null/non-UTF-8 `path`.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI and `path` must be
/// null or point to `path_len` readable bytes of valid UTF-8. The session
/// must not be used concurrently from another thread during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_save(
	session: *const EditorSessionHandle,
	path: *const u8,
	path_len: usize,
	options: u32,
) -> i32 {
	if session.is_null() {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	}
	let Some(path) = (unsafe { str_arg(path, path_len) }) else {
		return quercus_storage::error::QUERCUSSTORAGE_E_INVALID;
	};
	// Markers/work area live in the session, not the node graph; stash them
	// in the project settings so the storage serializer writes them.
	sync_auxiliary_to_settings(unsafe { &*session });
	let project = unsafe { quercus_editor_project(session) };
	let code =
		unsafe { crate::storage::quercus_storage_save(project, path.as_ptr(), path.len(), options) };
	unsafe { crate::node::quercus_project_release(project) };
	if code == quercus_storage::error::QUERCUSSTORAGE_OK {
		let project = unsafe { &*session }.project();
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		guard.set_filename(path);
		guard.set_modified(false);
	}
	code
}

/// Binds this exact project instance to the configured default library for
/// write-through persistence.
///
/// After binding, each successful undo-path command can write the project's
/// changes back to its library row. Binding itself does not save immediately;
/// the first undo-path operation creates the row when necessary. The returned
/// storage handle is retained in `SessionState::storage` so explicit unbind and
/// session destruction can flush and release the same binding.
///
/// A false result commonly means that no library is configured or storage is
/// disabled; this is a supported non-persistent mode, not a project-open error.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_bind_project_to_library(
	session: *mut EditorSessionHandle,
) -> bool {
	if session.is_null() {
		return false;
	}
	let (project, existing) = {
		let state = unsafe { &*session }
			.inner
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		(state.project.clone(), state.storage.is_some())
	};
	if existing {
		return true;
	}
	let handle = nodeutil::make_project_owned(project);
	quercus_storage::writethrough::bind_project(handle);
	let bound = quercus_storage::writethrough::is_bound(handle);
	let mut state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	state.storage = Some(handle);
	bound
}

/// Returns whether this session's project currently has an active
/// write-through library binding.
///
/// This is a status query only: it does not bind, save or flush the project.
/// False means a null session, no binding, or a binding disabled by storage
/// configuration. It does not report whether the last write succeeded; use
/// `quercus_editor_storage_last_error` for that separate diagnostic.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI; it is only read for
/// the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_storage_bound(session: *const EditorSessionHandle) -> bool {
	if session.is_null() {
		return false;
	}
	let state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	state
		.storage
		.is_some_and(|handle| quercus_storage::writethrough::is_bound(handle))
}

/// Flushes pending writes and closes the write-through library binding.
///
/// Explicit unbind provides deterministic cleanup before operations such as
/// deleting the project's library row. The session and in-memory project remain
/// alive; only automatic persistence stops. Session destruction performs the
/// same cleanup as a fallback, and repeated unbind calls are no-ops.
/// Returns false only for a null session.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_unbind(session: *mut EditorSessionHandle) -> bool {
	if session.is_null() {
		return false;
	}
	unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.unbind();
	true
}

/// Copies the project's most recent write-through or snapshot error as UTF-8.
///
/// The text is NUL-terminated in `buf` (truncated to `buf_len - 1` bytes when
/// too small) and the return value is the full byte length excluding the
/// NUL, so a null/zero-length buffer queries the required size and
/// `ret >= buf_len` means the text was truncated.
///
/// # Returns
///
/// `-1` for a null session, no binding, or no recorded error; these cases do
/// not distinguish success from an unavailable diagnostic by themselves.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and `buf` must be
/// null or point to at least `buf_len` writable bytes.
///
/// Paired with [`quercus_editor_storage_bound`] (binding status) and
/// [`quercus_editor_unbind`] (explicit cleanup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_storage_last_error(
	session: *const EditorSessionHandle,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if session.is_null() {
		return -1;
	}
	let state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	match state
		.storage
		.and_then(quercus_storage::writethrough::last_error)
	{
		Some(error) => copy_str_out(&error, buf, buf_len),
		None => -1,
	}
}

/// Selects a sequence and resets its sequence-local auxiliary state.
///
/// `id` must be the identity of a sequence node (a node whose behavior is
/// [`SequenceBehavior`]) already present in the session's project. On a
/// change of selection the marker list and work area are recreated empty,
/// because they are per-sequence and are not reloaded from the project here;
/// selecting the sequence that is already current leaves them untouched.
/// Both are persisted with the project on the next save.
///
/// # Returns
///
/// `false` for a null session, a `0`/malformed identity, or an identity that
/// does not name a sequence in this project.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_current_sequence(
	session: *mut EditorSessionHandle,
	id: u64,
) -> bool {
	if session.is_null() {
		return false;
	}
	let Some(id) = NodeId::from_identity(id) else {
		return false;
	};
	let project = unsafe { &*session }.project();
	let valid = {
		let guard = project.lock().unwrap_or_else(|e| e.into_inner());
		guard
			.graph
			.get(id)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
			.is_some()
	};
	if !valid {
		return false;
	}
	let mut state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	if state.current_sequence != id {
		state.current_sequence = id;
		state.markers = Arc::new(Mutex::new(TimelineMarkerList::new()));
		state.workarea = Arc::new(Mutex::new(TimelineWorkArea::new()));
	}
	true
}

/// Returns the raw identity of the sequence used by context-dependent edits.
///
/// Commands that do not take an explicit sequence id (markers, work area,
/// master-bus audio, timeline queries) read this value from session state.
/// Returns `QUERCUS_NODE_ID_INVALID` for a null session or when no sequence
/// has been selected. The returned ID is borrowed only as a value; no project
/// handle or graph reference crosses the ABI.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI; it is dereferenced
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_current_sequence(session: *const EditorSessionHandle) -> u64 {
	if session.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.current_sequence
		.identity()
}

/// Creates a sequence in the session's project and makes it current.
///
/// Creation goes through the existing node ABI
/// ([`crate::node::quercus_sequence_create`]), which also mounts the new
/// sequence under the project's bin folder and installs its default
/// video/audio track layout. `name` becomes the sequence label; the caller
/// keeps ownership of the input buffer and may pass an empty name.
///
/// # Returns
///
/// The new sequence's node identity, or `QUERCUS_NODE_ID_INVALID` for a null
/// session/project, a null or non-UTF-8 `name`, a failed node creation, or a
/// sequence that could not be selected afterwards.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI and `name` must be
/// null or point to `name_len` readable bytes of valid UTF-8. The session
/// must not be used concurrently from another thread during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_create_sequence(
	session: *mut EditorSessionHandle,
	name: *const u8,
	name_len: usize,
) -> u64 {
	let project = unsafe { quercus_editor_project(session) };
	if project.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	let id = unsafe { crate::node::quercus_sequence_create(project, name, name_len) };
	unsafe { crate::node::quercus_project_release(project) };
	if !unsafe { quercus_editor_set_current_sequence(session, id) } {
		return QUERCUS_NODE_ID_INVALID;
	}
	id
}

/// Extracts the `(project, clip)` pair shared by the clip audio setters.
///
/// Returns `None` for a null session or a null `clip` identity (`0`), which
/// the callers turn into a `false` ABI result. The returned project `Arc` is
/// a fresh clone of the session's live project, not a copy of it.
///
/// # Safety
///
/// `session` must be null or point to a live [`EditorSessionHandle`] obtained
/// from this ABI; it is dereferenced as `&*session` without any lifetime
/// guarantee beyond the call.
unsafe fn editor_audio_clip(
	session: *const EditorSessionHandle,
	clip: u64,
) -> Option<(std::sync::Arc<std::sync::Mutex<Project>>, NodeId)> {
	let session = unsafe { session.as_ref() }?;
	let clip = NodeId::from_identity(clip)?;
	Some((session.project(), clip))
}

/// Sets a clip's constant-power stereo pan as one undo entry.
///
/// `pan` is `-1.0` for hard left, `0.0` for centre and `1.0` for hard right;
/// values outside `-1.0..=1.0` or non-finite values are rejected. The change
/// is recorded as a "Set Clip Pan" undo entry, and writing the already
/// stored value records nothing.
///
/// # Returns
///
/// `false` for a null/invalid session or clip, a non-clip node, or a `pan`
/// outside the accepted range.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_pan(
	session: *const EditorSessionHandle,
	clip: u64,
	pan: f64,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	crate::editor_commands::set_clip_pan(&project, clip, pan).is_ok()
}

/// Sets a clip's linear fade-in/fade-out durations as one undo entry.
///
/// Both fades are timeline seconds expressed as rational `num`/`den` pairs
/// and are anchored at the clip's timeline in/out points. A zero denominator
/// is invalid, and a fade longer than the clip is allowed: the mixer clamps
/// the ramp at the clip's bounds. A call that would not change the stored
/// pair records no undo entry.
///
/// # Arguments
///
/// * `fade_in_num`, `fade_in_den` - fade-in duration as a rational number
///   of seconds; must be finite and non-negative.
/// * `fade_out_num`, `fade_out_den` - fade-out duration, same constraints.
///
/// # Returns
///
/// `false` for a null/invalid session or clip, a non-finite or negative
/// fade, or a fade with a zero denominator.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_fade(
	session: *const EditorSessionHandle,
	clip: u64,
	fade_in_num: i64,
	fade_in_den: i64,
	fade_out_num: i64,
	fade_out_den: i64,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	let fade_in = quercus_core::Rational::new(fade_in_num, fade_in_den);
	let fade_out = quercus_core::Rational::new(fade_out_num, fade_out_den);
	crate::editor_commands::set_clip_fade(&project, clip, fade_in, fade_out).is_ok()
}

/// Attaches an audio effect (`type_id`) to a clip's audio chain at
/// `index` (clamped to the chain length) as one undo entry.
///
/// `index` is the zero-based insertion position; values past the end of the
/// chain append. Phase 1 evaluates the built-in `Volume` and `Pan` kernels;
/// any other type id is kept in the chain and bypassed with a one-shot
/// render warning, so attaching an unsupported effect is non-destructive.
///
/// # Returns
///
/// `false` for a null/invalid session or clip, a null or non-UTF-8
/// `type_id`, an empty (or whitespace-only) type id, or a negative `index`.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and `type_id` must
/// be null or point to `type_id_len` readable bytes of valid UTF-8. The
/// session must not be used concurrently from another thread during the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_add_clip_audio_effect(
	session: *const EditorSessionHandle,
	clip: u64,
	type_id: *const u8,
	type_id_len: usize,
	index: i32,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	let Some(type_id) = (unsafe { str_arg(type_id, type_id_len) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	crate::editor_commands::add_clip_audio_effect(&project, clip, type_id, index).is_ok()
}

/// Removes one entry of a clip's audio effect chain as one undo entry.
///
/// `index` is the zero-based position in the clip's audio effect chain. On
/// success the remaining entries keep their relative order, and the removal
/// is recorded as one "Remove Clip Audio Effect" undo entry.
///
/// # Returns
///
/// `true` when the entry was removed; `false` for a null/invalid session or
/// clip (including a node that is not a clip), a negative `index`, or an
/// `index` at/after the chain length. On `false` no undo entry is recorded.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_remove_clip_audio_effect(
	session: *const EditorSessionHandle,
	clip: u64,
	index: i32,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	crate::editor_commands::remove_clip_audio_effect(&project, clip, index).is_ok()
}

/// Enables or disables one entry of a clip's audio effect chain.
///
/// A disabled effect stays in the chain but is bypassed by the mixer. The
/// change is recorded as one undo entry ("Set Clip Audio Effect Enabled");
/// writing the value that is already stored records no undo entry.
///
/// `index` is the zero-based chain position and `enabled` is the new state.
///
/// # Returns
///
/// `true` on success; `false` for a null/invalid session or clip, a negative
/// or out-of-range `index`, or a node that is not a clip.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and it must not be
/// used concurrently from another thread for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_audio_effect_enabled(
	session: *const EditorSessionHandle,
	clip: u64,
	index: i32,
	enabled: bool,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	crate::editor_commands::set_clip_audio_effect_enabled(&project, clip, index, enabled).is_ok()
}

/// Sets one static float parameter of one entry in a clip's audio effect
/// chain, as one undo entry.
///
/// `index` is the zero-based chain position and `input_id` names the
/// parameter (Phase 1 exposes only float parameters: the built-in `Volume`
/// kernel's `volume_in` and the `Pan` kernel's `panning_in`). A value equal
/// to the stored one records no undo entry.
///
/// # Returns
///
/// `true` on success; `false` for a null/invalid session or clip, a null or
/// non-UTF-8 `input_id`, an empty (or whitespace-only) input id, a negative
/// or out-of-range `index`, or a non-finite `value`.
///
/// # Safety
///
/// `session` must be null or a live handle from this ABI, and `input_id`
/// must be null or point to `input_id_len` readable bytes of valid UTF-8.
/// The session must not be used concurrently from another thread during the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_set_clip_audio_effect_param(
	session: *const EditorSessionHandle,
	clip: u64,
	index: i32,
	input_id: *const u8,
	input_id_len: usize,
	value: f64,
) -> bool {
	let Some((project, clip)) = (unsafe { editor_audio_clip(session, clip) }) else {
		return false;
	};
	let Some(input_id) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	let Ok(index) = usize::try_from(index) else {
		return false;
	};
	crate::editor_commands::set_clip_audio_effect_param(
		&project, clip, index, input_id, value,
	)
	.is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coretypes::QuercusTimeRange;

	#[test]
	fn session_owns_current_sequence_and_auxiliary_queries() {
		unsafe {
			let session = quercus_editor_new();
			assert!(!session.is_null());
			let name = "Sequence";
			let sequence = quercus_editor_create_sequence(session, name.as_ptr(), name.len());
			assert!(crate::node::quercus_node_id_is_valid(sequence));
			assert_eq!(quercus_editor_current_sequence(session), sequence);
			assert_eq!(auxiliary::quercus_editor_marker_count(session), 0);
			auxiliary::add_marker_for_test(session, "Marker");
			assert_eq!(auxiliary::quercus_editor_marker_count(session), 1);
			let mut marker = auxiliary::QuercusEditorMarker::default();
			assert!(auxiliary::quercus_editor_marker_at(session, 0, &mut marker));
			assert_eq!(marker.color, 3);
			assert_eq!(marker.time.in_.num, 1);
			assert_eq!(marker.time.out.num, 2);
			let mut name_buf = [0u8; 16];
			let name_len =
				auxiliary::quercus_editor_marker_name_at(session, 0, name_buf.as_mut_ptr(), name_buf.len());
			assert_eq!(
				std::str::from_utf8(&name_buf[..name_len as usize]).unwrap(),
				"Marker"
			);
			assert!(!auxiliary::quercus_editor_marker_at(session, 99, &mut marker));
			assert_eq!(
				auxiliary::quercus_editor_marker_name_at(session, 99, name_buf.as_mut_ptr(), 16),
				-1
			);
			let mut range = QuercusTimeRange::default();
			assert!(!auxiliary::quercus_editor_workarea(session, &mut range));

			let second = quercus_editor_create_sequence(session, name.as_ptr(), name.len());
			assert!(crate::node::quercus_node_id_is_valid(second));
			assert_eq!(quercus_editor_current_sequence(session), second);
			assert_eq!(
				auxiliary::quercus_editor_marker_count(session),
				0,
				"sequence switch resets aux state"
			);
			assert!(!quercus_editor_set_current_sequence(session, u64::MAX));
			assert_eq!(quercus_editor_current_sequence(session), second);
			quercus_editor_release(session);
		}
	}

	/// The explicit-library-URI open routes through the storage registry:
	/// a row created through the storage exports opens as a session with
	/// its settings intact, and an unknown uuid reports an error.
	#[test]
	fn open_project_from_explicit_library_uri() {
		unsafe {
			let path = std::env::temp_dir()
				.join(format!(
					"quercusengine_editor_lib_uri_{}.sqlite",
					std::process::id()
				))
				.to_string_lossy()
				.into_owned();
			let _ = std::fs::remove_file(&path);
			let uri = format!("quercusdb+sqlite://{path}");
			let (u, ul) = (uri.as_ptr(), uri.len());

			let name = b"EditorRow";
			let mut uuid_buf = [0u8; 128];
			let created = crate::storage::quercus_storage_library_create(
				u,
				ul,
				name.as_ptr(),
				name.len(),
				uuid_buf.as_mut_ptr(),
				uuid_buf.len(),
			);
			assert!(created > 0, "create failed: {created}");
			let uuid = uuid_buf[..created as usize].to_vec();

			let mut error = -999i32;
			let session = quercus_editor_open_project_from_library_uri(
				u,
				ul,
				uuid.as_ptr(),
				uuid.len(),
				&mut error,
			);
			assert!(!session.is_null(), "open failed: {error}");
			assert_eq!(error, quercus_storage::error::QUERCUSSTORAGE_OK);

			let project = quercus_editor_project(session);
			assert!(!project.is_null());
			{
				let inner = (&*project).project().clone();
				let guard = inner.lock().unwrap_or_else(|e| e.into_inner());
				assert_eq!(
					guard.settings.get("projectname").map(String::as_str),
					Some("EditorRow")
				);
			}
			crate::node::quercus_project_release(project);

			let mut missing_error = 0i32;
			let missing = quercus_editor_open_project_from_library_uri(
				u,
				ul,
				b"no-such-uuid".as_ptr(),
				13,
				&mut missing_error,
			);
			assert!(missing.is_null());
			assert!(
				missing_error < 0,
				"missing row must report a negative code, got {missing_error}"
			);

			quercus_editor_release(session);
			let _ = std::fs::remove_file(&path);
		}
	}

}
