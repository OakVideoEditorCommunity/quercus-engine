use super::*;
use std::ffi::c_char;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex as StdMutex;

/// Serializes every test that touches the process-global singleton and
/// the `QUERCUS_CONFIG_DIR` env override, so parallel tests cannot race.
///
/// Uses the crate-wide test lock so `filefunctions` tests (which also
/// mutate `QUERCUS_CONFIG_DIR`) serialize on the SAME mutex.
fn test_lock() -> &'static StdMutex<()> {
	crate::test_support::env_lock()
}

/// Point `QUERCUS_CONFIG_DIR` at an isolated temp dir, run `f`, then clean up.
fn with_temp_config<T>(f: impl FnOnce(&Path) -> T) -> T {
	let _guard = test_lock().lock().unwrap();
	let dir = std::env::temp_dir().join(format!(
		"quercus_core_configstore_test_{}",
		std::process::id()
	));
	let _ = std::fs::create_dir_all(&dir);
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let result = f(&dir);
	std::env::remove_var("QUERCUS_CONFIG_DIR");
	let _ = std::fs::remove_dir_all(&dir);
	result
}

// ---- Host config source ------------------------------------------------

/// In-memory safe host source used by the config-source tests.
///
/// `None` content makes `load` report failure, exactly like a host
/// that cannot provide settings.
struct MemSource(Arc<StdMutex<Option<String>>>);

/// Number of [`MemSource`] drops; tests reset it before asserting.
static MEM_SOURCE_DROPS: AtomicUsize = AtomicUsize::new(0);

impl ConfigSource for MemSource {
	fn load(&self) -> Option<String> {
		self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
	}

	fn save(&self, toml: &str) -> bool {
		*self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(toml.to_string());
		true
	}
}

impl Drop for MemSource {
	fn drop(&mut self) {
		MEM_SOURCE_DROPS.fetch_add(1, Ordering::SeqCst);
	}
}

/// Install an in-memory source (with optional initial TOML text) and
/// return the buffer holding the "persisted" text.
fn install_memory_source(content: Option<&str>) -> Arc<StdMutex<Option<String>>> {
	let content = Arc::new(StdMutex::new(content.map(str::to_string)));
	set_config_source(Some(Arc::new(MemSource(content.clone()))));
	content
}

/// Clears any installed source when a test ends (including on panic),
/// so one failing test cannot leak a source into the next.
struct ClearSourceOnDrop;

impl Drop for ClearSourceOnDrop {
	fn drop(&mut self) {
		set_config_source(None);
	}
}

// ---- Pure logic: %g formatting -------------------------------------

#[test]
fn test_format_g_matches_c() {
	assert_eq!(format_g(0.0), "0");
	assert_eq!(format_g(-0.0), "-0");
	assert_eq!(format_g(1920.0), "1920");
	assert_eq!(format_g(100.0), "100");
	assert_eq!(format_g(3.15), "3.15");
	assert_eq!(format_g(1.5), "1.5");
	assert_eq!(format_g(0.1), "0.1");
	assert_eq!(format_g(-3.5), "-3.5");
	assert_eq!(format_g(1.23456789), "1.23457");
	assert_eq!(format_g(1234567.0), "1.23457e+06");
	assert_eq!(format_g(0.00012345), "0.00012345");
	assert_eq!(format_g(0.000012345), "1.2345e-05");
	assert_eq!(format_g(999999.5), "1e+06");
	assert_eq!(format_g(f64::NAN), "nan");
	assert_eq!(format_g(f64::INFINITY), "inf");
	assert_eq!(format_g(f64::NEG_INFINITY), "-inf");
}

#[test]
fn test_join_key() {
	assert_eq!(join_key(None, "k"), "k");
	assert_eq!(join_key(Some(""), "k"), "k");
	assert_eq!(join_key(Some("g"), "k"), "g/k");
}

// ---- Defaults ------------------------------------------------------

#[test]
fn test_defaults() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
	assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
	assert_eq!(s.get_int(None, "DefaultSequenceInterlacing", -1), 0);
	assert_eq!(s.get_int(None, "DefaultSequenceAudioFrequency", -1), 48000);
	assert_eq!(s.get_int(None, "DefaultSequenceAudioLayout", -1), 3);
	assert_eq!(s.get_int(None, "OfflinePixelFormat", -1), 4);
	assert_eq!(s.get_int(None, "DiskCacheSaveInterval", -1), 10000);
	assert_eq!(s.get_int(None, "AutoCacheDelay", -1), 1000);
	assert_eq!(s.get_int(None, "ProxyWidth", -1), 1280);
	assert_eq!(s.get_int(None, "ProxyHeight", -1), 720);
	assert_eq!(s.get_int(None, "ProxyDivider", -1), 1);
	assert_eq!(s.get_int(None, "ProxyCRF", -1), 23);
	assert_eq!(s.get_int(None, "MarkerColor", -1), 6);
	assert_eq!(s.get_int(None, "CatColor0", -1), 0);
	assert_eq!(s.get_int(None, "CatColor11", -1), 11);

	assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 1);
	assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 1);
	assert_eq!(s.get_bool(None, "UseGLFinish", 1), 0);
	assert_eq!(s.get_bool(None, "ReassocLinToNonLin", 1), 0);
	assert_eq!(s.get_bool(None, "ProxyIncludeAudio", -1), 1);

	assert_eq!(s.get(None, "GraphicsBackend").unwrap(), "opengl");
	assert_eq!(
		s.get(None, "DefaultSequenceFrameRate").unwrap(),
		"1001/30000"
	);
	assert_eq!(s.get(None, "DefaultSequencePixelAspect").unwrap(), "1/1");
	assert_eq!(s.get(None, "LUTLibraryPaths").unwrap(), "");
	assert_eq!(s.get(None, "DiskCacheBehind").unwrap(), "0/1");
	assert_eq!(s.get(None, "DiskCacheAhead").unwrap(), "60/1");
	assert_eq!(s.get(None, "ProxyPreset").unwrap(), "veryfast");
}

// ---- Basic getters/setters -----------------------------------------

#[test]
fn test_set_get_string() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set(None, "MyString", "hello world");
	assert_eq!(s.get(None, "MyString").unwrap(), "hello world");
	assert_eq!(s.entry_type(None, "MyString").unwrap(), EntryType::String);
}

#[test]
fn test_get_missing_and_invalid() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	assert!(matches!(
		s.get(None, "DefinitelyMissing"),
		Err(Error::NotFound)
	));
	assert!(matches!(
		s.entry_type(None, "DefinitelyMissing"),
		Err(Error::NotFound)
	));
	assert!(matches!(s.get(None, ""), Err(Error::Invalid)));
	assert!(matches!(s.entry_type(None, ""), Err(Error::Invalid)));
}

#[test]
fn test_int_get_set_fallback() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_int(None, "MyInt", 42);
	assert_eq!(s.get_int(None, "MyInt", 0), 42);
	assert_eq!(s.get_int64(None, "MyInt", 0), 42);
	assert_eq!(s.entry_type(None, "MyInt").unwrap(), EntryType::Int);

	let big = i64::from(i32::MAX) + 1;
	s.set_int64(None, "MyBigInt", big);
	assert_eq!(s.get_int64(None, "MyBigInt", 0), big);

	assert_eq!(s.get_int(None, "MissingInt", 7), 7);
	assert_eq!(s.get_int64(None, "MissingInt", 7), 7);

	// Wrong type -> fallback.
	s.set(None, "MyStr", "abc");
	assert_eq!(s.get_int(None, "MyStr", 9), 9);
	assert_eq!(s.get_int64(None, "MyStr", 9), 9);
}

#[test]
fn test_double_get_set_fallback() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_double(None, "MyD", 3.15);
	assert_eq!(s.get_double(None, "MyD", -1.0), 3.15);
	assert_eq!(s.get(None, "MyD").unwrap(), "3.15");
	assert_eq!(s.entry_type(None, "MyD").unwrap(), EntryType::Double);

	assert_eq!(s.get_double(None, "MissingD", 2.5), 2.5);
	s.set(None, "MyStr2", "xyz");
	assert_eq!(s.get_double(None, "MyStr2", 2.5), 2.5);
}

#[test]
fn test_bool_get_set_fallback() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_bool(None, "MyB", 1);
	assert_eq!(s.get_bool(None, "MyB", -1), 1);
	assert_eq!(s.get(None, "MyB").unwrap(), "true");
	assert_eq!(s.entry_type(None, "MyB").unwrap(), EntryType::Bool);

	s.set_bool(None, "MyB2", 0);
	assert_eq!(s.get_bool(None, "MyB2", 1), 0);
	assert_eq!(s.get(None, "MyB2").unwrap(), "false");

	assert_eq!(s.get_bool(None, "MissingB", 5), 5);
	s.set(None, "MyStr3", "xyz");
	assert_eq!(s.get_bool(None, "MyStr3", 5), 5);
}

#[test]
fn test_set_string_parses_into_typed_entry() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	// Create a typed int entry, then set it via the string setter: the
	// string is parsed into the declared type.
	s.set_int(None, "Parsed", 0);
	s.set(None, "Parsed", "123");
	assert_eq!(s.get_int(None, "Parsed", 0), 123);
	assert_eq!(s.entry_type(None, "Parsed").unwrap(), EntryType::Int);

	// Unparseable string leaves the typed entry unchanged.
	s.set(None, "Parsed", "abc");
	assert_eq!(s.get_int(None, "Parsed", 0), 123);

	// Bool path.
	s.set_bool(None, "ParsedB", 0);
	s.set(None, "ParsedB", "true");
	assert_eq!(s.get_bool(None, "ParsedB", -1), 1);
	s.set(None, "ParsedB", "nonsense");
	assert_eq!(s.get_bool(None, "ParsedB", -1), 1);

	// Double path.
	s.set_double(None, "ParsedD", 0.0);
	s.set(None, "ParsedD", "2.5");
	assert_eq!(s.get_double(None, "ParsedD", -1.0), 2.5);
	s.set(None, "ParsedD", "junk");
	assert_eq!(s.get_double(None, "ParsedD", -1.0), 2.5);
}

#[test]
fn test_grouped_keys() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_int(Some("audio"), "sample_rate", 44100);
	assert_eq!(s.get_int(Some("audio"), "sample_rate", 0), 44100);
	assert_eq!(
		s.entry_type(Some("audio"), "sample_rate").unwrap(),
		EntryType::Int
	);

	// Empty group and None are equivalent (flat keys).
	s.set_int(Some(""), "flatkey", 7);
	assert_eq!(s.get_int(None, "flatkey", 0), 7);
	assert_eq!(s.get_int(Some(""), "flatkey", 0), 7);
}

// ---- TOML persistence --------------------------------------------------

#[test]
fn test_load_missing_file() {
	with_temp_config(|_dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		// Missing file (no config.toml, no config.ini) is not an error;
		// defaults remain.
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
	});
}

#[test]
fn test_save_load_roundtrip() {
	with_temp_config(|_dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set(None, "CustomString", "custom value");
		// A custom double is persisted but, not being a registered key, is
		// reloaded as a string (C++ parity: only known keys keep their type).
		s.set_double(Some("render"), "gain", 1.5);
		// Modify a KNOWN typed key so its type survives a reload.
		s.set_int(None, "DefaultSequenceWidth", 640);
		s.set_bool(None, "SplitClipsCopyNodes", 0);
		s.save().unwrap();

		// Wipe back to defaults, dropping custom keys.
		s.reset_defaults().unwrap();
		assert!(matches!(s.get(None, "CustomString"), Err(Error::NotFound)));

		s.load().unwrap();
		// Flat custom string roundtrips.
		assert_eq!(s.get(None, "CustomString").unwrap(), "custom value");
		// Custom typed keys lose their type on reload -> string form.
		assert_eq!(s.get(Some("render"), "gain").unwrap(), "1.5");
		assert_eq!(s.get_double(Some("render"), "gain", -1.0), -1.0);
		// Known typed keys roundtrip with their type preserved.
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 0);

		// Defaults are still present after load.
		assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
	});
}

#[test]
fn test_save_toml_format() {
	with_temp_config(|dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_int(None, "FlatKey", 1);
		s.set(None, "StrKey", "hello");
		s.set_double(None, "DblKey", 2.5);
		s.set_bool(None, "BoolKey", 1);
		s.set_int(Some("alpha"), "x", 2);
		s.set_int(Some("beta"), "y", 3);
		s.save().unwrap();

		let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
		// Native TOML types: ints/bools/floats unquoted, strings quoted.
		assert!(content.contains("FlatKey = 1"), "content: {}", content);
		assert!(
			content.contains("StrKey = \"hello\""),
			"content: {}",
			content
		);
		assert!(content.contains("DblKey = 2.5"), "content: {}", content);
		assert!(content.contains("BoolKey = true"), "content: {}", content);
		// Grouped keys land in [group] tables.
		assert!(content.contains("[alpha]"), "content: {}", content);
		assert!(content.contains("x = 2"), "content: {}", content);
		assert!(content.contains("[beta]"), "content: {}", content);
		assert!(content.contains("y = 3"), "content: {}", content);
		// Flat keys are written before any section header.
		let flat_pos = content.find("FlatKey = 1").unwrap();
		let alpha_pos = content.find("[alpha]").unwrap();
		assert!(flat_pos < alpha_pos);
		// The temp file is renamed away and does not linger.
		assert!(!dir.join("config.toml.tmp").exists());
		// save() writes TOML only; no legacy INI is produced.
		assert!(!dir.join("config.ini").exists());
	});
}

#[test]
fn test_migrate_ini_parses_types_and_skips_lines() {
	with_temp_config(|dir| {
		// A legacy config.ini with no config.toml: load() reads it via the
		// migration path.
		let ini = "\
# comment
; another comment

DefaultSequenceWidth=640
UseProxyMedia=false
[section]
UnknownTypedThing=hello
";
		std::fs::write(dir.join("config.ini"), ini).unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
		assert_eq!(
			s.get(Some("section"), "UnknownTypedThing").unwrap(),
			"hello"
		);
		// The migration immediately persisted a config.toml.
		assert!(dir.join("config.toml").is_file());
	});
}

#[test]
fn test_migrate_ini_unparseable_keeps_default() {
	with_temp_config(|dir| {
		std::fs::write(dir.join("config.ini"), "DefaultSequenceWidth=notanumber\n").unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();
		// Unparseable int keeps the compiled-in default.
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
	});
}

#[test]
fn test_migrate_ini_trims_values() {
	with_temp_config(|dir| {
		// parse_ini trims " \t\r\n" around keys and values.
		std::fs::write(dir.join("config.ini"), "  DefaultSequenceWidth = 320  \n").unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 320);
	});
}

#[test]
fn test_toml_roundtrip_all_types() {
	with_temp_config(|dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		// One custom key per ConfigValue type. Known keys (present in the
		// compiled-in defaults) keep their declared type across a reload;
		// unknown keys reload as strings (C++ parity).
		s.set(None, "CustomString", "hello world");
		s.set_int(None, "DefaultSequenceWidth", 640); // known Int
		s.set_bool(None, "SplitClipsCopyNodes", 0); // known Bool
		s.set(None, "ProxyPreset", "medium"); // known String
		s.set_double(Some("render"), "gain", 1.5); // unknown Double
		s.save().unwrap();

		// Wipe back to defaults, dropping custom keys.
		s.reset_defaults().unwrap();
		assert!(matches!(s.get(None, "CustomString"), Err(Error::NotFound)));

		s.load().unwrap();
		assert_eq!(s.get(None, "CustomString").unwrap(), "hello world");
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 0);
		assert_eq!(s.get(None, "ProxyPreset").unwrap(), "medium");
		// Unknown double reloads in string form ("1.5").
		assert_eq!(s.get(Some("render"), "gain").unwrap(), "1.5");
		assert_eq!(s.get_double(Some("render"), "gain", -1.0), -1.0);
		// The file holds native TOML types, not stringified values.
		let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
		assert!(
			content.contains("DefaultSequenceWidth = 640"),
			"content: {}",
			content
		);
		assert!(
			content.contains("SplitClipsCopyNodes = false"),
			"content: {}",
			content
		);
		assert!(
			content.contains("ProxyPreset = \"medium\""),
			"content: {}",
			content
		);
		assert!(content.contains("gain = 1.5"), "content: {}", content);
	});
}

#[test]
fn test_migrate_from_ini_generates_toml() {
	with_temp_config(|dir| {
		// A legacy config.ini with no config.toml: load() must read it and
		// immediately persist the values as TOML, leaving the INI in place.
		let ini = "\
DefaultSequenceWidth=640
UseProxyMedia=false
[section]
CustomKey=hello
";
		std::fs::write(dir.join("config.ini"), ini).unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();

		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
		assert_eq!(s.get(Some("section"), "CustomKey").unwrap(), "hello");

		// config.toml was generated with the migrated values; config.ini
		// is deliberately not deleted.
		assert!(dir.join("config.toml").is_file());
		assert!(dir.join("config.ini").is_file());
		let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
		assert!(
			content.contains("DefaultSequenceWidth = 640"),
			"content: {}",
			content
		);
		assert!(
			content.contains("UseProxyMedia = false"),
			"content: {}",
			content
		);
		assert!(content.contains("[section]"), "content: {}", content);
		assert!(
			content.contains("CustomKey = \"hello\""),
			"content: {}",
			content
		);

		// A second load reads the migrated TOML, not the INI.
		s.reset_defaults().unwrap();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
	});
}

#[test]
fn test_toml_preferred_over_ini() {
	with_temp_config(|dir| {
		// Both files present with conflicting values: config.toml wins and
		// the INI is neither consulted nor migrated.
		std::fs::write(
			dir.join("config.ini"),
			"DefaultSequenceWidth=800\nOnlyInIni=legacy\n",
		)
		.unwrap();
		std::fs::write(
			dir.join("config.toml"),
			"DefaultSequenceWidth = 640\nOnlyInToml = \"modern\"\n",
		)
		.unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
		assert_eq!(s.get(None, "OnlyInToml").unwrap(), "modern");
		assert!(matches!(s.get(None, "OnlyInIni"), Err(Error::NotFound)));
	});
}

// ---- Error paths ----------------------------------------------------

static REPORTED: StdMutex<Vec<(String, String)>> = StdMutex::new(Vec::new());

extern "C" fn record_handler(title: *const c_char, message: *const c_char, _userdata: *mut c_void) {
	unsafe {
		let title = std::ffi::CStr::from_ptr(title)
			.to_string_lossy()
			.into_owned();
		let message = std::ffi::CStr::from_ptr(message)
			.to_string_lossy()
			.into_owned();
		REPORTED.lock().unwrap().push((title, message));
	}
}

#[test]
fn test_load_directory_returns_failed_and_reports() {
	with_temp_config(|dir| {
		// A directory at the config path is not a readable config. With no
		// config.toml present, load() hits the legacy INI fallback, which
		// treats a directory at config.ini the same way.
		std::fs::create_dir(dir.join("config.ini")).unwrap();
		REPORTED.lock().unwrap().clear();
		let s = ConfigStore::instance();
		s.set_error_handler(Some(record_handler), std::ptr::null_mut())
			.unwrap();
		let res = s.load();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();
		assert!(res.is_err());
		assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);

		let reported = REPORTED.lock().unwrap().clone();
		assert_eq!(reported.len(), 1);
		assert_eq!(reported[0].0, "Error loading settings");
		assert!(reported[0]
			.1
			.contains("Failed to load application settings"));
	});
}

#[test]
fn test_error_handler_none_clears() {
	with_temp_config(|dir| {
		std::fs::create_dir(dir.join("config.ini")).unwrap();
		let s = ConfigStore::instance();
		s.set_error_handler(Some(record_handler), std::ptr::null_mut())
			.unwrap();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();
		REPORTED.lock().unwrap().clear();
		let res = s.load();
		// With no handler, the error goes to stderr instead of the callback.
		assert!(res.is_err());
		assert!(REPORTED.lock().unwrap().is_empty());
	});
}

// ---- Pure logic: value conversion -------------------------------------

#[test]
fn test_value_to_string_all_types() {
	assert_eq!(value_to_string(&ConfigValue::String("hi".into())), "hi");
	assert_eq!(value_to_string(&ConfigValue::Int(-42)), "-42");
	assert_eq!(
		value_to_string(&ConfigValue::Int(i64::MAX)),
		"9223372036854775807"
	);
	assert_eq!(value_to_string(&ConfigValue::Double(2.5)), "2.5");
	assert_eq!(value_to_string(&ConfigValue::Double(0.0)), "0");
	assert_eq!(value_to_string(&ConfigValue::Bool(true)), "true");
	assert_eq!(value_to_string(&ConfigValue::Bool(false)), "false");
}

#[test]
fn test_to_entry_type() {
	assert_eq!(
		to_entry_type(&ConfigValue::String(String::new())),
		EntryType::String
	);
	assert_eq!(to_entry_type(&ConfigValue::Int(0)), EntryType::Int);
	assert_eq!(to_entry_type(&ConfigValue::Double(0.0)), EntryType::Double);
	assert_eq!(to_entry_type(&ConfigValue::Bool(false)), EntryType::Bool);
}

#[test]
fn test_string_to_value_int() {
	// Mirrors std::stoll: leading whitespace is skipped, the rest must
	// parse fully (`configstore.cpp:183-194`).
	assert_eq!(
		string_to_value("42", EntryType::Int),
		Some(ConfigValue::Int(42))
	);
	assert_eq!(
		string_to_value("  42", EntryType::Int),
		Some(ConfigValue::Int(42))
	);
	assert_eq!(
		string_to_value("-7", EntryType::Int),
		Some(ConfigValue::Int(-7))
	);
	assert_eq!(
		string_to_value("+7", EntryType::Int),
		Some(ConfigValue::Int(7))
	);
	// Trailing junk / whitespace fails (std::stoll pos != size).
	assert!(string_to_value("42 ", EntryType::Int).is_none());
	assert!(string_to_value("42x", EntryType::Int).is_none());
	assert!(string_to_value("0x10", EntryType::Int).is_none());
	assert!(string_to_value("", EntryType::Int).is_none());
	assert!(string_to_value("99999999999999999999999", EntryType::Int).is_none());
}

#[test]
fn test_string_to_value_double() {
	assert_eq!(
		string_to_value("2.5", EntryType::Double),
		Some(ConfigValue::Double(2.5))
	);
	assert_eq!(
		string_to_value(" 2.5", EntryType::Double),
		Some(ConfigValue::Double(2.5))
	);
	assert_eq!(
		string_to_value("1e3", EntryType::Double),
		Some(ConfigValue::Double(1000.0))
	);
	// std::stod accepts inf/infinity/nan (case-insensitive).
	assert_eq!(
		string_to_value("inf", EntryType::Double),
		Some(ConfigValue::Double(f64::INFINITY))
	);
	assert_eq!(
		string_to_value("-Infinity", EntryType::Double),
		Some(ConfigValue::Double(f64::NEG_INFINITY))
	);
	match string_to_value("NaN", EntryType::Double) {
		Some(ConfigValue::Double(d)) => assert!(d.is_nan()),
		other => panic!("expected NaN double, got {:?}", other),
	}
	// Trailing junk fails.
	assert!(string_to_value("2.5 ", EntryType::Double).is_none());
	assert!(string_to_value("2.5x", EntryType::Double).is_none());
	assert!(string_to_value("", EntryType::Double).is_none());
}

#[test]
fn test_string_to_value_bool_and_misc() {
	assert_eq!(
		string_to_value("true", EntryType::Bool),
		Some(ConfigValue::Bool(true))
	);
	assert_eq!(
		string_to_value("1", EntryType::Bool),
		Some(ConfigValue::Bool(true))
	);
	assert_eq!(
		string_to_value("false", EntryType::Bool),
		Some(ConfigValue::Bool(false))
	);
	assert_eq!(
		string_to_value("0", EntryType::Bool),
		Some(ConfigValue::Bool(false))
	);
	// Case-sensitive in the C++ ("TRUE" does not parse).
	assert!(string_to_value("TRUE", EntryType::Bool).is_none());
	assert!(string_to_value("yes", EntryType::Bool).is_none());
	assert!(string_to_value("2", EntryType::Bool).is_none());

	// Strings always parse; None never does.
	assert_eq!(
		string_to_value("anything", EntryType::String),
		Some(ConfigValue::String("anything".into()))
	);
	assert!(string_to_value("anything", EntryType::None).is_none());
}

#[test]
fn test_trim() {
	assert_eq!(trim("  hello \t\r\n"), "hello");
	assert_eq!(trim("no-space"), "no-space");
	assert_eq!(trim("   "), "");
	assert_eq!(trim(""), "");
	assert_eq!(trim(" a b "), "a b");
}

#[test]
fn test_format_g_edge_cases() {
	// %g switches to scientific at exponent >= 6 or < -4.
	assert_eq!(format_g(100000.0), "100000");
	assert_eq!(format_g(1000000.0), "1e+06");
	assert_eq!(format_g(0.0001), "0.0001");
	assert_eq!(format_g(0.00001), "1e-05");
	// Trailing zeros dropped, integer digits never truncated.
	assert_eq!(format_g(1.0), "1");
	assert_eq!(format_g(1.20), "1.2");
	assert_eq!(format_g(-0.00012345), "-0.00012345");
	assert_eq!(format_g(6.5e100), "6.5e+100");
}

// ---- Empty-key / fallback semantics -----------------------------------

#[test]
fn test_empty_key_is_rejected_everywhere() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	// Setters with an empty key are silent no-ops.
	s.set(None, "", "v");
	s.set(Some("g"), "", "v");
	s.set_int(None, "", 1);
	s.set_int64(None, "", 1);
	s.set_double(None, "", 1.0);
	s.set_bool(None, "", 1);
	assert!(matches!(s.get(None, ""), Err(Error::Invalid)));
	assert!(matches!(s.get(Some("g"), ""), Err(Error::Invalid)));
	assert_eq!(s.get_int(None, "", 7), 7);
	assert_eq!(s.get_int64(None, "", 7), 7);
	assert_eq!(s.get_double(None, "", 7.0), 7.0);
	assert_eq!(s.get_bool(None, "", 7), 7);
	assert!(matches!(s.entry_type(None, ""), Err(Error::Invalid)));
}

#[test]
fn test_typed_getters_cross_type_fallbacks() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_int(None, "OnlyInt", 5);
	// Every other typed getter falls back on the int entry.
	assert_eq!(s.get_double(None, "OnlyInt", 1.5), 1.5);
	assert_eq!(s.get_bool(None, "OnlyInt", 3), 3);
	// ...but the string getter formats any type.
	assert_eq!(s.get(None, "OnlyInt").unwrap(), "5");
	s.set_bool(None, "OnlyBool", 1);
	assert_eq!(s.get_int(None, "OnlyBool", 9), 9);
	assert_eq!(s.get_double(None, "OnlyBool", 9.0), 9.0);
	assert_eq!(s.get(None, "OnlyBool").unwrap(), "true");
}

#[test]
fn test_unicode_keys_and_values() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set(Some("日本語グループ"), "キー", "値🎬");
	assert_eq!(s.get(Some("日本語グループ"), "キー").unwrap(), "値🎬");
	assert_eq!(
		s.entry_type(Some("日本語グループ"), "キー").unwrap(),
		EntryType::String
	);
	// A group containing '/' nests (join is just string concatenation),
	// so ("a/b", "c") and ("a", "b/c") address the SAME entry.
	s.set_int(Some("a/b"), "c", 5);
	assert_eq!(s.get_int(Some("a/b"), "c", 0), 5);
	assert_eq!(s.get_int(Some("a"), "b/c", 0), 5);
}

// ---- TOML byte-level format --------------------------------------------

#[test]
fn test_save_toml_contents_all_defaults() {
	with_temp_config(|dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.save().unwrap();

		let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
		// Re-parse the saved file with the toml crate itself: it must be
		// valid TOML carrying every registered default with its native type.
		let parsed: toml::Value = toml::from_str(&content).expect("saved config.toml parses");
		let table = parsed.as_table().unwrap();
		assert_eq!(table["DefaultSequenceWidth"].as_integer(), Some(1920));
		assert_eq!(table["DefaultSequenceHeight"].as_integer(), Some(1080));
		assert_eq!(table["ProxyPreset"].as_str(), Some("veryfast"));
		assert_eq!(table["SplitClipsCopyNodes"].as_bool(), Some(true));
		assert_eq!(table["UseGLFinish"].as_bool(), Some(false));
		assert_eq!(table["CatColor3"].as_integer(), Some(3));
		assert!(table.contains_key("DiskCacheSaveInterval"));
		assert!(table.contains_key("LUTLibraryPaths"));
		// No group defaults exist; every top-level key is a plain value.
		assert!(table.values().all(|v| !v.is_table()));
		// Serialized output must not contain INI-style "k=v" lines.
		assert!(
			!content.contains("DefaultSequenceWidth=1920"),
			"content: {}",
			content
		);
	});
}

#[test]
fn test_nested_group_roundtrip() {
	with_temp_config(|dir| {
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		// Group = everything before the LAST '/', so "a/b" + "c" lands in
		// a `["a/b"]` table (the '/' forces a quoted TOML key).
		s.set_int(Some("a/b"), "c", 5);
		s.save().unwrap();
		let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
		assert!(content.contains("[\"a/b\"]"), "content: {}", content);
		assert!(content.contains("c = 5"), "content: {}", content);

		s.reset_defaults().unwrap();
		s.load().unwrap();
		// Reloaded as an unknown key -> string, addressed as "a/b/c".
		assert_eq!(s.get(Some("a/b"), "c").unwrap(), "5");
		assert_eq!(s.get_int(Some("a/b"), "c", -1), -1);
	});
}

// ---- Merge order: defaults < loaded file < runtime sets -----------------

#[test]
fn test_merge_order_defaults_file_runtime() {
	with_temp_config(|dir| {
		std::fs::write(
			dir.join("config.ini"),
			"DefaultSequenceWidth=800\nCustomFromFile=yes\n",
		)
		.unwrap();
		let s = ConfigStore::instance();

		// A runtime set made BEFORE load() is wiped: load() resets to
		// defaults first (`configstore.cpp:241`).
		s.reset_defaults().unwrap();
		s.set_int(None, "DefaultSequenceWidth", 640);
		s.set(None, "RuntimeOnly", "r");
		s.load().unwrap();
		// File value wins over both defaults and the pre-load runtime set.
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 800);
		assert!(matches!(s.get(None, "RuntimeOnly"), Err(Error::NotFound)));
		assert_eq!(s.get(None, "CustomFromFile").unwrap(), "yes");

		// A runtime set AFTER load() wins over the file.
		s.set_int(None, "DefaultSequenceWidth", 1024);
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1024);
		// Keys not mentioned in the file keep their defaults.
		assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
	});
}

// ---- More load parsing edge cases ---------------------------------------

#[test]
fn test_load_lax_parsing_matrix() {
	with_temp_config(|dir| {
		// Hand-built INI exercising the C++ load() branches
		// (`configstore.cpp:268-312`).
		let ini = "\
BareLineWithoutEquals
=EmptyKeySkipped
[g]
KeyWithEquals=a=b
[]
FlatAfterEmptySection=ok
";
		std::fs::write(dir.join("config.ini"), ini).unwrap();
		let s = ConfigStore::instance();
		s.load().unwrap();
		// Malformed lines are skipped, not errors.
		assert!(matches!(
			s.get(None, "BareLineWithoutEquals"),
			Err(Error::NotFound)
		));
		// Value keeps everything after the FIRST '='.
		assert_eq!(s.get(Some("g"), "KeyWithEquals").unwrap(), "a=b");
		// "[]" empties the group, so the key is flat.
		assert_eq!(s.get(None, "FlatAfterEmptySection").unwrap(), "ok");
		assert!(s.get(Some(""), "FlatAfterEmptySection").is_ok());
	});
}

#[test]
fn test_load_invalid_utf8_reports_error() {
	with_temp_config(|dir| {
		// Invalid UTF-8: the C++ reads raw bytes and would muddle
		// through; the Rust port maps this to the same "unreadable file"
		// failure as a directory (documented CPP-PARITY note in load()).
		std::fs::write(dir.join("config.ini"), b"DefaultSequenceWidth=\xff\xfe\n").unwrap();
		REPORTED.lock().unwrap().clear();
		let s = ConfigStore::instance();
		s.set_error_handler(Some(record_handler), std::ptr::null_mut())
			.unwrap();
		let res = s.load();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();
		assert!(res.is_err());
		assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);
		assert_eq!(REPORTED.lock().unwrap().len(), 1);
	});
}

#[test]
fn test_load_invalid_toml_reports_error() {
	with_temp_config(|dir| {
		// A corrupt config.toml must be reported and surfaced as an error,
		// never silently ignored (which would lose the user's settings on
		// the next save).
		std::fs::write(dir.join("config.toml"), "DefaultSequenceWidth = =\n").unwrap();
		REPORTED.lock().unwrap().clear();
		let s = ConfigStore::instance();
		s.set_error_handler(Some(record_handler), std::ptr::null_mut())
			.unwrap();
		let res = s.load();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();
		assert!(res.is_err());
		assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);
		let reported = REPORTED.lock().unwrap().clone();
		assert_eq!(reported.len(), 1);
		assert_eq!(reported[0].0, "Error loading settings");
		assert!(reported[0]
			.1
			.contains("Failed to load application settings"));
	});
}

// ---- Save error path -----------------------------------------------------

#[test]
fn test_save_failure_reports_error() {
	let _g = test_lock().lock().unwrap();
	let dir = std::env::temp_dir().join(format!(
		"quercus_core_configstore_test_{}",
		std::process::id()
	));
	let _ = std::fs::create_dir_all(&dir);
	// Point QUERCUS_CONFIG_DIR at a regular FILE so writing
	// "<dir>/config.toml.tmp" fails (create_dir_all on it is a silent
	// no-op failure, exactly like the C++ ec-swallowing).
	let blocker = dir.join("not_a_dir");
	std::fs::write(&blocker, b"x").unwrap();
	std::env::set_var("QUERCUS_CONFIG_DIR", &blocker);

	REPORTED.lock().unwrap().clear();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set_error_handler(Some(record_handler), std::ptr::null_mut())
		.unwrap();
	let res = s.save();
	s.set_error_handler(None, std::ptr::null_mut()).unwrap();

	std::env::remove_var("QUERCUS_CONFIG_DIR");
	let _ = std::fs::remove_dir_all(&dir);

	assert!(res.is_err());
	assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);
	let reported = REPORTED.lock().unwrap().clone();
	assert_eq!(reported.len(), 1);
	assert_eq!(reported[0].0, "Error saving settings");
}

// ---- Error handler userdata ----------------------------------------------

static USERDATA_HITS: StdMutex<Vec<usize>> = StdMutex::new(Vec::new());

extern "C" fn userdata_handler(
	_title: *const c_char,
	_message: *const c_char,
	userdata: *mut c_void,
) {
	USERDATA_HITS.lock().unwrap().push(userdata as usize);
}

#[test]
fn test_error_handler_receives_userdata() {
	with_temp_config(|dir| {
		std::fs::create_dir(dir.join("config.ini")).unwrap();
		USERDATA_HITS.lock().unwrap().clear();
		let s = ConfigStore::instance();
		s.set_error_handler(Some(userdata_handler), 0xDEADusize as *mut c_void)
			.unwrap();
		let _ = s.load();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();
		assert_eq!(USERDATA_HITS.lock().unwrap().as_slice(), &[0xDEADusize]);
	});
}

// ---- Host config source: persistence --------------------------------

#[test]
fn test_config_source_save_load_roundtrip() {
	let _g = test_lock().lock().unwrap();
	let _cleanup = ClearSourceOnDrop;
	let saved = install_memory_source(None);
	assert!(config_source_installed());

	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	s.set(None, "HostString", "from host");
	s.set_int(None, "DefaultSequenceWidth", 640);
	s.save().unwrap();

	// save() handed TOML to the host instead of writing a file.
	let text = saved
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
		.expect("source received TOML");
	assert!(
		text.contains("HostString = \"from host\""),
		"text: {}",
		text
	);
	assert!(
		text.contains("DefaultSequenceWidth = 640"),
		"text: {}",
		text
	);

	// Wipe, then reload from the same in-memory source.
	s.reset_defaults().unwrap();
	s.load().unwrap();
	assert_eq!(s.get(None, "HostString").unwrap(), "from host");
	assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);

	MEM_SOURCE_DROPS.store(0, Ordering::SeqCst);
	assert!(ConfigStore::instance().remove_source());
	assert!(!config_source_installed());
	assert_eq!(MEM_SOURCE_DROPS.load(Ordering::SeqCst), 1);
}

#[test]
fn test_config_source_replacement_drops_old_exactly_once() {
	let _g = test_lock().lock().unwrap();
	let _cleanup = ClearSourceOnDrop;
	install_memory_source(None);
	MEM_SOURCE_DROPS.store(0, Ordering::SeqCst);
	install_memory_source(None);
	// Installing the second source dropped the first one once.
	assert_eq!(MEM_SOURCE_DROPS.load(Ordering::SeqCst), 1);
	assert!(config_source_installed());

	assert!(ConfigStore::instance().remove_source());
	assert_eq!(MEM_SOURCE_DROPS.load(Ordering::SeqCst), 2);
	assert!(!ConfigStore::instance().remove_source());
	assert_eq!(MEM_SOURCE_DROPS.load(Ordering::SeqCst), 2);
}

#[test]
fn test_config_source_invalid_toml_reports_error() {
	let _g = test_lock().lock().unwrap();
	let _cleanup = ClearSourceOnDrop;
	install_memory_source(Some("DefaultSequenceWidth = =\n"));
	REPORTED.lock().unwrap().clear();
	let s = ConfigStore::instance();
	s.set_error_handler(Some(record_handler), std::ptr::null_mut())
		.unwrap();
	let res = s.load();
	s.set_error_handler(None, std::ptr::null_mut()).unwrap();
	assert!(res.is_err());
	assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);
	let reported = REPORTED.lock().unwrap().clone();
	assert_eq!(reported.len(), 1);
	assert_eq!(reported[0].0, "Error loading settings");
}

#[test]
fn test_config_source_load_error_reports() {
	let _g = test_lock().lock().unwrap();
	let _cleanup = ClearSourceOnDrop;
	// `None` content makes the host loader report failure.
	install_memory_source(None);
	REPORTED.lock().unwrap().clear();
	let s = ConfigStore::instance();
	s.set_error_handler(Some(record_handler), std::ptr::null_mut())
		.unwrap();
	let res = s.load();
	s.set_error_handler(None, std::ptr::null_mut()).unwrap();
	assert!(res.is_err());
	assert_eq!(res.unwrap_err().code(), crate::error::QUERCUSCORE_E_FAILED);
	assert_eq!(REPORTED.lock().unwrap().len(), 1);
}

#[test]
fn test_clear_config_source_restores_file_behavior() {
	with_temp_config(|dir| {
		let _cleanup = ClearSourceOnDrop;
		let saved = install_memory_source(Some("DefaultSequenceWidth = 111\n"));
		let s = ConfigStore::instance();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 111);
		// Saving through the source leaves the config directory untouched.
		s.save().unwrap();
		assert!(saved.lock().unwrap().is_some());
		assert!(!dir.join("config.toml").exists());
		assert!(!dir.join("config.ini").exists());

		MEM_SOURCE_DROPS.store(0, Ordering::SeqCst);
		assert!(ConfigStore::instance().remove_source());
		assert_eq!(MEM_SOURCE_DROPS.load(Ordering::SeqCst), 1);

		// With no source, save/load are back on `<config_location>/config.toml`.
		s.reset_defaults().unwrap();
		s.set_int(None, "DefaultSequenceWidth", 222);
		s.save().unwrap();
		assert!(dir.join("config.toml").is_file());
		s.reset_defaults().unwrap();
		s.load().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 222);
	});
}

// ---- Thread-safety smoke test ---------------------------------------------

#[test]
fn test_singleton_thread_safety_smoke() {
	let _g = test_lock().lock().unwrap();
	let s = ConfigStore::instance();
	s.reset_defaults().unwrap();
	let mut handles = Vec::new();
	for t in 0..8 {
		handles.push(std::thread::spawn(move || {
			let store = ConfigStore::instance();
			for i in 0..50 {
				let key = format!("thread{}/key{}", t, i % 5);
				store.set_int(None, &key, i);
				let _ = store.get_int(None, &key, -1);
				let _ = store.get(None, &key);
				let _ = store.entry_type(None, &key);
			}
		}));
	}
	for h in handles {
		h.join().unwrap();
	}
	// Same instance across threads, and the last writer won.
	assert!(s.get_int(None, "thread3/key4", -1) >= 0);
	s.reset_defaults().unwrap();
}
