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

//! The backend interface. `StorageBackend` is the Rust trait shape of
//! M10 §2.3's manual C vtable; `ffi.rs` marshals foreign (C-side)
//! vtable registrations onto the same registry.

use crate::uri::StorageUri;

/// Outcome of a backend load: the loaded project plus the version
/// information code the caller surfaces through `quercusstorage_open`'s
/// `result_code` (M10 §2.2). Version probing (TOO_OLD / TOO_NEW /
/// UNKNOWN_VERSION) is the backend's own judgement; a "not loadable"
/// outcome carries an empty `project` handle and a positive info code,
/// so the caller can report it instead of a hard error.
#[derive(Clone, Debug)]
pub struct LoadResult {
	/// The loaded project handle (owned, refcount 1; empty when version
	/// probing declined to load).
	pub project: crate::handle::CHandle,
	/// Version info code: `QUERCUSSTORAGE_OK` or a positive info code
	/// (TOO_OLD / TOO_NEW / UNKNOWN_VERSION).
	pub version_info: i32,
}

impl LoadResult {
	/// A normal successful load.
	pub fn success(project: crate::handle::CHandle) -> Self {
		LoadResult {
			project,
			version_info: crate::error::QUERCUSSTORAGE_OK,
		}
	}

	/// A loaded project plus an info code (e.g. TOO_OLD: the project was
	/// written by an older build and has been upgraded on the fly).
	pub fn with_info(project: crate::handle::CHandle, version_info: i32) -> Self {
		LoadResult {
			project,
			version_info,
		}
	}

	/// No project — only an info code (TOO_NEW / UNKNOWN_VERSION).
	pub fn info_only(version_info: i32) -> Self {
		LoadResult {
			project: crate::handle::CHandle::null(),
			version_info,
		}
	}
}

/// One project-library row in a backend-neutral shape (unix timestamps,
/// no database row ids): what `quercus_storage_library_*` exposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryProjectInfo {
	/// Project uuid (serializer-identical).
	pub uuid: String,
	/// Display name.
	pub name: String,
	/// Payload schema version (the serializer's current version).
	pub schema_ver: i32,
	/// Row creation time (Unix seconds).
	pub created_at: i64,
	/// Last-write time (Unix seconds; the manager sort key).
	pub modified_at: i64,
}

/// Derived graph statistics of one library project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryProjectStats {
	/// Timeline duration in milliseconds (longest sequence).
	pub duration_ms: i64,
	/// Total tracks across all sequences.
	pub track_count: i32,
	/// Total clip blocks across all tracks.
	pub clip_count: i32,
	/// Total footage nodes in the graph.
	pub footage_count: i32,
}

/// Derive the manager statistics of a live project.
///
/// This is the backend-neutral derivation the engine uses when a backend
/// cannot answer [`StorageBackend::library_stats`] from its own store:
/// traversal starts at every sequence, counts its track lists' tracks and
/// their clip blocks, and takes the longest sequence's end point as the
/// duration. Footage nodes are counted directly in the graph. A clip that
/// appears in more than one track is counted once per membership.
pub fn derive_library_stats(project: &quercus_node::project::Project) -> LibraryProjectStats {
	use quercus_node::block::{ClipBlockBehavior, GapBlockBehavior};
	use quercus_node::sequence::SequenceBehavior;
	use quercus_node::track::{TrackBehavior, TrackListBehavior};

	let mut stats = LibraryProjectStats::default();
	for id in project.graph.node_ids() {
		let entry = match project.graph.get(id) {
			Some(e) => e,
			None => continue,
		};
		if let Some(seq) = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		{
			let mut seq_len = quercus_core::Rational::new(0, 1);
			for tl_id in &seq.track_lists {
				let tl = match project.graph.get(*tl_id).and_then(|e| {
					e.behavior
						.as_any()
						.and_then(|a| a.downcast_ref::<TrackListBehavior>())
				}) {
					Some(t) => t,
					None => continue,
				};
				stats.track_count += tl.tracks.len() as i32;
				for t_id in &tl.tracks {
					let t = match project.graph.get(*t_id).and_then(|e| {
						e.behavior
							.as_any()
							.and_then(|a| a.downcast_ref::<TrackBehavior>())
					}) {
						Some(t) => t,
						None => continue,
					};
					for b in &t.blocks {
						let entry = project.graph.get(*b);
						if let Some(c) = entry.and_then(|e| {
							e.behavior
								.as_any()
								.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
						}) {
							stats.clip_count += 1;
							if c.core.out() > seq_len {
								seq_len = c.core.out();
							}
						} else if let Some(g) = entry.and_then(|e| {
							e.behavior
								.as_any()
								.and_then(|a| a.downcast_ref::<GapBlockBehavior>())
						}) {
							if g.core.out() > seq_len {
								seq_len = g.core.out();
							}
						}
					}
				}
			}
			let ms = (seq_len.to_f64() * 1000.0).round() as i64;
			if ms > stats.duration_ms {
				stats.duration_ms = ms;
			}
		} else if entry.behavior.type_id() == "org.olivevideoeditor.Olive.footage" {
			stats.footage_count += 1;
		}
	}
	stats
}

/// Write policy: the backend did not choose (the engine default applies).
pub const WRITE_POLICY_DEFAULT: u32 = 0;
/// Write every successful command synchronously (crash-safe, local disks).
pub const WRITE_POLICY_IMMEDIATE: u32 = 1;
/// Mark dirty and let the engine's background thread coalesce writes.
pub const WRITE_POLICY_COALESCED: u32 = 2;
/// Write only on explicit flush / unbind / exit.
pub const WRITE_POLICY_EXPLICIT: u32 = 3;

/// A storage backend (M10 §2.3 vtable semantics).
pub trait StorageBackend: Send + Sync {
	/// Backend name ("ove-xml" / "otio" / "quercusdb").
	fn name(&self) -> &str;

	/// URI scheme this backend serves ("file" / "quercusdb").
	fn uri_scheme(&self) -> &str;

	/// Whether this backend claims the URI (suffix, magic bytes,
	/// reachability — backend's own judgement).
	fn can_handle(&self, uri: &StorageUri) -> bool;

	/// Load a project; returns an owned quercusnode project handle
	/// (CHandle with refcount 1) wrapped in a [`LoadResult`], or an
	/// error code/context.
	fn load(&self, uri: &StorageUri) -> crate::error::Result<LoadResult>;

	/// Save a project to the URI. `options` is the M10 bitmask
	/// (QUERCUSSTORAGE_SAVE_COMPRESS etc.); unknown bits are ignored.
	fn save(
		&self,
		project: crate::handle::CHandle,
		uri: &StorageUri,
		options: u32,
	) -> crate::error::Result<()>;

	/// Whether this backend implements the project-library operations
	/// below. The default is `false` (a pure file/interchange backend).
	fn supports_library(&self) -> bool {
		false
	}

	/// The library rows at `uri` (registration order / manager sort is the
	/// backend's choice; the engine preserves the returned order).
	fn library_list(&self, _uri: &StorageUri) -> crate::error::Result<Vec<LibraryProjectInfo>> {
		Err(crate::error::Error::Unsupported)
	}

	/// Delete the row `uuid` from the library at `uri`.
	fn library_delete(&self, _uri: &StorageUri, _uuid: &str) -> crate::error::Result<()> {
		Err(crate::error::Error::Unsupported)
	}

	/// Rename the row `uuid` in the library at `uri`.
	fn library_rename(
		&self,
		_uri: &StorageUri,
		_uuid: &str,
		_name: &str,
	) -> crate::error::Result<()> {
		Err(crate::error::Error::Unsupported)
	}

	/// Derived statistics for the row `uuid`. Backends that store the graph
	/// in a queryable form implement this; the engine otherwise falls back
	/// to `load` + [`crate::backend::LibraryProjectStats`] derivation.
	fn library_stats(
		&self,
		_uri: &StorageUri,
		_uuid: &str,
	) -> crate::error::Result<LibraryProjectStats> {
		Err(crate::error::Error::Unsupported)
	}

	/// Snapshot (compact / version) the row `uuid`. Whole-document
	/// backends can keep the default no-op: every `save` is already a
	/// snapshot. Journaled backends implement it.
	fn library_snapshot(&self, _uri: &StorageUri, _uuid: &str) -> crate::error::Result<()> {
		Ok(())
	}

	/// The backend's write policy hint for the write-through path
	/// ([`WRITE_POLICY_*`]). Built-ins are immediate; foreign transports
	/// map their vtable hint (`0` becomes coalesced).
	fn write_policy(&self) -> u32 {
		WRITE_POLICY_IMMEDIATE
	}
}
