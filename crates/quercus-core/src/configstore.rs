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

//! Process-wide configuration store, mirroring
//! `src/common/src/configstore.h` and `include/common/config.h`.
//!
//! Per the config-wave ruling this is a process-wide singleton, NOT a
//! refcounted handle (same precedent as `QuercusCurrent`). Keys are typed
//! (string / int64 / double / bool); typed getters return a caller-supplied
//! fallback when the key is absent or of a different type. Persistence is
//! a TOML file at `<config_location>/config.toml`; a legacy
//! `<config_location>/config.ini` (written by old C++/Rust builds) is
//! read once for migration and left in place.

use crate::filefunctions;
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::ffi::{c_char, c_void, CString};
use std::fs;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{Error, Result};

/// Error-handler callback for user-visible config errors
/// (`quercus_coreConfigErrorHandler`). Called with title, message, and the
/// registered userdata.
pub type ErrorHandler = Option<unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void)>;

/// Host-provided config persistence source: safe Rust half of the C ABI
/// adapter. The engine's `ext::config_source` implements it for a foreign
/// vtable; `quercus-core` itself only sees this trait.
pub trait ConfigSource: Send + Sync {
	/// Returns the persisted TOML text, or `None` when the host cannot
	/// provide it.
	fn load(&self) -> Option<String>;
	/// Hands the serialized TOML text to the host; false on rejection/error.
	fn save(&self, toml: &str) -> bool;
}

/// Entry types (`quercus_coreConfigEntryType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
	/// No entry / null type.
	None,
	/// String.
	String,
	/// Integer.
	Int,
	/// Double.
	Double,
	/// Boolean.
	Bool,
}

/// A single typed configuration value (`ConfigStore::Entry` in the C++).
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
	/// String.
	String(String),
	/// Integer.
	Int(i64),
	/// Double.
	Double(f64),
	/// Boolean.
	Bool(bool),
}

/// Process-wide singleton store. `AtomicPtr`-based error handler, a
/// `Mutex`-guarded key/value map, and an optional host persistence source.
pub struct ConfigStore {
	/// Registered error handler (may be null).
	error_handler: AtomicPtr<c_void>,
	/// Userdata passed to the error handler.
	error_userdata: AtomicPtr<c_void>,
	/// `group/key` -> typed value.
	entries: Mutex<HashMap<String, ConfigValue>>,
	/// Optional host persistence source; when set it replaces the
	/// `config.toml`/`config.ini` file path in `load`/`save`.
	source: Mutex<Option<Arc<dyn ConfigSource>>>,
}

impl ConfigStore {
	/// The process-wide singleton.
	///
	/// CPP-PARITY: the C++ constructor runs `set_defaults()` on construction
	/// (`configstore.cpp:41-44`), so the singleton starts pre-loaded with the
	/// compiled-in defaults, exactly as `ConfigStore::current()` would.
	pub fn instance() -> &'static ConfigStore {
		static INSTANCE: OnceLock<ConfigStore> = OnceLock::new();
		INSTANCE.get_or_init(|| {
			let store = ConfigStore {
				error_handler: AtomicPtr::new(ptr::null_mut()),
				error_userdata: AtomicPtr::new(ptr::null_mut()),
				entries: Mutex::new(HashMap::new()),
				source: Mutex::new(None),
			};
			store.set_defaults();
			store
		})
	}

	/// Reset to compiled-in defaults and then load settings. When a host
	/// config source is installed, its TOML text is read through
	/// [`ConfigSource::load`] and the file paths/legacy INI are not touched.
	/// Otherwise this loads `config.toml`, falling back to the legacy
	/// `config.ini` for migration when no TOML file exists yet (a missing
	/// config is not an error). Mirrors `ConfigStore::load()`
	/// (`configstore.cpp:239`).
	pub fn load(&self) -> Result<()> {
		self.set_defaults();

		if let Some(source) = self.source_snapshot() {
			let content = self.load_source_text(source.as_ref())?;
			return self.apply_toml_text(&content);
		}

		let toml_path = get_config_file_path();

		let metadata = match fs::metadata(&toml_path) {
			Ok(m) => m,
			Err(_) => {
				// No `config.toml` yet: either this is a fresh install
				// (defaults are fine, not an error) or an old `config.ini`
				// remains to be migrated.
				return self.load_legacy_ini();
			}
		};

		// CPP-PARITY: the C++ treats a directory as unreadable (exists() is
		// checked first, then is_regular_file()), reporting the error and
		// returning false (`configstore.cpp:245-258`).
		if !metadata.is_file() {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			return Err(Error::Failed("config.toml is not a regular file".into()));
		}

		let content = fs::read_to_string(&toml_path).map_err(|_| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed("config.toml could not be read".into())
		})?;

		self.apply_toml_text(&content)
	}

	/// Parse TOML text and apply it to the store. A corrupt document is
	/// reported and surfaced, not silently ignored: ignoring it would wipe
	/// the user's settings on the next save. Shared by the file path and a
	/// host [`ConfigSource`].
	fn apply_toml_text(&self, content: &str) -> Result<()> {
		let parsed: toml::Value = toml::from_str(content).map_err(|e| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed(format!("config.toml could not be parsed: {}", e))
		})?;

		self.apply_toml(&parsed);
		Ok(())
	}

	/// Read the TOML text of an installed host source through the safe
	/// [`ConfigSource`] trait. The C adapter performs the snprintf-style
	/// size query, buffer retry, UTF-8 validation and drop callback; core
	/// only reports a source that cannot provide settings.
	fn load_source_text(&self, source: &dyn ConfigSource) -> Result<String> {
		match source.load() {
			Some(content) => Ok(content),
			None => {
				self.report_error(
					"Error loading settings",
					"Failed to load application settings. This session will use defaults.",
				);
				Err(Error::Failed(
					"config source could not provide settings".into(),
				))
			}
		}
	}

	/// Legacy migration path: read an old `config.ini` (written by the C++
	/// or pre-TOML Rust builds) with the INI parser, then immediately persist
	/// the loaded values as `config.toml` so the next startup reads TOML. The
	/// INI file is deliberately left in place. A missing INI is not an error.
	fn load_legacy_ini(&self) -> Result<()> {
		let path = get_config_ini_path();

		let metadata = match fs::metadata(&path) {
			Ok(m) => m,
			Err(_) => {
				// No saved settings at all: defaults are fine, not an error.
				return Ok(());
			}
		};

		// CPP-PARITY: a directory at the config path is an error (see load()).
		if !metadata.is_file() {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			return Err(Error::Failed("config.ini is not a regular file".into()));
		}

		// CPP-PARITY: invalid UTF-8 maps to the same "unreadable file" failure.
		let content = fs::read_to_string(&path).map_err(|_| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed("config.ini could not be read".into())
		})?;

		self.parse_ini(&content);

		// Write the migrated values as TOML now. A failed write is reported by
		// save() but does not fail load(): the values are already in memory,
		// and the migration is retried on the next startup.
		let _ = self.save();

		Ok(())
	}

	/// Parse legacy INI text into the store (QSettings-style lax parsing).
	/// Kept solely for the migration path in `load_legacy_ini`.
	fn parse_ini(&self, content: &str) {
		let mut group = String::new();
		for raw_line in content.lines() {
			let line = trim(raw_line);
			if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
				continue;
			}

			if line.starts_with('[') && line.ends_with(']') {
				group = trim(&line[1..line.len() - 1]);
				continue;
			}

			let eq = match line.find('=') {
				Some(e) => e,
				None => {
					// Malformed line: skip, keep going (matches QSettings'
					// lax INI parsing).
					continue;
				}
			};

			let key = trim(&line[..eq]);
			let value = trim(&line[eq + 1..]);
			if key.is_empty() {
				continue;
			}
			let full_key = if group.is_empty() {
				key.to_string()
			} else {
				format!("{}/{}", group, key)
			};

			match self.get_entry(&full_key) {
				Some(existing) => {
					// Known key: honor its declared type. An unparseable value
					// keeps the default.
					let ty = to_entry_type(&existing);
					if let Some(parsed) = string_to_value(&value, ty) {
						self.set_entry(full_key, parsed);
					}
				}
				None => {
					// Unknown key: stored as a string.
					self.set_entry(full_key, ConfigValue::String(value));
				}
			}
		}
	}

	/// Apply parsed TOML to the store. Top-level keys become flat entries;
	/// `[group]` tables (including nested dotted tables) flatten back to
	/// `group/sub` keys. Known keys keep their declared type; unknown keys are
	/// stored as strings — the same semantics as the INI loader.
	fn apply_toml(&self, parsed: &toml::Value) {
		if let Some(table) = parsed.as_table() {
			self.flatten_toml("", table);
		}
	}

	/// Flatten a TOML table into `prefix`-joined store keys.
	///
	/// `prefix` is the table path accumulated so far (empty at the root) and
	/// `table` the TOML table to walk: nested tables recurse with an extended
	/// prefix, while leaf values go to [`Self::apply_toml_entry`]. Because the
	/// separator is `/`, `[group] key = 1` becomes the flat key `group/key`.
	fn flatten_toml(&self, prefix: &str, table: &toml::map::Map<String, toml::Value>) {
		for (key, value) in table {
			let full_key = if prefix.is_empty() {
				key.clone()
			} else {
				format!("{}/{}", prefix, key)
			};
			match value.as_table() {
				Some(sub) => self.flatten_toml(&full_key, sub),
				None => self.apply_toml_entry(&full_key, value),
			}
		}
	}

	/// Apply a single (already flattened) key/value to the store.
	///
	/// `full_key` is the `group/key`-joined name and `value` the parsed TOML
	/// value. A known key keeps its declared type (via [`to_entry_type`]) and an
	/// unconvertible value leaves the existing entry — normally the compiled-in
	/// default — untouched; an unknown key is stored as a string, matching the
	/// legacy INI loader.
	fn apply_toml_entry(&self, full_key: &str, value: &toml::Value) {
		match self.get_entry(full_key) {
			Some(existing) => {
				// Known key: honor its declared type. An unconvertible value
				// keeps the default.
				let ty = to_entry_type(&existing);
				if let Some(converted) = toml_value_to_typed(value, ty) {
					self.set_entry(full_key.to_string(), converted);
				}
			}
			None => {
				// Unknown key: stored as a string.
				self.set_entry(
					full_key.to_string(),
					ConfigValue::String(toml_value_to_string(value)),
				);
			}
		}
	}

	/// Persist the current store. With a host config source installed the
	/// TOML text goes to its [`ConfigSource::save`] and no file is touched;
	/// otherwise it is written to `config.toml` via temp file + rename.
	/// Mirrors `ConfigStore::save()` (`configstore.cpp:316`).
	pub fn save(&self) -> Result<()> {
		let out = self.to_toml_string()?;

		if let Some(source) = self.source_snapshot() {
			// The slot lock is not held here: only the cloned `Arc` keeps
			// the source alive during the call.
			if !source.save(&out) {
				self.report_error(
					"Error saving settings",
					"Failed to save application settings: the host config source \
					 rejected the data.",
				);
				return Err(Error::Failed("config source rejected the save".into()));
			}
			return Ok(());
		}

		let real_filename = get_config_file_path();
		let temp_filename = format!("{}.tmp", real_filename);

		if fs::write(&temp_filename, out.as_bytes()).is_err() {
			self.report_error(
				"Error saving settings",
				"Failed to save application settings. The application may lack write \
				 permissions for this location.",
			);
			return Err(Error::Failed(
				"temp config file could not be written".into(),
			));
		}

		// CPP-PARITY: rename temp -> real; on POSIX this overwrites
		// atomically, so the remove+retry fallback only matters on Windows,
		// mirrored for behavioral parity (`configstore.cpp:378-390`).
		if fs::rename(&temp_filename, &real_filename).is_err() {
			let _ = fs::remove_file(&real_filename);
			if fs::rename(&temp_filename, &real_filename).is_err() {
				self.report_error(
					"Error saving settings",
					"Failed to overwrite the application settings file.",
				);
				return Err(Error::Failed(
					"config.toml could not be renamed into place".into(),
				));
			}
		}

		Ok(())
	}

	/// Serialize the store to TOML text. Flat keys are written as top-level
	/// TOML keys; keys containing '/' become `[group]` tables (group =
	/// everything before the last '/'), keeping the QSettings INI key
	/// shape. Values are written as native TOML int/float/bool/string;
	/// non-finite doubles have no TOML literal and are written as strings,
	/// which load() restores by declared type.
	fn to_toml_string(&self) -> Result<String> {
		let mut sections: BTreeMap<String, BTreeMap<String, toml::Value>> = BTreeMap::new();
		{
			let guard = self.entries.lock().unwrap();
			for (key, value) in guard.iter() {
				let toml_value = config_value_to_toml(value);
				match key.rfind('/') {
					Some(slash) => {
						let group = key[..slash].to_string();
						let sub = key[slash + 1..].to_string();
						sections.entry(group).or_default().insert(sub, toml_value);
					}
					None => {
						sections
							.entry(String::new())
							.or_default()
							.insert(key.clone(), toml_value);
					}
				}
			}
		}

		let mut root: toml::map::Map<String, toml::Value> = toml::map::Map::new();
		if let Some(flat) = sections.remove("") {
			for (sub, value) in flat {
				root.insert(sub, value);
			}
		}
		for (group, subs) in sections {
			root.insert(group, toml::Value::Table(subs.into_iter().collect()));
		}

		toml::to_string_pretty(&toml::Value::Table(root)).map_err(|e| {
			self.report_error(
				"Error saving settings",
				"Failed to serialize application settings as TOML.",
			);
			Error::Failed(format!("config.toml could not be serialized: {}", e))
		})
	}

	/// Reset to compiled-in defaults (drop custom keys).
	pub fn reset_defaults(&self) -> Result<()> {
		self.set_defaults();
		Ok(())
	}

	/// Set a string entry. Mirrors `quercus_core_config_set` (`config.cpp:102`):
	/// a new key is created as a string; setting an existing typed entry
	/// parses the string into its declared type, and an unparseable value
	/// leaves the entry unchanged.
	pub fn set(&self, group: Option<&str>, key: &str, value: &str) {
		if key.is_empty() {
			return;
		}
		let joined = join_key(group, key);
		match self.get_entry(&joined) {
			Some(ConfigValue::String(_)) | None => {
				self.set_entry(joined, ConfigValue::String(value.to_string()));
			}
			Some(other) => {
				let ty = to_entry_type(&other);
				if let Some(parsed) = string_to_value(value, ty) {
					self.set_entry(joined, parsed);
				}
			}
		}
	}

	/// Read an entry as a string (two-stage getter semantics: formatted
	/// for numeric/bool entries). Mirrors `quercus_core_config_get`
	/// (`config.cpp:134`).
	///
	/// `group`/`key` identify the entry; the group is optional and is joined into
	/// the lookup key as `group/key`.
	///
	/// # Errors
	///
	/// Returns [`Error::Invalid`] for an empty `key` and [`Error::NotFound`] when
	/// no entry exists. Numeric, bool and double values are formatted rather than
	/// rejected (`%g` for doubles, `true`/`false` for bools), so the result is
	/// always a string.
	pub fn get(&self, group: Option<&str>, key: &str) -> Result<String> {
		if key.is_empty() {
			return Err(Error::Invalid);
		}
		match self.get_entry(&join_key(group, key)) {
			None => Err(Error::NotFound),
			Some(v) => Ok(value_to_string(&v)),
		}
	}

	/// Read an INT entry; `fallback` when absent or a different type.
	pub fn get_int(&self, group: Option<&str>, key: &str, fallback: i32) -> i32 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Int(v)) => v as i32,
			_ => fallback,
		}
	}

	/// Read a DOUBLE entry; `fallback` when absent or a different type.
	pub fn get_double(&self, group: Option<&str>, key: &str, fallback: f64) -> f64 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Double(v)) => v,
			_ => fallback,
		}
	}

	/// Set an INT entry.
	///
	/// `group`/`key` name the entry (an empty `key` is a no-op) and `v` is the
	/// 32-bit value, widened and stored through [`Self::set_int64`].
	/// [`Self::get_int`] narrows it back with `as i32`, so values outside the
	/// `i32` range only arise via [`Self::set_int64`].
	pub fn set_int(&self, group: Option<&str>, key: &str, v: i32) {
		self.set_int64(group, key, v as i64);
	}

	/// Read an INT entry as i64; `fallback` when absent or a different type.
	pub fn get_int64(&self, group: Option<&str>, key: &str, fallback: i64) -> i64 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Int(v)) => v,
			_ => fallback,
		}
	}

	/// Set an INT entry as i64.
	///
	/// `group`/`key` name the entry (an empty `key` is a no-op) and `v` is
	/// stored as a 64-bit `Int`, wide enough for frame counts and millisecond
	/// intervals. [`Self::get_int64`] returns it unchanged; [`Self::set_int`]
	/// forwards here for 32-bit values.
	pub fn set_int64(&self, group: Option<&str>, key: &str, v: i64) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Int(v));
	}

	/// Read a BOOL entry as int 0/1; `fallback` when absent or a different
	/// type.
	pub fn get_bool(&self, group: Option<&str>, key: &str, fallback: i32) -> i32 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Bool(b)) => {
				if b {
					1
				} else {
					0
				}
			}
			_ => fallback,
		}
	}

	/// Set a BOOL entry.
	///
	/// `group`/`key` name the entry (an empty `key` is a no-op) and `v` is an
	/// int because the C ABI passes booleans as ints: any non-zero value stores
	/// `true`, zero stores `false`. [`Self::get_bool`] reads it back as 0/1 and
	/// [`Self::get`] as `"true"`/`"false"`.
	pub fn set_bool(&self, group: Option<&str>, key: &str, v: i32) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Bool(v != 0));
	}

	/// Set a DOUBLE entry.
	///
	/// `group` is the optional group prefix (the stored key becomes
	/// `group/key`), `key` the entry name — an empty key is a no-op — and `v` the
	/// value, stored as-is (no clamping or validation; NaN and infinities are
	/// preserved). Read it back with [`Self::get_double`], or via [`Self::get`],
	/// which formats it with `%g`.
	pub fn set_double(&self, group: Option<&str>, key: &str, v: f64) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Double(v));
	}

	/// Entry type of a key, or `NotFound`. Mirrors `quercus_core_config_entry_type`
	/// (`config.cpp:270`).
	pub fn entry_type(&self, group: Option<&str>, key: &str) -> Result<EntryType> {
		if key.is_empty() {
			return Err(Error::Invalid);
		}
		match self.get_entry(&join_key(group, key)) {
			None => Err(Error::NotFound),
			Some(v) => Ok(to_entry_type(&v)),
		}
	}

	/// Install (or replace) the process-wide host persistence source.
	///
	/// The source then serves [`ConfigStore::load`] and
	/// [`ConfigStore::save`] instead of `config.toml` and the legacy INI
	/// migration. The previous source is swapped out under the lock and
	/// dropped outside it (so its `Drop` may itself install a source); a
	/// concurrent load/save may keep it alive until that call finishes.
	fn set_source(&self, source: Option<Arc<dyn ConfigSource>>) {
		// Swap under the lock, then drop the old entry outside it.
		let old = {
			let mut slot = self.source.lock().unwrap_or_else(|e| e.into_inner());
			mem::replace(&mut *slot, source)
		};
		mem::drop(old);
	}

	/// Clear the process-wide host persistence source, returning true when
	/// one was installed. Its `Drop` runs (outside the lock) exactly once,
	/// when the last reference to it is released.
	pub fn remove_source(&self) -> bool {
		let old = {
			let mut slot = self.source.lock().unwrap_or_else(|e| e.into_inner());
			slot.take()
		};
		let installed = old.is_some();
		mem::drop(old);
		installed
	}

	/// Whether a host persistence source is currently installed.
	pub fn has_source(&self) -> bool {
		self.source
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.is_some()
	}

	/// Clone the installed source so `load`/`save` can call it without
	/// holding the source slot lock.
	fn source_snapshot(&self) -> Option<Arc<dyn ConfigSource>> {
		self.source
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
	}

	/// Register (or clear, with a null handler) the error handler. Mirrors
	/// the domain half of `quercus_core_config_set_error_handler` (`config.cpp:288`).
	pub fn set_error_handler(&self, handler: ErrorHandler, userdata: *mut c_void) -> Result<()> {
		let ptr = handler.map_or(ptr::null_mut(), |h| h as *mut c_void);
		self.error_handler.store(ptr, Ordering::Release);
		self.error_userdata.store(userdata, Ordering::Release);
		Ok(())
	}

	/// Report a user-visible error through the registered handler, or to
	/// stderr when none is registered. Mirrors `ConfigStore::report_error`
	/// (`configstore.cpp:51`).
	///
	/// `title` is the short dialog title and `message` the full text; both are
	/// handed to the handler as NUL-terminated C strings (a string containing an
	/// interior NUL degrades to an empty string) together with the userdata given
	/// to [`Self::set_error_handler`]. Reporting never fails: with no handler the
	/// pair is printed to stderr as `title: message`.
	fn report_error(&self, title: &str, message: &str) {
		let handler = self.error_handler.load(Ordering::Acquire);
		if handler.is_null() {
			eprintln!("{}: {}", title, message);
			return;
		}
		let userdata = self.error_userdata.load(Ordering::Acquire);
		let t = CString::new(title).unwrap_or_default();
		let m = CString::new(message).unwrap_or_default();
		// The pointer stored by `set_error_handler` is a function pointer
		// cast to `*mut c_void`; recover it for the call.
		let h: unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void) =
			unsafe { mem::transmute(handler) };
		unsafe {
			h(t.as_ptr(), m.as_ptr(), userdata);
		}
	}

	/// Reset to compiled-in defaults (drop custom keys). Mirrors
	/// `ConfigStore::set_defaults()` (`configstore.cpp:76`).
	///
	/// Clears the whole entry map and re-inserts the compiled-in keys the de-Qt
	/// engine modules read: timeline thumbnail/waveform modes, the default
	/// sequence format (1920x1080, 1001/30000, stereo 48 kHz), the offline pixel
	/// format, split-clips/GL-finish flags, proxy and disk-cache settings, and
	/// the marker/category colors. App-layer keys of the old Qt config are not
	/// registered here. Called by the singleton constructor, [`Self::load`]
	/// (before applying the file) and [`Self::reset_defaults`]. Panics if the
	/// entries lock is poisoned.
	fn set_defaults(&self) {
		let mut guard = self.entries.lock().unwrap();
		guard.clear();

		// Only the keys the de-Qt engine modules (quercusnode/quercusrender/quercuscodec)
		// actually read are registered here; the app-layer keys of the old Qt
		// config arrive with the app/config wave. Enum-valued ints hardcode
		// the numeric values of their (still Qt-based) defining headers:
		//
		//  - Timeline::k_thumbnail_in_out / k_waveforms_enabled = 1
		//    (engine/timeline/timelinecommon.h)
		//  - PixelFormat::f32 = 4 (core/include/olive/core/render/pixelformat.h)
		//  - VideoParams::k_interlace_none = 0 (src/common/src/videoparams.h)
		//  - k_channel_layout_stereo = 3
		//    (core/include/olive/core/render/channellayout.h)
		//  - ColorCoding::k_red..k_navy = 0..11, k_lime = 6
		//    (engine/ui/colorcoding.h)

		guard.insert("TimelineThumbnailMode".into(), ConfigValue::Int(1));
		guard.insert("TimelineWaveformMode".into(), ConfigValue::Int(1));

		guard.insert("DefaultSequenceWidth".into(), ConfigValue::Int(1920));
		guard.insert("DefaultSequenceHeight".into(), ConfigValue::Int(1080));
		// Rational settings are stored as strings in quercuscore_rational
		// "num/den" form; this mirrors the old default Rational(1001, 30000).
		guard.insert(
			"DefaultSequenceFrameRate".into(),
			ConfigValue::String("1001/30000".into()),
		);
		guard.insert(
			"DefaultSequencePixelAspect".into(),
			ConfigValue::String("1/1".into()),
		);
		guard.insert("DefaultSequenceInterlacing".into(), ConfigValue::Int(0));
		guard.insert(
			"DefaultSequenceAudioFrequency".into(),
			ConfigValue::Int(48000),
		);
		guard.insert("DefaultSequenceAudioLayout".into(), ConfigValue::Int(3));
		guard.insert("OfflinePixelFormat".into(), ConfigValue::Int(4));

		guard.insert("SplitClipsCopyNodes".into(), ConfigValue::Bool(true));
		guard.insert("UseProxyMedia".into(), ConfigValue::Bool(true));
		guard.insert("UseGLFinish".into(), ConfigValue::Bool(false));
		guard.insert("ReassocLinToNonLin".into(), ConfigValue::Bool(false));

		guard.insert(
			"GraphicsBackend".into(),
			ConfigValue::String("opengl".into()),
		);
		guard.insert("LUTLibraryPaths".into(), ConfigValue::String(String::new()));

		guard.insert("DiskCacheSaveInterval".into(), ConfigValue::Int(10000));
		guard.insert("AutoCacheDelay".into(), ConfigValue::Int(1000));
		guard.insert("DiskCacheBehind".into(), ConfigValue::String("0/1".into()));
		guard.insert("DiskCacheAhead".into(), ConfigValue::String("60/1".into()));

		guard.insert("ProxyWidth".into(), ConfigValue::Int(1280));
		guard.insert("ProxyHeight".into(), ConfigValue::Int(720));
		guard.insert("ProxyDivider".into(), ConfigValue::Int(1));
		guard.insert("ProxyCRF".into(), ConfigValue::Int(23));
		guard.insert("ProxyPreset".into(), ConfigValue::String("veryfast".into()));
		guard.insert("ProxyIncludeAudio".into(), ConfigValue::Bool(true));

		guard.insert("MarkerColor".into(), ConfigValue::Int(6));
		for i in 0..=11 {
			guard.insert(format!("CatColor{}", i), ConfigValue::Int(i));
		}
	}

	/// Copy the entry for a joined key out of the map, or `None` when absent.
	fn get_entry(&self, key: &str) -> Option<ConfigValue> {
		let guard = self.entries.lock().unwrap();
		guard.get(key).cloned()
	}

	/// Insert (or replace) an entry.
	///
	/// `key` is the already-joined store key (`group/key` form, no re-joining
	/// here) and `value` the typed entry; an existing key is silently
	/// overwritten. Takes the entries lock, so it is safe from any thread, but
	/// it does not persist anything by itself — callers use
	/// [`ConfigStore::save`] for that. Panics if another holder of the lock
	/// panicked while holding it.
	fn set_entry(&self, key: String, value: ConfigValue) {
		let mut guard = self.entries.lock().unwrap();
		guard.insert(key, value);
	}
}

/// Install, replace, or clear (`source` = `None`) the process-wide host
/// config persistence source.
///
/// Once installed, [`ConfigStore::load`] reads TOML through
/// [`ConfigSource::load`] instead of `config.toml`/`config.ini`, and
/// [`ConfigStore::save`] hands the serialized TOML to
/// [`ConfigSource::save`] instead of writing a file. No source installed
/// keeps the file behavior unchanged. The previous source's `Arc` is
/// dropped outside the internal lock, so its `Drop` (for a C adapter, the
/// host drop callback) runs exactly once when it is replaced or cleared.
pub fn set_config_source(source: Option<Arc<dyn ConfigSource>>) {
	ConfigStore::instance().set_source(source);
}

/// Whether a process-wide host config persistence source is installed.
pub fn config_source_installed() -> bool {
	ConfigStore::instance().has_source()
}

/// Joins group and key into the stored "group/key" form. Mirrors
/// `ConfigStore::join_key` (`configstore.cpp:68`).
pub(crate) fn join_key(group: Option<&str>, key: &str) -> String {
	match group {
		Some(g) if !g.is_empty() => format!("{}/{}", g, key),
		_ => key.to_string(),
	}
}

/// Serializes a value for the string getter. Mirrors
/// `ConfigStore::value_to_string` (`configstore.cpp:154`).
fn value_to_string(value: &ConfigValue) -> String {
	match value {
		ConfigValue::String(s) => s.clone(),
		ConfigValue::Int(i) => i.to_string(),
		ConfigValue::Double(d) => format_g(*d),
		ConfigValue::Bool(b) => {
			if *b {
				"true".to_string()
			} else {
				"false".to_string()
			}
		}
	}
}

/// Converts a stored value to its TOML representation. Non-finite doubles
/// have no TOML literal, so they are written as strings ("nan"/"inf"/"-inf")
/// and restored by `toml_value_to_typed` via the declared-type string
/// conversion.
fn config_value_to_toml(value: &ConfigValue) -> toml::Value {
	match value {
		ConfigValue::String(s) => toml::Value::String(s.clone()),
		ConfigValue::Int(i) => toml::Value::Integer(*i),
		ConfigValue::Double(d) if d.is_finite() => toml::Value::Float(*d),
		ConfigValue::Double(_) => toml::Value::String(value_to_string(value)),
		ConfigValue::Bool(b) => toml::Value::Boolean(*b),
	}
}

/// Converts a TOML value to the declared entry type, mirroring the INI
/// loader's known-key conversion: a value whose TOML type does not match the
/// declared type is parsed from its textual form (so `"640"` still loads an
/// Int key), and an unparseable value keeps the default.
fn toml_value_to_typed(value: &toml::Value, ty: EntryType) -> Option<ConfigValue> {
	match ty {
		EntryType::Int => match value {
			toml::Value::Integer(i) => Some(ConfigValue::Int(*i)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Int),
		},
		EntryType::Double => match value {
			toml::Value::Float(f) => Some(ConfigValue::Double(*f)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Double),
		},
		EntryType::Bool => match value {
			toml::Value::Boolean(b) => Some(ConfigValue::Bool(*b)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Bool),
		},
		EntryType::String => Some(ConfigValue::String(toml_value_to_text(value))),
		EntryType::None => None,
	}
}

/// The textual form of a TOML value, used for declared-type coercion and
/// known String keys: the raw string for strings, the TOML literal otherwise.
fn toml_value_to_text(value: &toml::Value) -> String {
	match value {
		toml::Value::String(s) => s.clone(),
		_ => value.to_string(),
	}
}

/// The INI-parity string form for an unknown key read from TOML: matches what
/// the legacy INI writer would have persisted for the same value, so unknown
/// keys behave identically across formats.
fn toml_value_to_string(value: &toml::Value) -> String {
	match value {
		toml::Value::String(s) => s.clone(),
		toml::Value::Integer(i) => i.to_string(),
		toml::Value::Boolean(b) => b.to_string(),
		toml::Value::Float(f) => format_g(*f),
		_ => value.to_string(),
	}
}

/// Parses text into a value of the given type; `None` when it cannot be
/// parsed as the requested type (strings always parse). Mirrors
/// `ConfigStore::string_to_value` (`configstore.cpp:173`).
fn string_to_value(text: &str, ty: EntryType) -> Option<ConfigValue> {
	match ty {
		EntryType::String => Some(ConfigValue::String(text.to_string())),
		EntryType::Int => {
			// CPP-PARITY: std::stoll skips leading whitespace but requires the
			// whole remaining string to be consumed; `trim_start().parse()`
			// reproduces that (trailing whitespace/junk fails the parse).
			text.trim_start().parse::<i64>().ok().map(ConfigValue::Int)
		}
		EntryType::Double => {
			// CPP-PARITY: std::stod additionally accepts inf/infinity/nan
			// (case-insensitive); handled explicitly since Rust's f64 parse
			// rejects them.
			let t = text.trim_start();
			let parsed = if t.eq_ignore_ascii_case("inf")
				|| t.eq_ignore_ascii_case("+inf")
				|| t.eq_ignore_ascii_case("infinity")
				|| t.eq_ignore_ascii_case("+infinity")
			{
				Some(f64::INFINITY)
			} else if t.eq_ignore_ascii_case("-inf") || t.eq_ignore_ascii_case("-infinity") {
				Some(f64::NEG_INFINITY)
			} else if t.eq_ignore_ascii_case("nan")
				|| t.eq_ignore_ascii_case("+nan")
				|| t.eq_ignore_ascii_case("-nan")
			{
				Some(f64::NAN)
			} else {
				t.parse::<f64>().ok()
			};
			parsed.map(ConfigValue::Double)
		}
		EntryType::Bool => {
			if text == "true" || text == "1" {
				Some(ConfigValue::Bool(true))
			} else if text == "false" || text == "0" {
				Some(ConfigValue::Bool(false))
			} else {
				None
			}
		}
		EntryType::None => None,
	}
}

/// Maps a value back to its entry type.
///
/// `value` is an existing store entry; its variant decides the `EntryType`
/// (`String` / `Int` / `Double` / `Bool`). The loaders use this to coerce
/// incoming text without changing a known key's declared type. Total — it never
/// returns `EntryType::None` (that only means "no type" for a missing key).
fn to_entry_type(value: &ConfigValue) -> EntryType {
	match value {
		ConfigValue::String(_) => EntryType::String,
		ConfigValue::Int(_) => EntryType::Int,
		ConfigValue::Double(_) => EntryType::Double,
		ConfigValue::Bool(_) => EntryType::Bool,
	}
}

/// Trims `" \t\r\n"` from both ends. Mirrors the anonymous `trim()` helper
/// in `configstore.cpp:227`.
fn trim(s: &str) -> String {
	s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
		.to_string()
}

/// The configuration directory. Delegates to
/// `FileFunctions::get_configuration_location()` exactly like the C++ does
/// (`configstore.cpp:61-66` composes the path as
/// `FileFunctions::get_configuration_location() / "config.ini"`), so the
/// `QUERCUS_CONFIG_DIR` override, portable mode, the macOS Application Support
/// default, and the `<root>/quercus` suffix all match.
///
/// Falls back to the system temp directory when `FileFunctions` reports an
/// error, so the returned path is always usable (the store then simply lives
/// somewhere writable rather than under the user's home).
fn configuration_location() -> String {
	filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_else(|_| env::temp_dir().to_string_lossy().into_owned())
}

/// `<config_location>/config.toml`.
fn get_config_file_path() -> String {
	format!("{}/config.toml", configuration_location())
}

/// `<config_location>/config.ini` — the legacy format, read once for
/// migration when no `config.toml` exists yet.
fn get_config_ini_path() -> String {
	format!("{}/config.ini", configuration_location())
}

/// Hand-rolled C `%g` formatting with the default precision 6, used by
/// `value_to_string` for doubles. Faithful to `snprintf(buf, ..., "%g", v)`
/// in `configstore.cpp:163`.
///
/// CPP-PARITY: C `%g` rounds to 6 significant digits, drops trailing zeros
/// and a trailing decimal point, and switches to scientific notation when the
/// decimal exponent is < -4 or >= 6. Rust's `{:.*e}` / `{:.*}` both use
/// round-half-to-even like glibc, so results match for ordinary values. The
/// fixed-vs-scientific choice here is driven by the exact decimal exponent
/// (computed from the value), which also handles carry-over rounding (e.g.
/// 999999.5 -> "1000000") the way C does.
fn format_g(v: f64) -> String {
	if v.is_nan() {
		return "nan".to_string();
	}
	if v.is_infinite() {
		return if v.is_sign_negative() { "-inf" } else { "inf" }.to_string();
	}
	if v == 0.0 {
		return if v.is_sign_negative() { "-0" } else { "0" }.to_string();
	}
	let sign = if v.is_sign_negative() { "-" } else { "" };
	let a = v.abs();

	const PRECISION: i32 = 6;
	// Round to PRECISION significant digits via scientific formatting; `%g`
	// decides between fixed and scientific style using the exponent of the
	// ROUNDED value (so e.g. 999999.5 -> "1e+06", not "1000000").
	let sci = format!("{:.*e}", (PRECISION - 1) as usize, a); // "d.ddddd e±N"
	let exp = sci.split('e').nth(1).unwrap().parse::<i32>().unwrap();
	let xr = exp;
	let body = if !(-4..PRECISION).contains(&xr) {
		// Scientific notation: mantissa is already rounded; strip trailing zeros.
		let mant = sci.split('e').next().unwrap();
		let m = trim_mantissa(mant);
		format!("{}e{}", m, format_exp(&xr.to_string()))
	} else {
		// Fixed notation: (PRECISION - 1 - xr) decimals, trailing zeros removed.
		let decimals = (PRECISION - 1 - xr).max(0) as usize;
		trim_mantissa(&format!("{:.*}", decimals, a))
	};
	format!("{}{}", sign, body)
}

/// Removes trailing zeros after the decimal point and a trailing decimal
/// point, but never truncates integer digits (`%g` behavior).
fn trim_mantissa(m: &str) -> String {
	match m.split_once('.') {
		None => m.to_string(),
		Some((int, frac)) => {
			let frac = frac.trim_end_matches('0');
			if frac.is_empty() {
				int.to_string()
			} else {
				format!("{}.{}", int, frac)
			}
		}
	}
}

/// Formats a decimal exponent as C does: a sign always followed by at least
/// two digits (e.g. "e+06", "e-05", "e+100").
fn format_exp(e: &str) -> String {
	let (neg, mag) = match e.strip_prefix('-') {
		Some(rest) => (true, rest),
		None => (false, e.strip_prefix('+').unwrap_or(e)),
	};
	let sign = if neg { "-" } else { "+" };
	if mag.len() < 2 {
		format!("{}{:0>2}", sign, mag)
	} else {
		format!("{}{}", sign, mag)
	}
}

#[cfg(test)]
mod tests;
