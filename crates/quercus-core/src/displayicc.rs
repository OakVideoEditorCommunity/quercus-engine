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

//! Platform display ICC profile lookup for color management.
//!
//! Resolves the filesystem path of the main display's ICC profile for the
//! OCIO pipeline (`quercus_render::color::ColorProcessor::create_display_icc`).
//! `system_display_icc()` is the single entry point; it honors the
//! `QUERCUS_DISPLAY_ICC` override first, then asks the platform:
//!
//! * macOS — the CoreGraphics main-display color space; the ICC bytes are
//!   materialized under the disk cache (`<cache>/icc/<hash>.icc`).
//! * Windows — the ICM profile file path (`GetICMProfileW`); Windows
//!   profiles are already files, so no cache copy is needed.
//! * Linux — the `_ICC_PROFILE` X11/XWayland root-window property via
//!   `xprop`, then the `colormgr` CLI chain (colord); bytes from `xprop`
//!   are materialized into the disk cache.
//!
//! Multi-monitor setups: [`system_display_icc_for`] resolves the profile of
//! one specific physical monitor (a [`MonitorRef`]) — macOS keys on the
//! CoreGraphics display ID, Windows on the `\\.\DISPLAYn` device name
//! (`CreateDCW` + `GetICMProfileW`), Linux on the RandR output name
//! (`xrandr --prop`). An unrecognized fingerprint falls back to
//! [`system_display_icc`] (the main display), so callers can simply try the
//! per-monitor path first.
//!
//! Every platform query is best-effort: any failure (headless session,
//! missing tooling, unparseable output) silently degrades to `None`.

// Platform-specific imports live in the cfg-gated modules below; the only
// shared one is `Path`, which `write_icc_cache` (non-Windows) needs.
use crate::filefunctions;
use std::env;
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::Path;

/// The filesystem path of the main display's ICC profile, ready for an
/// OCIO FileTransform. None when the platform gives no answer (headless,
/// no colord, no X server). The `QUERCUS_DISPLAY_ICC` environment variable
/// overrides everything (tests, debugging).
pub fn system_display_icc() -> Option<String> {
	env_override_icc().or_else(platform_display_icc)
}

/// The `QUERCUS_DISPLAY_ICC` override path. The override wins outright — tests
/// and debugging bypass the platform queries entirely. An empty value is
/// treated as unset and falls through to the platform lookup.
fn env_override_icc() -> Option<String> {
	match env::var("QUERCUS_DISPLAY_ICC") {
		Ok(path) if !path.is_empty() => Some(path),
		_ => None,
	}
}

/// Identifies one physical monitor for a per-monitor ICC lookup (see
/// [`system_display_icc_for`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorRef {
	/// macOS: the `CGDirectDisplayID` of the display.
	MacDisplay(u32),
	/// Windows: the monitor device name (the `szDevice` of
	/// `MONITORINFOEXW`, e.g. `\\.\DISPLAY1`).
	WinDevice(String),
	/// Linux/X11: the RandR output name (e.g. `eDP-1`).
	X11Output(String),
}

/// The filesystem path of one specific monitor's ICC profile, ready for an
/// OCIO FileTransform. None when the platform gives no answer for that
/// monitor (unknown fingerprint, headless, no colord, no X server). The
/// `QUERCUS_DISPLAY_ICC` environment variable overrides everything, exactly as
/// in [`system_display_icc`].
///
/// Best-effort like the main-display lookup: an unrecognized or unresolvable
/// [`MonitorRef`] degrades to [`system_display_icc`] (the main display),
/// never to a panic.
pub fn system_display_icc_for(monitor: &MonitorRef) -> Option<String> {
	env_override_icc()
		.or_else(|| platform_display_icc_for(monitor))
		.or_else(platform_display_icc)
}

#[cfg(target_os = "macos")]
fn platform_display_icc_for(monitor: &MonitorRef) -> Option<String> {
	match monitor {
		MonitorRef::MacDisplay(id) => macos::display_icc_for(*id),
		_ => None,
	}
}

#[cfg(target_os = "windows")]
fn platform_display_icc_for(monitor: &MonitorRef) -> Option<String> {
	match monitor {
		MonitorRef::WinDevice(device) => windows::display_icc_for(device),
		_ => None,
	}
}

#[cfg(target_os = "linux")]
fn platform_display_icc_for(monitor: &MonitorRef) -> Option<String> {
	match monitor {
		MonitorRef::X11Output(name) => linux::display_icc_for_output(name),
		_ => None,
	}
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_display_icc_for(_monitor: &MonitorRef) -> Option<String> {
	None
}

/// Parse a monitor fingerprint (the strings
/// [`crate::quercus_app::quercusui::displaycolor`] records, `"mac:<id>"` /
/// `"win:<device>"` / `"x11:<output>"`) back into a [`MonitorRef`]. Any
/// malformed input yields `None`.
pub fn monitor_ref_from_fingerprint(fingerprint: &str) -> Option<MonitorRef> {
	let (kind, value) = fingerprint.split_once(':')?;
	match kind {
		"mac" => {
			let id: u32 = value.parse().ok()?;
			Some(MonitorRef::MacDisplay(id))
		}
		"win" if !value.is_empty() => Some(MonitorRef::WinDevice(value.to_string())),
		"x11" if !value.is_empty() => Some(MonitorRef::X11Output(value.to_string())),
		_ => None,
	}
}

/// The monitor fingerprint (`win:<device>`) for an HMONITOR value — the
/// `display_id` of a gpui window on Windows. None when the device name
/// cannot be resolved (invalid handle, call failure).
#[cfg(target_os = "windows")]
pub fn windows_monitor_fingerprint(hmonitor: u64) -> Option<String> {
	windows::monitor_device_name(hmonitor as usize).map(|name| format!("win:{name}"))
}

/// The monitor fingerprint (`x11:<RandR output>`) for the display covering
/// the given point in device pixels (global X11 screen coordinates). None
/// when no RandR monitor list is available (headless, no `xrandr`) or none
/// covers the point.
#[cfg(target_os = "linux")]
/// The monitor fingerprint (`x11:<RandR output>`) for the display covering a point.
///
/// `x`/`y` are in device pixels (global X11 screen coordinates, possibly negative)
/// and are rounded to integers before the RandR geometry test. Uses a 30 s cached
/// `xrandr --listmonitors` snapshot so a poll loop does not spawn a process every
/// tick. Returns `None` when no RandR monitor list is available (headless, no
/// `xrandr`) or no monitor contains the point.
pub fn x11_monitor_fingerprint_at(x: f64, y: f64) -> Option<String> {
	linux::x11_monitor_fingerprint_at(x, y)
}

#[cfg(target_os = "macos")]
fn platform_display_icc() -> Option<String> {
	macos::display_icc()
}

#[cfg(target_os = "windows")]
fn platform_display_icc() -> Option<String> {
	windows::display_icc()
}

/// True when Windows 11 Auto Color Management is active: the OS maps
/// (sRGB-declared) app output to the display, so the app must not apply
/// the display ICC itself. Windows 10 / older always returns false.
/// `QUERCUS_WINDOWS_COLOR=self|os` overrides the detection (debugging).
#[cfg(target_os = "windows")]
pub fn windows_acm_active() -> bool {
	windows::acm_active()
}

#[cfg(target_os = "linux")]
fn platform_display_icc() -> Option<String> {
	linux::display_icc()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_display_icc() -> Option<String> {
	None
}

/// Materialize `bytes` under `<disk cache>/icc/<fnv1a hex>.icc`.
///
/// The file name is the FNV-1a hash of the content
/// (`filefunctions::fnv1a_hex`), so an entry with identical bytes is reused
/// as-is and a different profile lands in a different file. Returns `None`
/// when the cache directory is not writable. Windows never needs this: ICM
/// profiles are already files on disk.
#[cfg(not(target_os = "windows"))]
fn write_icc_cache(bytes: &[u8]) -> Option<String> {
	if bytes.is_empty() {
		return None;
	}
	let cache_root = filefunctions::default_disk_cache_path();
	let dir = Path::new(&cache_root).join("icc");
	fs::create_dir_all(&dir).ok()?;
	let path = dir.join(format!("{}.icc", filefunctions::fnv1a_hex(bytes)));
	if !path.exists() {
		fs::write(&path, bytes).ok()?;
	}
	Some(path.to_string_lossy().into_owned())
}

/// Parse the `0xHH, 0xHH, ...` byte list from an `xprop` `_ICC_PROFILE` line.
///
/// `xprop -root _ICC_PROFILE` prints the property as
/// `_ICC_PROFILE(8)\t= 0x3c, 0x6f, ...`. Everything before the first `=` is
/// ignored (a suffixed `_ICC_PROFILE_1` name parses identically); after it,
/// each `0x`-prefixed 1-2 digit hex token contributes one byte and any other
/// token is skipped. Returns `None` when no byte can be parsed — the
/// property is absent (xprop prints an error instead), empty, or the value
/// is malformed.
#[cfg(any(target_os = "linux", test))]
fn parse_xprop_icc_hex(output: &str) -> Option<Vec<u8>> {
	let after_eq = output.split('=').nth(1)?.to_ascii_lowercase();
	let mut bytes = Vec::new();
	let mut rest = after_eq.as_str();
	while let Some(pos) = rest.find("0x") {
		let digits_start = pos + 2;
		let mut n_digits = 0;
		for ch in rest[digits_start..].chars() {
			if n_digits == 2 || !ch.is_ascii_hexdigit() {
				break;
			}
			n_digits += 1;
		}
		if n_digits > 0 {
			if let Ok(byte) = u8::from_str_radix(&rest[digits_start..digits_start + n_digits], 16) {
				bytes.push(byte);
			}
		}
		rest = &rest[digits_start + n_digits..];
	}
	if bytes.is_empty() {
		None
	} else {
		Some(bytes)
	}
}

/// One monitor from `xrandr --listmonitors`: geometry in device pixels
/// (global X11 screen coordinates) plus the RandR output names it spans.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RandRMonitor {
	x: i32,
	y: i32,
	width: i32,
	height: i32,
	outputs: Vec<String>,
}

/// Parse `xrandr --listmonitors` output.
///
/// Each monitor line is `  N: [flags]NAME W/MMWxH/MMH+X+Y  OUTPUT...`: the
/// first token is the flags+monitor-name (flags like `+*` mark primary and
/// current), the second the geometry (physical sizes in mm are ignored),
/// the rest the RandR outputs (a spanning mode lists several). X11 screen
/// coordinates may be negative, printed as `-X-Y`. The `Monitors:` header
/// and any malformed line are skipped.
#[cfg(any(target_os = "linux", test))]
fn parse_xrandr_monitors(output: &str) -> Vec<RandRMonitor> {
	/// Parse one `xrandr --listmonitors` geometry token
	/// (`W/MMWxH/MMH[+-]X[+-]Y`) into `(x, y, width, height)` in device pixels.
	///
	/// Physical sizes after `/` are ignored. Negative coordinates are carried by the
	/// leading `+`/`-` signs and restored here. Returns `None` when the token lacks the
	/// `WxH` part, has fewer than three `+`/`-`-separated pieces, or a component is not
	/// a valid `i32`.
	fn parse_geometry(token: &str) -> Option<(i32, i32, i32, i32)> {
		// `W/MMWxH/MMH[+-]X[+-]Y` — `[+-]` splits off the coordinates but
		// drops their signs, which the leading `+`/`-` in the body carries.
		let pieces: Vec<&str> = token.split(['+', '-']).collect();
		if pieces.len() < 3 {
			return None;
		}
		let dims = pieces[0].split_once('x')?;
		let width: i32 = dims.0.split('/').next()?.parse().ok()?;
		let height: i32 = dims.1.split('/').next()?.parse().ok()?;
		let mut x: i32 = pieces[1].trim().parse().ok()?;
		let mut y: i32 = pieces[2].trim().parse().ok()?;
		let signs: Vec<u8> = token[pieces[0].len()..]
			.bytes()
			.filter(|b| matches!(b, b'+' | b'-'))
			.collect();
		if signs.first() == Some(&b'-') {
			x = -x;
		}
		if signs.get(1) == Some(&b'-') {
			y = -y;
		}
		Some((x, y, width, height))
	}

	let mut monitors = Vec::new();
	for line in output.lines() {
		let line = line.trim_start();
		// Header line: `Monitors: 2`.
		if line.starts_with("Monitors") {
			continue;
		}
		// Monitor line; drop the leading index (`N:`).
		let Some((_, rest)) = line.split_once(':') else {
			continue;
		};
		let mut tokens = rest.split_whitespace();
		// First token: `[+*]NAME` (ignored), second: the geometry.
		if tokens.next().is_none() {
			continue;
		}
		let Some(geometry) = tokens.next() else {
			continue;
		};
		let Some((x, y, width, height)) = parse_geometry(geometry) else {
			continue;
		};
		let outputs: Vec<String> = tokens.map(str::to_string).collect();
		if outputs.is_empty() {
			continue;
		}
		monitors.push(RandRMonitor {
			x,
			y,
			width,
			height,
			outputs,
		});
	}
	monitors
}

/// Parse the `_ICC_PROFILE` property of one RandR output from `xrandr
/// --prop` output.
///
/// The output is a sequence of sections, one per output, each starting at
/// an unindented line whose first token is the output name. Properties
/// within a section are indented `KEY(BITS)\t= VALUE` lines; the ICC bytes
/// are the `_ICC_PROFILE` value (`0xHH, 0xHH, ...` — the same byte list
/// `xprop` prints, so [`parse_xprop_icc_hex`] re-parses it). Returns `None`
/// when the output is absent, carries no `_ICC_PROFILE`, or the bytes are
/// malformed.
#[cfg(any(target_os = "linux", test))]
fn parse_output_icc_prop(output: &str, wanted: &str) -> Option<Vec<u8>> {
	let mut in_section = false;
	for line in output.lines() {
		if line.starts_with(char::is_whitespace) {
			if !in_section {
				continue;
			}
			let (key, _) = line.trim().split_once('=')?;
			if key.starts_with("_ICC_PROFILE") {
				if let Some(bytes) = parse_xprop_icc_hex(line.trim()) {
					return Some(bytes);
				}
			}
		} else {
			// Unindented line: a new output section (or `Screen 0: ...`).
			let name = line.split_whitespace().next().unwrap_or("");
			in_section = name == wanted;
		}
	}
	None
}

/// macOS: CoreGraphics main-display color space → ICC bytes → cache file.
#[cfg(target_os = "macos")]
mod macos {
	use std::ffi::c_void;
	use std::slice;

	use super::write_icc_cache;

	#[link(name = "CoreGraphics", kind = "framework")]
	extern "C" {
		/// `CGDirectDisplayID` of the main display.
		fn CGMainDisplayID() -> u32;
		/// Copy rule: returns a retained `CGColorSpaceRef`, or NULL when the
		/// display has no color space (e.g. headless).
		fn CGDisplayCopyColorSpace(display: u32) -> *mut c_void;
		/// Copy rule: returns a retained `CFDataRef` of the ICC bytes, or
		/// NULL when the color space carries no ICC data.
		fn CGColorSpaceCopyICCData(space: *const c_void) -> *mut c_void;
	}

	#[link(name = "CoreFoundation", kind = "framework")]
	extern "C" {
		/// `CFIndex` byte length of a `CFDataRef`.
		fn CFDataGetLength(data: *const c_void) -> isize;
		/// Pointer to a `CFDataRef`'s bytes (valid while the data is alive).
		fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
		/// Release a Core Foundation object (Copy rule).
		fn CFRelease(obj: *const c_void);
	}

	/// macOS: ICC profile of the main display, materialized as a cache file.
	///
	/// Reads the main display's CoreGraphics color space, copies its ICC bytes out,
	/// releases the Core Foundation objects, and writes the bytes under
	/// `<disk cache>/icc/<hash>.icc` via `write_icc_cache`. Returns the file path;
	/// `None` when the display has no color space, the color space carries no ICC data,
	/// or the cache directory is not writable.
	pub(super) fn display_icc() -> Option<String> {
		display_icc_for(unsafe { CGMainDisplayID() })
	}

	/// ICC bytes for one `CGDirectDisplayID` (any display, not just the
	/// main one) → cache file.
	pub(super) fn display_icc_for(display: u32) -> Option<String> {
		let space = unsafe { CGDisplayCopyColorSpace(display) };
		if space.is_null() {
			return None;
		}
		let data = unsafe { CGColorSpaceCopyICCData(space) };
		if data.is_null() {
			unsafe { CFRelease(space) };
			return None;
		}
		let len = unsafe { CFDataGetLength(data) };
		let ptr = unsafe { CFDataGetBytePtr(data) };
		// The byte pointer is only valid while `data` is alive, so copy the
		// bytes out before releasing anything.
		let bytes = if len > 0 && !ptr.is_null() {
			unsafe { slice::from_raw_parts(ptr, len as usize) }.to_vec()
		} else {
			Vec::new()
		};
		unsafe {
			CFRelease(data);
			CFRelease(space);
		}
		write_icc_cache(&bytes)
	}
}

/// Windows: ICM profile file path of the main display (`GetICMProfileW`).
#[cfg(target_os = "windows")]
mod windows {
	use std::env;
	use std::ffi::c_void;
	use std::mem;
	use std::path::Path;
	use std::ptr;

	#[link(name = "user32")]
	extern "system" {
		/// Device context for the whole screen (`hwnd == NULL`); released
		/// with `ReleaseDC`.
		fn GetDC(hwnd: *const c_void) -> *mut c_void;
		fn ReleaseDC(hwnd: *const c_void, hdc: *mut c_void) -> i32;
		/// `BOOL GetMonitorInfoW(HMONITOR, LPMONITORINFOEXW)` — fills the
		/// structure, including the `szDevice` device name.
		fn GetMonitorInfoW(monitor: *const c_void, info: *mut c_void) -> i32;
	}

	#[link(name = "gdi32")]
	extern "system" {
		/// `BOOL GetICMProfileW`: two-stage — a NULL buffer first yields the
		/// required `WCHAR` count (including the NUL), then the profile file
		/// path is written into the caller's buffer.
		fn GetICMProfileW(hdc: *mut c_void, name_len: *mut u32, name: *mut u16) -> i32;
		/// `HDC CreateDCW(LPCWSTR pszDriver, LPCWSTR pszDevice, LPCWSTR
		/// pszPort, const DEVMODEW *pdm)` — a DC for one specific monitor
		/// device (passing the device name as both driver and device);
		/// released with `DeleteDC`.
		fn CreateDCW(
			driver: *const u16,
			device: *const u16,
			port: *const c_void,
			dev_mode: *const c_void,
		) -> *mut c_void;
		/// `BOOL DeleteDC(HDC)`.
		fn DeleteDC(hdc: *mut c_void) -> i32;
	}

	#[link(name = "ntdll")]
	extern "system" {
		/// The OS version record; `dwBuildNumber` identifies the Windows
		/// release (22000+ = Windows 11).
		fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
	}

	#[link(name = "advapi32")]
	extern "system" {
		fn RegOpenKeyExW(
			key: *const c_void,
			sub_key: *const u16,
			options: u32,
			desired: u32,
			result: *mut *mut c_void,
		) -> i32;
		fn RegQueryValueExW(
			key: *mut c_void,
			value_name: *const u16,
			reserved: *const u32,
			value_type: *mut u32,
			data: *mut u8,
			data_len: *mut u32,
		) -> i32;
		fn RegCloseKey(key: *mut c_void) -> i32;
	}

	/// `OSVERSIONINFOW` (the fields `RtlGetVersion` fills).
	#[repr(C)]
	struct OsVersionInfo {
		length: u32,
		major_version: u32,
		minor_version: u32,
		build_number: u32,
		platform_id: u32,
		csd_version: [u16; 128],
	}

	/// `RECT` (windef.h).
	#[repr(C)]
	struct Rect {
		left: i32,
		top: i32,
		right: i32,
		bottom: i32,
	}

	/// `MONITORINFOEXW` (winuser.h) — `MONITORINFO` plus the `szDevice`
	/// device name (`\\.\DISPLAY1`, ...).
	#[repr(C)]
	struct MonitorInfoExW {
		cb_size: u32,
		rc_monitor: Rect,
		rc_work: Rect,
		dw_flags: u32,
		sz_device: [u16; 32],
	}

	/// `HKEY_CURRENT_USER` (winreg.h).
	const HKEY_CURRENT_USER: *const c_void = 0x8000_0001usize as *const c_void;
	/// `KEY_READ` (winreg.h).
	const KEY_READ: u32 = 0x2_0019;
	/// `REG_DWORD` (winnt.h).
	const REG_DWORD: u32 = 4;
	/// `ERROR_SUCCESS` (winerror.h).
	const ERROR_SUCCESS: i32 = 0;
	/// Windows 11 (any release).
	const BUILD_WINDOWS_11: u32 = 22000;
	/// Windows 11 24H2 — Auto Color Management is on by default there.
	const BUILD_WIN11_24H2: u32 = 26100;

	fn wide_nul(s: &str) -> Vec<u16> {
		let mut wide: Vec<u16> = s.encode_utf16().collect();
		wide.push(0);
		wide
	}

	/// The OS build number (`RtlGetVersion`), or 0 when unavailable.
	fn windows_build() -> u32 {
		let mut info = OsVersionInfo {
			length: mem::size_of::<OsVersionInfo>() as u32,
			major_version: 0,
			minor_version: 0,
			build_number: 0,
			platform_id: 0,
			csd_version: [0; 128],
		};
		let status = unsafe { RtlGetVersion(&mut info) };
		if status == 0 {
			info.build_number
		} else {
			0
		}
	}

	/// The `EnableAutoColorManagement` DWORD under
	/// `HKCU\Software\Microsoft\Windows\CurrentVersion\VideoSettings`:
	/// `Some(flag)` when the value exists, `None` when the key or value is
	/// absent (or unreadable).
	fn acm_registry_flag() -> Option<bool> {
		let sub_key = wide_nul(r"Software\Microsoft\Windows\CurrentVersion\VideoSettings");
		let value_name = wide_nul("EnableAutoColorManagement");
		let mut key: *mut c_void = ptr::null_mut();
		let status =
			unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub_key.as_ptr(), 0, KEY_READ, &mut key) };
		if status != ERROR_SUCCESS {
			return None;
		}
		let mut data = [0u8; 4];
		let mut len = data.len() as u32;
		let mut kind = 0u32;
		let status = unsafe {
			RegQueryValueExW(
				key,
				value_name.as_ptr(),
				ptr::null(),
				&mut kind,
				data.as_mut_ptr(),
				&mut len,
			)
		};
		unsafe { RegCloseKey(key) };
		if status != ERROR_SUCCESS || kind != REG_DWORD || len != 4 {
			return None;
		}
		Some(u32::from_le_bytes(data) != 0)
	}

	/// True when Windows 11 Auto Color Management maps app output for us:
	/// the app must then deliver plain sRGB and NOT apply the display ICC
	/// itself (double correction). The user toggles ACM in
	/// Settings → Display → HDR / "Automatically manage color for apps";
	/// 24H2+ defaults it on, earlier Windows 11 off. The
	/// `QUERCUS_WINDOWS_COLOR` override (`self` / `os`) wins outright.
	pub(super) fn acm_active() -> bool {
		match env::var("QUERCUS_WINDOWS_COLOR") {
			Ok(v) if v.eq_ignore_ascii_case("os") => return true,
			Ok(v) if v.eq_ignore_ascii_case("self") => return false,
			_ => {}
		}
		let build = windows_build();
		if build < BUILD_WINDOWS_11 {
			// Windows 10 and earlier: no per-app OS color management.
			return false;
		}
		match acm_registry_flag() {
			Some(flag) => flag,
			// No explicit user choice: default since 24H2.
			None => build >= BUILD_WIN11_24H2,
		}
	}

	/// Windows: ICM profile path of the main display.
	///
	/// Opens a screen device context (`GetDC(NULL)`), asks GDI for the ICM profile
	/// (`GetICMProfileW`, two-stage size query), validates that the result is an
	/// existing file, and releases the device context. Returns the profile path; `None`
	/// when the DC or profile is unavailable (e.g. headless) or points to a missing
	/// file.
	pub(super) fn display_icc() -> Option<String> {
		let hdc = unsafe { GetDC(ptr::null()) };
		if hdc.is_null() {
			return None;
		}
		let profile = icm_profile_for_hdc(hdc);
		unsafe { ReleaseDC(ptr::null(), hdc) };
		profile
	}

	/// The ICM profile file path of one specific monitor device (e.g.
	/// `\\.\DISPLAY1`), via a per-monitor DC.
	pub(super) fn display_icc_for(device: &str) -> Option<String> {
		let name = wide_nul(device);
		let hdc = unsafe { CreateDCW(name.as_ptr(), name.as_ptr(), ptr::null(), ptr::null()) };
		if hdc.is_null() {
			return None;
		}
		let profile = icm_profile_for_hdc(hdc);
		unsafe { DeleteDC(hdc) };
		profile
	}

	/// The device name (`\\.\DISPLAY1`, ...) of the monitor owning
	/// `hmonitor` (an HMONITOR value), or `None` when the handle is invalid
	/// or the OS call fails.
	pub(super) fn monitor_device_name(hmonitor: usize) -> Option<String> {
		let mut info = MonitorInfoExW {
			cb_size: mem::size_of::<MonitorInfoExW>() as u32,
			rc_monitor: Rect {
				left: 0,
				top: 0,
				right: 0,
				bottom: 0,
			},
			rc_work: Rect {
				left: 0,
				top: 0,
				right: 0,
				bottom: 0,
			},
			dw_flags: 0,
			sz_device: [0; 32],
		};
		let ok = unsafe {
			GetMonitorInfoW(
				hmonitor as *const c_void,
				&mut info as *mut _ as *mut c_void,
			)
		};
		if ok == 0 {
			return None;
		}
		let end = info
			.sz_device
			.iter()
			.position(|&u| u == 0)
			.unwrap_or(info.sz_device.len());
		let name = String::from_utf16_lossy(&info.sz_device[..end]);
		if name.is_empty() {
			None
		} else {
			Some(name)
		}
	}

	/// The ICM profile file path of a device context, or `None` when the
	/// profile is unavailable or not a file.
	fn icm_profile_for_hdc(hdc: *mut c_void) -> Option<String> {
		// Stage 1: required buffer size, in `WCHAR`s including the NUL.
		let mut len: u32 = 0;
		let ok = unsafe { GetICMProfileW(hdc, &mut len, ptr::null_mut()) };
		if ok == 0 || len == 0 {
			return None;
		}

		// Stage 2: fetch the path. One spare `WCHAR` guards against drivers
		// that report a length without the terminator.
		let mut buf = vec![0u16; len as usize + 1];
		let ok = unsafe { GetICMProfileW(hdc, &mut len, buf.as_mut_ptr()) };
		if ok == 0 {
			return None;
		}

		let path = String::from_utf16_lossy(&buf[..len as usize]);
		let path = path.trim_end_matches('\0');
		if path.is_empty() || !Path::new(path).is_file() {
			return None;
		}
		Some(path.to_string())
	}
}

/// Linux: `_ICC_PROFILE` X11/XWayland property, then the colord `colormgr`
/// CLI chain. Sources are tried in order; every failure degrades to `None`.
#[cfg(target_os = "linux")]
mod linux {
	use std::io::Read;
	use std::process::{Command, Stdio};
	use std::sync::Mutex;
	use std::thread;
	use std::time::{Duration, Instant};

	use super::*;

	/// Linux: ICC profile as a cache file, from X11 root-window properties first, then
	/// colord.
	///
	/// Tries `xprop -root _ICC_PROFILE`, `_ICC_PROFILE_1`, `_ICC_PROFILE_2` (the bare
	/// name belongs to the primary monitor) with a 2 s timeout each, caches the first
	/// parsable byte blob, and falls back to the colord `colormgr` CLI. Returns the
	/// cached `.icc` path; `None` when neither source yields a profile (headless,
	/// missing tools, or unwritable cache).
	pub(super) fn display_icc() -> Option<String> {
		// 1. X11 (and XWayland) publish the ICC bytes as root-window
		//    properties. The primary monitor keeps the bare `_ICC_PROFILE`
		//    name; extra monitors append `_1`, `_2`, ... — the bare name is
		//    tried first.
		for prop in ["_ICC_PROFILE", "_ICC_PROFILE_1", "_ICC_PROFILE_2"] {
			let mut cmd = Command::new("xprop");
			cmd.args(["-root", prop]);
			let Some(out) = run_capture(&mut cmd, Duration::from_secs(2)) else {
				continue;
			};
			let Some(bytes) = parse_xprop_icc_hex(&String::from_utf8_lossy(&out)) else {
				continue;
			};
			if let Some(path) = write_icc_cache(&bytes) {
				return Some(path);
			}
		}

		// 2. colord, via the `colormgr` CLI.
		colord_icc_path()
	}

	/// ICC bytes of one RandR output via `xrandr --prop` → cache file.
	/// None when the output is absent, has no `_ICC_PROFILE`, or `xrandr`
	/// cannot run.
	pub(super) fn display_icc_for_output(output: &str) -> Option<String> {
		let mut cmd = Command::new("xrandr");
		cmd.args(["--prop"]);
		let out = run_capture(&mut cmd, Duration::from_secs(2))?;
		let text = String::from_utf8_lossy(&out);
		let bytes = parse_output_icc_prop(&text, output)?;
		write_icc_cache(&bytes)
	}

	/// RandR monitor-list snapshot (`xrandr --listmonitors`), cached briefly
	/// so the poll loop does not spawn a process every tick. A failed fetch
	/// is cached as empty and retried on the next expiry.
	static MONITORS_CACHE: Mutex<Option<(Instant, Vec<RandRMonitor>)>> = Mutex::new(None);
	/// How long a RandR monitor snapshot stays valid.
	const MONITORS_TTL: Duration = Duration::from_secs(30);

	/// The cached RandR monitor list, re-fetched at most once per TTL.
	fn monitors_snapshot() -> Vec<RandRMonitor> {
		let mut guard = MONITORS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
		if let Some((stamp, monitors)) = guard.as_ref() {
			if stamp.elapsed() < MONITORS_TTL {
				return monitors.clone();
			}
		}
		let monitors = fetch_monitors();
		*guard = Some((Instant::now(), monitors.clone()));
		monitors
	}

	/// Run `xrandr --listmonitors` and parse it; empty on any failure.
	fn fetch_monitors() -> Vec<RandRMonitor> {
		let mut cmd = Command::new("xrandr");
		cmd.args(["--listmonitors"]);
		match run_capture(&mut cmd, Duration::from_secs(2)) {
			Some(out) => parse_xrandr_monitors(&String::from_utf8_lossy(&out)),
			None => Vec::new(),
		}
	}

	/// The fingerprint (`x11:<RandR output>`) of the monitor covering the
	/// given point in device pixels (global X11 screen coordinates), or
	/// `None` when no monitor matches (headless, no `xrandr`, or the point
	/// lies outside every listed geometry).
	pub(super) fn x11_monitor_fingerprint_at(x: f64, y: f64) -> Option<String> {
		let monitors = monitors_snapshot();
		let px = x.round() as i32;
		let py = y.round() as i32;
		for monitor in &monitors {
			if px >= monitor.x
				&& px < monitor.x + monitor.width
				&& py >= monitor.y
				&& py < monitor.y + monitor.height
			{
				if let Some(output) = monitor.outputs.first() {
					return Some(format!("x11:{output}"));
				}
			}
		}
		None
	}

	/// Run `cmd`, returning its captured stdout.
	///
	/// Returns `None` when the command cannot be started, exits non-zero, is
	/// still running after `timeout` (it is killed), or produces no output.
	fn run_capture(cmd: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
		let mut child = cmd
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
			.ok()?;
		let deadline = Instant::now() + timeout;
		loop {
			match child.try_wait() {
				Ok(Some(status)) => {
					if !status.success() {
						return None;
					}
					break;
				}
				Ok(None) => {
					if Instant::now() >= deadline {
						let _ = child.kill();
						let _ = child.wait();
						return None;
					}
					thread::sleep(Duration::from_millis(10));
				}
				Err(_) => return None,
			}
		}
		let mut out = Vec::new();
		let _ = child.stdout.take()?.read_to_end(&mut out);
		if out.is_empty() {
			None
		} else {
			Some(out)
		}
	}

	/// colord default-display profile file path via the `colormgr` CLI.
	///
	/// The chain is `get-default-device` → `device-get-default-profile` →
	/// `get-profile` (its "Filename:" field). NOT verified on a live colord
	/// installation (none available in this environment): when any step's
	/// output cannot be parsed, we silently return `None` rather than guess
	/// at the format.
	fn colord_icc_path() -> Option<String> {
		// 1. Object path of the default device.
		let device = colormgr_value(&["get-default-device"])?;
		// 2. Object path of that device's default profile.
		let profile = colormgr_value(&["device-get-default-profile", &device])?;
		// 3. The profile's file name.
		let mut cmd = Command::new("colormgr");
		cmd.args(["get-profile", &profile]);
		let out = run_capture(&mut cmd, Duration::from_secs(2))?;
		let text = String::from_utf8_lossy(&out);
		let line = text
			.lines()
			.map(str::trim_start)
			.find(|l| l.starts_with("Filename:"))?;
		let name = line.split_once(':')?.1.trim();
		if name.is_empty() {
			None
		} else {
			Some(name.to_string())
		}
	}

	/// Run `colormgr <args>` and extract its value: a ColorManager object
	/// path when the output contains one, otherwise the first non-empty
	/// line. Deliberately permissive — no specific output layout is assumed.
	fn colormgr_value(args: &[&str]) -> Option<String> {
		let mut cmd = Command::new("colormgr");
		cmd.args(args);
		let out = run_capture(&mut cmd, Duration::from_secs(2))?;
		let text = String::from_utf8_lossy(&out);
		let value = text
			.split_whitespace()
			.find(|t| t.starts_with("/org/freedesktop/ColorManager/"))
			.or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
			.unwrap_or("");
		if value.is_empty() {
			None
		} else {
			Some(value.to_string())
		}
	}
}

#[cfg(test)]
mod tests;
