//! Safe wrapper over the engine's process-wide state C ABI: the config
//! store (`quercus_core_config_*`), the color pipeline settings
//! (`quercus_core_color_*`), display ICC queries (`quercus_core_displayicc_*`),
//! and the log level/sink (`quercus_core_log_*`).
//!
//! These are singletons inside the engine, so — like the audio manager —
//! the SDK exposes plain free functions.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::codec::string_out;

/// Raw sink-callback signature (`repr(C)`-free: plain pointers and ints).
type LogFn =
	unsafe extern "C" fn(level: i32, message: *const u8, len: usize, userdata: *mut c_void);
/// Raw sink-drop callback signature.
type LogDropFn = unsafe extern "C" fn(userdata: *mut c_void);

/// Raw config-source load callback signature: snprintf-style, returning
/// the full TOML byte length excluding the NUL.
pub type QuercusConfigLoadFn =
	unsafe extern "C" fn(userdata: *mut c_void, buf: *mut u8, cap: usize) -> i32;
/// Raw config-source save callback signature (borrowed UTF-8 TOML).
pub type QuercusConfigSaveFn =
	unsafe extern "C" fn(userdata: *mut c_void, data: *const u8, len: usize) -> bool;
/// Raw config-source cleanup callback signature.
pub type QuercusConfigDropFn = unsafe extern "C" fn(userdata: *mut c_void);

/// `repr(C)` mirror of the engine's `QuercusConfigSourceVTable`. The field
/// order is the ABI and must stay byte-identical with the engine struct.
#[repr(C)]
#[allow(dead_code)] // Fields are read by the engine through the raw pointer.
struct QuercusConfigSourceVTable {
	struct_size: u32,
	abi_version: u32,
	// ABI: order matters — byte-identical with the engine struct.
	// The header generator parses these types textually, so keep each
	// `fn(...)` on one line.
	load: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> i32>,
	save: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize) -> bool>,
}

/// The only supported config-source vtable ABI version.
const CONFIG_SOURCE_ABI_VERSION: u32 = 1;

#[cfg_attr(windows, link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim"))]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_core_config_set(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		value: *const u8,
		value_len: usize,
	) -> bool;
	fn quercus_core_config_get(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_core_config_get_bool(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		fallback: i32,
	) -> i32;
	fn quercus_core_config_set_bool(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		value: i32,
	) -> bool;
	fn quercus_core_config_get_int64(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		fallback: i64,
	) -> i64;
	fn quercus_core_config_set_int64(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		value: i64,
	) -> bool;
	fn quercus_core_config_get_double(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		fallback: f64,
	) -> f64;
	fn quercus_core_config_set_double(
		group: *const u8,
		group_len: usize,
		key: *const u8,
		key_len: usize,
		value: f64,
	) -> bool;
	fn quercus_core_config_load() -> bool;
	fn quercus_core_config_save() -> bool;
	fn quercus_core_config_set_source(
		vt: *const QuercusConfigSourceVTable,
		userdata: *mut c_void,
		drop_cb: Option<QuercusConfigDropFn>,
	) -> bool;
	fn quercus_core_config_clear_source() -> bool;
	fn quercus_core_config_has_source() -> bool;

	fn quercus_core_log_set_level(level: i32) -> bool;
	fn quercus_core_log_get_level() -> i32;
	fn quercus_core_log_set_sink(
		sink: Option<LogFn>,
		userdata: *mut c_void,
		drop_cb: Option<LogDropFn>,
	);
	fn quercus_core_log_write(level: i32, msg: *const u8, len: usize);

	fn quercus_core_cancelatom_new() -> *mut std::ffi::c_void;
	fn quercus_core_cancelatom_cancel(this: *const std::ffi::c_void);
	fn quercus_core_cancelatom_is_cancelled(this: *const std::ffi::c_void) -> bool;
	fn quercus_core_cancelatom_heard_cancel(this: *const std::ffi::c_void) -> bool;
	fn quercus_core_cancelatom_add_ref(this: *const std::ffi::c_void);
	fn quercus_core_cancelatom_release(this: *const std::ffi::c_void);

	fn quercus_core_color_set_pipeline_settings(
		working: *const u8,
		working_len: usize,
		gamut: *const u8,
		gamut_len: usize,
		transfer: *const u8,
		transfer_len: usize,
	) -> bool;
	fn quercus_core_color_get_working_space(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_color_get_output_gamut(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_color_get_output_transfer(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_color_set_up_default_config() -> bool;
	fn quercus_core_color_set_up_default_config_from(path: *const u8, path_len: usize) -> bool;
	fn quercus_core_color_config_path(buf: *mut u8, buf_len: usize) -> i32;

	fn quercus_core_displayicc_system_display_icc(buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_displayicc_system_display_icc_for_monitor(
		fingerprint: *const u8,
		fingerprint_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	#[cfg(target_os = "windows")]
	fn quercus_core_displayicc_windows_acm_active() -> bool;
	#[cfg(target_os = "windows")]
	fn quercus_core_displayicc_windows_monitor_fingerprint(
		hmonitor: u64,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	#[cfg(target_os = "linux")]
	fn quercus_core_displayicc_x11_monitor_fingerprint_at(x: f64, y: f64, buf: *mut u8, buf_len: usize) -> i32;
}

/// A failed engine state call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the engine state call failed")]
pub struct Error;

/// Idiomatic access to the process-wide engine configuration.
///
/// Keys are the same full paths used by the native store (`"Storage/Backend"`,
/// `"UseProxyMedia"`, …), so callers do not need to split a group or think
/// about the C ABI's optional-group representation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Config;

impl Config {
	/// The process-wide configuration stored inside the engine.
	pub fn global() -> Self {
		Self
	}

	/// Loads persisted settings.
	pub fn load(self) -> Result<(), Error> {
		config_load()
	}

	/// Persists current settings.
	pub fn save(self) -> Result<(), Error> {
		config_save()
	}

	/// Reads a string or formatted scalar (`None` when absent).
	pub fn get(self, key: &str) -> Option<String> {
		config_get(None, key)
	}

	/// Writes a string or parses it into an existing typed entry.
	pub fn set(self, key: &str, value: &str) -> Result<(), Error> {
		config_set(None, key, value)
	}

	/// The `get_bool` operation.
	pub fn get_bool(self, key: &str, fallback: bool) -> bool {
		config_get_bool(None, key, fallback)
	}

	/// The `set_bool` operation.
	pub fn set_bool(self, key: &str, value: bool) -> Result<(), Error> {
		config_set_bool(None, key, value)
	}

	/// The `get_i64` operation.
	pub fn get_i64(self, key: &str, fallback: i64) -> i64 {
		config_get_i64(None, key, fallback)
	}

	/// The `set_i64` operation.
	pub fn set_i64(self, key: &str, value: i64) -> Result<(), Error> {
		config_set_i64(None, key, value)
	}

	/// The `get_f64` operation.
	pub fn get_f64(self, key: &str, fallback: f64) -> f64 {
		config_get_f64(None, key, fallback)
	}

	/// The `set_f64` operation.
	pub fn set_f64(self, key: &str, value: f64) -> Result<(), Error> {
		config_set_f64(None, key, value)
	}
}

fn arg(s: &str) -> (*const u8, usize) {
	(s.as_ptr(), s.len())
}

fn opt_arg(s: Option<&str>) -> (*const u8, usize) {
	match s {
		Some(s) => (s.as_ptr(), s.len()),
		None => (std::ptr::null(), 0),
	}
}

// ---------------------------------------------------------------------------
// Config store
// ---------------------------------------------------------------------------

/// Sets a string config value (`group` = None for the global group).
pub fn config_set(group: Option<&str>, key: &str, value: &str) -> Result<(), Error> {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	let (v, vl) = arg(value);
	if unsafe { quercus_core_config_set(g, gl, k, kl, v, vl) } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// Reads a string config value; `None` when the key does not exist.
pub fn config_get(group: Option<&str>, key: &str) -> Option<String> {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	string_out(|buf, len| unsafe { quercus_core_config_get(g, gl, k, kl, buf, len) })
}

/// Reads a boolean config value, `fallback` when unset (0/1 over the
/// ABI, like the C++ store).
pub fn config_get_bool(group: Option<&str>, key: &str, fallback: bool) -> bool {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	unsafe { quercus_core_config_get_bool(g, gl, k, kl, fallback as i32) != 0 }
}

/// The `config_set_bool` operation.
pub fn config_set_bool(group: Option<&str>, key: &str, value: bool) -> Result<(), Error> {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	if unsafe { quercus_core_config_set_bool(g, gl, k, kl, value as i32) } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// Reads an i64 config value, `fallback` when unset.
pub fn config_get_i64(group: Option<&str>, key: &str, fallback: i64) -> i64 {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	unsafe { quercus_core_config_get_int64(g, gl, k, kl, fallback) }
}

/// The `config_set_i64` operation.
pub fn config_set_i64(group: Option<&str>, key: &str, value: i64) -> Result<(), Error> {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	if unsafe { quercus_core_config_set_int64(g, gl, k, kl, value) } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// Reads an f64 config value, `fallback` when unset.
pub fn config_get_f64(group: Option<&str>, key: &str, fallback: f64) -> f64 {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	unsafe { quercus_core_config_get_double(g, gl, k, kl, fallback) }
}

/// The `config_set_f64` operation.
pub fn config_set_f64(group: Option<&str>, key: &str, value: f64) -> Result<(), Error> {
	let (g, gl) = opt_arg(group);
	let (k, kl) = arg(key);
	if unsafe { quercus_core_config_set_double(g, gl, k, kl, value) } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// Loads the config store from its persistence backend (the installed
/// [`ConfigSource`] when there is one, else `config.toml`).
pub fn config_load() -> Result<(), Error> {
	if unsafe { quercus_core_config_load() } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// Saves the config store to its persistence backend (the installed
/// [`ConfigSource`] when there is one, else `config.toml`).
pub fn config_save() -> Result<(), Error> {
	if unsafe { quercus_core_config_save() } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// A host-supplied config persistence backend.
///
/// Install one with [`set_config_source`]; the engine then calls [`load`]
/// whenever settings are loaded and [`save`] whenever they are persisted,
/// instead of touching `<config_location>/config.toml` (and the legacy INI
/// migration is skipped). With no source installed the file behavior is
/// unchanged.
///
/// [`load`]: ConfigSource::load
/// [`save`]: ConfigSource::save
pub trait ConfigSource: Send + 'static {
	/// The full TOML text to load, or `None` on read failure. An invalid
	/// TOML document is reported by the engine as a load error.
	fn load(&self) -> Option<String>;

	/// Persist `toml`; false when the host rejects or fails the write.
	fn save(&self, toml: &str) -> bool;
}

/// Heap-owned source userdata: the source behind a mutex (the trait only
/// requires `Send`, so this also serializes concurrent engine calls), plus
/// the liveness flag shared with the registration guard.
struct SourceBox {
	source: Mutex<Box<dyn ConfigSource>>,
	/// False once the engine dropped this source (replaced or cleared).
	active: Arc<AtomicBool>,
}

/// C trampoline for `load`: snprintf-style, panic-safe.
unsafe extern "C" fn config_source_load(userdata: *mut c_void, buf: *mut u8, cap: usize) -> i32 {
	if userdata.is_null() {
		return -1;
	}
	// SAFETY: `userdata` is the Box<SourceBox> handed to the engine and
	// stays alive until the paired drop trampoline runs.
	let state = unsafe { &*(userdata as *const SourceBox) };
	// A panic inside the host source must not unwind across the C ABI.
	let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		state
			.source
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.load()
	}));
	let text = match loaded {
		Ok(Some(text)) => text,
		_ => return -1,
	};
	if !buf.is_null() && cap > 0 {
		let n = text.len().min(cap - 1);
		// SAFETY: `buf` is writable for `cap` bytes; `n < cap`.
		unsafe {
			std::ptr::copy_nonoverlapping(text.as_ptr(), buf, n);
			*buf.add(n) = 0;
		}
	}
	i32::try_from(text.len()).unwrap_or(-1)
}

/// C trampoline for `save`: borrowed UTF-8 TOML, panic-safe.
unsafe extern "C" fn config_source_save(
	userdata: *mut c_void,
	data: *const u8,
	len: usize,
) -> bool {
	if userdata.is_null() {
		return false;
	}
	if len > 0 && data.is_null() {
		return false;
	}
	let bytes = if len == 0 {
		&[][..]
	} else {
		// SAFETY: the engine passes `len` readable bytes for this call.
		unsafe { std::slice::from_raw_parts(data, len) }
	};
	let Ok(toml) = std::str::from_utf8(bytes) else {
		return false;
	};
	// SAFETY: see `config_source_load`.
	let state = unsafe { &*(userdata as *const SourceBox) };
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		state
			.source
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.save(toml)
	}))
	.unwrap_or(false)
}

/// C trampoline for `drop`: marks the guard inactive and reclaims the box.
unsafe extern "C" fn config_source_drop(userdata: *mut c_void) {
	if userdata.is_null() {
		return;
	}
	// The source's destructor may panic; keep any unwind inside the
	// trampoline.
	let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		// SAFETY: reclaims exactly the Box<SourceBox> handed to the engine.
		let state = unsafe { Box::from_raw(userdata as *mut SourceBox) };
		state.active.store(false, Ordering::Release);
		drop(state);
	}));
}

/// A live process-wide config source registration.
///
/// Dropping it clears the source (unless it was already replaced by a
/// newer registration), at which point the engine calls the source's drop
/// trampoline and the boxed [`ConfigSource`] is freed. Registration is
/// process-wide: installing another source replaces this one, so guards
/// should be dropped promptly.
#[must_use = "dropping the registration immediately clears the source"]
pub struct ConfigSourceRegistration {
	active: Arc<AtomicBool>,
}

impl Drop for ConfigSourceRegistration {
	fn drop(&mut self) {
		if self.active.swap(false, Ordering::AcqRel) {
			// SAFETY: the engine reclaims the userdata box through the drop
			// trampoline before returning.
			unsafe { quercus_core_config_clear_source() };
		}
	}
}

/// Installs `source` as the process-wide config persistence backend,
/// replacing (and dropping) any previously installed source.
///
/// Once installed, [`config_load`]/[`config_save`] exchange TOML text with
/// the source instead of using `config.toml`. The returned
/// [`ConfigSourceRegistration`] clears the source when dropped. Returns
/// `Err(-1)` when the engine rejects the registration (for example a
/// vtable mismatch); the source is reclaimed in that case.
pub fn set_config_source(source: Box<dyn ConfigSource>) -> Result<ConfigSourceRegistration, i32> {
	const E_INVALID: i32 = -1;
	let active = Arc::new(AtomicBool::new(true));
	let userdata = Box::into_raw(Box::new(SourceBox {
		source: Mutex::new(source),
		active: active.clone(),
	}));
	let vt = QuercusConfigSourceVTable {
		struct_size: std::mem::size_of::<QuercusConfigSourceVTable>() as u32,
		abi_version: CONFIG_SOURCE_ABI_VERSION,
		load: Some(config_source_load),
		save: Some(config_source_save),
	};
	// SAFETY: the vtable and userdata are live for the call; on success the
	// engine owns the userdata until the drop trampoline runs.
	let ok = unsafe {
		quercus_core_config_set_source(&vt, userdata as *mut c_void, Some(config_source_drop))
	};
	if !ok {
		// The engine never took ownership of the userdata on failure.
		active.store(false, Ordering::Release);
		// SAFETY: `userdata` is the box created above and not registered.
		unsafe { drop(Box::from_raw(userdata)) };
		return Err(E_INVALID);
	}
	Ok(ConfigSourceRegistration { active })
}

/// Whether a process-wide config persistence source is installed.
pub fn config_source_installed() -> bool {
	unsafe { quercus_core_config_has_source() }
}

// ---------------------------------------------------------------------------
// Logging (process-wide)
// ---------------------------------------------------------------------------

/// Sets the engine's minimum log level (0 = DEBUG, 1 = INFO, 2 = WARNING,
/// 3 = ERROR, 4 = FATAL); `false` when the code is out of range.
pub fn log_set_level(level: i32) -> bool {
	unsafe { quercus_core_log_set_level(level) }
}

/// The engine's current minimum log level (0 = DEBUG … 4 = FATAL).
pub fn log_get_level() -> i32 {
	unsafe { quercus_core_log_get_level() }
}

/// Writes `message` through the engine's process-wide log at `level`
/// (0 = DEBUG … 4 = FATAL). Out-of-range levels are printed with the
/// engine's `UNKNOWN` tag.
pub fn log_write(level: i32, message: &str) {
	let (msg, len) = arg(message);
	unsafe { quercus_core_log_write(level, msg, len) };
}

/// Installs (or with `None`, clears) the engine's process-wide log sink:
/// `sink(level, message)` receives lines already formatted as
/// `"[LEVEL] message"`. Replacing or clearing a sink drops the previous
/// closure. Returns `false` if the engine rejected the registration
/// (not expected).
///
/// The closure is called on whichever thread logs (and from more than one
/// thread at a time is possible), so it must be `Send` and safe to call
/// concurrently. A panic inside it is caught and does not cross the C ABI.
pub fn set_log_sink<F>(sink: Option<F>) -> bool
where
	F: Fn(i32, &str) + Send + 'static,
{
	unsafe extern "C" fn trampoline<F>(
		level: i32,
		message: *const u8,
		len: usize,
		userdata: *mut c_void,
	) where
		F: Fn(i32, &str) + Send + 'static,
	{
		if userdata.is_null() {
			return;
		}
		let message = if message.is_null() {
			""
		} else {
			std::str::from_utf8(unsafe { std::slice::from_raw_parts(message, len) }).unwrap_or("")
		};
		// SAFETY: `userdata` is the Box<F> handed to the engine below and
		// stays alive until the paired drop trampoline runs.
		let callback = unsafe { &*(userdata as *const F) };
		// A panic must not unwind across the C ABI.
		let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			callback(level, message)
		}));
	}

	unsafe extern "C" fn drop_trampoline<F>(userdata: *mut c_void)
	where
		F: Fn(i32, &str) + Send + 'static,
	{
		if userdata.is_null() {
			return;
		}
		// SAFETY: reclaims exactly the Box<F> that set_log_sink leaked.
		drop(unsafe { Box::from_raw(userdata as *mut F) });
	}

	match sink {
		Some(sink) => {
			let userdata = Box::into_raw(Box::new(sink)) as *mut c_void;
			// SAFETY: the pointer and drop trampoline pair with F; the
			// engine calls drop before replacing or clearing this sink.
			unsafe {
				quercus_core_log_set_sink(
					Some(trampoline::<F>),
					userdata,
					Some(drop_trampoline::<F>),
				)
			};
			true
		}
		None => {
			unsafe { quercus_core_log_set_sink(None, std::ptr::null_mut(), None) };
			true
		}
	}
}

// ---------------------------------------------------------------------------
// Color pipeline (process-wide)
// ---------------------------------------------------------------------------

/// Sets the pipeline color settings from their persisted setting
/// strings ("acescg"/"srgb_legacy", "srgb"/"displayp3"/"bt2020", and the
/// transfer names).
pub fn color_set_pipeline_settings(
	working: &str,
	gamut: &str,
	transfer: &str,
) -> Result<(), Error> {
	let (w, wl) = arg(working);
	let (g, gl) = arg(gamut);
	let (t, tl) = arg(transfer);
	if unsafe { quercus_core_color_set_pipeline_settings(w, wl, g, gl, t, tl) } {
		Ok(())
	} else {
		Err(Error)
	}
}

/// The working color space's setting string.
pub fn color_working_space() -> String {
	string_out(|buf, len| unsafe { quercus_core_color_get_working_space(buf, len) }).unwrap_or_default()
}

/// The output gamut's setting string.
pub fn color_output_gamut() -> String {
	string_out(|buf, len| unsafe { quercus_core_color_get_output_gamut(buf, len) }).unwrap_or_default()
}

/// The output transfer's setting string.
pub fn color_output_transfer() -> String {
	string_out(|buf, len| unsafe { quercus_core_color_get_output_transfer(buf, len) })
		.unwrap_or_default()
}

/// Loads the default OCIO config: `Some(path)` for an explicit file,
/// `None` for `$OCIO`/the bundled default.
pub fn color_set_up_default_config(path: Option<&str>) -> Result<(), Error> {
	let ok = match path {
		Some(path) => {
			let (p, pl) = arg(path);
			unsafe { quercus_core_color_set_up_default_config_from(p, pl) }
		}
		None => unsafe { quercus_core_color_set_up_default_config() },
	};
	if ok { Ok(()) } else { Err(Error) }
}

/// The active OCIO config path; `None` when none is loaded (or the
/// bundled config is in use).
pub fn color_config_path() -> Option<String> {
	string_out(|buf, len| unsafe { quercus_core_color_config_path(buf, len) })
}

// ---------------------------------------------------------------------------
// Display ICC (system queries)
// ---------------------------------------------------------------------------

/// The system display ICC profile path; `None` when unavailable
/// (headless, unsupported platform).
pub fn displayicc_system_display_icc() -> Option<String> {
	string_out(|buf, len| unsafe { quercus_core_displayicc_system_display_icc(buf, len) })
}

/// The display ICC profile path for the monitor identified by
/// `fingerprint`; `None` when unknown/unavailable.
pub fn displayicc_system_display_icc_for(fingerprint: &str) -> Option<String> {
	let (f, fl) = arg(fingerprint);
	string_out(|buf, len| unsafe {
		quercus_core_displayicc_system_display_icc_for_monitor(f, fl, buf, len)
	})
}

/// Whether Windows ACM (Auto Color Management) is active.
#[cfg(target_os = "windows")]
pub fn displayicc_windows_acm_active() -> bool {
	unsafe { quercus_core_displayicc_windows_acm_active() }
}

/// The monitor fingerprint for a Windows HMONITOR (cast to u64).
#[cfg(target_os = "windows")]
pub fn displayicc_windows_monitor_fingerprint(hmonitor: u64) -> Option<String> {
	string_out(|buf, len| unsafe {
		quercus_core_displayicc_windows_monitor_fingerprint(hmonitor, buf, len)
	})
}

/// The fingerprint of the X11 monitor covering (`x`, `y`) in global
/// coordinates.
#[cfg(target_os = "linux")]
pub fn displayicc_x11_monitor_fingerprint_at(x: f64, y: f64) -> Option<String> {
	string_out(|buf, len| unsafe {
		quercus_core_displayicc_x11_monitor_fingerprint_at(x, y, buf, len)
	})
}

// ---------------------------------------------------------------------------
// CancelAtom (cooperative cancellation token)
// ---------------------------------------------------------------------------

/// A cooperative cancellation token shared with the engine (e.g. a
/// probe). `Clone` adds a reference, `Drop` releases; cancelling one
/// clone cancels them all.
pub struct CancelAtom {
	ptr: *mut std::ffi::c_void,
}

unsafe impl Send for CancelAtom {}
unsafe impl Sync for CancelAtom {}

impl CancelAtom {
	/// A not-cancelled token.
	pub fn new() -> Self {
		Self {
			ptr: unsafe { quercus_core_cancelatom_new() },
		}
	}

	/// Sets the cancel flag.
	pub fn cancel(&self) {
		unsafe { quercus_core_cancelatom_cancel(self.ptr) };
	}

	/// Reads the cancel flag (reading a set flag records the
	/// cancellation as heard).
	pub fn is_cancelled(&self) -> bool {
		unsafe { quercus_core_cancelatom_is_cancelled(self.ptr) }
	}

	/// Whether any consumer has observed the cancel flag.
	pub fn heard_cancel(&self) -> bool {
		unsafe { quercus_core_cancelatom_heard_cancel(self.ptr) }
	}
}

impl Default for CancelAtom {
	fn default() -> Self {
		Self::new()
	}
}

impl Clone for CancelAtom {
	fn clone(&self) -> Self {
		unsafe { quercus_core_cancelatom_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for CancelAtom {
	fn drop(&mut self) {
		unsafe { quercus_core_cancelatom_release(self.ptr) };
	}
}

/// The raw handle, for APIs taking a token (e.g.
/// [`crate::codec::decoder_probe`]).
pub(crate) fn atom_ptr(atom: Option<&CancelAtom>) -> *const std::ffi::c_void {
	atom.map_or(std::ptr::null(), |a| a.ptr)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn idiomatic_config_uses_full_keys_and_native_rust_types() {
		let config = Config::global();
		config.set("QuercusSdkConfigObject", "value").expect("set");
		assert_eq!(config.get("QuercusSdkConfigObject").as_deref(), Some("value"));
		config.set_bool("QuercusSdkConfigBool", true).expect("set bool");
		assert!(config.get_bool("QuercusSdkConfigBool", false));
		config.set_i64("QuercusSdkConfigInt", 42).expect("set int");
		assert_eq!(config.get_i64("QuercusSdkConfigInt", 0), 42);
		config.set_f64("QuercusSdkConfigFloat", 1.5).expect("set float");
		assert_eq!(config.get_f64("QuercusSdkConfigFloat", 0.0), 1.5);
		assert_eq!(config.get("QuercusSdkConfigMissing"), None);
	}

	#[test]
	fn config_roundtrip_and_fallbacks() {
		let group = Some("QuercusSdkTest");
		let key = "greeting";
		config_set(group, key, "hello").expect("set");
		assert_eq!(config_get(group, key).as_deref(), Some("hello"));
		assert_eq!(config_get(group, "missing"), None);

		config_set_bool(group, "flag", true).expect("set_bool");
		assert!(config_get_bool(group, "flag", false));
		assert!(config_get_bool(group, "unset", true), "fallback on missing key");

		config_set_i64(group, "count", -42).expect("set_i64");
		assert_eq!(config_get_i64(group, "count", 0), -42);

		config_set_f64(group, "ratio", 2.5).expect("set_f64");
		assert_eq!(config_get_f64(group, "ratio", 0.0), 2.5);

		// The global (group-less) lane works too.
		config_set(None, "nogroup", "v").expect("set global");
		assert_eq!(config_get(None, "nogroup").as_deref(), Some("v"));
	}

	#[test]
	fn color_settings_roundtrip_and_restore() {
		let saved = (
			color_working_space(),
			color_output_gamut(),
			color_output_transfer(),
		);

		color_set_pipeline_settings("srgb_legacy", "displayp3", "srgb").expect("set");
		assert_eq!(color_working_space(), "srgb_legacy");
		assert_eq!(color_output_gamut(), "displayp3");

		color_set_pipeline_settings(&saved.0, &saved.1, &saved.2).expect("restore");
	}

	#[test]
	fn color_default_config_loads_headless() {
		color_set_up_default_config(None).expect("bundled config");
		let _ = color_config_path();
	}

	#[test]
	fn cancel_atom_lifecycle() {
		let atom = CancelAtom::new();
		assert!(!atom.is_cancelled());
		assert!(!atom.heard_cancel());
		atom.cancel();
		assert!(atom.is_cancelled());
		assert!(atom.heard_cancel());

		// Clones share the flag.
		let clone = atom.clone();
		assert!(clone.is_cancelled());
	}

	#[test]
	fn log_sink_receives_messages() {
		use std::sync::{Arc, Mutex};
		let seen: Arc<Mutex<Vec<(i32, String)>>> = Arc::new(Mutex::new(Vec::new()));
		let sink = seen.clone();
		assert!(set_log_sink(Some(move |level: i32, message: &str| {
			sink.lock()
				.unwrap_or_else(|e| e.into_inner())
				.push((level, message.to_string()));
		})));
		log_write(2, "sdk-log-sink-probe");
		// The engine formats messages before the sink sees them.
		let hit = seen
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.any(|(level, message)| *level == 2 && message.contains("sdk-log-sink-probe"));
		assert!(hit, "sink did not receive the warning");
		// Clear: the typed None selects the erased callback type.
		set_log_sink::<fn(i32, &str)>(None);
	}

	#[test]
	fn displayicc_queries_never_crash() {
		let _ = displayicc_system_display_icc();
		assert_eq!(displayicc_system_display_icc_for("no-such-monitor"), None);
		// The platform-gated entries: the SDK declares and the engine
		// exports exactly the same per-platform symbol set, so calling them
		// here keeps the pairing linker-checked on every platform CI
		// builds. Headless platforms answer None/false — never crash.
		#[cfg(target_os = "windows")]
		{
			let _ = displayicc_windows_acm_active();
			assert_eq!(displayicc_windows_monitor_fingerprint(0), None);
		}
		#[cfg(target_os = "linux")]
		let _ = displayicc_x11_monitor_fingerprint_at(0.0, 0.0);
	}
}
