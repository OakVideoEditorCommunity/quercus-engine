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

//! File/directory helpers, mirroring `src/common/src/filefunctions.h`
//! and `include/common/filefunctions.h`. Mirrors the filefunctions C++
//! namespace as a handle-bearing family for C ABI shape uniformity; the
//! underlying operations are stateless.
//!
//! Two-stage string getters here return their result via `buf`/`buf_size`
//! (see `crate::ffi`); the domain functions below return the owned string.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// FNV-1a 64-bit hash of `data`, returned as lowercase hex (`%016llx`).
///
/// Mirrors the anonymous `fnv1a_hex` helper in `src/common/src/filefunctions.cpp`.
/// `pub(crate)` since `displayicc` reuses it to name ICC cache files.
pub(crate) fn fnv1a_hex(data: &[u8]) -> String {
	let mut hash: u64 = 14695981039346656037;
	for &c in data {
		hash ^= u64::from(c);
		hash = hash.wrapping_mul(1099511628211);
	}
	format!("{:016x}", hash)
}

/// Case-insensitive check whether `s` ends with `suffix`.
///
/// Mirrors `ends_with_case_insensitive` in `filefunctions.cpp`; only `A-Z`
/// are folded to lower case (like the C++ `a >= 'A' && a <= 'Z'` test).
fn ends_with_case_insensitive(s: &str, suffix: &str) -> bool {
	if suffix.len() > s.len() {
		return false;
	}
	let offset = s.len() - suffix.len();
	s.as_bytes()[offset..]
		.iter()
		.zip(suffix.as_bytes().iter())
		.all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// The filefunctions family (stateless; the handle only exists for C ABI
/// uniformity).
pub struct FileFunctions;

impl FileFunctions {
	/// Creates the filefunctions family object.
	pub fn new() -> Self {
		Self
	}

	/// A deterministic identifier for the file at `filename`, used as a
	/// cache key for project media.
	///
	/// The identifier is the FNV-1a hex hash of the absolute path (not
	/// symlink-resolved) plus the file's modification time in nanoseconds
	/// since the Unix epoch. It therefore changes when the file is
	/// touched, and the empty string means "no identifier": the path
	/// cannot be made absolute, or the file does not exist, or its
	/// metadata/mtime cannot be read.
	///
	/// # Arguments
	///
	/// * `filename` - path to the file, absolute or relative to the
	///   current directory.
	///
	/// # Returns
	///
	/// Always `Ok`; the `Result` exists for C ABI uniformity.
	pub fn get_unique_file_identifier(&self, filename: &str) -> Result<String> {
		// `std::path::absolute` is the std analog of `fs::absolute`: it
		// yields an absolute path without resolving symlinks or normalizing.
		let abs = match std::path::absolute(filename) {
			Ok(p) => p,
			Err(_) => return Ok(String::new()),
		};
		if !abs.exists() {
			return Ok(String::new());
		}

		let metadata = match std::fs::metadata(&abs) {
			Ok(m) => m,
			Err(_) => return Ok(String::new()),
		};
		let mtime = match metadata.modified() {
			Ok(t) => t,
			Err(_) => return Ok(String::new()),
		};

		// C++ appends `to_string(mtime.time_since_epoch().count())`; on
		// macOS `file_time_type` has nanosecond precision, so `.as_nanos()`
		// is the closest equivalent (used only as a cache-key salt).
		// CPP-PARITY: `time_since_epoch().count()` on a pre-epoch timestamp
		// is negative; Rust's `duration_since` returns an error which we fold
		// into the negated magnitude.
		let count: i128 = mtime
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos() as i128)
			.unwrap_or_else(|e| -(e.duration().as_nanos() as i128));

		let mut hash_input = abs.to_string_lossy().into_owned();
		hash_input.push_str(&count.to_string());

		Ok(fnv1a_hex(hash_input.as_bytes()))
	}

	/// The directory holding user configuration (and, since most other
	/// locations derive from it, the cache/data roots).
	///
	/// Resolution order: `$QUERCUS_CONFIG_DIR` when set and non-empty
	/// (also created here), the application directory in portable mode,
	/// then `~/Library/Application Support/quercus` on macOS or
	/// `$XDG_CONFIG_HOME/quercus` (falling back to `~/.config/quercus`)
	/// elsewhere; when no home is known the system temp directory is
	/// used. The resulting directory is created on a best-effort basis and
	/// returned as a lossy UTF-8 path.
	///
	/// Returns `Ok` unconditionally; the `Result` exists for C ABI
	/// uniformity.
	pub fn get_configuration_location(&self) -> Result<String> {
		// Tests and tooling can redirect the configuration (and, since most
		// locations derive from it, the cache/data) root.
		if let Ok(dir) = std::env::var("QUERCUS_CONFIG_DIR") {
			if !dir.is_empty() {
				let _ = std::fs::create_dir_all(&dir);
				return Ok(dir);
			}
		}

		if Self::is_portable() {
			return Ok(Self::application_path());
		}

		// CPP-PARITY: the C++ `#ifdef __APPLE__` picks `~/Library/Application
		// Support`; the non-Apple branch prefers `$XDG_CONFIG_HOME` then
		// `~/.config`. The empty-root fallback is the temp directory.
		#[cfg(target_os = "macos")]
		let config_root = match std::env::var("HOME") {
			Ok(h) if !h.is_empty() => PathBuf::from(h).join("Library").join("Application Support"),
			_ => PathBuf::new(),
		};
		#[cfg(not(target_os = "macos"))]
		let config_root = match std::env::var("XDG_CONFIG_HOME") {
			Ok(x) if !x.is_empty() => PathBuf::from(x),
			_ => match std::env::var("HOME") {
				Ok(h) if !h.is_empty() => PathBuf::from(h).join(".config"),
				_ => PathBuf::new(),
			},
		};

		let config_root = if config_root.as_os_str().is_empty() {
			std::env::temp_dir()
		} else {
			config_root
		};

		let config_dir = config_root.join("quercus");
		let _ = std::fs::create_dir_all(&config_dir);
		Ok(config_dir.to_string_lossy().into_owned())
	}

	/// The directory containing the application executable.
	///
	/// Resolved from the canonicalised `current_exe` (macOS),
	/// `/proc/self/exe` (other Unix) or the current working directory as a
	/// last resort; the empty string means no path could be determined.
	/// Returns `Ok` unconditionally; the `Result` exists for C ABI
	/// uniformity.
	pub fn get_application_path(&self) -> Result<String> {
		Ok(Self::application_path())
	}

	/// Whether the application is running in portable mode (a `portable` file
	/// sits next to the application executable). Mirrors
	/// `FileFunctions::is_portable`.
	fn is_portable() -> bool {
		let app = Self::application_path();
		!app.is_empty() && Path::new(&app).join("portable").exists()
	}

	/// Application path, without the `Result` wrapper (used internally).
	fn application_path() -> String {
		#[cfg(target_os = "macos")]
		{
			// CPP-PARITY: C++ uses `_NSGetExecutablePath` + `weakly_canonical`;
			// `std::env::current_exe` + `canonicalize` is the portable
			// equivalent and yields the same parent directory.
			if let Ok(exe) = std::env::current_exe() {
				let canonical = exe.canonicalize().unwrap_or(exe);
				if let Some(parent) = canonical.parent() {
					return parent.to_string_lossy().into_owned();
				}
			}
		}
		#[cfg(all(unix, not(target_os = "macos")))]
		{
			if let Ok(target) = std::fs::read_link("/proc/self/exe") {
				if let Some(parent) = target.parent() {
					return parent.to_string_lossy().into_owned();
				}
			}
		}

		// Fallback: current working directory
		std::env::current_dir()
			.map(|c| c.to_string_lossy().into_owned())
			.unwrap_or_default()
	}

	/// A per-user scratch directory for temporary files:
	/// `<system temp>/quercus`, created on a best-effort basis.
	///
	/// No file is created, and callers are responsible for cleaning up
	/// what they place there. Returns `Ok` unconditionally; the `Result`
	/// exists for C ABI uniformity.
	pub fn get_temp_file_path(&self) -> Result<String> {
		// CPP-PARITY: `std::env::temp_dir()` never fails the way
		// `fs::temp_directory_path(ec)` can, so the `"." / "quercus-temp"`
		// fallback is unreachable here and omitted.
		let temp_path = std::env::temp_dir().join("quercus");
		let _ = std::fs::create_dir_all(&temp_path);
		Ok(temp_path.to_string_lossy().into_owned())
	}

	/// Auto-recovery root directory.
	pub fn get_auto_recovery_root(&self) -> Result<String> {
		let config = self.get_configuration_location()?;
		Ok(PathBuf::from(config)
			.join("autorecovery")
			.to_string_lossy()
			.into_owned())
	}

	/// Whether copying `source` into `dest` would leave every existing
	/// destination file untouched.
	///
	/// Walks both trees: returns `false` as soon as a source file has a
	/// same-named entry under `dest`, and recurses for subdirectories.
	/// A `source` that cannot be read counts as empty, so the answer is
	/// `true` (matching the C++ iterator semantics).
	///
	/// # Arguments
	///
	/// * `source` - directory that would be copied.
	/// * `dest` - target directory, which need not exist.
	pub fn can_copy_directory_without_overwriting(&self, source: &str, dest: &str) -> bool {
		// CPP-PARITY: a failed `directory_iterator` (e.g. `source` missing)
		// yields an empty iteration, so the function returns `true`.
		let Ok(entries) = std::fs::read_dir(source) else {
			return true;
		};
		for entry in entries.flatten() {
			let dest_equivalent = PathBuf::from(dest).join(entry.file_name());
			// `entry.is_directory(ec)` failing (ec set) falls through to the
			// "else exists" branch in C++; `unwrap_or(false)` mirrors that.
			if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
				if !self.can_copy_directory_without_overwriting(
					&entry.path().to_string_lossy(),
					&dest_equivalent.to_string_lossy(),
				) {
					return false;
				}
			} else if dest_equivalent.exists() {
				return false;
			}
		}
		true
	}

	/// Recursively copies the directory `source` into `dest`, creating
	/// both `dest` and any missing subdirectories.
	///
	/// With `overwrite` the destination files are replaced (on Unix their
	/// write bits are granted first so read-only files can be removed);
	/// without it an existing destination file is reported and skipped.
	/// The function is deliberately forgiving: a missing `source`, a
	/// `dest` that cannot be created, an unreadable source directory and
	/// individual copy failures are logged and skipped rather than
	/// returned as errors, so a partial copy is possible.
	///
	/// # Arguments
	///
	/// * `source` - directory to copy; when it is not a directory the call
	///   is a no-op.
	/// * `dest` - destination directory, created if absent.
	/// * `overwrite` - `true` to replace existing files, `false` to keep
	///   them.
	///
	/// # Errors
	///
	/// Does not currently return `Err`; the `Result` exists for C ABI
	/// uniformity.
	pub fn copy_directory(&self, source: &str, dest: &str, overwrite: bool) -> Result<()> {
		if !Path::new(source).is_dir() {
			eprintln!("Failed to copy directory, source {} didn't exist", source);
			return Ok(());
		}

		if std::fs::create_dir_all(dest).is_err() {
			eprintln!("Failed to create destination directory {}", dest);
			return Ok(());
		}

		// CPP-PARITY: a failed `directory_iterator` on `source` aborts the
		// copy silently (C++ prints nothing); skipping matches that.
		let Ok(entries) = std::fs::read_dir(source) else {
			return Ok(());
		};
		for entry in entries.flatten() {
			let entry_path = entry.path();
			let dest_file_path = PathBuf::from(dest).join(entry.file_name());

			if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
				// Copy dir
				self.copy_directory(
					&entry_path.to_string_lossy(),
					&dest_file_path.to_string_lossy(),
					overwrite,
				)?;
			} else {
				// Copy file
				if overwrite {
					// C++ adds owner/group/others write permission then
					// removes the destination (so read-only files can be
					// replaced). CPP-PARITY: gated to `unix` because Rust's
					// `PermissionsExt` is Unix-only; on Windows the write
					// permissions already allow removal.
					#[cfg(unix)]
					{
						use std::os::unix::fs::PermissionsExt;
						if let Ok(meta) = std::fs::metadata(&dest_file_path) {
							let mut perms = meta.permissions();
							// owner_write|group_write|others_write
							perms.set_mode(perms.mode() | 0o200 | 0o020 | 0o002);
							let _ = std::fs::set_permissions(&dest_file_path, perms);
						}
					}
					let _ = std::fs::remove_file(&dest_file_path);
				}

				// CPP-PARITY: C++ `fs::copy_file` with `copy_options::none`
				// FAILS when the destination exists, but Rust's
				// `std::fs::copy` would silently overwrite it — so the
				// no-overwrite case must be guarded explicitly.
				if !overwrite && dest_file_path.exists() {
					eprintln!(
						"Failed to copy file {} to {}: File exists",
						entry_path.to_string_lossy(),
						dest_file_path.to_string_lossy()
					);
					continue;
				}

				if let Err(e) = std::fs::copy(&entry_path, &dest_file_path) {
					eprintln!(
						"Failed to copy file {} to {}: {}",
						entry_path.to_string_lossy(),
						dest_file_path.to_string_lossy(),
						e
					);
				}
			}
		}
		Ok(())
	}

	/// Whether `dir` is an existing directory, optionally creating it when
	/// it is missing.
	///
	/// # Arguments
	///
	/// * `dir` - path to check.
	/// * `try_to_create` - when `true` and `dir` is absent, attempt
	///   `create_dir_all` and report whether that succeeded; when `false`
	///   a missing directory simply yields `false`.
	pub fn directory_is_valid(&self, dir: &str, try_to_create: bool) -> bool {
		// Return whether the directory exists, or whether it could be created
		// if it doesn't
		if Path::new(dir).is_dir() {
			return true;
		}
		try_to_create && std::fs::create_dir_all(dir).is_ok()
	}

	/// Returns `filename` with `.<extension>` appended unless it already
	/// ends with that suffix (compared case-insensitively).
	///
	/// The extension is given without the leading dot. Empty `filename` or
	/// `extension` makes the function a no-op, and the path is otherwise
	/// returned unchanged.
	///
	/// # Arguments
	///
	/// * `filename` - path to normalise; directories are not inspected.
	/// * `extension` - extension without the dot, e.g. `"ofx"` or `"xml"`.
	pub fn ensure_filename_extension(&self, filename: &str, extension: &str) -> Result<String> {
		let mut fn_str = filename.to_string();
		// No-op if either input is empty
		if !fn_str.is_empty() && !extension.is_empty() {
			let extension_with_dot = format!(".{}", extension);
			if !ends_with_case_insensitive(&fn_str, &extension_with_dot) {
				fn_str.push_str(&extension_with_dot);
			}
		}
		Ok(fn_str)
	}

	/// The entire file read into a string (empty if it cannot be read).
	pub fn read_file_as_string(&self, filename: &str) -> Result<String> {
		// CPP-PARITY: the C++ returns the raw byte buffer as a `std::string`;
		// Rust `String` must be valid UTF-8, so invalid bytes are replaced
		// (lossy) rather than preserved. Project/XML files are UTF-8, so this
		// matches in practice.
		match std::fs::read(filename) {
			Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
			Err(_) => Ok(String::new()),
		}
	}

	/// A sibling path of `original` that is free at the moment of the
	/// call, for writing a temporary replacement next to the real file.
	///
	/// The name is built as `<basename>.tmp<counter><complete suffix>`
	/// in the same directory, incrementing `counter` from `0` until the
	/// path does not exist. The suffix starts at the first dot (so
	/// `clip.v2.mkv` yields `clip.tmp0.v2.mkv`). The file is not
	/// created, so another process can still claim the name before the
	/// caller does.
	///
	/// # Arguments
	///
	/// * `original` - path of the file being replaced; its parent
	///   directory is reused.
	pub fn get_safe_temporary_filename(&self, original: &str) -> Result<String> {
		let mut counter: i32 = 0;

		let original_path = PathBuf::from(original);
		let dir = original_path
			.parent()
			.map(Path::to_path_buf)
			.unwrap_or_default();
		let filename = original_path
			.file_name()
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_default();

		// Split off the complete suffix (everything from the first dot), like
		// QFileInfo::completeSuffix()
		let mut basename = filename.clone();
		let mut complete_suffix = String::new();
		if let Some(first_dot) = filename.find('.') {
			basename = filename[..first_dot].to_string();
			complete_suffix = filename[first_dot..].to_string();
		}

		let mut temp_abs_path;
		loop {
			temp_abs_path = dir.join(format!("{}.tmp{}{}", basename, counter, complete_suffix));
			counter += 1;
			if !temp_abs_path.exists() {
				break;
			}
		}

		Ok(temp_abs_path.to_string_lossy().into_owned())
	}

	/// Renames `from` to `to`, deleting `to` first when it exists.
	///
	/// Returns `false` (after logging to stderr) when the existing `to`
	/// cannot be removed or the rename fails, e.g. a missing source or a
	/// cross-device target. Only files are removed: an existing `to` that
	/// is a directory makes the call fail rather than recurse.
	///
	/// # Arguments
	///
	/// * `from` - existing source path.
	/// * `to` - destination path, replaced if present.
	pub fn rename_file_allow_overwrite(&self, from: &str, to: &str) -> bool {
		if Path::new(to).exists() {
			// CPP-PARITY: C++ `fs::remove` handles both files and dirs; Rust
			// has no single "remove" that does both, and the use case here is
			// file renames, so `remove_file` is used.
			if std::fs::remove_file(to).is_err() {
				eprintln!("Couldn't remove existing file {} for overwrite", to);
				return false;
			}
		}

		// By this point, we can assume `to` either never existed or has now
		// been deleted
		if std::fs::rename(from, to).is_err() {
			eprintln!("Failed to rename file {} to {}", from, to);
			return false;
		}

		true
	}

	/// Returns the name of a helper executable for this platform.
	///
	/// On Windows `".exe"` is appended to `unformatted`; on every other
	/// platform the input is returned unchanged. No existence check is
	/// performed.
	///
	/// # Arguments
	///
	/// * `unformatted` - executable name or path without a platform
	///   suffix.
	pub fn get_formatted_executable_for_platform(&self, unformatted: &str) -> Result<String> {
		#[cfg(target_os = "windows")]
		{
			Ok(format!("{}.exe", unformatted))
		}
		#[cfg(not(target_os = "windows"))]
		{
			Ok(unformatted.to_string())
		}
	}
}

/// Location query helper reserved for the two-stage C getters; returns a
/// path suitable for `std::fs`.
///
/// Only the configuration-location tests in this module call it today, so
/// it is kept (with an `allow`) as the reserved helper rather than deleted.
#[allow(dead_code)]
pub(crate) fn config_location_path() -> Result<PathBuf> {
	FileFunctions::new()
		.get_configuration_location()
		.map(PathBuf::from)
}

/// The default disk cache directory (C++ `DiskManager::
/// get_default_disk_cache_path`): `<configuration location>/mediacache`.
///
/// The `DiskCachePath` config key overrides the location when set (the
/// preferences dialog's cache-directory setting); an empty/absent value
/// keeps the default.
///
/// Single-lib unification: this used to live in the quercusrender crate's
/// `bridge::common` fallback (see `docs/zh/plans/riir/single-lib.md`);
/// quercusnode and quercusrender both call it directly now.
pub fn default_disk_cache_path() -> String {
	// A configured override wins (whitespace-only counts as absent).
	if let Ok(custom) = crate::configstore::ConfigStore::instance().get(None, "DiskCachePath") {
		if !custom.trim().is_empty() {
			return custom;
		}
	}
	Path::new(
		&FileFunctions::new()
			.get_configuration_location()
			.unwrap_or_default(),
	)
	.join("mediacache")
	.to_string_lossy()
	.into_owned()
}

#[cfg(test)]
mod tests;
