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

//! Live write-through to the project library (plan M13 §2/§3; M14 R1:
//! sunk from the engine facade's `storage.rs`).
//!
//! Every opened project is bound to a library session — a
//! `(library uri, project uuid)` pair — and its edits are persisted
//! according to the *write policy* of the backend that claims the uri
//! (M10's [`StorageBackend::write_policy`], see [`StorageBackend`]). The
//! module subscribes to the quercusundo process-wide stack's
//! **command-success observers** ([`quercus_undo::global::add_observer`]),
//! so each command's diff lands in the journal transactionally (the
//! quercusstorage database backend does the diff itself) with no facade
//! round-trip.
//!
//! ## Binding model
//!
//! The map is keyed by the project box's identity (`Arc::as_ptr` — one
//! per in-memory project instance), so several projects can be bound at
//! once (multi-project, plan §3) without confusing their library rows.
//! Every undo-path operation notifies ALL bound projects
//! ([`note_command`]): the backend diffs each project against its own
//! library head, so untouched projects are no-op touches and only the
//! project the command actually changed advances its journal. Closing a
//! project ([`unbind_project`], hooked from the facade's `project_free`)
//! flushes its pending writes and drops the binding. The binding holds
//! the boxed project ([`ProjectArc`]) and the resolved backend
//! (`Arc<dyn StorageBackend>`) — the `CHandle` facade entries convert at
//! the boundary — so a bound project outlives the caller's own handle
//! reference and needs no second backend instance.
//!
//! ## Write policies
//!
//! The policy is sampled once, from the resolved backend, when the
//! project is bound ([`WRITE_POLICY_*`](crate::backend::WRITE_POLICY_IMMEDIATE)):
//!
//! - [`WRITE_POLICY_IMMEDIATE`] (the default, and what
//!   [`WRITE_POLICY_DEFAULT`](crate::backend::WRITE_POLICY_DEFAULT) maps to
//!   defensively): every successful command writes through synchronously
//!   on the edit thread — today's behavior for the local database
//!   backend;
//! - [`WRITE_POLICY_COALESCED`]: the command only marks the binding
//!   pending (no I/O on the edit thread) and wakes the background thread,
//!   which writes once the edit stream has been quiet for
//!   `Storage/WriteDebounceMs` (default 500 ms);
//! - [`WRITE_POLICY_EXPLICIT`]: the command only marks the binding
//!   pending; the write happens on [`flush_all`] / [`unbind_project`] /
//!   exit.
//!
//! No backend I/O ever runs while the bindings mutex is held: the
//! project/backend/uri are cloned under the lock, the lock is released,
//! then the call runs.
//!
//! ## Configuration
//!
//! The library is selected from the `Storage` config group (all defaults
//! are config-driven, plan §5):
//!
//! - `Storage/LibraryUri` — an explicit library URI, returned verbatim by
//!   [`library_uri`] and resolved by the process-wide registry
//!   ([`library_backend`]). When set (non-empty) it overrides the legacy
//!   `Backend` + `SqlitePath`/`PgUrl` derivation.
//! - `Storage/Backend` — the legacy selector: `"sqlite"` (the documented
//!   default value), `"database"` or `"pg"` enable the write-through; any
//!   other explicit value (e.g. `"off"`) disables it. When the key is
//!   absent and no `LibraryUri` is set, no library is configured:
//!   projects stay unbound and the undo path runs without touching a
//!   database (graceful degradation — this is what keeps headless
//!   consumers and the test suite from writing to the user's default
//!   library).
//! - `Storage/SqlitePath` — the SQLite library file; default (used when
//!   storage is enabled) `<system data directory>/library.db` (the same
//!   location `FileFunctions::get_configuration_location` derives,
//!   honoring `QUERCUS_CONFIG_DIR` and portable mode).
//! - `Storage/PgUrl` — the PostgreSQL connection string (plan D3), used
//!   when `Storage/Backend` is `"pg"`: `user:pass@host:5432/dbname`
//!   (libpq URL form; an optional `postgres://`/`postgresql://` scheme is
//!   accepted and stripped). The resolved library URI is
//!   `quercusdb+pg://<PgUrl>`. When `Backend = "pg"` but `PgUrl` is absent
//!   or empty, no library is configured (same graceful degradation).
//! - `Storage/WriteDebounceMs` — coalesced-write quiet period (default
//!   500; ≤ 0 writes on the next tick).
//! - `Storage/SnapshotIntervalSec` — snapshot period (default 600; ≤ 0
//!   acts every tick).
//!
//! ## Background thread and exit flush
//!
//! One background thread serves both coalescing and snapshots. It wakes
//! at most every 250 ms (and immediately when a binding is marked
//! pending / bound): it writes every coalesced binding whose pending
//! state is older than the debounce, and snapshot-dirties the bindings
//! that were written since the last `Storage/SnapshotIntervalSec` pass.
//! Snapshots are latest-wins: the backend writes the full payload at the
//! current head seq and prunes to the newest three. The thread stops on
//! the exit flush. [`flush_all`] (exported by the facade as
//! `quercusengine_storage_flush`) is the exit path: it stops the thread,
//! then writes through and snapshots every still-bound project (save +
//! snapshot drain). A write-through failure never propagates to the
//! caller — it is recorded in the binding's `last_error` ([`last_error`])
//! and the project keeps working (graceful degradation, plan §5).

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use quercus_undo::global;

use crate::backend::{
	StorageBackend, WRITE_POLICY_COALESCED, WRITE_POLICY_EXPLICIT, WRITE_POLICY_IMMEDIATE,
};
use crate::handle::CHandle;
use crate::nodeutil::{make_project_owned, release_project, ProjectArc};
use crate::uri::StorageUri;

/// Maximum background-tick latency: the coalescing thread never sleeps
/// longer than this, so a pending write and the exit signal are seen
/// promptly (the condvar wakes it earlier on bind/notification).
const BACKGROUND_TICK: Duration = Duration::from_millis(250);

/// Default `Storage/WriteDebounceMs` (coalesced-write quiet period).
const DEFAULT_WRITE_DEBOUNCE_MS: i32 = 500;

/// Default `Storage/SnapshotIntervalSec` (snapshot period).
const DEFAULT_SNAPSHOT_INTERVAL_SEC: i32 = 600;

/// One bound project: its library session (uri + row uuid + resolved
/// backend) plus the write state.
struct Binding {
	/// The boxed project (kept alive here past the caller's own handle
	/// reference).
	project: ProjectArc,
	/// Library uri (`quercusdb+sqlite:///…`, `mock://…`, …).
	uri: String,
	/// Library row uuid.
	uuid: String,
	/// The backend that claimed `uri` in the registry (resolved at bind
	/// time; no private fallback instance).
	backend: Arc<dyn StorageBackend>,
	/// The backend's write policy hint, normalized so `0` is
	/// [`WRITE_POLICY_IMMEDIATE`] (defensive).
	policy: u32,
	/// A write is pending for the background/flush path (coalesced /
	/// explicit policies only).
	pending_write: bool,
	/// Whether a write-through happened since the last successful
	/// snapshot (the background thread and the flush drain on this).
	needs_snapshot: bool,
	/// When the pending write was last marked (the coalescing debounce
	/// clock; also distinguishes a write started before a newer command
	/// from one that already covers it).
	dirty_since: Option<Instant>,
	/// Monotonic counter bumped on every write-through; a snapshot only
	/// clears `needs_snapshot` when the generation still matches, so a
	/// write that lands while the snapshot runs is not lost.
	write_gen: u64,
	/// Last write-through / snapshot error (graceful degradation).
	last_error: Option<String>,
}

/// project identity (boxed project allocation) -> binding.
fn bindings() -> &'static Mutex<HashMap<usize, Binding>> {
	static BINDINGS: OnceLock<Mutex<HashMap<usize, Binding>>> = OnceLock::new();
	BINDINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Snapshot/coalescing-thread runtime state.
struct SnapshotRuntime {
	/// The background thread (None after the exit flush joined it).
	handle: Option<std::thread::JoinHandle<()>>,
	/// Exit request (set by [`flush_all`]).
	stop: bool,
}

static SNAPSHOT: Mutex<SnapshotRuntime> = Mutex::new(SnapshotRuntime {
	handle: None,
	stop: false,
});

/// Wakes the background thread on bind/pending/flush/stop.
static SNAPSHOT_CV: Condvar = Condvar::new();

// ---------------------------------------------------------------------------
// Registry lookup
// ---------------------------------------------------------------------------

/// The backend the registry arbitrates for `uri` (the write-through,
/// snapshot and library-manager paths all use this one resolution point).
pub fn library_backend(uri: &StorageUri) -> crate::error::Result<Arc<dyn StorageBackend>> {
	crate::registry::Registry::global().resolve(uri)
}

// ---------------------------------------------------------------------------
// Command-success subscription
// ---------------------------------------------------------------------------

/// Subscribe this module's [`note_command`] to the quercusundo process-wide
/// stack's command-success observers (idempotent; the callback is a
/// `fn()`, so a single registration covers every future command).
fn ensure_command_observer() {
	static ONCE: OnceLock<()> = OnceLock::new();
	ONCE.get_or_init(|| {
		global::add_observer(note_command);
	});
}

// ---------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------

/// Bind `project` to the configured default library. No-op when the
/// backend is disabled by config, the configured uri has no claiming
/// backend, the project cannot be addressed (no uuid), or it is already
/// bound. No database write happens here — the first undo-path operation
/// (or the policy's flush) creates the library row.
///
/// Facade entry: takes the project as a `CHandle` (the engine's project
/// handle form) and converts it to the boxed project at the boundary;
/// everything from here on works with the [`ProjectArc`].
pub fn bind_project(project: CHandle) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		if project.is_null() {
			return;
		}
		// SAFETY: facade project handles box a `ProjectArc`.
		let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
			return;
		};
		let key = Arc::as_ptr(&arc) as usize;
		{
			let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if g.contains_key(&key) {
				return; // Already bound (re-bind): nothing to refresh.
			}
		}
		if !storage_enabled() {
			return;
		}
		let Some(uri) = library_uri() else {
			return;
		};
		let uuid = {
			let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
			guard.uuid.clone()
		};
		if uuid.is_empty() {
			return;
		}
		let Ok(parsed) = StorageUri::parse(&uri) else {
			return;
		};
		// The registry is the only resolution point: no private fallback
		// backend instance (another backend may claim the scheme).
		let Ok(backend) = library_backend(&parsed) else {
			return;
		};
		let policy = normalize_policy(backend.write_policy());
		bindings().lock().unwrap_or_else(|e| e.into_inner()).insert(
			key,
			Binding {
				project: arc,
				uri,
				uuid,
				backend,
				policy,
				pending_write: false,
				needs_snapshot: false,
				dirty_since: None,
				write_gen: 0,
				last_error: None,
			},
		);
		ensure_command_observer();
		ensure_thread();
		SNAPSHOT_CV.notify_all();
	}));
}

/// Normalize a backend's policy hint: `WRITE_POLICY_DEFAULT` (0) and any
/// value this module does not know are immediate (defensive — a foreign
/// or future value must not mean "never write").
fn normalize_policy(raw: u32) -> u32 {
	match raw {
		WRITE_POLICY_COALESCED => WRITE_POLICY_COALESCED,
		WRITE_POLICY_EXPLICIT => WRITE_POLICY_EXPLICIT,
		_ => WRITE_POLICY_IMMEDIATE,
	}
}

/// Flush `project`'s pending writes and drop its binding (closing the
/// project). No-op when not bound.
pub fn unbind_project(project: CHandle) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		// SAFETY: facade project handles box a `ProjectArc`.
		let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
			return;
		};
		let key = Arc::as_ptr(&arc) as usize;
		flush_one(key);
		let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		// Dropping the binding releases the project reference.
		g.remove(&key);
	}));
}

/// Whether `project` currently has a binding (status-bar / D5 surface).
pub fn is_bound(project: CHandle) -> bool {
	if project.is_null() {
		return false;
	}
	// SAFETY: facade project handles box a `ProjectArc`.
	let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
		return false;
	};
	bindings()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.contains_key(&(Arc::as_ptr(&arc) as usize))
}

/// The last write-through / snapshot error of `project` (empty when none
/// or not bound).
pub fn last_error(project: CHandle) -> Option<String> {
	if project.is_null() {
		return None;
	}
	// SAFETY: facade project handles box a `ProjectArc`.
	let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
		return None;
	};
	bindings()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&(Arc::as_ptr(&arc) as usize))
		.and_then(|b| b.last_error.clone())
}

// ---------------------------------------------------------------------------
// Write-through
// ---------------------------------------------------------------------------

/// Persist every bound project after a successful undo-path operation
/// (push / group_end / jump). Registered as the quercusundo global-stack
/// command-success observer; the backend diffs each project against its
/// own head, so unchanged projects are no-op touches.
///
/// Immediate bindings write synchronously here (today's behavior).
/// Coalesced bindings are only marked pending and wake the background
/// thread; explicit bindings are marked pending for the flush path. The
/// flags are updated under the bindings mutex, but no backend I/O runs
/// while it is held.
pub fn note_command() {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		let mut immediate = Vec::new();
		let mut wake = false;
		{
			let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			let now = Instant::now();
			for (key, b) in g.iter_mut() {
				match b.policy {
					WRITE_POLICY_COALESCED => {
						// Restart the debounce clock: a continuous edit
						// stream is written once it goes quiet.
						b.pending_write = true;
						b.dirty_since = Some(now);
						wake = true;
					}
					WRITE_POLICY_EXPLICIT => {
						b.pending_write = true;
						b.dirty_since = Some(now);
					}
					_ => immediate.push(*key),
				}
			}
		}
		if wake {
			SNAPSHOT_CV.notify_all();
		}
		// I/O outside the bindings lock.
		for key in immediate {
			write_through(key);
		}
	}));
}

/// Save the project of `key` through its bound backend. Failures are
/// recorded in `last_error`, never propagated; `true` means the save
/// landed (and a snapshot is now due).
fn write_through(key: usize) -> bool {
	let (project, backend, uri, marker) = {
		let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		match g.get_mut(&key) {
			Some(b) => (
				b.project.clone(),
				b.backend.clone(),
				b.uri.clone(),
				b.dirty_since,
			),
			None => return false,
		}
	};
	let parsed = match StorageUri::parse(&uri) {
		Ok(u) => u,
		Err(_) => {
			record_error(key, "invalid storage uri");
			return false;
		}
	};
	// Facade boundary: the trait's `save` takes an owned handle (refcount
	// 1), which is released again after the backend cloned the project.
	let handle = make_project_owned(project);
	let result = backend.save(handle, &parsed, 0);
	release_project(handle);
	match result {
		Ok(()) => {
			let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if let Some(b) = g.get_mut(&key) {
				b.write_gen = b.write_gen.wrapping_add(1);
				b.needs_snapshot = true;
				b.last_error = None;
				// Only clear the pending flag when no newer command
				// arrived while the save ran (its `dirty_since` differs);
				// otherwise the next pass writes the newer state too.
				if b.dirty_since == marker {
					b.pending_write = false;
					b.dirty_since = None;
				}
			}
			true
		}
		Err(e) => {
			record_error(key, &format!("{e}"));
			false
		}
	}
}

/// Record a write-through / snapshot failure (graceful degradation). A
/// still-pending coalesced write keeps its pending flag so the debounce
/// thread / flush retries it, but the debounce clock restarts so a dead
/// backend is retried at most once per period.
fn record_error(key: usize, message: &str) {
	let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
	if let Some(b) = g.get_mut(&key) {
		b.last_error = Some(message.to_string());
		if b.pending_write {
			b.dirty_since = Some(Instant::now());
		}
	}
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// The legacy-`Backend` values that select the write-through (the
/// documented default is `"sqlite"`).
fn is_legacy_backend(value: &str) -> bool {
	matches!(value, "sqlite" | "database" | "pg")
}

/// Whether write-through storage is enabled (config-driven, plan §5).
///
/// - an explicit `Storage/Backend` value other than `"sqlite"`,
///   `"database"` or `"pg"` (e.g. `"off"`) disables it, even when a
///   `Storage/LibraryUri` is set;
/// - a non-empty `Storage/LibraryUri` enables it;
/// - otherwise the legacy `Backend` selector enables it for those three
///   values.
///
/// When no `Storage` configuration is present the backend is NOT enabled
/// — "no library configured" degrades gracefully to plain unbound
/// projects, which keeps headless consumers (the CLI) and the test suite
/// from ever writing to the user's default library. The documented
/// default *values* are `Backend = "sqlite"` and `SqlitePath = <system
/// data dir>/library.db` (used once storage is enabled without an
/// explicit path).
pub fn storage_enabled() -> bool {
	let store = quercus_core::configstore::ConfigStore::instance();
	let backend = store
		.get(Some("Storage"), "Backend")
		.ok()
		.map(|b| b.trim().to_string());
	if let Some(value) = backend.as_deref() {
		if !value.is_empty() && !is_legacy_backend(value) {
			return false;
		}
	}
	if configured_library_uri().is_some() {
		return true;
	}
	backend.as_deref().is_some_and(is_legacy_backend)
}

/// The explicit `Storage/LibraryUri` (empty value = not configured).
fn configured_library_uri() -> Option<String> {
	let store = quercus_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "LibraryUri") {
		Ok(u) if !u.trim().is_empty() => Some(u.trim().to_string()),
		_ => None,
	}
}

/// The configured SQLite library path (empty value = the default).
fn configured_sqlite_path() -> Option<String> {
	let store = quercus_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "SqlitePath") {
		Ok(p) if !p.trim().is_empty() => Some(p.trim().to_string()),
		_ => None,
	}
}

/// The configured PostgreSQL connection string (`Storage/PgUrl`; empty
/// value = not configured).
fn configured_pg_url() -> Option<String> {
	let store = quercus_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "PgUrl") {
		Ok(u) if !u.trim().is_empty() => Some(u.trim().to_string()),
		_ => None,
	}
}

/// The `quercusdb+pg://…` uri of the configured PostgreSQL library (None
/// when `Storage/PgUrl` is absent). A `postgres://`/`postgresql://`
/// scheme on the config value is stripped — the quercusdb uri body is the
/// bare connection string (`user:pass@host:5432/dbname`).
fn pg_library_uri() -> Option<String> {
	let url = configured_pg_url()?;
	let body = url
		.strip_prefix("postgres://")
		.or_else(|| url.strip_prefix("postgresql://"))
		.unwrap_or(&url);
	Some(format!("quercusdb+pg://{body}"))
}

/// The default library file: `<system data directory>/library.db`, where
/// the data directory is the standard per-user location
/// (`FileFunctions::get_configuration_location`: macOS Application
/// Support / XDG config, honoring `QUERCUS_CONFIG_DIR` and portable mode).
pub fn default_library_path() -> String {
	let dir = quercus_core::filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
	format!("{}/library.db", dir)
}

/// The `quercusdb+…` uri of the configured library (None when the path
/// cannot be made absolute, or the PG url is missing). Shared with the
/// facade's library-manager exports.
///
/// `Storage/LibraryUri`, when set, wins verbatim (trimmed) — the registry
/// decides which backend serves it; otherwise the legacy `Backend` +
/// `SqlitePath`/`PgUrl` derivation applies.
pub fn library_uri() -> Option<String> {
	if let Some(uri) = configured_library_uri() {
		return Some(uri);
	}
	let store = quercus_core::configstore::ConfigStore::instance();
	if store.get(Some("Storage"), "Backend").ok().as_deref() == Some("pg") {
		return pg_library_uri();
	}
	let path = match configured_sqlite_path() {
		Some(p) => p,
		None => default_library_path(),
	};
	let abs = std::path::absolute(&path).ok()?;
	Some(format!("quercusdb+sqlite://{}", abs.display()))
}

// ---------------------------------------------------------------------------
// Background thread: coalescing + snapshots
// ---------------------------------------------------------------------------

/// Spawn the background thread (a no-op when one is already running). The
/// thread is (re)startable after an exit flush.
fn ensure_thread() {
	let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
	let alive = rt
		.handle
		.as_ref()
		.map(|h| !h.is_finished())
		.unwrap_or(false);
	if !alive {
		rt.stop = false;
		rt.handle = Some(
			std::thread::Builder::new()
				.name("quercus-storage-snapshot".into())
				.spawn(snapshot_loop)
				.expect("snapshot thread spawn"),
		);
	}
}

/// The thread loop: on every tick, flush the coalesced writes whose
/// debounce elapsed and (when the snapshot period elapsed) snapshot the
/// dirty bindings. The condvar makes the 250 ms sleep interruptible
/// (pending marks, bind notifications and the exit flush's stop signal
/// wake it immediately).
fn snapshot_loop() {
	let mut last_snapshot = Instant::now();
	loop {
		{
			let rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
			if rt.stop {
				break;
			}
		}
		flush_pending_coalesced();
		if last_snapshot.elapsed() >= snapshot_interval() {
			snapshot_dirty();
			last_snapshot = Instant::now();
		}
		let rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
		let (rt, _) = SNAPSHOT_CV
			.wait_timeout(rt, BACKGROUND_TICK)
			.unwrap_or_else(|e| e.into_inner());
		if rt.stop {
			break;
		}
	}
}

/// The coalesced-write quiet period: `Storage/WriteDebounceMs` (default
/// 500), with ≤ 0 treated as "write on the next tick".
fn write_debounce() -> Duration {
	let ms = quercus_core::configstore::ConfigStore::instance().get_int(
		Some("Storage"),
		"WriteDebounceMs",
		DEFAULT_WRITE_DEBOUNCE_MS,
	);
	if ms <= 0 {
		Duration::ZERO
	} else {
		Duration::from_millis(ms as u64)
	}
}

/// The snapshot period: `Storage/SnapshotIntervalSec` (default 600), with
/// ≤ 0 treated as "every tick".
fn snapshot_interval() -> Duration {
	let secs = quercus_core::configstore::ConfigStore::instance().get_int(
		Some("Storage"),
		"SnapshotIntervalSec",
		DEFAULT_SNAPSHOT_INTERVAL_SEC,
	);
	if secs <= 0 {
		Duration::ZERO
	} else {
		Duration::from_secs(secs as u64)
	}
}

/// Write every coalesced binding whose pending state has been quiet for
/// the debounce period (the edit thread never sees this I/O).
fn flush_pending_coalesced() {
	let debounce = write_debounce();
	let keys: Vec<usize> = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		g.iter()
			.filter(|(_, b)| {
				b.policy == WRITE_POLICY_COALESCED
					&& b.pending_write
					&& b.dirty_since
						.map(|t| t.elapsed() >= debounce)
						.unwrap_or(true)
			})
			.map(|(k, _)| *k)
			.collect()
	};
	for key in keys {
		write_through(key);
	}
}

/// Snapshot every binding whose write-through landed since the last
/// pass. Snapshots are latest-wins (a project edited since the last pass
/// snapshots at the newer head; an unchanged head is a backend no-op).
fn snapshot_dirty() {
	let jobs: Vec<(usize, Arc<dyn StorageBackend>, String, String, u64)> = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		g.iter()
			.filter(|(_, b)| b.needs_snapshot)
			.map(|(k, b)| {
				(
					*k,
					b.backend.clone(),
					b.uri.clone(),
					b.uuid.clone(),
					b.write_gen,
				)
			})
			.collect()
	};
	for (key, backend, uri, uuid, gen) in jobs {
		snapshot_binding(key, &*backend, &uri, &uuid, gen);
	}
}

/// Snapshot one binding (its current pending generation). Backend I/O
/// runs without the bindings mutex held.
fn snapshot_one(key: usize) {
	let job = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		match g.get(&key) {
			Some(b) => Some((
				b.backend.clone(),
				b.uri.clone(),
				b.uuid.clone(),
				b.write_gen,
			)),
			None => None,
		}
	};
	let Some((backend, uri, uuid, gen)) = job else {
		return;
	};
	snapshot_binding(key, &*backend, &uri, &uuid, gen);
}

/// Snapshot the row at `uri`/`uuid`; on success clear `needs_snapshot`
/// only when no write landed while the snapshot ran (the generation
/// counter detects it). Failures are recorded, never propagated.
fn snapshot_binding(key: usize, backend: &dyn StorageBackend, uri: &str, uuid: &str, gen: u64) {
	let parsed = match StorageUri::parse(uri) {
		Ok(u) => u,
		Err(_) => {
			record_error(key, "invalid storage uri");
			return;
		}
	};
	match backend.library_snapshot(&parsed, uuid) {
		Ok(()) => {
			let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if let Some(b) = g.get_mut(&key) {
				if b.write_gen == gen {
					b.needs_snapshot = false;
				}
			}
		}
		Err(e) => record_error(key, &format!("{e}")),
	}
}

// ---------------------------------------------------------------------------
// Exit flush
// ---------------------------------------------------------------------------

/// The exit path: stop the background thread, then save + snapshot every
/// still-bound project (plan §2 "退出前 flush"). Idempotent; safe to call
/// again after new projects are bound (the thread restarts on the next
/// bind).
pub fn flush_all() {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		// 1. Stop the thread and wait for it to finish its pass.
		{
			let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
			rt.stop = true;
		}
		SNAPSHOT_CV.notify_all();
		{
			let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
			if let Some(h) = rt.handle.take() {
				drop(rt);
				let _ = h.join();
			}
		}
		// 2. Drain every binding (write-through + snapshot).
		let keys: Vec<usize> = {
			let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			g.keys().copied().collect()
		};
		for key in keys {
			flush_one(key);
		}
	}));
}

/// Flush one binding: pending write-through first, then a snapshot when
/// one is due. A clean binding (nothing pending, nothing to snapshot)
/// does no I/O — the flush must not invent library rows. Idempotent.
fn flush_one(key: usize) {
	let pending = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		match g.get(&key) {
			Some(b) => b.pending_write,
			None => return,
		}
	};
	if pending && !write_through(key) {
		// The save failed (recorded in `last_error`): do not snapshot a
		// head that did not advance.
		return;
	}
	let needs_snapshot = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		g.get(&key).map(|b| b.needs_snapshot).unwrap_or(false)
	};
	if needs_snapshot {
		snapshot_one(key);
	}
}
