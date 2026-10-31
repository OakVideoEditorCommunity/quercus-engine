//! C ABI for quercus-core's process-wide singletons and global state:
//! the config store, the color pipeline settings, display ICC queries,
//! and the process-wide log level/sink (prefix `quercus_core_`).
//!
//! Strings cross as borrowed `(ptr, len)` inputs and snprintf-style
//! output buffers (see `codec::copy_str_out`); nullable group/path
//! pointers mean "no group" / "default". `GpuContext` is deliberately
//! NOT exported: it bridges wgpu objects, which C consumers cannot
//! construct — it needs a separate engine-owns-the-device design.

use std::ffi::c_void;

use quercus_core::color;
use quercus_core::colormath::{OutputColorSpec, WorkingColorSpace};
use quercus_core::configstore::{self, ConfigStore};
use quercus_core::debug::{self, Level};
use quercus_core::displayicc;

use crate::codec::{copy_str_out, str_arg};
use crate::ext::{config_source, log_sink};

// ---------------------------------------------------------------------------
// Logging (process-wide level and sink)
// ---------------------------------------------------------------------------

pub use crate::ext::log_sink::{QuercusLogDropFn, QuercusLogFn};

/// Sets the process-wide minimum log level; false when `level` is outside
/// 0..=4.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_log_set_level(level: i32) -> bool {
	match Level::from_code(level) {
		Some(level) => {
			debug::log_set_level(level);
			true
		}
		None => false,
	}
}

/// The process-wide minimum log level (0 = DEBUG … 4 = FATAL).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_log_get_level() -> i32 {
	debug::log_get_level() as i32
}

/// Installs, replaces, or clears (`sink` = null) the process-wide log
/// sink. `drop_cb`, if non-null, runs with `userdata` when the installed
/// sink is replaced or cleared. `userdata` and `drop_cb` are ignored when
/// `sink` is null. Callbacks are `extern "C"` and must not panic.
///
/// # Safety
///
/// Everything the callbacks access through `userdata` must stay valid
/// until `drop_cb` runs (or, when `drop_cb` is null, for the life of the
/// process). `message` is only valid during a sink call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_log_set_sink(
	sink: QuercusLogFn,
	userdata: *mut c_void,
	drop_cb: QuercusLogDropFn,
) {
	match sink {
		Some(cb) => log_sink::install(cb, userdata, drop_cb),
		None => log_sink::clear(),
	}
}

/// Writes one UTF-8 message at `level` (0..=4) through the process-wide
/// log. Out-of-range levels print with the `UNKNOWN` tag on stderr;
/// a null pointer or invalid UTF-8 is ignored.
///
/// # Safety
///
/// `msg` must point to `len` readable bytes (or be null, which is a
/// no-op).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_log_write(level: i32, msg: *const u8, len: usize) {
	let Some(message) = (unsafe { str_arg(msg, len) }) else {
		return;
	};
	let _ = debug::log_raw(level, message);
}

// ---------------------------------------------------------------------------
// Config store
// ---------------------------------------------------------------------------

/// Parses the nullable group argument: null pointer → `None` (no group),
/// invalid UTF-8 → error (the whole call fails).
unsafe fn group_arg<'a>(ptr: *const u8, len: usize) -> Option<Option<&'a str>> {
	if ptr.is_null() {
		return Some(None);
	}
	unsafe { str_arg(ptr, len) }.map(Some)
}

/// Sets a string config value; false on invalid input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_set(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	value: *const u8,
	value_len: usize,
) -> bool {
	let (Some(group), Some(key), Some(value)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
		unsafe { str_arg(value, value_len) },
	) else {
		return false;
	};
	ConfigStore::instance().set(group, key, value);
	true
}

/// Reads a string config value, snprintf-style; -1 when the key does
/// not exist (or the input is invalid).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_get(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return -1;
	};
	match ConfigStore::instance().get(group, key) {
		Ok(value) => copy_str_out(&value, buf, buf_len),
		Err(_) => -1,
	}
}

/// Reads a boolean config value (0/1), `fallback` when unset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_get_bool(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	fallback: i32,
) -> i32 {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return fallback;
	};
	ConfigStore::instance().get_bool(group, key, fallback)
}

/// Writes a boolean config value (0/1); false on invalid input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_set_bool(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	value: i32,
) -> bool {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return false;
	};
	ConfigStore::instance().set_bool(group, key, value);
	true
}

/// Reads an i64 config value, `fallback` when unset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_get_int64(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	fallback: i64,
) -> i64 {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return fallback;
	};
	ConfigStore::instance().get_int64(group, key, fallback)
}

/// Writes an i64 config value; false on invalid input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_set_int64(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	value: i64,
) -> bool {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return false;
	};
	ConfigStore::instance().set_int64(group, key, value);
	true
}

/// Reads an f64 config value, `fallback` when unset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_get_double(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	fallback: f64,
) -> f64 {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return fallback;
	};
	ConfigStore::instance().get_double(group, key, fallback)
}

/// Writes an f64 config value; false on invalid input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_set_double(
	group: *const u8,
	group_len: usize,
	key: *const u8,
	key_len: usize,
	value: f64,
) -> bool {
	let (Some(group), Some(key)) = (
		unsafe { group_arg(group, group_len) },
		unsafe { str_arg(key, key_len) },
	) else {
		return false;
	};
	ConfigStore::instance().set_double(group, key, value);
	true
}

/// Loads the config store from its persistence backend (the host config
/// source when one is installed, else `config.toml`/legacy INI); false on
/// load error.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_config_load() -> bool {
	ConfigStore::instance().load().is_ok()
}

/// Saves the config store to its persistence backend (the host config
/// source when one is installed, else `config.toml`); false on save error.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_config_save() -> bool {
	ConfigStore::instance().save().is_ok()
}

// ---------------------------------------------------------------------------
// Config persistence source (host-provided load/save backend)
// ---------------------------------------------------------------------------

pub use crate::ext::config_source::{
	QuercusConfigDropFn, QuercusConfigLoadFn, QuercusConfigSaveFn, QuercusConfigSourceVTable,
};

/// Installs (or replaces) the process-wide host config persistence source.
///
/// Once installed, `quercus_core_config_load`/`quercus_core_config_save`
/// exchange TOML text with the host instead of using
/// `<config_location>/config.toml` (or migrating the legacy `config.ini`).
/// `drop_cb`, if non-null, runs with `userdata` exactly once after the
/// source is replaced or cleared. Returns false for a null vtable, a
/// `struct_size` smaller than the v1 layout, an unsupported
/// `abi_version`, or a null `load`/`save` callback; nothing is installed
/// then.
///
/// # Safety
///
/// `vtable` must point to a readable [`QuercusConfigSourceVTable`] whose
/// declared `struct_size` covers the full v1 layout. `userdata` is opaque
/// to the engine, owned by the caller, and must stay valid until
/// `drop_cb` runs. All callbacks are `extern "C"` and must not unwind.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_config_set_source(
	vt: *const QuercusConfigSourceVTable,
	userdata: *mut c_void,
	drop_cb: Option<QuercusConfigDropFn>,
) -> bool {
	unsafe { config_source::install(vt, userdata, drop_cb) }
}

/// Removes the process-wide host config persistence source, restoring the
/// file behavior. Returns true when one was installed; its drop callback
/// runs exactly once.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_config_clear_source() -> bool {
	config_source::clear()
}

/// Whether a process-wide host config persistence source is installed.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_config_has_source() -> bool {
	configstore::config_source_installed()
}

// ---------------------------------------------------------------------------
// Color pipeline (process-wide)
// ---------------------------------------------------------------------------

/// Sets the pipeline color settings from their persisted setting
/// strings ("acescg"/"srgb_legacy", "srgb"/"displayp3"/"bt2020",
/// transfer names); false on invalid UTF-8 input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_color_set_pipeline_settings(
	working: *const u8,
	working_len: usize,
	gamut: *const u8,
	gamut_len: usize,
	transfer: *const u8,
	transfer_len: usize,
) -> bool {
	let (Some(working), Some(gamut), Some(transfer)) = (
		unsafe { str_arg(working, working_len) },
		unsafe { str_arg(gamut, gamut_len) },
		unsafe { str_arg(transfer, transfer_len) },
	) else {
		return false;
	};
	color::set_pipeline_color_settings(
		WorkingColorSpace::from_setting(working),
		OutputColorSpec::from_settings(gamut, transfer),
	);
	true
}

/// The working color space's setting string, snprintf-style.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_color_get_working_space(buf: *mut u8, buf_len: usize) -> i32 {
	copy_str_out(color::pipeline_working_space().as_setting(), buf, buf_len)
}

/// The output gamut's setting string, snprintf-style.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_color_get_output_gamut(buf: *mut u8, buf_len: usize) -> i32 {
	copy_str_out(color::pipeline_output_spec().gamut.as_setting(), buf, buf_len)
}

/// The output transfer's setting string, snprintf-style.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_color_get_output_transfer(buf: *mut u8, buf_len: usize) -> i32 {
	copy_str_out(color::pipeline_output_spec().transfer.as_setting(), buf, buf_len)
}

/// Loads the default OCIO config (`$OCIO` when set, else the bundled
/// one); false on failure.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_color_set_up_default_config() -> bool {
	color::set_up_default_config().is_ok()
}

/// Loads the default OCIO config from an explicit path (null = the
/// bundled default); false on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_color_set_up_default_config_from(
	path: *const u8,
	path_len: usize,
) -> bool {
	let path = if path.is_null() {
		None
	} else {
		match unsafe { str_arg(path, path_len) } {
			Some(p) => Some(p),
			None => return false,
		}
	};
	color::set_up_default_config_from(path).is_ok()
}

/// The active OCIO config path, snprintf-style; -1 when none is loaded.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_color_config_path(buf: *mut u8, buf_len: usize) -> i32 {
	match color::config_path() {
		Some(path) => copy_str_out(&path, buf, buf_len),
		None => -1,
	}
}

// ---------------------------------------------------------------------------
// Display ICC (system queries)
// ---------------------------------------------------------------------------

/// Whether Windows ACM (Auto Color Management) is active.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_displayicc_windows_acm_active() -> bool {
	displayicc::windows_acm_active()
}

/// The system display ICC profile path, snprintf-style; -1 when
/// unavailable (headless, unsupported platform).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_displayicc_system_display_icc(buf: *mut u8, buf_len: usize) -> i32 {
	match displayicc::system_display_icc() {
		Some(path) => copy_str_out(&path, buf, buf_len),
		None => -1,
	}
}

/// The display ICC profile path for the monitor identified by
/// `fingerprint`, snprintf-style; -1 when unknown/unavailable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_displayicc_system_display_icc_for_monitor(
	fingerprint: *const u8,
	fingerprint_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(fingerprint) = (unsafe { str_arg(fingerprint, fingerprint_len) }) else {
		return -1;
	};
	let path = displayicc::monitor_ref_from_fingerprint(fingerprint)
		.and_then(|monitor| displayicc::system_display_icc_for(&monitor));
	match path {
		Some(path) => copy_str_out(&path, buf, buf_len),
		None => -1,
	}
}

/// The monitor fingerprint for a Windows HMONITOR (cast to u64),
/// snprintf-style; -1 when unavailable.
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_displayicc_windows_monitor_fingerprint(
	hmonitor: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	match displayicc::windows_monitor_fingerprint(hmonitor) {
		Some(fp) => copy_str_out(&fp, buf, buf_len),
		None => -1,
	}
}

/// The fingerprint of the X11 monitor covering (`x`, `y`) (global
/// coordinates), snprintf-style; -1 when unavailable.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_displayicc_x11_monitor_fingerprint_at(
	x: f64,
	y: f64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	match displayicc::x11_monitor_fingerprint_at(x, y) {
		Some(fp) => copy_str_out(&fp, buf, buf_len),
		None => -1,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ext::config_source::CONFIG_SOURCE_ABI_VERSION;
	use std::sync::Mutex;

	/// Serializes the log-sink test against itself (the sink is global).
	static LOG_TEST_LOCK: Mutex<()> = Mutex::new(());
	static LOG_CAPTURED: Mutex<Vec<(i32, String)>> = Mutex::new(Vec::new());

	/// Serializes tests that reset or replace the process-wide config store
	/// or its host source.
	static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());
	/// TOML text the test config source loads from / saves to.
	static CONFIG_SOURCE_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());
	/// Number of times the test config source's drop callback ran.
	static CONFIG_SOURCE_DROPS: std::sync::atomic::AtomicUsize =
		std::sync::atomic::AtomicUsize::new(0);

	unsafe extern "C" fn test_config_source_load(
		_userdata: *mut c_void,
		buf: *mut u8,
		cap: usize,
	) -> i32 {
		let data = CONFIG_SOURCE_BUF.lock().unwrap_or_else(|e| e.into_inner());
		if !buf.is_null() && cap > 0 {
			let n = data.len().min(cap - 1);
			unsafe {
				std::ptr::copy_nonoverlapping(data.as_ptr(), buf, n);
				*buf.add(n) = 0;
			}
		}
		i32::try_from(data.len()).unwrap_or(-1)
	}

	unsafe extern "C" fn test_config_source_save(
		_userdata: *mut c_void,
		data: *const u8,
		len: usize,
	) -> bool {
		if len > 0 && data.is_null() {
			return false;
		}
		let bytes = if len == 0 {
			&[][..]
		} else {
			// SAFETY: the engine passes `len` readable bytes.
			unsafe { std::slice::from_raw_parts(data, len) }
		};
		*CONFIG_SOURCE_BUF.lock().unwrap_or_else(|e| e.into_inner()) = bytes.to_vec();
		true
	}

	unsafe extern "C" fn test_config_source_drop(_userdata: *mut c_void) {
		CONFIG_SOURCE_DROPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
	}

	/// A valid v1 vtable pointing at the in-memory test source.
	fn test_config_source_vtable() -> QuercusConfigSourceVTable {
		QuercusConfigSourceVTable {
			struct_size: std::mem::size_of::<QuercusConfigSourceVTable>() as u32,
			abi_version: CONFIG_SOURCE_ABI_VERSION,
			load: Some(test_config_source_load),
			save: Some(test_config_source_save),
		}
	}

	unsafe extern "C" fn capture_sink(
		level: i32,
		message: *const u8,
		len: usize,
		_userdata: *mut c_void,
	) {
		let bytes = unsafe { std::slice::from_raw_parts(message, len) };
		let message = std::str::from_utf8(bytes)
			.unwrap_or("<invalid utf-8>")
			.to_string();
		LOG_CAPTURED
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push((level, message));
	}

	fn cstr_arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	unsafe fn read_out(buf: &[u8], n: i32) -> &str {
		std::str::from_utf8(&buf[..n as usize]).unwrap()
	}

	/// Reads one snprintf-style setting string into an owned String.
	fn read_setting(f: impl Fn(*mut u8, usize) -> i32) -> String {
		let mut buf = [0u8; 64];
		let n = f(buf.as_mut_ptr(), buf.len());
		assert!(n >= 0);
		std::str::from_utf8(&buf[..n as usize]).unwrap().to_string()
	}

	#[test]
	fn log_level_and_sink_roundtrip() {
		let _guard = LOG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let saved_level = quercus_core_log_get_level();

		// Out-of-range levels are rejected and leave the level unchanged.
		assert!(!quercus_core_log_set_level(-1));
		assert!(!quercus_core_log_set_level(5));
		assert_eq!(quercus_core_log_get_level(), saved_level);
		assert!(quercus_core_log_set_level(3));
		assert_eq!(quercus_core_log_get_level(), 3);

		LOG_CAPTURED
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
		unsafe {
			quercus_core_log_set_sink(Some(capture_sink), std::ptr::null_mut(), None);
		}
		assert!(debug::log_is_sink_installed());

		let message = "corestate sink test";
		unsafe {
			quercus_core_log_write(3, message.as_ptr(), message.len());
			// Out-of-range writes take the UNKNOWN stderr path, not the sink.
			quercus_core_log_write(5, message.as_ptr(), message.len());
		}
		let captured = LOG_CAPTURED
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone();
		assert!(
			captured
				.iter()
				.any(|(level, text)| *level == 3 && text == "[ERROR] corestate sink test"),
			"captured: {captured:?}"
		);
		assert!(!captured.iter().any(|(level, _)| *level == 5));

		unsafe {
			quercus_core_log_set_sink(None, std::ptr::null_mut(), None);
		}
		assert!(!debug::log_is_sink_installed());
		assert!(quercus_core_log_set_level(saved_level));
		assert_eq!(quercus_core_log_get_level(), saved_level);
	}

	#[test]
	fn config_string_and_typed_roundtrip() {
		let _guard = CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let unique = format!("QuercusEngineTest/{}", std::process::id());
		let (g, gl) = cstr_arg(&unique);

		unsafe {
			assert!(quercus_core_config_set(g, gl, cstr_arg("greeting").0, 8, cstr_arg("hello").0, 5));
			let mut buf = [0u8; 32];
			let n = quercus_core_config_get(g, gl, cstr_arg("greeting").0, 8, buf.as_mut_ptr(), buf.len());
			assert_eq!(read_out(&buf, n), "hello");

			assert!(quercus_core_config_set_bool(g, gl, cstr_arg("flag").0, 4, 1));
			assert_eq!(quercus_core_config_get_bool(g, gl, cstr_arg("flag").0, 4, 0), 1);
			// Unset key: fallback comes back.
			assert_eq!(quercus_core_config_get_bool(g, gl, cstr_arg("unset").0, 5, 7), 7);

			assert!(quercus_core_config_set_int64(g, gl, cstr_arg("count").0, 5, -42));
			assert_eq!(quercus_core_config_get_int64(g, gl, cstr_arg("count").0, 5, 0), -42);

			assert!(quercus_core_config_set_double(g, gl, cstr_arg("ratio").0, 5, 2.5));
			assert_eq!(quercus_core_config_get_double(g, gl, cstr_arg("ratio").0, 5, 0.0), 2.5);

			// Missing key reads as -1.
			assert_eq!(
				quercus_core_config_get(g, gl, cstr_arg("missing").0, 7, buf.as_mut_ptr(), buf.len()),
				-1
			);
			// Null group = global group.
			assert!(quercus_core_config_set(
				std::ptr::null(),
				0,
				cstr_arg("nogroup").0,
				7,
				cstr_arg("v").0,
				1
			));
			assert!(quercus_core_config_get(
				std::ptr::null(),
				0,
				cstr_arg("nogroup").0,
				7,
				buf.as_mut_ptr(),
				buf.len()
			) >= 0);
		}
	}

	#[test]
	fn config_source_vtable_roundtrip_and_clear() {
		let _guard = CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// Clear the source (and its drop callback) on any exit path.
		struct ClearOnDrop;
		impl Drop for ClearOnDrop {
			fn drop(&mut self) {
				quercus_core_config_clear_source();
			}
		}
		let _cleanup = ClearOnDrop;

		// Invalid vtables are rejected without installing anything.
		unsafe {
			assert!(!quercus_core_config_set_source(
				std::ptr::null(),
				std::ptr::null_mut(),
				None
			));
		}
		let mut vt = test_config_source_vtable();
		vt.struct_size -= 1;
		assert!(!unsafe {
			quercus_core_config_set_source(&vt, std::ptr::null_mut(), None)
		});
		vt = test_config_source_vtable();
		vt.abi_version = CONFIG_SOURCE_ABI_VERSION + 1;
		assert!(!unsafe {
			quercus_core_config_set_source(&vt, std::ptr::null_mut(), None)
		});
		vt = test_config_source_vtable();
		vt.load = None;
		assert!(!unsafe {
			quercus_core_config_set_source(&vt, std::ptr::null_mut(), None)
		});
		vt = test_config_source_vtable();
		vt.save = None;
		assert!(!unsafe {
			quercus_core_config_set_source(&vt, std::ptr::null_mut(), None)
		});
		assert!(!quercus_core_config_has_source());

		// Install a source backed by a static TOML buffer.
		*CONFIG_SOURCE_BUF
			.lock()
			.unwrap_or_else(|e| e.into_inner()) =
			b"DefaultSequenceWidth = 321\nHostOnly = \"yes\"\n".to_vec();
		CONFIG_SOURCE_DROPS.store(0, std::sync::atomic::Ordering::SeqCst);
		let vt = test_config_source_vtable();
		assert!(unsafe {
			quercus_core_config_set_source(
				&vt,
				std::ptr::null_mut(),
				Some(test_config_source_drop),
			)
		});
		assert!(quercus_core_config_has_source());

		// The ABI load reads the host TOML, not a file.
		assert!(quercus_core_config_load());
		let store = ConfigStore::instance();
		assert_eq!(store.get_int(None, "DefaultSequenceWidth", -1), 321);
		assert_eq!(store.get(None, "HostOnly").unwrap(), "yes");

		// The ABI save writes TOML back to the host buffer.
		store.set_int(None, "DefaultSequenceWidth", 777);
		assert!(quercus_core_config_save());
		let saved = CONFIG_SOURCE_BUF
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone();
		let text = String::from_utf8(saved).unwrap();
		assert!(
			text.contains("DefaultSequenceWidth = 777"),
			"text: {}",
			text
		);
		assert!(text.contains("HostOnly = \"yes\""), "text: {}", text);

		// Clearing restores the file behavior and drops the source once.
		assert!(quercus_core_config_clear_source());
		assert!(!quercus_core_config_has_source());
		assert_eq!(
			CONFIG_SOURCE_DROPS.load(std::sync::atomic::Ordering::SeqCst),
			1
		);
		assert!(!quercus_core_config_clear_source());
		assert_eq!(
			CONFIG_SOURCE_DROPS.load(std::sync::atomic::Ordering::SeqCst),
			1
		);
	}

	#[test]
	fn color_settings_roundtrip_and_restore() {
		let saved_working = read_setting(|b, l| quercus_core_color_get_working_space(b, l));
		let saved_gamut = read_setting(|b, l| quercus_core_color_get_output_gamut(b, l));
		let saved_transfer = read_setting(|b, l| quercus_core_color_get_output_transfer(b, l));

		unsafe {
			assert!(quercus_core_color_set_pipeline_settings(
				cstr_arg("srgb_legacy").0,
				11,
				cstr_arg("displayp3").0,
				9,
				cstr_arg("srgb").0,
				4,
			));
		}
		assert_eq!(read_setting(|b, l| quercus_core_color_get_working_space(b, l)), "srgb_legacy");
		assert_eq!(read_setting(|b, l| quercus_core_color_get_output_gamut(b, l)), "displayp3");

		// Restore so later tests in this process see the defaults.
		unsafe {
			assert!(quercus_core_color_set_pipeline_settings(
				saved_working.as_ptr(),
				saved_working.len(),
				saved_gamut.as_ptr(),
				saved_gamut.len(),
				saved_transfer.as_ptr(),
				saved_transfer.len(),
			));
		}
	}

	#[test]
	fn color_default_config_loads() {
		// The bundled OCIO config must load headless.
		assert!(quercus_core_color_set_up_default_config());
		assert!(unsafe {
			quercus_core_color_set_up_default_config_from(std::ptr::null(), 0)
		});
		// config_path may legitimately be None for a builtin config.
		let mut buf = [0u8; 1024];
		let _ = quercus_core_color_config_path(buf.as_mut_ptr(), buf.len());
	}

	#[test]
	fn displayicc_queries_never_crash() {
		let mut buf = [0u8; 1024];
		// Headless CI may have no display: -1 is a valid answer.
		let _ = quercus_core_displayicc_system_display_icc(buf.as_mut_ptr(), buf.len());
		#[cfg(target_os = "windows")]
		{
			let _ = quercus_core_displayicc_windows_acm_active();
			let _ = quercus_core_displayicc_windows_monitor_fingerprint(0, buf.as_mut_ptr(), buf.len());
		}
		#[cfg(target_os = "linux")]
		let _ = quercus_core_displayicc_x11_monitor_fingerprint_at(0.0, 0.0, buf.as_mut_ptr(), buf.len());
		assert_eq!(
			unsafe {
				quercus_core_displayicc_system_display_icc_for_monitor(
					cstr_arg("no-such-monitor").0,
					15,
					buf.as_mut_ptr(),
					buf.len(),
				)
			},
			-1
		);
	}
}

// ---------------------------------------------------------------------------
// CancelAtom (cooperative cancellation token)
// ---------------------------------------------------------------------------

use quercus_core::cancelatom::CancelAtom;

use crate::handle::into_ffi;

crate::export_handle!(
	CancelAtom,
	quercus_core_cancelatom_add_ref,
	quercus_core_cancelatom_release
);

/// A not-cancelled cancellation token (ref count 1).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_cancelatom_new() -> *mut CancelAtom {
	into_ffi(CancelAtom::new()) as *mut CancelAtom
}

/// Sets the cancel flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_cancelatom_cancel(this: *const CancelAtom) {
	if this.is_null() {
		return;
	}
	unsafe { &*this }.cancel();
}

/// Reads the cancel flag (reading a set flag records the cancellation
/// as heard).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_cancelatom_is_cancelled(this: *const CancelAtom) -> bool {
	!this.is_null() && unsafe { &*this }.is_cancelled()
}

/// Whether any consumer has observed the cancel flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_cancelatom_heard_cancel(this: *const CancelAtom) -> bool {
	!this.is_null() && unsafe { &*this }.heard_cancel()
}

#[cfg(test)]
mod cancel_tests {
	#[test]
	fn cancel_lifecycle() {
		unsafe {
			let atom = super::quercus_core_cancelatom_new();
			assert!(!super::quercus_core_cancelatom_is_cancelled(atom));
			assert!(!super::quercus_core_cancelatom_heard_cancel(atom));
			super::quercus_core_cancelatom_cancel(atom);
			assert!(super::quercus_core_cancelatom_is_cancelled(atom));
			assert!(super::quercus_core_cancelatom_heard_cancel(atom));

			// Null is a no-op everywhere.
			super::quercus_core_cancelatom_cancel(std::ptr::null());
			assert!(!super::quercus_core_cancelatom_is_cancelled(std::ptr::null()));
			assert!(!super::quercus_core_cancelatom_heard_cancel(std::ptr::null()));

			super::quercus_core_cancelatom_add_ref(atom);
			super::quercus_core_cancelatom_release(atom);
			super::quercus_core_cancelatom_release(atom);
		}
	}
}
