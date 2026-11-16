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
static CONFIG_SOURCE_DROPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
		assert!(quercus_core_config_set(
			g,
			gl,
			cstr_arg("greeting").0,
			8,
			cstr_arg("hello").0,
			5
		));
		let mut buf = [0u8; 32];
		let n = quercus_core_config_get(
			g,
			gl,
			cstr_arg("greeting").0,
			8,
			buf.as_mut_ptr(),
			buf.len(),
		);
		assert_eq!(read_out(&buf, n), "hello");

		assert!(quercus_core_config_set_bool(
			g,
			gl,
			cstr_arg("flag").0,
			4,
			1
		));
		assert_eq!(
			quercus_core_config_get_bool(g, gl, cstr_arg("flag").0, 4, 0),
			1
		);
		// Unset key: fallback comes back.
		assert_eq!(
			quercus_core_config_get_bool(g, gl, cstr_arg("unset").0, 5, 7),
			7
		);

		assert!(quercus_core_config_set_int64(
			g,
			gl,
			cstr_arg("count").0,
			5,
			-42
		));
		assert_eq!(
			quercus_core_config_get_int64(g, gl, cstr_arg("count").0, 5, 0),
			-42
		);

		assert!(quercus_core_config_set_double(
			g,
			gl,
			cstr_arg("ratio").0,
			5,
			2.5
		));
		assert_eq!(
			quercus_core_config_get_double(g, gl, cstr_arg("ratio").0, 5, 0.0),
			2.5
		);

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
		assert!(
			quercus_core_config_get(
				std::ptr::null(),
				0,
				cstr_arg("nogroup").0,
				7,
				buf.as_mut_ptr(),
				buf.len()
			) >= 0
		);
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
	assert!(!unsafe { quercus_core_config_set_source(&vt, std::ptr::null_mut(), None) });
	vt = test_config_source_vtable();
	vt.abi_version = CONFIG_SOURCE_ABI_VERSION + 1;
	assert!(!unsafe { quercus_core_config_set_source(&vt, std::ptr::null_mut(), None) });
	vt = test_config_source_vtable();
	vt.load = None;
	assert!(!unsafe { quercus_core_config_set_source(&vt, std::ptr::null_mut(), None) });
	vt = test_config_source_vtable();
	vt.save = None;
	assert!(!unsafe { quercus_core_config_set_source(&vt, std::ptr::null_mut(), None) });
	assert!(!quercus_core_config_has_source());

	// Install a source backed by a static TOML buffer.
	*CONFIG_SOURCE_BUF.lock().unwrap_or_else(|e| e.into_inner()) =
		b"DefaultSequenceWidth = 321\nHostOnly = \"yes\"\n".to_vec();
	CONFIG_SOURCE_DROPS.store(0, std::sync::atomic::Ordering::SeqCst);
	let vt = test_config_source_vtable();
	assert!(unsafe {
		quercus_core_config_set_source(&vt, std::ptr::null_mut(), Some(test_config_source_drop))
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
	assert_eq!(
		read_setting(|b, l| quercus_core_color_get_working_space(b, l)),
		"srgb_legacy"
	);
	assert_eq!(
		read_setting(|b, l| quercus_core_color_get_output_gamut(b, l)),
		"displayp3"
	);

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
	assert!(unsafe { quercus_core_color_set_up_default_config_from(std::ptr::null(), 0) });
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
