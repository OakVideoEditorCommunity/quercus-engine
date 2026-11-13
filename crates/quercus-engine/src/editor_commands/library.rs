//! Storage-backed project-library DTOs and CRUD helpers.
//!
//! This module supplies the domain facade in `media.rs`; it does not represent
//! an open editor session or own a live `ProjectRef`.
//!
//! Every operation resolves its backend through
//! [`quercus_storage::writethrough::library_backend`] and then uses only the
//! backend-neutral `StorageBackend` library capability — there are no direct
//! database-backend calls here. Create/duplicate/import/export are generic
//! compositions (load + save through the registry) shared with the
//! `quercus_storage_library_*` C exports.

use std::sync::Arc;

use quercus_storage::backend::StorageBackend;
use quercus_storage::uri::StorageUri;

/// Owned project-library row enriched with aggregate timeline statistics.
/// Timestamps are whole Unix seconds; duration is milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryProject {
	pub uuid: String,
	pub name: String,
	pub created_at: i64,
	pub modified_at: i64,
	pub duration_ms: i64,
	pub track_count: i32,
	pub clip_count: i32,
	pub footage_count: i32,
}

/// Storage-backed project collection located by a parsed library URI.
pub(crate) struct ProjectLibrary {
	uri: StorageUri,
}

impl ProjectLibrary {
	/// Parses and validates the library URI without opening a project.
	pub(crate) fn new(uri: String) -> Result<Self, String> {
		let uri = StorageUri::parse(&uri).map_err(|e| e.to_string())?;
		Ok(Self { uri })
	}

	/// The backend claiming this library (registry resolution).
	fn backend(&self) -> Result<Arc<dyn StorageBackend>, String> {
		quercus_storage::writethrough::library_backend(&self.uri).map_err(|e| e.to_string())
	}

	/// Lists projects and attaches best-effort statistics to each row.
	/// A failed statistics lookup yields zeroes rather than hiding the project.
	pub(crate) fn projects(&self) -> Result<Vec<LibraryProject>, String> {
		let backend = self.backend()?;
		let infos = backend
			.library_list(&self.uri)
			.map_err(|e| e.to_string())?;
		Ok(infos
			.into_iter()
			.map(|info| {
				let stats =
					crate::storage::library_project_stats(backend.as_ref(), &self.uri, &info.uuid)
						.unwrap_or_default();
				LibraryProject {
					uuid: info.uuid,
					name: info.name,
					created_at: info.created_at,
					modified_at: info.modified_at,
					duration_ms: stats.duration_ms,
					track_count: stats.track_count,
					clip_count: stats.clip_count,
					footage_count: stats.footage_count,
				}
			})
			.collect())
	}

	/// Creates and persists an initialized project, returning its stable UUID.
	pub(crate) fn create(&self, name: &str) -> Result<String, String> {
		if name.trim().is_empty() {
			return Err("invalid name".into());
		}
		crate::storage::create_library_project(&self.uri, name).map_err(|e| e.to_string())
	}

	/// Permanently deletes one project from this library.
	pub(crate) fn delete(&self, uuid: &str) -> Result<(), String> {
		let backend = self.backend()?;
		backend
			.library_delete(&self.uri, uuid)
			.map_err(|e| e.to_string())
	}

	/// Changes the library display name without changing the project UUID.
	pub(crate) fn rename(&self, uuid: &str, name: &str) -> Result<(), String> {
		let backend = self.backend()?;
		backend
			.library_rename(&self.uri, uuid, name)
			.map_err(|e| e.to_string())
	}

	/// Duplicates a project and returns the new project's UUID.
	///
	/// Generic composition (load + `deep_copy` + save): the copy has no
	/// journal history, unlike the database backend's native duplicate.
	pub(crate) fn duplicate(&self, uuid: &str) -> Result<String, String> {
		crate::storage::duplicate_library_project(&self.uri, uuid).map_err(|e| e.to_string())
	}

	/// Imports a storage URI into this library and returns the imported UUID.
	pub(crate) fn import(&self, file_uri: &str) -> Result<String, String> {
		let file_uri = StorageUri::parse(file_uri).map_err(|e| e.to_string())?;
		crate::storage::import_library_project(&self.uri, &file_uri).map_err(|e| e.to_string())
	}

	/// Exports one library project to a registry-resolvable storage URI.
	pub(crate) fn export(&self, uuid: &str, file_uri: &str) -> Result<(), String> {
		let file_uri = StorageUri::parse(file_uri).map_err(|e| e.to_string())?;
		crate::storage::export_library_project(&self.uri, uuid, &file_uri)
			.map_err(|e| e.to_string())
	}
}
