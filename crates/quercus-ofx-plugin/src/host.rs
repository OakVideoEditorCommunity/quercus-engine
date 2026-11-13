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

//! Host singleton and plugin cache.
//!
//! Corresponds to C++ `OliveHost` + HostSupport's `PluginCache`. Responsibilities:
//! bundle scanning, `dlopen`-ing plugins, calling `setHost`/`load`/`describe`,
//! building [`Plugin`] records, and creating instances by identifier. References:
//! HS: ofxhPluginCache.cpp (scan dedupe and index semantics),
//! HS: ofxhImageEffectAPI.cpp:150-238 (the load/describe/
//! describeInContext action sequence).
//!
//! ## Scan semantics (cf. olivehost.cpp:118-191 + the official OFX spec)
//!
//! The default path set is organized per platform:
//! user level (when `$HOME` is set)—`$HOME/.OFX/Plugins`,
//! `$HOME/.local/share/OFX/Plugins`, `$HOME/.local/share/olive/ofx/Plugins`
//! (macOS also has `$HOME/Library/OFX/Plugins`);
//! system level—macOS `/Library/OFX/Plugins`, Linux `/usr/OFX/Plugins`,
//! `/usr/local/OFX/Plugins`, Windows `%ProgramFiles%\Common Files\OFX\Plugins`
//! (falls back to `C:\Program Files\...` when `%ProgramFiles%` is unset);
//! Olive app-relative—`../OFX/Plugins`, `../share/olive/ofx/Plugins`,
//! `../lib/olive/ofx/Plugins` (olivehost.cpp:104-107);
//! plus the three environment variables `OFX_PLUGIN_PATH` (official OFX) and
//! `OLIVE_OFX_PLUGIN_PATH`/`OLIVE_PLUGIN_PATH` (Olive extensions; platform path
//! separator: Unix ':' / Windows ';'). Directories that do not exist are skipped;
//! repeated scans dedupe by canonicalized path.
//! `// [P2]`: Info.plist parsing (CFBundleExecutable) is not done—the binary inside
//! a bundle is located by probing (`Contents/MacOS/*`, `Contents/Linux-*/*`,
//! `*.so`/`*.dylib` at the root).
//!
//! ## dlopen (zero dependencies)
//!
//! `dlopen`/`dlsym`/`dlclose` are declared directly (built into macOS libSystem /
//! Linux libc; RTLD constants take platform-specific values). The load order
//! follows ofxCore.h:574: `OfxGetNumberOfPlugins` → `OfxGetPlugin`, then
//! `OfxPlugin::setHost` (mandatory, ofxCore.h:124-132) → `load` → `describe`
//! (HS: ofxhImageEffectAPI.cpp:158-190).

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::descriptor::EffectDescriptor;
use crate::handle::RefBox;
use crate::instance::Instance;
use crate::property::{PropertySet, Value};
use crate::suites::status;

// ---- dlopen FFI（零依赖；RTLD 常量按平台）----

#[cfg(target_os = "macos")]
const RTLD_NOW: c_int = 0x2;
#[cfg(target_os = "linux")]
const RTLD_NOW: c_int = 0x2;
#[cfg(target_os = "macos")]
const RTLD_LOCAL: c_int = 0x4;
#[cfg(target_os = "linux")]
const RTLD_LOCAL: c_int = 0x0;

// Windows has no dlopen/dlsym/dlclose: the POSIX FFI below is compiled
// out and replaced by LoadLibraryExW/GetProcAddress/FreeLibrary (see the
// `win32` module further down), keeping the same function signatures.
#[cfg(not(target_os = "windows"))]
extern "C" {
	fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
	fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
	fn dlclose(handle: *mut c_void) -> c_int;
}

#[cfg(not(target_os = "windows"))]
/// Dynamically loads a shared library and returns an opaque handle (POSIX
/// `dlopen(RTLD_NOW | RTLD_LOCAL)`).
///
/// `path` is first converted to a `CString`: returns `None` when the path is not
/// valid UTF-8 or contains an embedded NUL. Also returns `None` when the library
/// file is missing, the architecture does not match, or dependency resolution
/// fails (`dlopen` returns a null pointer); the specific cause stays in the
/// system's `dlerror`, which this function does not read. The caller is
/// responsible for releasing the handle with `dlclose`.
fn dl_open(path: &Path) -> Option<*mut c_void> {
	let c = CString::new(path.to_str()?).ok()?;
	let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
	if h.is_null() {
		None
	} else {
		Some(h)
	}
}

#[cfg(not(target_os = "windows"))]
/// Looks up the address of an exported symbol in an already-loaded library.
///
/// `handle` is the handle returned by [`dl_open`], and `name` is the C symbol name
/// (converted to a `CString` internally). Returns `None` when the symbol does not
/// exist or the name contains an embedded NUL; the result is a `*mut c_void` and
/// must be converted with [`dlsym_fn`] to be used as a function pointer.
fn dl_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
	let c = CString::new(name).ok()?;
	let p = unsafe { dlsym(handle, c.as_ptr()) };
	if p.is_null() {
		None
	} else {
		Some(p)
	}
}

#[cfg(not(target_os = "windows"))]
/// Looks up a symbol in an already-loaded library and converts it to a function
/// pointer (the same conversion libloading uses).
///
/// Returns `None` when the symbol does not exist, the name contains an embedded
/// NUL, or it cannot be converted to a `CString`. `handle` is the handle returned
/// by [`dl_open`], and `name` is the exported symbol name (a C symbol such as
/// `OfxGetNumberOfPlugins`/`OfxGetPlugin`). The conversion is done with
/// `transmute_copy`, so `T` must be exactly the symbol's real signature.
///
/// # Safety
///
/// The symbol must really be the function type `T`, and `handle` must still be
/// valid.
unsafe fn dlsym_fn<T>(handle: *mut c_void, name: &str) -> Option<T> {
	let p = dl_sym(handle, name)?;
	Some(unsafe { std::mem::transmute_copy(&p) })
}

// ---- Windows dynamic loading (kernel32) ----
//
// LoadLibraryExW / GetProcAddress / FreeLibrary in place of the POSIX
// dlopen family. The Windows handle is an HMODULE, which is pointer-sized
// and stored in the same `*mut c_void` slot, so the public signatures
// (`dl_open` / `dl_sym` / `dlsym_fn` / `dlclose`) are unchanged.

/// Windows dynamic loading (`LoadLibraryExW` / `GetProcAddress` /
/// `FreeLibrary`, kernel32). The handle is an `HMODULE` kept as
/// `*mut c_void` for signature parity with the POSIX path.
#[cfg(target_os = "windows")]
mod win32 {
	use super::*;
	use std::os::windows::ffi::OsStrExt;

	/// `LOAD_WITH_ALTERED_SEARCH_PATH` (winbase.h): resolve the loaded
	/// module's dependent DLLs from the module's own directory first, so a
	/// plugin bundle's sibling DLLs are found next to the binary.
	const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;

	#[link(name = "kernel32")]
	unsafe extern "C" {
		fn LoadLibraryExW(
			lp_file_name: *const u16,
			h_file: *mut c_void,
			dw_flags: u32,
		) -> *mut c_void;
		fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const c_char) -> *mut c_void;
		fn FreeLibrary(h_module: *mut c_void) -> c_int;
	}

	/// Dynamically load a shared library from an arbitrary (possibly
	/// non-UTF-8) path; `None` on failure. The path is converted to a
	/// NUL-terminated UTF-16 string for `LoadLibraryExW`.
	pub(super) fn dl_open(path: &Path) -> Option<*mut c_void> {
		let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
		let h = unsafe {
			LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
		};
		if h.is_null() {
			None
		} else {
			Some(h)
		}
	}

	/// Look up an exported symbol by name; `None` on failure. The symbol
	/// name is a narrow (ANSI) string — `OfxGetNumberOfPlugins` /
	/// `OfxGetPlugin`, both plain ASCII.
	pub(super) fn dl_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
		let c = CString::new(name).ok()?;
		let p = unsafe { GetProcAddress(handle, c.as_ptr()) };
		if p.is_null() {
			None
		} else {
			Some(p)
		}
	}

	/// Function pointer from a `GetProcAddress` result (same transmute as
	/// the POSIX `dlsym_fn`; the caller guarantees the symbol type).
	///
	/// # Safety
	/// The symbol must actually be that function type.
	pub(super) unsafe fn dlsym_fn<T>(handle: *mut c_void, name: &str) -> Option<T> {
		let p = dl_sym(handle, name)?;
		Some(unsafe { std::mem::transmute_copy(&p) })
	}

	/// Unload a library (`FreeLibrary`; reference-counted like POSIX
	/// `dlclose`).
	///
	/// Only the Win32 build uses this wrapper; the POSIX path calls the
	/// `dlclose` symbol directly from `Drop for LoadedBinary`.
	///
	/// # Safety
	///
	/// `handle` must be a live `HMODULE` returned by [`dl_open`] and not
	/// already released; calling it twice, or while plugin entry points from
	/// the module may still run, is undefined behaviour.
	pub(super) unsafe fn dlclose(handle: *mut c_void) -> c_int {
		unsafe { FreeLibrary(handle) }
	}
}

#[cfg(target_os = "windows")]
use win32::{dl_open, dl_sym, dlsym_fn, dlclose};

// ---- OFX 宿主侧结构（ofxCore.h，字段序与 SDK 逐字一致）----

/// `OfxHost`（ofxCore.h:44）。
#[repr(C)]
struct OfxHost {
	/// Host property-set handle (the raw pointer to `Host::props`, tag 0).
	host: *mut c_void,
	/// fetchSuite。
	fetch_suite: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> *const c_void,
}

/// `OfxPlugin`（ofxCore.h:96）。
#[repr(C)]
struct OfxPlugin {
	plugin_api: *const c_char,
	api_version: c_int,
	plugin_identifier: *const c_char,
	plugin_version_major: c_uint,
	plugin_version_minor: c_uint,
	set_host: Option<unsafe extern "C" fn(*mut OfxHost)>,
	main_entry: Option<
		unsafe extern "C" fn(*const c_char, *const c_void, *mut c_void, *mut c_void) -> c_int,
	>,
}

/// The host-side exit for `OfxHost::fetchSuite`: forwards the plugin's suite request
/// to the [`crate::suites::fetch_suite`] dispatch table.
///
/// `host` is the host property-set handle (unused by this implementation, kept only
/// for ABI shape); `name` is the NUL-terminated suite name; `version` is the
/// requested suite version. Returns the suite function-table pointer on a hit and a
/// null pointer on a miss. Every failure path—null pointers, a non-UTF-8 name, the
/// host lacking that suite, and a panic wrapped around the whole operation—returns
/// `std::ptr::null()` and never unwinds into C. When `QUERCUS_OFX_TRACE` is set,
/// hits/misses are printed to stderr (vendor suite probing is normal plugin
/// behaviour, so nothing is logged by default).
///
/// # Safety
///
/// Called directly by plugins per the OFX ABI: `name` must point to a readable
/// NUL-terminated string, and `host` must be the pointer returned by
/// [`global_ofx_host`] or a null pointer.
unsafe extern "C" fn host_fetch_suite(
	host: *mut c_void,
	name: *const c_char,
	version: c_int,
) -> *const c_void {
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if host.is_null() || name.is_null() {
			return None;
		}
		let name = unsafe { CStr::from_ptr(name) }.to_str().ok()?;
		crate::suites::fetch_suite(name, version)
	}));
	match result {
		Ok(Some(p)) => {
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() && !name.is_null() {
				if let Ok(n) = unsafe { CStr::from_ptr(name) }.to_str() {
					eprintln!("[ofx] fetchSuite hit: {n} v{version}");
				}
			}
			p
		}
		Ok(None) => {
			// 诊断：插件请求的 suite 宿主没有。厂商套件（Nuke/Vegas/
			// Foundry）的探测是插件的正常行为，不打正式日志——需要
			// 排查时用 QUERCUS_OFX_TRACE 打开。
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() && !name.is_null() {
				if let Ok(n) = unsafe { CStr::from_ptr(name) }.to_str() {
					eprintln!("[ofx] fetchSuite miss: {n} v{version}");
				}
			}
			std::ptr::null()
		}
		_ => std::ptr::null(),
	}
}

/// Process-level `OfxHost` (the ofxs support library's setHost only stores the
/// **pointer** and does not copy the struct—a stack temporary would dangle after
/// setHost returns, and the fetchSuite callback during describe/render would then be
/// a wild pointer). Leaked on the heap once, valid forever.
fn global_ofx_host() -> *mut OfxHost {
	static PTR: OnceLock<usize> = OnceLock::new();
	*PTR.get_or_init(|| {
		let host = Host::global();
		Box::into_raw(Box::new(OfxHost {
			host: &host.props as *const PropertySet as *mut c_void,
			fetch_suite: host_fetch_suite,
		})) as usize
	}) as *mut OfxHost
}

// ---- 动作与属性常量（ofxCore.h / ofxImageEffect.h）----

/// kOfxActionLoad。
pub(crate) const ACTION_LOAD: &str = "OfxActionLoad";
/// kOfxActionDescribe。
pub(crate) const ACTION_DESCRIBE: &str = "OfxActionDescribe";
/// kOfxActionCreateInstance。
pub(crate) const ACTION_CREATE_INSTANCE: &str = "OfxActionCreateInstance";
/// kOfxActionDestroyInstance。
pub(crate) const ACTION_DESTROY_INSTANCE: &str = "OfxActionDestroyInstance";
/// kOfxImageEffectActionDescribeInContext。
pub(crate) const ACTION_DESCRIBE_IN_CONTEXT: &str = "OfxImageEffectActionDescribeInContext";
/// kOfxImageEffectActionGetClipPreferences。
pub(crate) const ACTION_GET_CLIP_PREFERENCES: &str = "OfxImageEffectActionGetClipPreferences";
/// kOfxImageEffectActionGetRegionOfDefinition。
pub(crate) const ACTION_GET_ROD: &str = "OfxImageEffectActionGetRegionOfDefinition";
/// kOfxImageEffectActionGetRegionsOfInterest。
pub(crate) const ACTION_GET_ROI: &str = "OfxImageEffectActionGetRegionsOfInterest";
/// kOfxImageEffectActionIsIdentity。
pub(crate) const ACTION_IS_IDENTITY: &str = "OfxImageEffectActionIsIdentity";
/// kOfxImageEffectActionRender。
pub(crate) const ACTION_RENDER: &str = "OfxImageEffectActionRender";
/// kOfxImageEffectActionBeginSequenceRender。
pub(crate) const ACTION_BEGIN_SEQUENCE: &str = "OfxImageEffectActionBeginSequenceRender";
/// kOfxImageEffectActionEndSequenceRender。
pub(crate) const ACTION_END_SEQUENCE: &str = "OfxImageEffectActionEndSequenceRender";
/// kOfxActionOpenGLContextAttached（ofxGPURender.h:345）。
pub(crate) const ACTION_GL_CONTEXT_ATTACHED: &str = "OfxActionOpenGLContextAttached";
/// kOfxActionOpenGLContextDetached (ofxGPURender.h:371; the macro value already
/// carries the "kOfx" prefix in the spec as written).
pub(crate) const ACTION_GL_CONTEXT_DETACHED: &str = "kOfxActionOpenGLContextDetached";
/// kOfxImageEffectActionGetOutputColourspace（ofxColour.h:283）。
pub(crate) const ACTION_GET_OUTPUT_COLOURSPACE: &str = "OfxImageEffectActionGetOutputColourspace";
/// kOfxActionInstanceChanged (ofxCore.h:449): host-side parameter/time change
/// notification. After a push button is pressed, the host must call it with the
/// kOfxChangeUserEdited reason (the inArgs contract in ofxCore.h:405-435).
pub(crate) const ACTION_INSTANCE_CHANGED: &str = "OfxActionInstanceChanged";
// ---- Interact（OFX 自定义交互；ofxInteract.h / ofxDrawSuite.h / ofxKeySyms.h）----

/// kOfxActionNewInteract: the first action the host sends to an interact entry when
/// it creates an interact. The vendored ofxInteract.h does not define this macro
/// (the official headers have no such action; the task contract names it after
/// "send kOfxActionNewInteract to the plugin's main entry")—the value is
/// "OfxActionNewInteract", consistent with the OFX action naming convention. Plugins
/// that do not implement this action return kOfxStatReplyDefault (the host treats
/// this as "no interact" or "not participating in the NewInteract protocol"; see
/// [`crate::instance::Instance::new_interact`]).
pub(crate) const ACTION_NEW_INTERACT: &str = "OfxActionNewInteract";
/// kOfxInteractActionIdle: the host's idle pump (task contract; not included in the
/// vendored ofxInteract.h—the official OFX spec has no Idle action, so this is a
/// host extension, with the value "OfxInteractActionIdle"). Plugins implement it
/// voluntarily; if unhandled they return kOfxStatReplyDefault.
pub(crate) const ACTION_INTERACT_IDLE: &str = "OfxInteractActionIdle";
/// kOfxInteractActionDraw（ofxInteract.h:265）。
pub(crate) const ACTION_INTERACT_DRAW: &str = "OfxInteractActionDraw";
/// kOfxInteractActionPenMotion（ofxInteract.h:302）。
pub(crate) const ACTION_INTERACT_PEN_MOTION: &str = "OfxInteractActionPenMotion";
/// kOfxInteractActionPenDown（ofxInteract.h:340）。
pub(crate) const ACTION_INTERACT_PEN_DOWN: &str = "OfxInteractActionPenDown";
/// kOfxInteractActionPenUp（ofxInteract.h:376）。
pub(crate) const ACTION_INTERACT_PEN_UP: &str = "OfxInteractActionPenUp";
/// kOfxInteractActionKeyDown（ofxInteract.h:410）。
pub(crate) const ACTION_INTERACT_KEY_DOWN: &str = "OfxInteractActionKeyDown";
/// kOfxInteractActionKeyUp（ofxInteract.h:443）。
pub(crate) const ACTION_INTERACT_KEY_UP: &str = "OfxInteractActionKeyUp";
/// kOfxInteractActionGainFocus（ofxInteract.h:501）。
pub(crate) const ACTION_INTERACT_GAIN_FOCUS: &str = "OfxInteractActionGainFocus";
/// kOfxInteractActionLoseFocus（ofxInteract.h:526）。
pub(crate) const ACTION_INTERACT_LOSE_FOCUS: &str = "OfxInteractActionLoseFocus";

/// kOfxInteractPropPixelScale (ofxInteract.h:58): canonical→screen-pixel conversion
/// scale (Double×2).
pub(crate) const PROP_INTERACT_PIXEL_SCALE: &str = "OfxInteractPropPixelScale";
/// kOfxInteractPropViewportSize (named "OfxInteractPropViewport" in OFX 1.3; it was
/// removed from the vendored 1.5 headers, and the task contract requires the draw
/// inArgs to carry the viewport size).
pub(crate) const PROP_INTERACT_VIEWPORT_SIZE: &str = "OfxInteractPropViewport";
/// kOfxInteractPropBackgroundImage: the draw inArgs background-image handle from the
/// task contract (Pointer; null when there is no background—this host has no
/// composite background in Phase 1, so it is always null). A non-official OFX
/// property (the official set only has BackgroundColour), so this is a host
/// extension.
pub(crate) const PROP_INTERACT_BACKGROUND_IMAGE: &str = "OfxInteractPropBackgroundImage";
/// kOfxInteractPropBackgroundColour (ofxInteract.h:71): the host viewport background
/// colour (Double×3).
pub(crate) const PROP_INTERACT_BACKGROUND_COLOUR: &str = "OfxInteractPropBackgroundColour";
/// kOfxInteractPropSuggestedColour (ofxInteract.h:86): the overlay colour suggested
/// by the host (Double×3; returns ReplyDefault when the host does not support colour
/// selection).
pub(crate) const PROP_INTERACT_SUGGESTED_COLOUR: &str = "OfxInteractPropSuggestedColour";
/// kOfxInteractPropSlaveToParam (ofxInteract.h:50): parameter names whose value
/// changes trigger an interact redraw (String×N).
pub(crate) const PROP_INTERACT_SLAVE_TO_PARAM: &str = "OfxInteractPropSlaveToParam";
/// kOfxInteractPropPenPosition (ofxInteract.h:95): the pen's canonical position
/// (Double×2, read-only inArgs).
pub(crate) const PROP_INTERACT_PEN_POSITION: &str = "OfxInteractPropPenPosition";
/// kOfxInteractPropPenViewportPosition (ofxInteract.h:104): the pen's viewport pixel
/// position (Int×2, read-only inArgs).
pub(crate) const PROP_INTERACT_PEN_VIEWPORT_POSITION: &str = "OfxInteractPropPenViewportPosition";
/// kOfxInteractPropPenPressure (ofxInteract.h:114): pen pressure (Double×1, 0..1;
/// two-state pens map to 0/1).
pub(crate) const PROP_INTERACT_PEN_PRESSURE: &str = "OfxInteractPropPenPressure";
/// kOfxInteractPropBitDepth (ofxInteract.h:122): interact framebuffer bit depth
/// (Int×1, read-only).
pub(crate) const PROP_INTERACT_BIT_DEPTH: &str = "OfxInteractPropBitDepth";
/// kOfxInteractPropHasAlpha (ofxInteract.h:132): whether the interact framebuffer has
/// alpha (Int×1, read-only).
pub(crate) const PROP_INTERACT_HAS_ALPHA: &str = "OfxInteractPropHasAlpha";
/// kOfxInteractPropDrawContext (ofxDrawSuite.h:34): the Draw suite context handle
/// (Pointer; carried in the draw inArgs and fetched by the plugin to pass to Draw
/// suite functions).
pub(crate) const PROP_INTERACT_DRAW_CONTEXT: &str = "OfxInteractPropDrawContext";
/// kOfxPropKeySym (ofxKeySyms.h:30): the key code of a keyboard event (Int×1).
pub(crate) const PROP_KEY_SYM: &str = "kOfxPropKeySym";
/// kOfxPropKeyString (ofxKeySyms.h:49): the UTF-8 character of a keyboard event
/// (String×1).
pub(crate) const PROP_KEY_STRING: &str = "kOfxPropKeyString";
/// kOfxImageEffectPluginPropOverlayInteractV2 (ofxImageEffect.h:825): the overlay
/// interact entry declared by the plugin (Pointer→OfxPluginEntryPoint; V2 requires
/// drawing through the Draw suite).
pub(crate) const PROP_OVERLAY_INTERACT_V2: &str = "OfxImageEffectPluginPropOverlayInteractV2";
/// kOfxImageEffectPluginPropOverlayInteractV1（ofxImageEffect.h:812）。
pub(crate) const PROP_OVERLAY_INTERACT_V1: &str = "OfxImageEffectPluginPropOverlayInteractV1";
/// kOfxImageEffectPropSupportsOverlays (ofxImageEffect.h:801): whether the host
/// allows plugins to draw overlays on the output image (capability declaration).
pub(crate) const PROP_SUPPORTS_OVERLAYS: &str = "OfxImageEffectPropSupportsOverlays";

// ---- OFX 关键码（ofxKeySyms.h；X11 keysym 值，测试/宿主常用子集）----
//
// 公共：app 侧（WG3b）经 [`crate::suites::interact::Interact::key_down`]/
// `key_up` 传关键码。

/// kOfxKey_Unknown（ofxKeySyms.h:121）。
pub const KEY_UNKNOWN: i32 = 0x0;
/// kOfxKey_BackSpace（ofxKeySyms.h:128）。
pub const KEY_BACKSPACE: i32 = 0xFF08;
/// kOfxKey_Tab（ofxKeySyms.h:129）。
pub const KEY_TAB: i32 = 0xFF09;
/// kOfxKey_Return（ofxKeySyms.h:132）。
pub const KEY_RETURN: i32 = 0xFF0D;
/// kOfxKey_Escape（ofxKeySyms.h:136）。
pub const KEY_ESCAPE: i32 = 0xFF1B;
/// kOfxKey_Delete（ofxKeySyms.h:137）。
pub const KEY_DELETE: i32 = 0xFFFF;
/// kOfxKey_Home（ofxKeySyms.h:172）。
pub const KEY_HOME: i32 = 0xFF50;
/// kOfxKey_Left（ofxKeySyms.h:173）。
pub const KEY_LEFT: i32 = 0xFF51;
/// kOfxKey_Up（ofxKeySyms.h:174）。
pub const KEY_UP: i32 = 0xFF52;
/// kOfxKey_Right（ofxKeySyms.h:175）。
pub const KEY_RIGHT: i32 = 0xFF53;
/// kOfxKey_Down（ofxKeySyms.h:176）。
pub const KEY_DOWN: i32 = 0xFF54;
/// kOfxKey_Page_Up（ofxKeySyms.h:178）。
pub const KEY_PAGE_UP: i32 = 0xFF55;
/// kOfxKey_Page_Down（ofxKeySyms.h:180）。
pub const KEY_PAGE_DOWN: i32 = 0xFF56;
/// kOfxKey_End（ofxKeySyms.h:181）。
pub const KEY_END: i32 = 0xFF57;
/// kOfxKey_F1（ofxKeySyms.h:252）。
pub const KEY_F1: i32 = 0xFFBE;
/// kOfxKey_Shift_L（ofxKeySyms.h:315）。
pub const KEY_SHIFT_L: i32 = 0xFFE1;
/// kOfxKey_Control_L（ofxKeySyms.h:317）。
pub const KEY_CONTROL_L: i32 = 0xFFE3;
/// kOfxKey_Alt_L（ofxKeySyms.h:323）。
pub const KEY_ALT_L: i32 = 0xFFE9;
/// kOfxKey_space（ofxKeySyms.h:331）。
pub const KEY_SPACE: i32 = 0x020;
/// kOfxKey_a（ofxKeySyms.h:398）。
pub const KEY_A: i32 = 0x061;
/// kOfxKey_z（ofxKeySyms.h:423）。
pub const KEY_Z: i32 = 0x07a;

/// kOfxPropChangeReason (ofxCore.h:763): states the change source in the
/// instanceChanged inArgs (UserEdited / PluginEdited / Time).
pub(crate) const PROP_CHANGE_REASON: &str = "OfxPropChangeReason";
/// kOfxChangeUserEdited（ofxCore.h:792）。
pub(crate) const CHANGE_USER_EDITED: &str = "OfxChangeUserEdited";
/// kOfxChangePluginEdited（ofxCore.h:795）。
pub(crate) const CHANGE_PLUGIN_EDITED: &str = "OfxChangePluginEdited";
/// kOfxChangeTime（ofxCore.h:798）。
pub(crate) const CHANGE_TIME: &str = "OfxChangeTime";

/// kOfxPropTime（ofxCore.h:613）。
pub(crate) const PROP_TIME: &str = "OfxPropTime";
/// kOfxPropEffectInstance (ofxCore.h:776): the pointer property in the
/// interact/render inArgs that points to the effect instance handle.
pub(crate) const PROP_EFFECT_INSTANCE: &str = "OfxPropEffectInstance";
/// kOfxImageEffectPropRenderScale。
pub(crate) const PROP_RENDER_SCALE: &str = "OfxImageEffectPropRenderScale";
/// kOfxImageEffectPropRenderWindow。
pub(crate) const PROP_RENDER_WINDOW: &str = "OfxImageEffectPropRenderWindow";
/// kOfxImageEffectPropRegionOfInterest。
pub(crate) const PROP_ROI: &str = "OfxImageEffectPropRegionOfInterest";
/// kOfxImageEffectPropRegionOfDefinition。
pub(crate) const PROP_ROD: &str = "OfxImageEffectPropRegionOfDefinition";
/// kOfxImageEffectPropIsIdentity。
pub(crate) const PROP_IS_IDENTITY: &str = "OfxImageEffectPropIsIdentity";
/// kOfxImageEffectPropFieldToRender。
pub(crate) const PROP_FIELD_TO_RENDER: &str = "OfxImageEffectPropFieldToRender";
/// kOfxImageEffectPropSequentialRenderStatus。
pub(crate) const PROP_SEQUENTIAL_RENDER: &str = "OfxImageEffectPropSequentialRenderStatus";
/// kOfxImageEffectPropInteractiveRenderStatus。
pub(crate) const PROP_INTERACTIVE_RENDER: &str = "OfxImageEffectPropInteractiveRenderStatus";
/// kOfxImageEffectPropRenderQualityDraft。
pub(crate) const PROP_RENDER_QUALITY_DRAFT: &str = "OfxImageEffectPropRenderQualityDraft";
/// kOfxImageEffectPropNoSpatialAwareness。
pub(crate) const PROP_NO_SPATIAL_AWARENESS: &str = "OfxImageEffectPropNoSpatialAwareness";
/// kOfxImageEffectPropFrameRange。
pub(crate) const PROP_FRAME_RANGE: &str = "OfxImageEffectPropFrameRange";
/// kOfxImageEffectPropFrameStep。
pub(crate) const PROP_FRAME_STEP: &str = "OfxImageEffectPropFrameStep";
/// kOfxImageEffectPropFrameRate。
pub(crate) const PROP_FRAME_RATE: &str = "OfxImageEffectPropFrameRate";
/// kOfxImageEffectPropPreMultiplication。
pub(crate) const PROP_PREMULT: &str = "OfxImageEffectPropPreMultiplication";
/// kOfxImageClipPropFieldOrder。
pub(crate) const PROP_FIELD_ORDER: &str = "OfxImageClipPropFieldOrder";
/// kOfxImageClipPropContinuousSamples。
pub(crate) const PROP_CONTINUOUS_SAMPLES: &str = "OfxImageClipPropContinuousSamples";
/// kOfxImageEffectFrameVarying。
pub(crate) const PROP_FRAME_VARYING: &str = "OfxImageEffectFrameVarying";
/// kOfxImageClipPropConnected。
pub(crate) const PROP_CLIP_CONNECTED: &str = "OfxImageClipPropConnected";
/// kOfxImageEffectPropContext。
pub(crate) const PROP_CONTEXT: &str = "OfxImageEffectPropContext";
/// kOfxImageEffectPropSupportedContexts。
pub(crate) const PROP_SUPPORTED_CONTEXTS: &str = "OfxImageEffectPropSupportedContexts";
/// kOfxImageEffectPropProjectSize。
pub(crate) const PROP_PROJECT_SIZE: &str = "OfxImageEffectPropProjectSize";
/// kOfxImageEffectPropProjectOffset。
pub(crate) const PROP_PROJECT_OFFSET: &str = "OfxImageEffectPropProjectOffset";
/// kOfxImageEffectPropProjectExtent。
pub(crate) const PROP_PROJECT_EXTENT: &str = "OfxImageEffectPropProjectExtent";
/// kOfxImageEffectPropProjectPixelAspectRatio (the macro value is PixelAspectRatio).
pub(crate) const PROP_PROJECT_PAR: &str = "OfxImageEffectPropPixelAspectRatio";
/// kOfxImageEffectInstancePropEffectDuration。
pub(crate) const PROP_EFFECT_DURATION: &str = "OfxImageEffectInstancePropEffectDuration";
/// kOfxImageEffectInstancePropSequentialRender。
pub(crate) const PROP_SEQUENTIAL: &str = "OfxImageEffectInstancePropSequentialRender";
/// kOfxImageEffectPropPluginHandle。
pub(crate) const PROP_PLUGIN_HANDLE: &str = "OfxImageEffectPropPluginHandle";
/// kOfxPluginPropFilePath。
pub(crate) const PROP_PLUGIN_FILE_PATH: &str = "OfxPluginPropFilePath";
/// kOfxImageEffectPropSupportsTiles。
pub(crate) const PROP_SUPPORTS_TILES: &str = "OfxImageEffectPropSupportsTiles";
/// kOfxPropType。
pub(crate) const PROP_TYPE: &str = "OfxPropType";
/// kOfxPropLabel。
pub(crate) const PROP_LABEL: &str = "OfxPropLabel";

// ---- GL 能力与协商属性（ofxGPURender.h，M11 第 2 期）----

/// kOfxImageEffectPropOpenGLRenderSupported (ofxGPURender.h:62): the host descriptor
/// ("true") and the plugin descriptor ("false"/"true"/"needed").
pub(crate) const PROP_GL_RENDER_SUPPORTED: &str = "OfxImageEffectPropOpenGLRenderSupported";
/// kOfxImageEffectPropOpenGLEnabled (ofxGPURender.h:117): in args of
/// Render/Begin/EndSequenceRender.
pub(crate) const PROP_GL_ENABLED: &str = "OfxImageEffectPropOpenGLEnabled";
/// kOfxOpenGLPropPixelDepth (ofxGPURender.h:89): the GL render-supported bit depth
/// in the plugin descriptor (optional; a host-side input to format negotiation).
pub(crate) const PROP_GL_PIXEL_DEPTH: &str = "OfxOpenGLPropPixelDepth";

// ---- ofxColour 属性（ofxColour.h，M11 第 2 期）----

/// kOfxImageEffectPropColourManagementStyle（ofxColour.h:60）。
pub(crate) const PROP_COLOUR_STYLE: &str = "OfxImageEffectPropColourManagementStyle";
/// kOfxImageEffectColourManagementOCIO (ofxColour.h:73): the colour management mode
/// declared by the host (task requirement).
pub(crate) const COLOUR_STYLE_OCIO: &str = "OfxImageEffectColourManagementOCIO";
/// kOfxImageEffectPropColourManagementAvailableConfigs（ofxColour.h:85）。
pub(crate) const PROP_COLOUR_AVAILABLE_CONFIGS: &str =
	"OfxImageEffectPropColourManagementAvailableConfigs";
/// kOfxImageEffectPropColourManagementConfig (ofxColour.h:98): the native config
/// negotiated at instance time.
pub(crate) const PROP_COLOUR_CONFIG: &str = "OfxImageEffectPropColourManagementConfig";
/// kOfxImageEffectPropOCIOConfig (ofxColour.h:110): the OCIO config path/URI for the
/// instance.
pub(crate) const PROP_OCIO_CONFIG: &str = "OfxImageEffectPropOCIOConfig";
/// kOfxImageClipPropColourspace (ofxColour.h:142): the clip's actual colour space.
pub(crate) const PROP_CLIP_COLOURSPACE: &str = "OfxImageClipPropColourspace";
/// kOfxImageClipPropPreferredColourspaces (ofxColour.h:201): the clip's preferred
/// colour spaces (writable in the GetClipPreferences out args).
pub(crate) const PROP_CLIP_PREFERRED_COLOURSPACES: &str = "OfxImageClipPropPreferredColourspaces";
/// The native colour config identifier supported by the host (the only current value
/// in ofxColour.h:78).
pub(crate) const NATIVE_CONFIG_ID: &str = "ofx-native-v1.5_aces-v1.3_ocio-v2.3";
/// The host OCIO config URI (ofxColour.h:110 allows ocio:// built-in configs; this
/// host has no config file of its own, so it uses the OCIO built-in default config
/// and conveys the ACEScg working space through the clip colour-space property).
pub(crate) const OCIO_CONFIG_URI: &str = "ocio://default";

/// Per-clip convention names used during negotiation (HS:
/// "OfxImageClipPropComponents_<clip>" etc., ofxhImageEffect.cpp:1725-1727).
pub(crate) fn clip_pref_prop(prefix: &str, clip: &str) -> String {
	format!("{prefix}_{clip}")
}

/// Components name prefix.
pub(crate) const CLIP_PREF_COMPONENTS: &str = "OfxImageClipPropComponents";
/// Bit-depth name prefix.
pub(crate) const CLIP_PREF_DEPTH: &str = "OfxImageClipPropDepth";
/// Pixel-aspect-ratio name prefix.
pub(crate) const CLIP_PREF_PAR: &str = "OfxImageClipPropPAR";

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

// ---- Plugin --------------------------------------------------------------

/// A loaded plugin (one effect entry inside a bundle).
pub struct Plugin {
	/// Plugin identifier (OfxPlugin::pluginIdentifier, e.g. "net.sf.cimg.CImgInvert").
	pub identifier: String,
	/// Version number (major/minor).
	pub version: (u32, u32),
	/// Bundle path.
	pub bundle_path: PathBuf,
	/// Supported contexts from describe (filter/generator/transition).
	pub contexts: Vec<String>,
	/// The describe output (root descriptor).
	pub descriptor: EffectDescriptor,
	/// The dlopen handle (public so tests can build fake plugins).
	pub lib: *mut c_void,
	/// The plugin entry function `OfxPluginEntryPoint` (public so tests can build fake
	/// plugins; the signature matches SDK ofxCore.h:84 verbatim—handle is
	/// `const void*`).
	pub entry: unsafe extern "C" fn(
		action: *const c_char,
		handle: *const c_void,
		in_args: *mut c_void,
		out_args: *mut c_void,
	) -> i32,
	/// The OfxPlugin struct pointer (used for the kOfxImageEffectPropPluginHandle
	/// property; public because tests need it to build fake plugins).
	pub ofx_plugin: *mut c_void,
	/// The binary has been unloaded (host shutdown / unload_all). Once set, any
	/// call_action/call_entry fails immediately without touching the entry—the plugin
	/// entry points into dlclose'd code, so calling it would SIGSEGV (tests that shut
	/// down/rescan serially produce cross-generation instances that hold exactly the
	/// old-generation plugin records).
	pub unloaded: std::sync::atomic::AtomicBool,
}

// dlopen 句柄与 OfxPlugin 指针是**不透明令牌**（只经 dlsym/插件
// 结构字段访问，不直接解引用）；跨线程搬运句柄是宿主分发语义。
// 与 property.rs 的 Value::Pointer 同理。
unsafe impl Send for Plugin {}
unsafe impl Sync for Plugin {}

/// Plugin entry-function type (`OfxPluginEntryPoint`, ofxCore.h:84).
pub(crate) type EntryPoint = unsafe extern "C" fn(
	action: *const c_char,
	handle: *const c_void,
	in_args: *mut c_void,
	out_args: *mut c_void,
) -> i32;

/// The overlay interact entry declared by the plugin (ofxImageEffect.h:825/812): V2
/// takes precedence, then V1; returns None when neither is declared. The property
/// value is `Pointer→OfxPluginEntryPoint` (predefined by the host and written by the
/// plugin during describe).
pub(crate) fn overlay_interact_entry(props: &PropertySet) -> Option<EntryPoint> {
	for name in [PROP_OVERLAY_INTERACT_V2, PROP_OVERLAY_INTERACT_V1] {
		if let Some(Value::Pointer(p)) = props.get(name, 0) {
			if !p.is_null() {
				// 属性值是函数指针（void* 存放）；转换与 dlsym_fn 同款
				// （调用方保证类型正确）。
				return Some(unsafe { std::mem::transmute_copy(&p) });
			}
		}
	}
	None
}

impl Plugin {
	/// Calls a plugin action. `handle` depends on the action (the descriptor for
	/// describe, the instance for render). `in_args`/`out_args` are property sets
	/// assembled per the OFX spec.
	///
	/// Returns the OFX status code (kOfxStatOK == 0). Any plugin-side exception or crash
	/// signal cannot be caught—the FFI discipline only covers Rust-side panics.
	///
	/// # Safety
	/// `handle`/the argument sets must match the action (spec convention).
	pub(crate) unsafe fn call_action(
		&self,
		action: &str,
		handle: *mut c_void,
		in_args: &PropertySet,
		out_args: &PropertySet,
	) -> i32 {
		if self.unloaded.load(std::sync::atomic::Ordering::Acquire) {
			return status::ERR_FATAL;
		}
		// 属性集经裸指针（标签 0）传给插件（property suite 接受）。
		let in_ptr = in_args as *const PropertySet as *mut c_void;
		let out_ptr = out_args as *const PropertySet as *mut c_void;
		let action = cs(action);
		unsafe { (self.entry)(action.as_ptr(), handle as *const c_void, in_ptr, out_ptr) }
	}

	/// Calls through a given entry function (an interact's overlay entry can differ from
	/// the main entry—the plugin declares it via
	/// kOfxImageEffectPluginPropOverlayInteractV2). Semantics match
	/// [`Plugin::call_action`].
	///
	/// # Safety
	/// `entry` must be a valid OfxPluginEntryPoint exported by the plugin;
	/// `handle`/the argument sets must match the action.
	pub(crate) unsafe fn call_entry(
		&self,
		entry: EntryPoint,
		action: &str,
		handle: *mut c_void,
		in_args: &PropertySet,
		out_args: &PropertySet,
	) -> i32 {
		if self.unloaded.load(std::sync::atomic::Ordering::Acquire) {
			return status::ERR_FATAL;
		}
		let in_ptr = in_args as *const PropertySet as *mut c_void;
		let out_ptr = out_args as *const PropertySet as *mut c_void;
		let action = cs(action);
		unsafe { (entry)(action.as_ptr(), handle as *const c_void, in_ptr, out_ptr) }
	}
}

// ---- PluginCache ---------------------------------------------------------

/// A loaded binary (dlopen handle; several Plugins may share one binary).
struct LoadedBinary {
	handle: *mut c_void,
	bundle_path: PathBuf,
}

impl Drop for LoadedBinary {
	fn drop(&mut self) {
		unsafe { dlclose(self.handle) };
	}
}

// 同 Plugin：dlopen 句柄为不透明令牌。
unsafe impl Send for LoadedBinary {}
unsafe impl Sync for LoadedBinary {}

/// Plugin cache (a process singleton; see [`Host::global`]).
pub struct PluginCache {
	/// Loaded plugins (unique by identifier).
	plugins: Mutex<Vec<Arc<Plugin>>>,
	/// Search paths already scanned (prevents rescanning).
	scanned_paths: Mutex<Vec<PathBuf>>,
	/// Loaded binaries (owning dlclose; released on shutdown).
	binaries: Mutex<Vec<LoadedBinary>>,
}

/// Process-wide serialization of dlopen + `OfxGetNumberOfPlugins`: an OFX binary runs
/// its own global initialization on the first query, and that initialization is not
/// thread-safe (two threads entering CImg's static map at once corrupts memory; see
/// the SIGSEGV/SIGABRT during concurrent loading in tests). Loading is a startup
/// operation, so the lock's cost is negligible while making concurrent scans safe
/// for any caller (multiple `PluginCache` instances, background scan threads).
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Whether a directory name ends in `.bundle` or `.plugin`.
///
/// macOS OFX bundles use `.bundle`, while Linux/Windows OFX directories use
/// `.plugin`. Only the last path component is inspected—neither existence nor the
/// directory-ness of the path is checked (the caller guarantees it); returns `false`
/// when the file name is not valid UTF-8.
fn is_bundle_dir(path: &Path) -> bool {
	let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
		return false;
	};
	name.ends_with(".bundle") || name.ends_with(".plugin")
}

/// Locates the binary inside a bundle directory (probing; reading Info.plist's
/// CFBundleExecutable is deferred to `// [P2]`).
fn find_binary_in_bundle(bundle: &Path) -> Option<PathBuf> {
	let contents = bundle.join("Contents");
	for platform_dir in [
		contents.join("MacOS"),
		contents.join("Linux-x86-64"),
		contents.join("Linux-aarch64"),
		// OFX 规范的 64 位 Windows 平台目录。
		contents.join("Win64"),
	] {
		if let Ok(entries) = std::fs::read_dir(&platform_dir) {
			for e in entries.flatten() {
				let p = e.path();
				if p.is_file() && !p.extension().is_some_and(|x| x == "plist") {
					return Some(p);
				}
			}
		}
	}
	// 退而求其次：bundle 根下的 .so/.dylib/.ofx。
	if let Ok(entries) = std::fs::read_dir(bundle) {
		for e in entries.flatten() {
			let p = e.path();
			let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
			if p.is_file() && (ext == "so" || ext == "dylib" || ext == "ofx") {
				return Some(p);
			}
		}
	}
	None
}

/// Default plugin search paths (scan order): the OFX-spec user/system locations
/// (organized by platform cfg) and the Olive app-relative locations (C++ parity,
/// olivehost.cpp:104-107).
///
/// Official OFX standard locations (see the OFX "plug-in discovery" spec):
///
/// * macOS: user `~/Library/OFX/Plugins`, system `/Library/OFX/Plugins`
/// * Windows: `%ProgramFiles%\Common Files\OFX\Plugins` (falls back to the literal
///   `C:\Program Files\...` when `%ProgramFiles%` is unset)
/// * Linux: `/usr/OFX/Plugins`, `/usr/local/OFX/Plugins`
///
/// Plus Olive's legacy locations: `$HOME/.OFX/Plugins`,
/// `$HOME/.local/share/OFX/Plugins`, `$HOME/.local/share/olive/ofx/Plugins`,
/// `../OFX/Plugins`, `../share/olive/ofx/Plugins`, `../lib/olive/ofx/Plugins`.
/// Environment variables (`OFX_PLUGIN_PATH`/`OLIVE_OFX_PLUGIN_PATH`/
/// `OLIVE_PLUGIN_PATH`) are appended by [`PluginCache::scan`]. `home` is `$HOME`;
/// when `None`, all home-based paths are skipped.
fn default_plugin_paths(home: Option<&Path>) -> Vec<PathBuf> {
	let mut paths = Vec::new();
	if let Some(home) = home {
		paths.push(home.join(".OFX/Plugins"));
		paths.push(home.join(".local/share/OFX/Plugins"));
		paths.push(home.join(".local/share/olive/ofx/Plugins"));
		// OFX 规范：macOS 用户级位置。
		#[cfg(target_os = "macos")]
		paths.push(home.join("Library/OFX/Plugins"));
	}
	// OFX 规范：macOS 系统级位置。
	#[cfg(target_os = "macos")]
	paths.push(PathBuf::from("/Library/OFX/Plugins"));
	// OFX 规范：Windows 系统级位置（优先真实安装根 `%ProgramFiles%`）。
	#[cfg(target_os = "windows")]
	{
		match std::env::var("ProgramFiles")
			.or_else(|_| std::env::var("ProgramW6432"))
			.map(PathBuf::from)
		{
			Ok(root) => paths.push(root.join("Common Files").join("OFX").join("Plugins")),
			Err(_) => paths.push(PathBuf::from(r"C:\Program Files\Common Files\OFX\Plugins")),
		}
	}
	// OFX 规范：Linux 系统级位置。
	#[cfg(target_os = "linux")]
	{
		paths.push(PathBuf::from("/usr/OFX/Plugins"));
		paths.push(PathBuf::from("/usr/local/OFX/Plugins"));
	}
	// Olive app-relative 位置（olivehost.cpp:104-107）。
	paths.push(PathBuf::from("../OFX/Plugins"));
	paths.push(PathBuf::from("../share/olive/ofx/Plugins"));
	paths.push(PathBuf::from("../lib/olive/ofx/Plugins"));
	paths
}

/// Records `canonical` in the scanned-path table; a `false` return means it was
/// already present (the caller skips scanning it again). First-seen order is
/// preserved; the dedupe key is the canonicalized path (HS: addFileToPath's
/// weakly_canonical semantics).
fn record_scanned_path(seen: &mut Vec<PathBuf>, canonical: PathBuf) -> bool {
	if seen.iter().any(|p| p == &canonical) {
		return false;
	}
	seen.push(canonical);
	true
}

impl PluginCache {
	/// Scans the standard directories (OFX spec + C++ parity): user-level `$HOME` paths,
	/// platform system-level paths (macOS/Linux/Windows), Olive app-relative paths, and
	/// the three environment variables
	/// `OFX_PLUGIN_PATH`/`OLIVE_OFX_PLUGIN_PATH`/`OLIVE_PLUGIN_PATH`. Repeated calls are
	/// a no-op (dedupe by canonicalized path); nonexistent directories are skipped (see
	/// [`Self::scan_path`]).
	pub fn scan(&self) -> crate::error::Result<()> {
		let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
		let mut paths = default_plugin_paths(home.as_deref().map(Path::new));
		for var in [
			"OLIVE_OFX_PLUGIN_PATH",
			"OLIVE_PLUGIN_PATH",
			"OFX_PLUGIN_PATH",
		] {
			if let Ok(raw) = std::env::var(var) {
				// 平台路径分隔符（Unix ':' / Windows ';'），与 C++ 的
				// `QDir::listSeparator` 一致。
				paths.extend(std::env::split_paths(&raw));
			}
		}
		for p in paths {
			self.scan_path(&p)?;
		}
		Ok(())
	}

	/// Appends a custom search directory and scans the plugin bundles inside it.
	///
	/// The path is `canonicalize`d first (used as-is on failure) and then deduped by
	/// canonical path; a repeated call returns `Ok(())` immediately, and a non-directory
	/// likewise returns silently. The scan depth is capped at 3 (`walk_dirs`), and each
	/// matching bundle is handed to `PluginCache::load_bundle`.
	///
	/// # Errors
	///
	/// Failures for a single bundle are only logged and not propagated, so today this
	/// always returns `Ok(())`; the `Result` is kept for a future scan implementation
	/// that can genuinely fail.
	pub fn scan_path(&self, path: &std::path::Path) -> crate::error::Result<()> {
		// 规范化去重（HS: addFileToPath 的 weakly_canonical）。
		let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
		{
			let mut seen = self.scanned_paths.lock().unwrap_or_else(|e| e.into_inner());
			if !record_scanned_path(&mut seen, canonical.clone()) {
				return Ok(());
			}
		}
		if !canonical.is_dir() {
			return Ok(());
		}
		let mut bundles = Vec::new();
		walk_dirs(&canonical, 0, 3, &mut bundles);
		for bundle in bundles {
			self.load_bundle(&bundle);
		}
		Ok(())
	}

	/// Loads the plugins in one bundle and registers them in the cache (idempotent:
	/// deduped by bundle path).
	///
	/// Flow: locate the binary inside the bundle (`find_binary_in_bundle`) → `dl_open` →
	/// run setHost/load/describe under [`LOAD_LOCK`] (`collect_plugins`) → dedupe by
	/// identifier and insert; the handle is handed to `LoadedBinary` and only `dlclose`d
	/// when the cache unloads. A missing binary, a dlopen failure, no usable plugins, or
	/// a bundle already loaded all return early and release the handle; every
	/// early-return branch logs a diagnostic—silent failures leave the effect library
	/// missing plugins with no clue.
	fn load_bundle(&self, bundle: &Path) {
		// 串行化插件二进制的首次查询（见 [`LOAD_LOCK`]）。
		let _load = LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let Some(binary) = find_binary_in_bundle(bundle) else {
			eprintln!("[ofx] {}: no plugin binary in bundle", bundle.display());
			return;
		};
		let Some(handle) = dl_open(&binary) else {
			eprintln!("[ofx] {}: dlopen failed", binary.display());
			return;
		};
		{
			let bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
			if bins.iter().any(|b| b.bundle_path == bundle) {
				unsafe { dlclose(handle) };
				return;
			}
		}
		let plugins = unsafe { self.collect_plugins(handle, bundle) };
		if plugins.is_empty() {
			eprintln!(
				"[ofx] {}: no usable plugins (setHost/load/describe failed)",
				binary.display()
			);
			unsafe { dlclose(handle) };
			return;
		}
		let mut plugs = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
		let mut bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
		for p in plugins {
			// identifier 去重（后加载者忽略，HS 语义）。
			if !plugs.iter().any(|e| e.identifier == p.identifier) {
				plugs.push(p);
			}
		}
		bins.push(LoadedBinary {
			handle,
			bundle_path: bundle.to_path_buf(),
		});
	}

	/// Collects plugins from a loaded binary (setHost → load → describe).
	///
	/// # Safety
	/// `handle` must be a valid dlopen handle.
	unsafe fn collect_plugins(&self, handle: *mut c_void, bundle: &Path) -> Vec<Arc<Plugin>> {
		let get_no: unsafe extern "C" fn() -> c_int =
			match unsafe { dlsym_fn(handle, "OfxGetNumberOfPlugins") } {
				Some(f) => f,
				None => {
					return Vec::new();
				}
			};
		let get_plug: unsafe extern "C" fn(c_int) -> *mut OfxPlugin =
			match unsafe { dlsym_fn(handle, "OfxGetPlugin") } {
				Some(f) => f,
				None => {
					return Vec::new();
				}
			};
		let count = unsafe { get_no() };
		if count <= 0 {
			return Vec::new();
		}
		let mut out = Vec::new();
		for i in 0..count {
			let ofx = unsafe { get_plug(i) };
			if ofx.is_null() {
				continue;
			}
			if let Some(p) = unsafe { self.build_plugin(ofx, handle, bundle) } {
				out.push(p);
			}
		}
		out
	}

	/// Validates the `OfxPlugin` header and runs setHost → load → describe in order to
	/// build a plugin record.
	///
	/// Only accepts plugins whose API is `OfxImageEffectPluginAPI` v1, whose identifier
	/// is readable, and that have a main entry; the process-level host struct handed to
	/// setHost comes from [`global_ofx_host`]. load and describe accept
	/// `kOfxStatOK`/`kOfxStatReplyDefault`; any other status, a null pointer, or invalid
	/// UTF-8 returns `None` with one diagnostic log line. After describe, the supported
	/// contexts are read and only Filter/Generator/Transition/General are kept; if none
	/// remains, `None` is returned as well.
	///
	/// `lib` is the dlopen handle, stored verbatim in the new plugin for later main entry
	/// calls; `bundle` is used for diagnostics and the descriptor's
	/// `OfxPropPluginFilePath`. The returned plugin is not yet in the cache—the caller
	/// (`collect_plugins`) dedupes by identifier and registers it.
	///
	/// # Safety
	///
	/// `ofx` must point to a valid `OfxPlugin` struct exported by the plugin, and `lib`
	/// must be the dlopen handle of the library containing that plugin, still unloaded;
	/// `bundle` must stay alive until this call returns.
	unsafe fn build_plugin(
		&self,
		ofx: *mut OfxPlugin,
		lib: *mut c_void,
		bundle: &Path,
	) -> Option<Arc<Plugin>> {
		let ofx_ref = unsafe { &*ofx };
		// API 匹配（kOfxImageEffectPluginApi "OfxImageEffectPluginAPI" v1，
		// ofxImageEffect.h:28-32）。每个拒绝分支都打诊断日志（静默拒绝
		// 会让效果库毫无线索地缺插件）。
		let api = if ofx_ref.plugin_api.is_null() {
			eprintln!("[ofx] {}: null plugin_api", bundle.display());
			return None;
		} else {
			unsafe { CStr::from_ptr(ofx_ref.plugin_api) }
				.to_str()
				.ok()?
		};
		if api != "OfxImageEffectPluginAPI" || ofx_ref.api_version != 1 {
			eprintln!(
				"[ofx] {}: unsupported api {api} v{}",
				bundle.display(),
				ofx_ref.api_version
			);
			return None;
		}
		let identifier = unsafe { CStr::from_ptr(ofx_ref.plugin_identifier) }
			.to_str()
			.ok()?
			.to_string();
		let entry = ofx_ref.main_entry?;

		// setHost 是 mandatory 的第一个调用（ofxCore.h:124-132）。宿主
		// 结构体是进程级静态（ofxs 只存指针，见 global_ofx_host）。
		if let Some(f) = ofx_ref.set_host {
			unsafe { f(global_ofx_host()) };
		}

		let mut plugin = Plugin {
			identifier,
			version: (ofx_ref.plugin_version_major, ofx_ref.plugin_version_minor),
			bundle_path: bundle.to_path_buf(),
			contexts: Vec::new(),
			descriptor: EffectDescriptor::new(),
			lib,
			entry,
			ofx_plugin: ofx as *mut c_void,
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};

		// 根描述符属性（HS: effectDescriptorStuff，ofxhImageEffect.cpp:133-155）。
		init_descriptor_props(&plugin.descriptor.props, bundle);

		let empty = PropertySet::new();

		// load（HS: ofxhImageEffectAPI.cpp:158-165；OK/ReplyDefault 接受）。
		let stat = unsafe { plugin.call_action(ACTION_LOAD, std::ptr::null_mut(), &empty, &empty) };
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			eprintln!("[ofx] {}: load action returned {stat}", plugin.identifier);
			return None;
		}

		// describe（handle = 打标描述符；HS: ofxhImageEffectAPI.cpp:173-180）。
		let desc_handle = crate::suites::tag::make(
			&plugin.descriptor.props as *const PropertySet,
			crate::suites::tag::DESCRIPTOR,
		);
		let stat = unsafe { plugin.call_action(ACTION_DESCRIBE, desc_handle, &empty, &empty) };
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			eprintln!("[ofx] {}: describe action returned {stat}", plugin.identifier);
			return None;
		}

		// 支持上下文（describe 产物；HS 从 props 读）。
		let contexts = read_contexts(&plugin.descriptor.props);
		// 只收标准上下文（Filter/Generator/Transition 之外还有
		// General——kOfxImageEffectContextGeneral 同样是规范上下文，
		// Roto/AppendClip/STMap 等插件只声明它）。
		let contexts: Vec<String> = contexts
			.into_iter()
			.filter(|c| {
				matches!(
					c.as_str(),
					"OfxImageEffectContextFilter"
						| "OfxImageEffectContextGenerator"
						| "OfxImageEffectContextTransition"
						| "OfxImageEffectContextGeneral"
				)
			})
			.collect();
		if contexts.is_empty() {
			eprintln!(
				"[ofx] {}: no standard contexts after describe",
				plugin.identifier
			);
			return None;
		}
		plugin.contexts = contexts;
		Some(Arc::new(plugin))
	}

	/// Number of loaded plugins.
	pub fn count(&self) -> usize {
		self.plugins.lock().unwrap_or_else(|e| e.into_inner()).len()
	}

	/// The `index`-th loaded plugin, in discovery (load) order; returns `None` when out
	/// of range.
	///
	/// Returns a shared handle; the plugin is not unloaded while the caller holds it. Use
	/// together with [`PluginCache::count`] when iterating the whole table.
	pub fn at(&self, index: usize) -> Option<Arc<Plugin>> {
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(index)
			.cloned()
	}

	/// Finds a loaded plugin by its plugin identifier and returns a shared handle;
	/// returns `None` when it is not loaded.
	///
	/// The identifier comparison is case-sensitive and must exactly match the identifier
	/// the plugin reports during describe; only the first loaded record is kept for a
	/// given identifier (see `PluginCache::load_bundle`).
	pub fn find(&self, identifier: &str) -> Option<Arc<Plugin>> {
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.find(|p| p.identifier == identifier)
			.cloned()
	}

	/// Unloads everything (the shutdown path). `scanned_paths` is cleared as well—
	/// otherwise a scan after re-init would be short-circuited by dedupe (the cache is
	/// empty but the paths remain). Before unloading, every plugin is marked
	/// `unloaded`: instances may survive across generations (serial shutdown/rescan in
	/// tests) and the old-generation entry points they hold have been invalidated by
	/// dlclose, so with the flag set call_action fails immediately instead of jumping to
	/// a wild pointer.
	pub(crate) fn unload_all(&self) {
		{
			let plugins = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
			for p in plugins.iter() {
				p.unloaded.store(true, std::sync::atomic::Ordering::Release);
			}
		}
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
		let mut bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
		bins.clear(); // Drop → dlclose
		self.scanned_paths
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
	}
}

/// Recursively enumerates bundle directories under a directory.
///
/// Starts at `dir`, with `depth` the current depth (callers pass 0); returns
/// immediately when `depth > max_depth`. Only directories whose name ends in
/// `.bundle`/`.plugin` are collected into `out` (see [`is_bundle_dir`]), and bundle
/// interiors are not entered. The depth cap also guards against symlink loops;
/// directories that fail to read are skipped silently.
fn walk_dirs(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
	if depth > max_depth {
		return;
	}
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for e in entries.flatten() {
		let p = e.path();
		if !p.is_dir() {
			continue;
		}
		if is_bundle_dir(&p) {
			out.push(p);
		} else {
			walk_dirs(&p, depth + 1, max_depth, out);
		}
	}
}

/// Writes the root descriptor (effect descriptor) property table; called before the
/// describe action (HS: a subset of effectDescriptorStuff,
/// ofxhImageEffect.cpp:133-155).
///
/// `props` is the plugin's descriptor property set, and `bundle` is used for
/// `OfxPropPluginFilePath` so the plugin knows which bundle it lives in. What is
/// preset here is the "property universe + defaults": type `OfxTypeImageEffect`,
/// empty label/grouping/description, tile and multi-resolution support, overlay
/// interact V1/V2 and GL capability slots as null pointers or `false`, and ofxColour
/// style None; the plugin overrides them as needed through the property suite during
/// describe. This only writes properties and never calls plugin code.
fn init_descriptor_props(props: &PropertySet, bundle: &Path) {
	props.set_one(PROP_TYPE, Value::String(cs("OfxTypeImageEffect")));
	props.set_one(PROP_LABEL, Value::String(cs("")));
	props.set_one("OfxPropShortLabel", Value::String(cs("")));
	props.set_one("OfxPropLongLabel", Value::String(cs("")));
	props.define("OfxPropVersion", vec![Value::Int(0)]);
	props.set_one("OfxPropVersionLabel", Value::String(cs("")));
	props.set_one("OfxPropPluginDescription", Value::String(cs("")));
	props.define(PROP_SUPPORTED_CONTEXTS, vec![]);
	props.set_one("OfxImageEffectPluginPropGrouping", Value::String(cs("")));
	props.set_one("OfxImageEffectPluginPropSingleInstance", Value::Int(0));
	props.set_one(
		"OfxImageEffectPluginRenderThreadSafety",
		Value::String(cs("OfxImageEffectRenderInstanceSafe")),
	);
	props.set_one("OfxImageEffectPluginPropHostFrameThreading", Value::Int(1));
	props.set_one(
		"OfxImageEffectPluginPropOverlayInteractV1",
		Value::Pointer(std::ptr::null_mut()),
	);
	// overlay interact V2（ofxImageEffect.h:825）：插件 describe 期声明
	// 自定义交互入口（Pointer→OfxPluginEntryPoint；V2 要求 Draw suite
	// 绘制）；宿主预定义空指针默认（propSet 不创建属性，宿主预定义
	// 属性宇宙，与 V1 同款）。
	props.set_one(PROP_OVERLAY_INTERACT_V2, Value::Pointer(std::ptr::null_mut()));
	props.set_one("OfxImageEffectPropSupportsMultiResolution", Value::Int(1));
	props.set_one(PROP_SUPPORTS_TILES, Value::Int(1));
	props.set_one("OfxImageEffectPropTemporalClipAccess", Value::Int(0));
	props.define("OfxImageEffectPropSupportedPixelDepths", vec![]);
	props.set_one(
		"OfxImageEffectPluginPropFieldRenderTwiceAlways",
		Value::Int(1),
	);
	props.set_one(
		"OfxImageEffectPropSupportsMultipleClipDepths",
		Value::Int(0),
	);
	props.set_one("OfxImageEffectPropSupportsMultipleClipPARs", Value::Int(0));
	props.define("OfxImageEffectPropClipPreferencesSlaveParam", vec![]);
	props.set_one(PROP_SEQUENTIAL, Value::Int(0));
	props.set_one(
		PROP_PLUGIN_FILE_PATH,
		Value::String(cs(bundle.to_str().unwrap_or(""))),
	);
	// GL 能力声明（M11 §4；ofxGPURender.h）：描述符预定义
	// OpenGLRenderSupported（默认 "false"）与 kOfxOpenGLPropPixelDepth
	// （空数组）——插件在 describe 期经属性 suite 写入（HostSupport
	// 同款：propSet 不创建属性，宿主预定义属性宇宙）。
	props.set_one(PROP_GL_RENDER_SUPPORTED, Value::String(cs("false")));
	props.define(PROP_GL_PIXEL_DEPTH, vec![]);
	// ofxColour 描述符预定义（M11 §4；ofxColour.h）：style（默认
	// None）与可用配置（空数组），插件 describe 期声明自身能力。
	props.set_one(
		PROP_COLOUR_STYLE,
		Value::String(cs("OfxImageEffectColourManagementNone")),
	);
	props.define(PROP_COLOUR_AVAILABLE_CONFIGS, vec![]);
}

/// Reads the supported contexts the plugin writes during describe
/// (`OfxImageEffectPropSupportedContexts`).
///
/// Reads the String value of each property dimension in turn, skipping non-string
/// slots; returns an empty `Vec` when the property is absent. The order matches the
/// plugin's write order and no filtering is done—standard-context selection is the
/// caller's (`build_plugin`) responsibility.
fn read_contexts(props: &PropertySet) -> Vec<String> {
	let mut out = Vec::new();
	let dim = props.dimension(PROP_SUPPORTED_CONTEXTS);
	for i in 0..dim {
		if let Some(Value::String(s)) = props.get(PROP_SUPPORTED_CONTEXTS, i) {
			out.push(s.to_string_lossy().into_owned());
		}
	}
	out
}

// ---- Host -----------------------------------------------------------------

/// The host. Owns the host-level property set (kOfxPropName/Version and the
/// capability declaration properties) and the plugin cache; it is the registration
/// and dispatch point for suites.
pub struct Host {
	/// The host property set (kOfxProp* host-level properties; capability declarations
	/// such as kOfxImageEffectPropSupportedPixelDepths—the host-side input to
	/// negotiation).
	pub props: PropertySet,
	/// Plugin cache.
	pub cache: PluginCache,
	/// Registry of live instances (leak assertions and debugging).
	pub(crate) instances: Mutex<Vec<std::sync::Weak<RefBox<Instance>>>>,
}

impl Host {
	/// The process singleton. The first call builds the host property set (capability
	/// declarations are written here).
	pub fn global() -> &'static Host {
		static HOST: OnceLock<Host> = OnceLock::new();
		HOST.get_or_init(|| {
			let props = PropertySet::new();
			init_host_props(&props);
			Host {
				props,
				cache: PluginCache {
					plugins: Mutex::new(Vec::new()),
					scanned_paths: Mutex::new(Vec::new()),
					binaries: Mutex::new(Vec::new()),
				},
				instances: Mutex::new(Vec::new()),
			}
		})
	}

	/// Context selection + instantiation (filter first; on failure, fall back in the
	/// order general → tracker → paint → the remaining supported contexts—plugins such
	/// as TrackerPM describe filter support, but their createInstance blows up with
	/// BadHandle outside paint/tracker/general because it cannot grab the Mask clip; in
	/// Natron it naturally instantiates in the tracker context). Returns the context
	/// actually used together with the instance; when every context fails, returns the
	/// last error.
	pub fn create_instance_preferred(
		&self,
		identifier: &str,
	) -> crate::error::Result<(String, Arc<RefBox<Instance>>)> {
		let plugin = self.cache.find(identifier).ok_or(crate::error::Error::NotFound)?;
		let mut order: Vec<&str> = Vec::with_capacity(plugin.contexts.len());
		for pref in [
			"OfxImageEffectContextFilter",
			"OfxImageEffectContextGeneral",
			"OfxImageEffectContextTracker",
			"OfxImageEffectContextPaint",
		] {
			if plugin.contexts.iter().any(|c| c == pref) {
				order.push(pref);
			}
		}
		for c in &plugin.contexts {
			if !order.contains(&c.as_str()) {
				order.push(c);
			}
		}
		let mut last_err = crate::error::Error::Failed("插件无支持上下文".into());
		for ctx in order {
			match self.create_instance(identifier, Some(ctx)) {
				Ok(inst) => return Ok((ctx.to_string(), inst)),
				Err(e) => last_err = e,
			}
		}
		Err(last_err)
	}

	/// Creates an instance by identifier (describeInContext → createInstance). References
	/// HS: ofxhImageEffectAPI.cpp:200-238 (describeInContext's in-args contain only
	/// kOfxImageEffectPropContext) and ofxhImageEffect.cpp:389-460 (populate: clip/param
	/// instantiation). An empty `context` uses the plugin's first supported context.
	/// Failures return [`crate::error::Error::Failed`]/[`crate::error::Error::NotFound`].
	pub fn create_instance(
		&self,
		identifier: &str,
		context: Option<&str>,
	) -> crate::error::Result<Arc<RefBox<Instance>>> {
		let plugin = self
			.cache
			.find(identifier)
			.ok_or(crate::error::Error::NotFound)?;

		let context = match context {
			Some(c) => c.to_string(),
			None => plugin
				.contexts
				.first()
				.cloned()
				.ok_or(crate::error::Error::Failed("插件无支持上下文".into()))?,
		};
		if !plugin.contexts.iter().any(|c| c == &context) {
			return Err(crate::error::Error::Failed(format!(
				"插件不支持上下文 {context}"
			)));
		}

		// describeInContext：上下文描述符 = 根描述符属性拷贝 +
		// 插件在 action 内重定义参数/clip。
		let mut ctx_desc = EffectDescriptor::new();
		ctx_desc.props = plugin.descriptor.props.clone();
		let in_args = PropertySet::new();
		in_args.set_one(PROP_CONTEXT, Value::String(cs(&context)));
		let desc_handle = crate::suites::tag::make(
			&ctx_desc.props as *const PropertySet,
			crate::suites::tag::DESCRIPTOR,
		);
		let stat = unsafe {
			plugin.call_action(ACTION_DESCRIBE_IN_CONTEXT, desc_handle, &in_args, &in_args)
		};
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!(
				"describeInContext 失败：{stat}"
			)));
		}

		// populate（HS: Instance::populate：clip 实例 + param 实例，
		// ofxhImageEffect.cpp:389-460）。
		let clips = ctx_desc
			.clips
			.iter()
			.map(|c| Box::new(crate::clip::ClipInstance::from_descriptor(c.as_ref())))
			.collect();
		let params = crate::param::ParamSetInstance {
			params: ctx_desc
				.params
				.iter()
				.map(|d| Box::new(crate::param::ParamInstance::from_def((**d).clone())))
				.collect(),
		};

		let instance = Instance {
			props: PropertySet::new(),
			plugin,
			context: context.clone(),
			params,
			clips,
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		init_instance_props(&instance.props, &instance);

		let arc = Arc::new(RefBox {
			refs: std::sync::atomic::AtomicU32::new(1),
			value: instance,
		});

		// createInstance action（HS: ofxhImageEffect.cpp:675-688）。
		let inst_handle = crate::suites::tag::make(
			&arc.value.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let empty = PropertySet::new();
		let stat = unsafe {
			arc.value
				.plugin
				.call_action(ACTION_CREATE_INSTANCE, inst_handle, &empty, &empty)
		};
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			// 插件拒绝了 createInstance——它从没认领这个实例；把
			// destroyed 门置位，让随后的 drop 跳过 destroyInstance 通知
			// （否则插件对一个它没创建的实例回 BadIndex 之类的错误）。
			arc.value
				.destroyed
				.store(true, std::sync::atomic::Ordering::Relaxed);
			return Err(crate::error::Error::Failed(format!(
				"createInstance 失败：{stat}"
			)));
		}

		// 登记活跃实例表（泄漏断言用）+ param→instance 回写表。
		self.instances
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(Arc::downgrade(&arc));
		let inst_props = &arc.value.props as *const PropertySet as usize;
		for p in &arc.value.params.params {
			let p_addr = &p.props as *const PropertySet as usize;
			crate::suites::param::register_param_owner(p_addr, inst_props);
		}

		Ok(arc)
	}

	/// Shuts down the host (the host side of `quercusplugin_host_shutdown`):
	/// 1. send the destroyInstance notification to all live instances (plugins are still
	///    loaded at this point);
	/// 2. set cancel flags—handles still held by the C side after shutdown must not call
	///    unloaded plugin entry points again (the render entry short-circuits);
	/// 3. unload plugins and binaries.
	///
	/// Note: C-side handles hold the Arc, so the host cannot (and should not) force
	/// destruction—instance memory is released with the handle; the destroyed flag
	/// ensures Drop no longer touches plugin entry points.
	pub fn shutdown(&self) {
		let list = self.instances.lock().unwrap_or_else(|e| e.into_inner());
		for w in list.iter() {
			if let Some(arc) = w.upgrade() {
				// 置 destroyed 标记后再通知：实例随后的 Drop 不得再
				// 调已卸载的插件入口（二次 destroyInstance → 悬垂）。
				arc.value
					.destroyed
					.store(true, std::sync::atomic::Ordering::Relaxed);
				arc.value.notify_destroy();
				arc.value
					.cancel
					.store(true, std::sync::atomic::Ordering::Relaxed);
			}
		}
		self.cache.unload_all();
	}

	/// Current number of live instances (leak assertion).
	pub fn alive_count(&self) -> usize {
		let mut list = self.instances.lock().unwrap_or_else(|e| e.into_inner());
		let alive = list.iter().filter(|w| w.strong_count() > 0).count();
		list.retain(|w| w.strong_count() > 0);
		alive
	}
}

/// Builds the host-level property set (called exactly once when the process singleton
/// [`Host::global`] initializes; cf. C++ `OliveHost::OliveHost`,
/// olivehost.cpp:193-207).
///
/// First writes the three identity entries Name/Label/Version, then all capability
/// slots the ofxs support library reads with `throwOnFailure=true`: OfxType=OfxTypeHost,
/// OfxPropAPIVersion 1.4, supported contexts/components/pixel depths (phase 1 only
/// `OfxBitDepthFloat` and RGBA/RGB/Alpha), frame thread safety FullySafe, temporal
/// clip access, tile/multi-resolution support, GL render and overlay support, and
/// ofxColour's OCIO mode + native config.
///
/// A missing entry makes the first plugin's load fail outright (once
/// gHostDescriptionHasInit is set, plugins only see a half-initialized description).
/// `props` must be an empty property set and this must run before any plugin is
/// loaded.
fn init_host_props(props: &PropertySet) {
	// ofxCore.h 宿主属性集的必备项：OfxType=OfxTypeHost 与
	// OfxPropAPIVersion（int[2]，宿主实现的 API 版本）——ofxs 支持库的
	// loadAction 以 throwOnFailure=true 读它们，缺失会被映射成
	// kOfxStatErrMissingHostFeature 让插件加载直接失败。
	props.set_one("OfxPropType", Value::String(cs("OfxTypeHost")));
	props.define("OfxPropAPIVersion", vec![Value::Int(1), Value::Int(4)]);
	props.set_one("OfxPropName", Value::String(cs("Quercus Video Editor")));
	props.set_one("OfxPropLabel", Value::String(cs("Quercus Video Editor")));
	props.set_one(
		"OfxPropVersionLabel",
		Value::String(cs(env!("CARGO_PKG_VERSION"))),
	);
	props.define(
		"OfxPropVersion",
		vec![Value::Int(0), Value::Int(0), Value::Int(0)],
	);
	// 能力宣告：协商的宿主侧输入（phase 1 全链路 F32+RGBA）。
	props.define(
		"OfxImageEffectPropSupportedPixelDepths",
		vec![Value::String(cs("OfxBitDepthFloat"))],
	);
	// 宿主支持的上下文集（ofxs 以 throwOnFailure=true 读）。
	props.define(
		"OfxImageEffectPropSupportedContexts",
		vec![
			Value::String(cs("OfxImageEffectContextFilter")),
			Value::String(cs("OfxImageEffectContextGenerator")),
			Value::String(cs("OfxImageEffectContextTransition")),
			Value::String(cs("OfxImageEffectContextGeneral")),
		],
	);
	props.define(
		"OfxImageEffectPropSupportedComponents",
		vec![
			Value::String(cs("OfxImageComponentRGBA")),
			Value::String(cs("OfxImageComponentRGB")),
			Value::String(cs("OfxImageComponentAlpha")),
		],
	);
	// ofxs 支持库 fetchHostDescription 以 throwOnFailure=true 读取的
	// 全部宿主能力位（IsBackground 缺一个都会让读取链中断——
	// gHostDescriptionHasInit 已置位，后续插件拿到半初始化描述，
	// temporalClipAccess 为 0 → 时序类插件集体拒载）。
	props.set_one("OfxImageEffectHostPropIsBackground", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsStringAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsChoiceAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsBooleanAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsCustomAnimation", Value::Int(0));
	// 自定义 interact（检视器叠加层）宿主支持。
	props.set_one("OfxParamHostPropSupportsCustomInteract", Value::Int(1));
	// 能力宣告（ofxImageEffect.h 宿主属性集；ofxs 支持库在 load 时读成
	// ImageEffectHostDescription，temporalClipAccess 等为 0 时整类插件
	// ——Retime/FrameHold/TimeOffset/SlitScan——直接在 load 里拒载）。
	props.set_one("OfxImageEffectPropTemporalClipAccess", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsMultiResolution", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsTiles", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsMultipleClipPARs", Value::Int(1));
	// 像素深度只支持 Float（phase 1 全链路 F32），不做多深度协商。
	props.set_one("OfxImageEffectPropSupportsMultipleClipDepths", Value::Int(0));
	// 旧版 ofxs 的 fetchHostDescription 读的是不带 Supports 的同义名
	// （ofxImageEffect.h 的 kOfxImageEffectPropMultipleClipDepths）。
	props.set_one("OfxImageEffectPropMultipleClipDepths", Value::Int(0));
	props.set_one("OfxImageEffectPropSetableFrameRate", Value::Int(0));
	props.set_one("OfxImageEffectPropSetableFielding", Value::Int(0));
	// fetchHostDescription 的其余读取项（部分 ofxs 版本以
	// throwOnFailure=true 读，缺一个首插件的 load 就炸）：
	// 顺序渲染状态 0=宿主不强制；Draft 渲染质量支持（播放降档）；
	// 参数数无上限；不支持参数化曲线动画；macOS 无 OS 窗口句柄；
	// 原生坐标原点按 OFX 规范默认 BottomLeft。
	props.set_one("OfxImageEffectInstancePropSequentialRender", Value::Int(0));
	props.set_one("OfxImageEffectPropRenderQualityDraft", Value::Int(1));
	props.set_one("OfxParamHostPropMaxParameters", Value::Int(-1));
	// 参数页数无上限；页内行列 0,0 = 自动排布（这两个也是
	// throwOnFailure=true 的读取项）。
	props.set_one("OfxParamHostPropMaxPages", Value::Int(-1));
	props.define(
		"OfxParamHostPropPageRowColumnCount",
		vec![Value::Int(0), Value::Int(0)],
	);
	props.set_one("OfxParamHostPropSupportsParametricAnimation", Value::Int(0));
	props.set_one("OfxPropHostOSHandle", Value::Pointer(std::ptr::null_mut()));
	props.set_one(
		"OfxImageEffectHostPropNativeOrigin",
		Value::String(cs("OfxHostNativeOriginBottomLeft")),
	);
	// 序列渲染：插件可随意选（0 = 宿主不强制）。
	props.set_one("OfxImageEffectPropSequentialRenderStatus", Value::Int(0));
	// 逐帧线程化程度：全帧线程安全（kOfxImageEffectRenderUnsafe 之外的
	// 最强档——渲染在独立 worker 进程内串行驱动，无共享状态）。
	props.set_one("OfxImageEffectPropRenderThreadSafety", Value::String(cs("OfxImageEffectRenderFullySafe")));
	// GL 能力宣告（M11 §4；ofxGPURender.h "OpenGL House Keeping"：
	// 宿主在描述符置 "true"）。
	props.set_one(PROP_GL_RENDER_SUPPORTED, Value::String(cs("true")));
	// overlay 能力宣告（ofxImageEffect.h:801）：宿主允许插件在输出
	// 图像上绘制 overlay（interact draw 的前置）。
	props.set_one(PROP_SUPPORTS_OVERLAYS, Value::Int(1));
	// ofxColour 能力宣告（M11 §4）：OCIO 模式 + native 配置列表。
	props.set_one(PROP_COLOUR_STYLE, Value::String(cs(COLOUR_STYLE_OCIO)));
	props.define(
		PROP_COLOUR_AVAILABLE_CONFIGS,
		vec![Value::String(cs(NATIVE_CONFIG_ID))],
	);
}

/// Writes the instance-level property table (HS: the phase 1 subset of
/// effectInstanceStuff, ofxhImageEffect.cpp:313-330); called before createInstance.
///
/// `props` is the new instance's property set; `instance` supplies the context name
/// (OfxPropContext) and the plugin handle (OfxImageEffectPropPluginHandle). Project
/// size/offset/extent are written as 1920×1080, PAR 1.0, frame rate 24, duration
/// 1.0—currently fixed placeholder values; the code tree has no path that
/// overwrites them from the actual project. Colour management is fixed to OCIO mode
/// + the native config (ofxColour.h: style/config/OCIOConfig must be set on the
/// instance by the host).
fn init_instance_props(props: &PropertySet, instance: &Instance) {
	props.set_one(PROP_TYPE, Value::String(cs("OfxTypeImageEffectInstance")));
	props.set_one(PROP_CONTEXT, Value::String(cs(&instance.context)));
	props.set_one(
		PROP_PLUGIN_HANDLE,
		Value::Pointer(instance.plugin.ofx_plugin),
	);
	props.define(
		PROP_PROJECT_SIZE,
		vec![Value::Double(1920.0), Value::Double(1080.0)],
	);
	props.define(
		PROP_PROJECT_OFFSET,
		vec![Value::Double(0.0), Value::Double(0.0)],
	);
	props.define(
		PROP_PROJECT_EXTENT,
		vec![Value::Double(1920.0), Value::Double(1080.0)],
	);
	props.set_one(PROP_PROJECT_PAR, Value::Double(1.0));
	props.set_one(PROP_EFFECT_DURATION, Value::Double(1.0));
	props.set_one(PROP_SEQUENTIAL, Value::Int(0));
	props.set_one(PROP_FRAME_RATE, Value::Double(24.0));
	props.set_one("OfxPropIsInteractive", Value::Int(0));
	props.set_one(PROP_SUPPORTS_TILES, Value::Int(1));
	// ofxColour 实例期协商结果（M11 §4；ofxColour.h：宿主必须在实例
	// 上设置 style/config/OCIOConfig）。
	props.set_one(PROP_COLOUR_STYLE, Value::String(cs(COLOUR_STYLE_OCIO)));
	props.set_one(PROP_COLOUR_CONFIG, Value::String(cs(NATIVE_CONFIG_ID)));
	props.set_one(PROP_OCIO_CONFIG, Value::String(cs(OCIO_CONFIG_URI)));
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The default path table includes every home- and app-relative location
	/// plus the platform-gated OFX standard locations.
	#[test]
	fn default_paths_cover_home_and_standard_locations() {
		let home = Path::new("/home/octa");
		let paths = default_plugin_paths(Some(home));
		// Olive legacy + home locations.
		assert!(paths.contains(&home.join(".OFX/Plugins")));
		assert!(paths.contains(&home.join(".local/share/OFX/Plugins")));
		assert!(paths.contains(&home.join(".local/share/olive/ofx/Plugins")));
		// App-relative (C++ parity).
		assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
		assert!(paths.contains(&PathBuf::from("../share/olive/ofx/Plugins")));
		assert!(paths.contains(&PathBuf::from("../lib/olive/ofx/Plugins")));
		// OFX standard locations, gated by platform.
		#[cfg(target_os = "macos")]
		{
			assert!(paths.contains(&home.join("Library/OFX/Plugins")));
			assert!(paths.contains(&PathBuf::from("/Library/OFX/Plugins")));
		}
		#[cfg(target_os = "linux")]
		{
			assert!(paths.contains(&PathBuf::from("/usr/OFX/Plugins")));
			assert!(paths.contains(&PathBuf::from("/usr/local/OFX/Plugins")));
		}
		#[cfg(target_os = "windows")]
		{
			assert!(paths
				.iter()
				.any(|p| p.ends_with("Common Files/OFX/Plugins") || p.ends_with("Common Files\\OFX\\Plugins")));
		}
	}

	/// With no `$HOME` the home-based entries are skipped; the rest remain.
	#[test]
	fn default_paths_skip_home_entries_when_home_unset() {
		let paths = default_plugin_paths(None);
		assert!(!paths.iter().any(|p| p.to_string_lossy().starts_with("/home/")));
		assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
		assert_eq!(
			paths
				.iter()
				.filter(|p| p.to_string_lossy().contains(".OFX"))
				.count(),
			0
		);
	}

	/// The dedupe record rejects an already-seen path and keeps first-seen
	/// order.
	#[test]
	fn record_scanned_path_dedupes() {
		let mut seen: Vec<PathBuf> = Vec::new();
		assert!(record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
		assert!(record_scanned_path(&mut seen, PathBuf::from("/b/Plugins")));
		// A duplicate is rejected without being recorded again.
		assert!(!record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
		assert_eq!(seen, vec![PathBuf::from("/a/Plugins"), PathBuf::from("/b/Plugins")]);
	}

	/// Scanning an existing directory twice only records it once, and a
	/// non-existent directory is skipped without error.
	#[test]
	fn scan_path_skips_missing_and_dedupes_existing() {
		let dir = std::env::temp_dir().join(format!("quercusplugin-scan-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let cache = PluginCache {
			plugins: Mutex::new(Vec::new()),
			scanned_paths: Mutex::new(Vec::new()),
			binaries: Mutex::new(Vec::new()),
		};
		cache.scan_path(&dir).expect("existing dir scans");
		cache.scan_path(&dir).expect("duplicate scan is a no-op");
		{
			let seen = cache.scanned_paths.lock().unwrap_or_else(|e| e.into_inner());
			assert_eq!(seen.len(), 1, "the same directory is scanned only once");
		} // drop the guard before scanning again

		// A missing directory is skipped (no error, no plugin scan).
		let missing = dir.join("does-not-exist");
		cache.scan_path(&missing).expect("missing dir scan is a no-op");
		let plugins = cache.plugins.lock().unwrap_or_else(|e| e.into_inner());
		assert!(plugins.is_empty(), "no bundles were loaded");

		let _ = std::fs::remove_dir_all(&dir);
	}

	// ---- branch coverage batch: loader/cache/host ------------------------

	fn temp_dir(tag: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!(
			"quercus-ofx-plugin-host-{}-{tag}",
			std::process::id()
		));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	fn test_cache() -> PluginCache {
		PluginCache {
			plugins: Mutex::new(Vec::new()),
			scanned_paths: Mutex::new(Vec::new()),
			binaries: Mutex::new(Vec::new()),
		}
	}

	/// The minimal test-plugin shared library built by build.rs.
	fn fixture_lib() -> Option<PathBuf> {
		let out = PathBuf::from(env!("OUT_DIR"));
		let lib = out.join(if cfg!(target_os = "macos") {
			"quercus_test_plugin.dylib"
		} else {
			"quercus_test_plugin.so"
		});
		lib.is_file().then_some(lib)
	}

	/// Assembles a `.ofx.bundle` containing the test-plugin binary on the fly (platform
	/// directory probing).
	fn make_fixture_bundle(tag: &str) -> Option<(PathBuf, PathBuf)> {
		let lib = fixture_lib()?;
		let platform = if cfg!(target_os = "macos") {
			"MacOS"
		} else if cfg!(target_os = "windows") {
			"Win64"
		} else {
			"Linux-x86-64"
		};
		let root = temp_dir(tag);
		let bundle = root.join("quercus-test-plugin.ofx.bundle");
		let bin_dir = bundle.join("Contents").join(platform);
		std::fs::create_dir_all(&bin_dir).unwrap();
		let target = bin_dir.join(if cfg!(target_os = "windows") {
			"plugin.dll"
		} else {
			"plugin"
		});
		std::fs::copy(&lib, &target).unwrap();
		Some((root, bundle))
	}

	fn action_is(action: *const c_char, name: &str) -> bool {
		!action.is_null()
			&& unsafe { CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
	}

	unsafe extern "C" fn entry_ok(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}

	unsafe extern "C" fn entry_reply_default(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::REPLY_DEFAULT
	}

	unsafe extern "C" fn entry_describe_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, ACTION_DESCRIBE_IN_CONTEXT) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	unsafe extern "C" fn entry_create_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, ACTION_CREATE_INSTANCE) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	fn fake_plugin(identifier: &str, contexts: Vec<String>, entry: EntryPoint) -> Arc<Plugin> {
		Arc::new(Plugin {
			identifier: identifier.into(),
			version: (1, 0),
			bundle_path: PathBuf::new(),
			contexts,
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		})
	}

	/// An isolated Host (local cache/instances, avoiding the process singleton).
	fn local_host(plugins: Vec<Arc<Plugin>>) -> Host {
		Host {
			props: PropertySet::new(),
			cache: PluginCache {
				plugins: Mutex::new(plugins),
				scanned_paths: Mutex::new(Vec::new()),
				binaries: Mutex::new(Vec::new()),
			},
			instances: Mutex::new(Vec::new()),
		}
	}

	// ---- dlopen/dlsym error paths ----------------------------------------

	#[cfg(not(target_os = "windows"))]
	#[test]
	fn dl_open_and_dl_sym_error_paths_return_none() {
		// 不存在的库 → None。
		assert!(dl_open(Path::new("/definitely/not/a/library-quercus.so")).is_none());
		// 可加载的库 + 不存在的符号 → None。
		let lib = fixture_lib().expect("build.rs 产出测试插件库");
		let handle = dl_open(&lib).expect("测试插件库可加载");
		assert!(dl_sym(handle, "quercus_definitely_missing_symbol").is_none());
		unsafe { dlclose(handle) };
	}

	// ---- fetchSuite 入口 -------------------------------------------------

	#[test]
	fn host_fetch_suite_null_args_and_trace_paths() {
		use std::ffi::c_void as cv;
		static TRACE_LOCK: Mutex<()> = Mutex::new(());
		let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

		let host_set = PropertySet::new();
		let host = &host_set as *const PropertySet as *mut cv;
		let name = CString::new("OfxPropertySuite").unwrap();
		let bogus = CString::new("OfxBogusSuite").unwrap();

		// NULL host / NULL name → None（不触达 suite 分发表）。
		assert!(unsafe { host_fetch_suite(std::ptr::null_mut(), name.as_ptr(), 1) }.is_null());
		assert!(unsafe { host_fetch_suite(host, std::ptr::null(), 1) }.is_null());

		// QUERCUS_OFX_TRACE 打开：hit 与 miss 诊断分支。
		let prev = std::env::var_os("QUERCUS_OFX_TRACE");
		std::env::set_var("QUERCUS_OFX_TRACE", "1");
		assert!(!unsafe { host_fetch_suite(host, name.as_ptr(), 1) }.is_null());
		assert!(unsafe { host_fetch_suite(host, bogus.as_ptr(), 1) }.is_null());
		// 版本不符也走 miss。
		assert!(unsafe { host_fetch_suite(host, name.as_ptr(), 2) }.is_null());
		match prev {
			Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
			None => std::env::remove_var("QUERCUS_OFX_TRACE"),
		}
	}

	// ---- overlay interact 入口声明 ---------------------------------------

	#[test]
	fn overlay_interact_entry_null_pointer_and_v1_fallback() {
		unsafe extern "C" fn overlay(
			_: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> c_int {
			status::OK
		}
		let props = PropertySet::new();
		// 属性缺失 → None。
		assert!(overlay_interact_entry(&props).is_none());
		// 空指针 → 跳过（V2 空、V1 也缺）。
		props.set_one(PROP_OVERLAY_INTERACT_V2, Value::Pointer(std::ptr::null_mut()));
		assert!(overlay_interact_entry(&props).is_none());
		// 类型不是 Pointer → 跳过。
		props.set_one(PROP_OVERLAY_INTERACT_V2, Value::Int(1));
		assert!(overlay_interact_entry(&props).is_none());
		// V1 回退命中。
		props.set_one(
			PROP_OVERLAY_INTERACT_V1,
			Value::Pointer(overlay as *const () as *mut std::ffi::c_void),
		);
		assert!(overlay_interact_entry(&props).is_some());
		// V2 优先。
		props.set_one(
			PROP_OVERLAY_INTERACT_V2,
			Value::Pointer(overlay as *const () as *mut std::ffi::c_void),
		);
		assert!(overlay_interact_entry(&props).is_some());
	}

	// ---- 卸载后的入口保护 -------------------------------------------------

	#[test]
	fn unloaded_plugin_actions_fail_without_touching_entry() {
		let plugin = fake_plugin("test.unloaded", vec![], entry_ok);
		plugin
			.unloaded
			.store(true, std::sync::atomic::Ordering::Release);
		let empty = PropertySet::new();
		let st = unsafe {
			plugin.call_action(ACTION_LOAD, std::ptr::null_mut(), &empty, &empty)
		};
		assert_eq!(st, status::ERR_FATAL);
		let st = unsafe {
			plugin.call_entry(entry_ok, ACTION_LOAD, std::ptr::null_mut(), &empty, &empty)
		};
		assert_eq!(st, status::ERR_FATAL);
	}

	// ---- bundle/二进制探测 -----------------------------------------------

	#[test]
	fn is_bundle_dir_matches_suffixes_only() {
		assert!(is_bundle_dir(Path::new("/tmp/Foo.bundle")));
		assert!(is_bundle_dir(Path::new("Bar.plugin")));
		assert!(!is_bundle_dir(Path::new("/tmp/NotIt")));
		// 无 file_name（根路径）→ false。
		assert!(!is_bundle_dir(Path::new("/")));
	}

	#[test]
	fn find_binary_in_bundle_prefers_platform_dir_and_falls_back_to_root() {
		let dir = temp_dir("find-bin");

		// 空 bundle → None。
		assert!(find_binary_in_bundle(&dir).is_none());

		// 根目录 .so 回退。
		std::fs::write(dir.join("libquercus.so"), b"not a real lib").unwrap();
		assert_eq!(find_binary_in_bundle(&dir).unwrap(), dir.join("libquercus.so"));

		// 平台目录优先，且跳过 .plist。
		let platform = dir.join("Contents").join("Linux-x86-64");
		std::fs::create_dir_all(&platform).unwrap();
		std::fs::write(platform.join("Info.plist"), b"meta").unwrap();
		std::fs::write(platform.join("plugin"), b"bin").unwrap();
		assert_eq!(find_binary_in_bundle(&dir).unwrap(), platform.join("plugin"));

		// 平台目录里只有 .plist → 回退根目录。
		std::fs::remove_file(platform.join("plugin")).unwrap();
		assert_eq!(find_binary_in_bundle(&dir).unwrap(), dir.join("libquercus.so"));

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn walk_dirs_respects_depth_and_read_errors() {
		let root = temp_dir("walk");
		std::fs::write(root.join("file.txt"), b"x").unwrap();
		let a = root.join("a");
		let bundle = a.join("One.bundle");
		let b = a.join("b");
		let c = b.join("c");
		std::fs::create_dir_all(&bundle).unwrap();
		std::fs::create_dir_all(&c).unwrap();
		std::fs::create_dir_all(b.join("Two.bundle")).unwrap();

		let mut out = Vec::new();
		walk_dirs(&root, 0, 1, &mut out);
		assert_eq!(out, vec![bundle.clone()], "深度 1 内只发现 One.bundle（文件与更深目录跳过）");

		// 深度上限内全收。
		let mut deep = Vec::new();
		walk_dirs(&root, 0, 4, &mut deep);
		assert_eq!(deep.len(), 2);

		// read_dir 失败（路径是文件）→ 空返回。
		let mut errs = Vec::new();
		walk_dirs(&root.join("file.txt"), 0, 4, &mut errs);
		assert!(errs.is_empty());

		let _ = std::fs::remove_dir_all(&root);
	}

	// ---- load_bundle ------------------------------------------------------

	#[test]
	fn load_bundle_reports_missing_and_invalid_binaries() {
		let cache = test_cache();

		// 空目录（无二进制）→ 记录并返回。
		let empty = temp_dir("load-empty");
		cache.load_bundle(&empty);
		assert_eq!(cache.count(), 0);

		// 根目录垃圾 .so → dlopen 失败，记录并返回。
		let bad = temp_dir("load-bad");
		std::fs::write(bad.join("garbage.so"), b"definitely not an ELF").unwrap();
		cache.load_bundle(&bad);
		assert_eq!(cache.count(), 0);
		assert!(cache.binaries.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

		let _ = std::fs::remove_dir_all(&empty);
		let _ = std::fs::remove_dir_all(&bad);
	}

	#[test]
	fn load_bundle_loads_fixture_plugins_and_dedupes() {
		let Some((root, bundle)) = make_fixture_bundle("load-ok") else {
			eprintln!("SKIP: 测试插件未构建");
			return;
		};
		let cache = test_cache();
		cache.load_bundle(&bundle);
		assert_eq!(cache.count(), 5, "测试插件导出 5 个效果");
		assert!(cache.find("org.quercus.test-plugin").is_some());
		assert_eq!(
			cache.binaries.lock().unwrap_or_else(|e| e.into_inner()).len(),
			1
		);

		// 同一 bundle 二次加载 → 路径去重（直接 dlclose 返回）。
		cache.load_bundle(&bundle);
		assert_eq!(cache.count(), 5);
		assert_eq!(
			cache.binaries.lock().unwrap_or_else(|e| e.into_inner()).len(),
			1
		);

		// at()/find() 边界。
		assert!(cache.at(0).is_some());
		assert!(cache.at(usize::MAX).is_none());
		assert!(cache.find("org.quercus.test-plugin.nope").is_none());

		let _ = std::fs::remove_dir_all(&root);
	}

	#[test]
	fn load_bundle_ignores_duplicate_plugin_identifiers() {
		let Some((root1, bundle1)) = make_fixture_bundle("dup-a") else {
			eprintln!("SKIP: 测试插件未构建");
			return;
		};
		let Some((root2, bundle2)) = make_fixture_bundle("dup-b") else {
			eprintln!("SKIP: 测试插件未构建");
			return;
		};
		let cache = test_cache();
		cache.load_bundle(&bundle1);
		assert_eq!(cache.count(), 5);
		// 第二个 bundle 的 identifier 全部重复 → 忽略，但二进制登记两份。
		cache.load_bundle(&bundle2);
		assert_eq!(cache.count(), 5);
		assert_eq!(
			cache.binaries.lock().unwrap_or_else(|e| e.into_inner()).len(),
			2
		);
		let _ = std::fs::remove_dir_all(&root1);
		let _ = std::fs::remove_dir_all(&root2);
	}

	/// A loadable library with no OFX export symbols → collect_plugins is empty (Linux
	/// libc).
	#[cfg(target_os = "linux")]
	#[test]
	fn collect_plugins_without_ofx_symbols_is_empty() {
		let Some(libc) = [
			"/lib/x86_64-linux-gnu/libc.so.6",
			"/usr/lib/x86_64-linux-gnu/libc.so.6",
			"/lib64/libc.so.6",
			"/usr/lib64/libc.so.6",
			"/lib/libc.so.6",
			"/usr/lib/libc.so.6",
		]
		.iter()
		.map(PathBuf::from)
		.find(|p| p.is_file()) else {
			eprintln!("SKIP: 未找到 libc");
			return;
		};
		let Some(handle) = dl_open(&libc) else {
			eprintln!("SKIP: libc 不可 dlopen");
			return;
		};
		let cache = test_cache();
		let plugins = unsafe { cache.collect_plugins(handle, Path::new("no-ofx")) };
		unsafe { dlclose(handle) };
		assert!(plugins.is_empty());
	}

	/// A loadable bundle with no plugins → logs no usable plugins and dlcloses.
	#[cfg(target_os = "linux")]
	#[test]
	fn load_bundle_without_ofx_plugins_reports_and_unloads() {
		let Some(libc) = [
			"/lib/x86_64-linux-gnu/libc.so.6",
			"/usr/lib/x86_64-linux-gnu/libc.so.6",
			"/lib64/libc.so.6",
			"/usr/lib64/libc.so.6",
			"/lib/libc.so.6",
			"/usr/lib/libc.so.6",
		]
		.iter()
		.map(PathBuf::from)
		.find(|p| p.is_file()) else {
			eprintln!("SKIP: 未找到 libc");
			return;
		};
		let dir = temp_dir("load-no-plugin");
		let platform = dir.join("Contents").join("Linux-x86-64");
		std::fs::create_dir_all(&platform).unwrap();
		std::os::unix::fs::symlink(&libc, platform.join("plugin")).unwrap();

		let cache = test_cache();
		cache.load_bundle(&dir);
		assert_eq!(cache.count(), 0);
		assert!(cache.binaries.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

		let _ = std::fs::remove_dir_all(&dir);
	}

	// ---- build_plugin 拒绝与上下文过滤 -----------------------------------

	unsafe extern "C" fn noop_set_host(_: *mut OfxHost) {}

	fn raw_ofx_plugin(
		api: Option<&CString>,
		api_version: c_int,
		identifier: Option<&CString>,
		set_host: Option<unsafe extern "C" fn(*mut OfxHost)>,
		main_entry: Option<EntryPoint>,
	) -> OfxPlugin {
		OfxPlugin {
			plugin_api: api.map_or(std::ptr::null(), |c| c.as_ptr()),
			api_version,
			plugin_identifier: identifier.map_or(std::ptr::null(), |c| c.as_ptr()),
			plugin_version_major: 1,
			plugin_version_minor: 0,
			set_host,
			main_entry,
		}
	}

	unsafe extern "C" fn entry_multi_contexts(
		_: *const c_char,
		handle: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if !handle.is_null() {
			let props = unsafe {
				&*crate::suites::tag::strip(handle as *mut c_void)
			};
			props.define(
				PROP_SUPPORTED_CONTEXTS,
				vec![
					Value::String(CString::new("OfxImageEffectContextGeneral").unwrap()),
					Value::String(CString::new("OfxImageEffectContextTransition").unwrap()),
					Value::String(CString::new("OfxImageEffectContextGenerator").unwrap()),
					Value::String(CString::new("OfxImageEffectContextFilter").unwrap()),
					Value::String(CString::new("OfxBogusContext").unwrap()),
				],
			);
		}
		status::OK
	}

	unsafe extern "C" fn entry_bogus_contexts(
		_: *const c_char,
		handle: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if !handle.is_null() {
			let props = unsafe {
				&*crate::suites::tag::strip(handle as *mut c_void)
			};
			props.define(
				PROP_SUPPORTED_CONTEXTS,
				vec![Value::String(CString::new("OfxImageEffectContextRetimer").unwrap())],
			);
		}
		status::OK
	}

	unsafe extern "C" fn entry_load_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, ACTION_LOAD) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	unsafe extern "C" fn entry_build_describe_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, ACTION_DESCRIBE) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	#[test]
	fn build_plugin_rejects_invalid_headers_and_filters_contexts() {
		let cache = test_cache();
		let bundle = Path::new("test-build.ofx.bundle");
		let api = CString::new("OfxImageEffectPluginAPI").unwrap();
		let wrong_api = CString::new("OfxSomethingElsePluginAPI").unwrap();
		let identifier = CString::new("test.build").unwrap();

		// NULL plugin_api → None。
		let mut ofx = raw_ofx_plugin(None, 1, Some(&identifier), None, Some(entry_ok));
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// API 名不符 → None。
		let mut ofx = raw_ofx_plugin(Some(&wrong_api), 1, Some(&identifier), None, Some(entry_ok));
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// API 版本不符 → None。
		let mut ofx = raw_ofx_plugin(Some(&api), 2, Some(&identifier), None, Some(entry_ok));
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// main_entry 缺 → None。
		let mut ofx = raw_ofx_plugin(Some(&api), 1, Some(&identifier), None, None);
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// load action 失败 → None（setHost 存在分支也被走到）。
		let mut ofx = raw_ofx_plugin(
			Some(&api),
			1,
			Some(&identifier),
			Some(noop_set_host),
			Some(entry_load_fails),
		);
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// describe action 失败 → None。
		let mut ofx = raw_ofx_plugin(
			Some(&api),
			1,
			Some(&identifier),
			None,
			Some(entry_build_describe_fails),
		);
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// describe 后无非标准上下文 → None。
		let mut ofx = raw_ofx_plugin(
			Some(&api),
			1,
			Some(&identifier),
			None,
			Some(entry_bogus_contexts),
		);
		assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

		// 四个标准上下文全收；非标准值被过滤。
		let mut ofx = raw_ofx_plugin(
			Some(&api),
			1,
			Some(&identifier),
			Some(noop_set_host),
			Some(entry_multi_contexts),
		);
		let plugin = unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }
			.expect("标准上下文应接受");
		assert_eq!(plugin.identifier, "test.build");
		assert_eq!(plugin.contexts.len(), 4);
		assert!(!plugin.contexts.iter().any(|c| c == "OfxBogusContext"));
		assert_eq!(plugin.version, (1, 0));
	}

	// ---- scan 环境变量 ----------------------------------------------------

	#[test]
	fn scan_reads_plugin_path_env_vars() {
		static ENV_LOCK: Mutex<()> = Mutex::new(());
		let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let Some((root, _bundle)) = make_fixture_bundle("scan-env") else {
			eprintln!("SKIP: 测试插件未构建");
			return;
		};

		let prev = std::env::var_os("OLIVE_OFX_PLUGIN_PATH");
		std::env::set_var("OLIVE_OFX_PLUGIN_PATH", &root);
		let cache = test_cache();
		let result = cache.scan();
		match prev {
			Some(v) => std::env::set_var("OLIVE_OFX_PLUGIN_PATH", v),
			None => std::env::remove_var("OLIVE_OFX_PLUGIN_PATH"),
		}
		result.expect("scan");
		assert!(
			cache.find("org.quercus.test-plugin").is_some(),
			"环境变量目录里的 bundle 应被发现"
		);
		let _ = std::fs::remove_dir_all(&root);
	}

	// ---- create_instance / create_instance_preferred ---------------------

	#[test]
	fn create_instance_preferred_uses_custom_context_fallback() {
		let host = local_host(vec![fake_plugin(
			"test.pref.custom",
			vec!["OfxImageEffectContextCustom".into()],
			entry_ok,
		)]);
		let (context, inst) = host
			.create_instance_preferred("test.pref.custom")
			.expect("自定义上下文可实例化");
		assert_eq!(context, "OfxImageEffectContextCustom");
		assert_eq!(inst.value.context, "OfxImageEffectContextCustom");
		// 实例属性表（init_instance_props）已建好。
		assert!(inst.value.props.get("OfxPropType", 0).is_some());
		// 未登记标识 → NotFound。
		assert!(host.create_instance_preferred("nope").is_err());
	}

	#[test]
	fn create_instance_preferred_returns_last_error() {
		let host = local_host(vec![fake_plugin(
			"test.pref.fail",
			vec![
				"OfxImageEffectContextFilter".into(),
				"OfxImageEffectContextGeneral".into(),
			],
			entry_describe_fails,
		)]);
		let err = host
			.create_instance_preferred("test.pref.fail")
			.err()
			.expect("全部上下文失败应返回错误");
		assert!(matches!(
			err,
			crate::error::Error::Failed(ref m) if m.contains("describeInContext")
		));
	}

	#[test]
	fn create_instance_rejects_unsupported_context_and_missing_contexts() {
		let host = local_host(vec![fake_plugin(
			"test.ctx.one",
			vec!["OfxImageEffectContextFilter".into()],
			entry_ok,
		)]);
		// 插件不支持请求的上下文。
		assert!(host
			.create_instance("test.ctx.one", Some("OfxImageEffectContextGenerator"))
			.is_err());
		// None → 取首个支持上下文并成功。
		let inst = host.create_instance("test.ctx.one", None).expect("首个上下文");
		assert_eq!(inst.value.context, "OfxImageEffectContextFilter");

		// 无支持上下文 → 失败。
		let none = local_host(vec![fake_plugin("test.ctx.none", vec![], entry_ok)]);
		assert!(none.create_instance("test.ctx.none", None).is_err());
		// 未登记标识 → 失败。
		assert!(none.create_instance("missing", None).is_err());

		// ReplyDefault 的 describeInContext/createInstance 也被接受。
		let default = local_host(vec![fake_plugin(
			"test.ctx.default",
			vec!["OfxImageEffectContextFilter".into()],
			entry_reply_default,
		)]);
		assert!(default.create_instance("test.ctx.default", None).is_ok());
	}

	#[test]
	fn create_instance_action_failure_is_not_registered_alive() {
		let host = local_host(vec![fake_plugin(
			"test.create.fail",
			vec!["OfxImageEffectContextFilter".into()],
			entry_create_fails,
		)]);
		let err = host
			.create_instance("test.create.fail", None)
			.err()
			.expect("createInstance 失败");
		assert!(matches!(
			err,
			crate::error::Error::Failed(ref m) if m.contains("createInstance")
		));
		assert_eq!(host.alive_count(), 0);
	}

	#[test]
	fn alive_count_prunes_released_instances() {
		let host = local_host(vec![fake_plugin(
			"test.alive",
			vec!["OfxImageEffectContextFilter".into()],
			entry_ok,
		)]);
		let a = host.create_instance("test.alive", None).expect("a");
		let b = host.create_instance("test.alive", None).expect("b");
		assert_eq!(host.alive_count(), 2);
		drop(a);
		assert_eq!(host.alive_count(), 1);
		drop(b);
		assert_eq!(host.alive_count(), 0);
	}

	/// `register_plugin_nodes` logs the reason and skips cache entries that cannot be
	/// instantiated, and registers instantiable ones as dynamic nodes.
	#[test]
	fn register_plugin_nodes_skips_uninstantiable_and_registers_ok_plugins() {
		static LOCK: Mutex<()> = Mutex::new(());
		let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

		let failing_id = "test.node-factory.fail";
		let ok_id = "test.node-factory.ok";
		{
			let mut plugins = Host::global()
				.cache
				.plugins
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			plugins.push(fake_plugin(failing_id, vec![], entry_ok));
			plugins.push(fake_plugin(
				ok_id,
				vec!["OfxImageEffectContextFilter".into()],
				entry_ok,
			));
		}

		let registered = crate::node_factory::register_plugin_nodes();

		{
			let mut plugins = Host::global()
				.cache
				.plugins
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			plugins.retain(|p| p.identifier != failing_id && p.identifier != ok_id);
		}

		assert!(
			registered.iter().any(|id| id == ok_id),
			"可实例化的插件应登记为动态节点"
		);
		assert!(
			!registered.iter().any(|id| id == failing_id),
			"无法实例化的插件应跳过"
		);
	}
}
