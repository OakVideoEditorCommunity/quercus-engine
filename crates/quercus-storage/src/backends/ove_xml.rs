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

//! The `.ove` XML file backend. Graph (de)serialization is delegated to
//! quercusnode's serializer (`bridge::node`); this backend owns the file
//! container: the plain XML file I/O and the mapping of the serializer's
//! version probe to the M10 §2.1 info codes.
//!
//! The file format is the current-version `<project version="1">`
//! document produced by `quercus_node::serializer::save`. Historical files
//! carry an `<olive version="NNNNNN">` root; the serializer probes the
//! root element (`quercus_node::serializer::probe_version`) and this
//! backend reports TOO_OLD / TOO_NEW / UNKNOWN_VERSION per M10 §2.1
//! before delegating to the serializer.
//!
//! `QUERCUSSTORAGE_SAVE_COMPRESS` is accepted but not implemented: the
//! quercusnode serializer emits plain XML only (the OVEC compressed
//! container is a C++ follow-up), so the bit is ignored and the file is
//! written uncompressed. Unknown option bits are ignored too (M10 §2.2).

use crate::backend::LoadResult;
use crate::error::{Error, QUERCUSSTORAGE_OK, QUERCUSSTORAGE_TOO_NEW, QUERCUSSTORAGE_TOO_OLD, QUERCUSSTORAGE_UNKNOWN_VERSION};
use crate::nodeutil::ProjectArc;
use crate::uri::StorageUri;
use quercus_node::serializer::{probe_version, VersionProbe};

/// The ove-xml backend (`file://` + `.ove`).
pub struct OveXmlBackend;

impl OveXmlBackend {
	/// Construct.
	pub fn new() -> Self {
		OveXmlBackend
	}

	/// Save a project (already read out of its handle) to the URI. The
	/// Rust-typed inner path: the facade-facing trait `save` converts the
	/// project handle and forwards here.
	pub fn save_project(
		&self,
		project: &ProjectArc,
		uri: &StorageUri,
		_options: u32,
	) -> crate::error::Result<()> {
		let path = uri
			.local_path()
			.ok_or(Error::Invalid)?
			.to_string();
		let xml = {
			let guard = project
				.lock()
				.map_err(|_| Error::State)?;
			crate::nodeutil::serializer_save(&guard)?
		};
		std::fs::write(&path, xml).map_err(|e| Error::Io(e.to_string()))?;
		Ok(())
	}
}

impl Default for OveXmlBackend {
	fn default() -> Self {
		Self::new()
	}
}

impl crate::backend::StorageBackend for OveXmlBackend {
	fn name(&self) -> &'static str {
		"ove-xml"
	}

	fn uri_scheme(&self) -> &'static str {
		"file"
	}

	fn can_handle(&self, uri: &StorageUri) -> bool {
		uri.extension().map(|e| e == "ove").unwrap_or(false)
	}

	fn load(&self, uri: &StorageUri) -> crate::error::Result<LoadResult> {
		let path = uri
			.local_path()
			.ok_or(Error::Invalid)?
			.to_string();
		let xml = std::fs::read_to_string(&path).map_err(|e| Error::Io(e.to_string()))?;

		let probe = probe_version(&xml).ok_or_else(|| {
			Error::Format(format!("'{}' is not a parseable project document", path))
		})?;
		match probe {
			VersionProbe::TooNew => return Ok(LoadResult::info_only(QUERCUSSTORAGE_TOO_NEW)),
			VersionProbe::Unknown => {
				return Ok(LoadResult::info_only(QUERCUSSTORAGE_UNKNOWN_VERSION));
			}
			VersionProbe::Current | VersionProbe::Old => {}
		}

		let project = crate::nodeutil::serializer_load(&xml)?;
		let handle = crate::nodeutil::make_project_owned(project);
		let version_info = if matches!(probe, VersionProbe::Old) {
			QUERCUSSTORAGE_TOO_OLD
		} else {
			QUERCUSSTORAGE_OK
		};
		Ok(LoadResult::with_info(handle, version_info))
	}

	fn save(
		&self,
		project: crate::handle::CHandle,
		uri: &StorageUri,
		options: u32,
	) -> crate::error::Result<()> {
		// Facade boundary: convert the project handle to the boxed
		// project, then run the Rust-typed save.
		let arc = unsafe { crate::nodeutil::project_arc(&project)? };
		self.save_project(&arc, uri, options)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::backend::StorageBackend;

	/// Write `xml` to a temp `.ove` file and return its path + URI.
	fn temp_ove(tag: &str, xml: &str) -> (std::path::PathBuf, StorageUri) {
		let path = std::env::temp_dir().join(format!(
			"quercusstorage_ove_{}_{}.ove",
			tag,
			std::process::id()
		));
		std::fs::write(&path, xml).expect("write temp ove");
		let uri = StorageUri::parse(path.to_str().expect("utf-8 temp path")).expect("parse uri");
		(path, uri)
	}

	#[test]
	fn load_maps_the_shared_probe_to_info_codes() {
		let backend = OveXmlBackend::new();

		let (path, uri) = temp_ove("too_new", "<olive version=\"999999\"></olive>");
		let result = backend.load(&uri).expect("probe load");
		assert!(result.project.is_null());
		assert_eq!(result.version_info, QUERCUSSTORAGE_TOO_NEW);
		let _ = std::fs::remove_file(path);

		let (path, uri) = temp_ove("unknown", "<thirdparty></thirdparty>");
		let result = backend.load(&uri).expect("probe load");
		assert!(result.project.is_null());
		assert_eq!(result.version_info, QUERCUSSTORAGE_UNKNOWN_VERSION);
		let _ = std::fs::remove_file(path);

		// Unparseable input stays a format error, not a version code.
		let (path, uri) = temp_ove("not_xml", "not xml");
		assert!(matches!(backend.load(&uri), Err(Error::Format(_))));
		let _ = std::fs::remove_file(path);

		// Current document: serialize a fresh project through the same
		// serializer so the probe accepts it as Current.
		let project = quercus_node::project::Project::new();
		let xml = {
			let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
			guard.initialize().expect("initialize");
			quercus_node::serializer::save(&guard).expect("save")
		};
		let (path, uri) = temp_ove("current", &xml);
		let result = backend.load(&uri).expect("load current");
		assert_eq!(result.version_info, QUERCUSSTORAGE_OK);
		assert!(!result.project.is_null());
		crate::nodeutil::release_project(result.project);
		let _ = std::fs::remove_file(path);
	}
}
