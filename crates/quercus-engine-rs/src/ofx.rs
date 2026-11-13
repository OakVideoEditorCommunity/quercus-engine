//! Safe wrapper over the engine's OFX plugin host C ABI
//! (`quercus_ofx_*`): plugin discovery/enumeration, instances with
//! parameter introspection/editing, headless frame rendering,
//! interacts, the GL bridge, progress/viewer-time plumbing, and
//! parametric curves.
//!
//! This mirrors the wiring the app does in `quercusui/ofx.rs`, plus the
//! headless surface functional plugins (batch processing,
//! AI-assisted editing driving effects) need. Instance identity is a
//! registry id; [`Instance`] releases it on drop. [`Interact`] and
//! [`CurveSet`] manage their engine-side handles on drop too.

use std::ffi::c_void;
use std::sync::Mutex;

use crate::codec::string_out;

#[cfg_attr(all(windows, feature = "engine-cdylib"),
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(all(not(windows), feature = "engine-cdylib"), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_ofx_scan() -> i32;
	fn quercus_ofx_scan_path(path: *const u8, path_len: usize) -> bool;
	fn quercus_ofx_plugin_count() -> i32;
	fn quercus_ofx_plugin_identifier_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_plugin_label_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_plugin_path_at(index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_plugin_version_at(index: i32, major: *mut u32, minor: *mut u32) -> bool;
	fn quercus_ofx_plugin_context_count_at(index: i32) -> i32;
	fn quercus_ofx_plugin_context_at(
		index: i32,
		context_index: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_ofx_shutdown();
	fn quercus_ofx_alive_count() -> i32;

	fn quercus_ofx_instance_create(
		plugin_id: *const u8,
		plugin_id_len: usize,
		context: *const u8,
		context_len: usize,
	) -> u64;
	fn quercus_ofx_instance_release(id: u64);
	fn quercus_ofx_instance_count() -> i32;
	fn quercus_ofx_instance_context(id: u64, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_instance_push_button(id: u64, param: *const u8, param_len: usize) -> bool;
	fn quercus_ofx_instance_set_cancelled(id: u64, cancelled: bool);
	fn quercus_ofx_instance_is_cancelled(id: u64) -> bool;

	fn quercus_ofx_register_plugin_nodes() -> i32;
	fn quercus_ofx_install_render_executor();
	fn quercus_ofx_set_project_extent(width: f64, height: f64);
	fn quercus_ofx_set_viewer_time(time: f64, range_min: f64, range_max: f64);
	fn quercus_ofx_cancel_render();

	fn quercus_ofx_set_progress_handler(
		cb: Option<ProgressFn>,
		end_cb: Option<ProgressEndFn>,
		userdata: *mut c_void,
	);
	fn quercus_ofx_has_progress_handler() -> bool;

	fn quercus_ofx_instance_persistent_message_count(id: u64) -> i32;
	fn quercus_ofx_instance_clear_persistent_messages(id: u64);

	fn quercus_ofx_instance_param_count(id: u64) -> i32;
	fn quercus_ofx_param_name_at(id: u64, index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_param_type_at(id: u64, index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_param_label_at(id: u64, index: i32, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_param_get_int(
		id: u64,
		name: *const u8,
		name_len: usize,
		out: *mut i32,
		cap: usize,
	) -> i32;
	fn quercus_ofx_param_set_int(
		id: u64,
		name: *const u8,
		name_len: usize,
		values: *const i32,
		dim: usize,
	) -> bool;
	fn quercus_ofx_param_get_double(
		id: u64,
		name: *const u8,
		name_len: usize,
		out: *mut f64,
		cap: usize,
	) -> i32;
	fn quercus_ofx_param_set_double(
		id: u64,
		name: *const u8,
		name_len: usize,
		values: *const f64,
		dim: usize,
	) -> bool;
	fn quercus_ofx_param_get_bool(id: u64, name: *const u8, name_len: usize) -> i32;
	fn quercus_ofx_param_set_bool(id: u64, name: *const u8, name_len: usize, value: bool) -> bool;
	fn quercus_ofx_param_get_choice(id: u64, name: *const u8, name_len: usize) -> i32;
	fn quercus_ofx_param_set_choice(id: u64, name: *const u8, name_len: usize, index: i32) -> bool;
	fn quercus_ofx_param_get_color(
		id: u64,
		name: *const u8,
		name_len: usize,
		out: *mut f64,
		cap: usize,
	) -> i32;
	fn quercus_ofx_param_set_color(
		id: u64,
		name: *const u8,
		name_len: usize,
		values: *const f64,
		dim: usize,
	) -> bool;
	fn quercus_ofx_param_get_string(
		id: u64,
		name: *const u8,
		name_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_ofx_param_set_string(
		id: u64,
		name: *const u8,
		name_len: usize,
		value: *const u8,
		value_len: usize,
	) -> bool;
	fn quercus_ofx_param_get_curves_json(
		id: u64,
		name: *const u8,
		name_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_ofx_param_set_curves_json(
		id: u64,
		name: *const u8,
		name_len: usize,
		json: *const u8,
		json_len: usize,
	) -> bool;

	fn quercus_ofx_instance_render_frame(
		id: u64,
		time: f64,
		src: *const f32,
		src_w: i32,
		src_h: i32,
		dst: *mut f32,
		dst_w: i32,
		dst_h: i32,
	) -> bool;
	fn quercus_ofx_instance_begin_sequence(id: u64, min: f64, max: f64) -> bool;
	fn quercus_ofx_instance_end_sequence(id: u64, min: f64, max: f64) -> bool;
	fn quercus_ofx_instance_get_rod(id: u64, time: f64, out: *mut QuercusOfxRectD) -> bool;

	fn quercus_ofx_interact_create(instance: u64) -> u64;
	fn quercus_ofx_interact_destroy(id: u64);
	fn quercus_ofx_interact_draw(
		id: u64,
		viewport_width: f64,
		viewport_height: f64,
		pixel_scale_x: f64,
		pixel_scale_y: f64,
		time: f64,
	) -> i32;
	fn quercus_ofx_interact_pen_motion(id: u64, x: f64, y: f64, pen_down: bool, time: f64) -> i32;
	fn quercus_ofx_interact_pen_down(id: u64, x: f64, y: f64, time: f64) -> i32;
	fn quercus_ofx_interact_pen_up(id: u64, x: f64, y: f64, time: f64) -> i32;
	fn quercus_ofx_interact_key_down(
		id: u64,
		key_sym: i32,
		key: *const u8,
		key_len: usize,
		time: f64,
	) -> i32;
	fn quercus_ofx_interact_key_up(
		id: u64,
		key_sym: i32,
		key: *const u8,
		key_len: usize,
		time: f64,
	) -> i32;
	fn quercus_ofx_interact_idle(id: u64) -> i32;
	fn quercus_ofx_interact_gain_focus(id: u64, time: f64) -> i32;
	fn quercus_ofx_interact_lose_focus(id: u64, time: f64) -> i32;
	fn quercus_ofx_interact_take_redraw(id: u64) -> bool;
	fn quercus_ofx_interact_take_swap(id: u64) -> bool;

	fn quercus_ofx_curveset_new() -> *mut c_void;
	fn quercus_ofx_curveset_add_ref(set: *const c_void);
	fn quercus_ofx_curveset_release(set: *const c_void);
	fn quercus_ofx_curveset_from_json(json: *const u8, json_len: usize) -> *mut c_void;
	fn quercus_ofx_curveset_to_json(set: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_ofx_curveset_curve_count(set: *const c_void) -> i32;
	fn quercus_ofx_curveset_add_curve(set: *const c_void) -> i32;
	fn quercus_ofx_curveset_add_identity_curve(set: *const c_void, lo: f64, hi: f64) -> i32;
	fn quercus_ofx_curveset_point_count(set: *const c_void, curve: i32) -> i32;
	fn quercus_ofx_curveset_point_at(
		set: *const c_void,
		curve: i32,
		index: i32,
		out: *mut QuercusOfxCurvePoint,
	) -> bool;
	fn quercus_ofx_curveset_upsert_point(set: *const c_void, curve: i32, key: f64, value: f64) -> bool;
	fn quercus_ofx_curveset_push_point(
		set: *const c_void,
		curve: i32,
		point: QuercusOfxCurvePoint,
	) -> bool;
	fn quercus_ofx_curveset_delete_point(set: *const c_void, curve: i32, index: i32) -> bool;
	fn quercus_ofx_curveset_clear_curve(set: *const c_void, curve: i32) -> bool;
	fn quercus_ofx_curveset_evaluate(set: *const c_void, curve: i32, x: f64) -> f64;

	fn quercus_ofx_gl_available() -> bool;
	fn quercus_ofx_gl_acquire() -> bool;
	fn quercus_ofx_gl_release();
	fn quercus_ofx_gl_create_output_texture(width: i32, height: i32) -> i32;
	fn quercus_ofx_gl_create_input_texture(width: i32, height: i32, pixels: *const f32) -> i32;
	fn quercus_ofx_gl_delete_texture(texture: i32);
	fn quercus_ofx_gl_create_fbo(texture: i32, width: i32, height: i32) -> i32;
	fn quercus_ofx_gl_bind_fbo(fbo: i32);
	fn quercus_ofx_gl_delete_fbo(fbo: i32);
	fn quercus_ofx_gl_set_viewport(width: i32, height: i32);
	fn quercus_ofx_gl_clear_color(r: f32, g: f32, b: f32, a: f32);
	fn quercus_ofx_gl_clear();
	fn quercus_ofx_gl_read_pixels_f32(
		width: i32,
		height: i32,
		out: *mut f32,
		out_capacity: usize,
	) -> i32;
	fn quercus_ofx_gl_version(buf: *mut u8, buf_len: usize) -> i32;
}

/// OFX action status codes (`kOfxStat*`); interact action entry
/// points return these.
pub mod status {
	/// The `OK` constant.
	pub const OK: i32 = 0;
	/// The `FAILED` constant.
	pub const FAILED: i32 = 1;
	/// The `ERR_FATAL` constant.
	pub const ERR_FATAL: i32 = 2;
	/// The `ERR_UNKNOWN` constant.
	pub const ERR_UNKNOWN: i32 = 3;
	/// The `ERR_MISSING_HOST_FEATURE` constant.
	pub const ERR_MISSING_HOST_FEATURE: i32 = 4;
	/// The `ERR_UNSUPPORTED` constant.
	pub const ERR_UNSUPPORTED: i32 = 5;
	/// The `ERR_EXISTS` constant.
	pub const ERR_EXISTS: i32 = 6;
	/// The `ERR_FORMAT` constant.
	pub const ERR_FORMAT: i32 = 7;
	/// The `ERR_MEMORY` constant.
	pub const ERR_MEMORY: i32 = 8;
	/// The `ERR_BAD_HANDLE` constant.
	pub const ERR_BAD_HANDLE: i32 = 9;
	/// The `ERR_BAD_INDEX` constant.
	pub const ERR_BAD_INDEX: i32 = 10;
	/// The `ERR_VALUE` constant.
	pub const ERR_VALUE: i32 = 11;
	/// The `REPLY_YES` constant.
	pub const REPLY_YES: i32 = 12;
	/// The `REPLY_NO` constant.
	pub const REPLY_NO: i32 = 13;
	/// The `REPLY_DEFAULT` constant.
	pub const REPLY_DEFAULT: i32 = 14;
}

/// ofxKeySyms values for [`Interact::key_down`]/[`Interact::key_up`]
/// (printable ASCII keys carry their ASCII code as the symbol).
pub mod key {
	/// The `UNKNOWN` constant.
	pub const UNKNOWN: i32 = 0x0;
	/// The `BACKSPACE` constant.
	pub const BACKSPACE: i32 = 0xFF08;
	/// The `TAB` constant.
	pub const TAB: i32 = 0xFF09;
	/// The `RETURN` constant.
	pub const RETURN: i32 = 0xFF0D;
	/// The `ESCAPE` constant.
	pub const ESCAPE: i32 = 0xFF1B;
	/// The `DELETE` constant.
	pub const DELETE: i32 = 0xFFFF;
	/// The `HOME` constant.
	pub const HOME: i32 = 0xFF50;
	/// The `LEFT` constant.
	pub const LEFT: i32 = 0xFF51;
	/// The `UP` constant.
	pub const UP: i32 = 0xFF52;
	/// The `RIGHT` constant.
	pub const RIGHT: i32 = 0xFF53;
	/// The `DOWN` constant.
	pub const DOWN: i32 = 0xFF54;
	/// The `PAGE_UP` constant.
	pub const PAGE_UP: i32 = 0xFF55;
	/// The `PAGE_DOWN` constant.
	pub const PAGE_DOWN: i32 = 0xFF56;
	/// The `END` constant.
	pub const END: i32 = 0xFF57;
	/// The `F1` constant.
	pub const F1: i32 = 0xFFBE;
	/// The `SHIFT_L` constant.
	pub const SHIFT_L: i32 = 0xFFE1;
	/// The `CONTROL_L` constant.
	pub const CONTROL_L: i32 = 0xFFE3;
	/// The `ALT_L` constant.
	pub const ALT_L: i32 = 0xFFE9;
	/// The `SPACE` constant.
	pub const SPACE: i32 = 0x020;
	/// The `A` constant.
	pub const A: i32 = 0x061;
	/// The `Z` constant.
	pub const Z: i32 = 0x07a;
}

// ---------------------------------------------------------------------------
// Discovery / enumeration
// ---------------------------------------------------------------------------

/// Scans the standard OFX plugin paths. None when the scan failed
/// (plugins are optional; a failure never aborts the caller).
pub fn scan() -> Option<usize> {
	let n = unsafe { quercus_ofx_scan() };
	(n >= 0).then_some(n as usize)
}

/// Scans one extra plugin directory on top of the standard OFX search
/// paths, loading any bundles found into the discovered set.
///
/// Returns `false` only on invalid input (a path that is not valid UTF-8
/// or contains NUL); a path that is not a directory, or one already
/// scanned, is a silent no-op returning `true`. Check
/// [`plugins`]/[`plugin_count`] for the resulting list.
///
/// # Arguments
///
/// * `path` - filesystem path of the directory to scan, as UTF-8 text.
pub fn scan_path(path: &str) -> bool {
	unsafe { quercus_ofx_scan_path(path.as_ptr(), path.len()) }
}

/// A discovered plugin's metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginInfo {
	/// Unique reverse-DNS identifier.
	pub identifier: String,
	/// Display label.
	pub label: String,
	/// (major, minor).
	pub version: (u32, u32),
	/// Bundle path.
	pub path: String,
	/// Supported contexts (`OfxImageEffectContext*`).
	pub contexts: Vec<String>,
}

/// Number of discovered plugins.
pub fn plugin_count() -> usize {
	unsafe { quercus_ofx_plugin_count() }.max(0) as usize
}

/// The metadata of the plugin at `index` in scan order, or `None` when
/// `index` is out of range or one of its metadata fields cannot be
/// read.
///
/// Queries the engine once per field; use [`plugins`] to fetch them all
/// at once.
///
/// # Arguments
///
/// * `index` - zero-based index into the discovered-plugin list.
pub fn plugin_at(index: usize) -> Option<PluginInfo> {
	if index >= plugin_count() {
		return None;
	}
	let i = index as i32;
	let identifier = string_out(|buf, len| unsafe { quercus_ofx_plugin_identifier_at(i, buf, len) })?;
	let label = string_out(|buf, len| unsafe { quercus_ofx_plugin_label_at(i, buf, len) })?;
	let path = string_out(|buf, len| unsafe { quercus_ofx_plugin_path_at(i, buf, len) })?;
	let (mut major, mut minor) = (0u32, 0u32);
	if !unsafe { quercus_ofx_plugin_version_at(i, &mut major, &mut minor) } {
		return None;
	}
	let context_count = unsafe { quercus_ofx_plugin_context_count_at(i) }.max(0);
	let contexts = (0..context_count)
		.filter_map(|c| string_out(|buf, len| unsafe { quercus_ofx_plugin_context_at(i, c, buf, len) }))
		.collect();
	Some(PluginInfo {
		identifier,
		label,
		version: (major, minor),
		path,
		contexts,
	})
}

/// All discovered plugins in scan order, built by calling [`plugin_at`]
/// for every index.
///
/// Plugins whose metadata cannot be read are skipped rather than
/// failing the whole list; run [`scan`] or [`scan_path`] first so there
/// is something to enumerate.
pub fn plugins() -> Vec<PluginInfo> {
	(0..plugin_count()).filter_map(plugin_at).collect()
}

/// Shuts the host down (destroys live instances, unloads plugins).
pub fn shutdown() {
	unsafe { quercus_ofx_shutdown() }
}

/// Live instance count (leak diagnostic).
pub fn alive_count() -> usize {
	unsafe { quercus_ofx_alive_count() }.max(0) as usize
}

/// Registers every discovered plugin into the node factory as a
/// dynamic node type (idempotent; also installs the render executor
/// and the plugin-node duplicator). Returns the number of newly
/// registered node types.
pub fn register_plugin_nodes() -> usize {
	unsafe { quercus_ofx_register_plugin_nodes() }.max(0) as usize
}

/// Installs the render executor / plugin-node duplicator (idempotent).
pub fn install_render_executor() {
	unsafe { quercus_ofx_install_render_executor() }
}

/// Pushes the project extent (canvas size in canonical pixels) that the
/// OFX normalised-coordinate default conversion maps the `[0, 1]` range
/// onto.
///
/// Call it whenever the project resolution changes; this only affects
/// plugin parameters whose normalised-coordinate defaults still need
/// converting. Each component is clamped to at least `1.0`.
///
/// # Arguments
///
/// * `width` - project width in canonical pixels.
/// * `height` - project height in canonical pixels.
pub fn set_project_extent(width: f64, height: f64) {
	unsafe { quercus_ofx_set_project_extent(width, height) }
}

/// Updates the active-viewer time snapshot the timeline suite falls
/// back to when no render context is live.
pub fn set_viewer_time(time: f64, range_min: f64, range_max: f64) {
	unsafe { quercus_ofx_set_viewer_time(time, range_min, range_max) }
}

/// Cancels inline and worker plugin renders at their next progress boundary.
pub fn cancel_render() {
	unsafe { quercus_ofx_cancel_render() }
}

// ---------------------------------------------------------------------------
// Progress handler
// ---------------------------------------------------------------------------

type ProgressFn = unsafe extern "C" fn(
	label: *const std::ffi::c_char,
	message: *const std::ffi::c_char,
	fraction: f64,
	userdata: *mut c_void,
) -> bool;
type ProgressEndFn = unsafe extern "C" fn(userdata: *mut c_void);

struct ProgressCallbacks {
	on_progress: Box<dyn Fn(&str, &str, f64) -> bool + Send>,
	on_end: Box<dyn Fn() + Send>,
}

// The boxed callbacks live in this slot for the whole registration;
// the engine hands the pointer back as userdata on every event.
// (Stored as usize: raw pointers aren't Send; the pointer targets a
// Send closure pair and the slot is mutex-guarded.)
static PROGRESS_CALLBACKS: Mutex<Option<usize>> = Mutex::new(None);

/// C-ABI callback the engine invokes on every plugin `progressUpdate`,
/// decoding the label/message strings and forwarding them to the
/// registered `on_progress` closure.
///
/// Returns the closure's `true` to continue the render or `false` to
/// request cancellation; a null `userdata` (no registration) returns
/// `true`.
///
/// # Safety
///
/// `userdata` must be null or the `*mut ProgressCallbacks` registered
/// with the engine via [`set_progress_handler`], and must stay alive for
/// the whole registration. `label` and `message` must be null or valid
/// NUL-terminated C strings for the duration of the call.
unsafe extern "C" fn progress_trampoline(
	label: *const std::ffi::c_char,
	message: *const std::ffi::c_char,
	fraction: f64,
	userdata: *mut c_void,
) -> bool {
	if userdata.is_null() {
		return true;
	}
	let cb = unsafe { &*userdata.cast::<ProgressCallbacks>() };
	let label = if label.is_null() {
		""
	} else {
		unsafe { std::ffi::CStr::from_ptr(label) }
			.to_str()
			.unwrap_or("")
	};
	let message = if message.is_null() {
		""
	} else {
		unsafe { std::ffi::CStr::from_ptr(message) }
			.to_str()
			.unwrap_or("")
	};
	(cb.on_progress)(label, message, fraction)
}

/// C-ABI callback the engine invokes when a plugin render sequence ends
/// (`progressEnd`), forwarding to the registered `on_end` closure.
///
/// # Safety
///
/// `userdata` must be null or the `*mut ProgressCallbacks` registered
/// with the engine via [`set_progress_handler`]; a non-null pointer must
/// stay alive for the whole registration (it is owned by
/// `PROGRESS_CALLBACKS` until the handler is replaced or cleared).
unsafe extern "C" fn end_trampoline(userdata: *mut c_void) {
	if userdata.is_null() {
		return;
	}
	let cb = unsafe { &*userdata.cast::<ProgressCallbacks>() };
	(cb.on_end)();
}

/// Registers the progress handler: `on_progress(label, message,
/// fraction)` fires on every plugin progressUpdate (return false to
/// request cancellation), `on_end()` on progressEnd. Re-registering
/// replaces the previous handler.
pub fn set_progress_handler(
	on_progress: impl Fn(&str, &str, f64) -> bool + Send + 'static,
	on_end: impl Fn() + Send + 'static,
) {
	let callbacks = Box::into_raw(Box::new(ProgressCallbacks {
		on_progress: Box::new(on_progress),
		on_end: Box::new(on_end),
	}));
	let mut slot = PROGRESS_CALLBACKS.lock().unwrap_or_else(|e| e.into_inner());
	if let Some(old) = slot.take() {
		unsafe { drop(Box::from_raw(old as *mut ProgressCallbacks)) };
	}
	*slot = Some(callbacks as usize);
	unsafe {
		quercus_ofx_set_progress_handler(
			Some(progress_trampoline),
			Some(end_trampoline),
			callbacks as *mut c_void,
		)
	};
}

/// Unregisters the progress handler installed by
/// [`set_progress_handler`] and drops the stored callbacks.
///
/// Safe to call when no handler is registered; afterwards the engine
/// reports no further `progressUpdate`/`progressEnd` events to this
/// process.
pub fn clear_progress_handler() {
	unsafe { quercus_ofx_set_progress_handler(None, None, std::ptr::null_mut()) };
	if let Some(old) = PROGRESS_CALLBACKS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.take()
	{
		unsafe { drop(Box::from_raw(old as *mut ProgressCallbacks)) };
	}
}

/// Whether a progress handler is registered.
pub fn has_progress_handler() -> bool {
	unsafe { quercus_ofx_has_progress_handler() }
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

/// A live OFX effect instance (registry id). Drop unregisters it; the
/// instance is destroyed once no references remain.
#[derive(Debug)]
pub struct Instance {
	id: u64,
}

/// Number of registered instances (diagnostic).
pub fn instance_count() -> usize {
	unsafe { quercus_ofx_instance_count() }.max(0) as usize
}

/// Reads an existing instance's persistent-message count without taking
/// ownership of its registry handle.
pub fn persistent_message_count(instance: u64) -> Option<usize> {
	let count = unsafe { quercus_ofx_instance_persistent_message_count(instance) };
	(count >= 0).then_some(count as usize)
}

impl Instance {
	/// Creates an instance of `plugin_id`. `None` context uses the
	/// host's preferred-context fallback chain (filter → general →
	/// tracker → paint → rest). None on failure (unknown plugin, no
	/// supported context, plugin error).
	pub fn create(plugin_id: &str, context: Option<&str>) -> Option<Instance> {
		let (ctx, ctx_len) = match context {
			Some(c) => (c.as_ptr(), c.len()),
			None => (std::ptr::null(), 0),
		};
		let id =
			unsafe { quercus_ofx_instance_create(plugin_id.as_ptr(), plugin_id.len(), ctx, ctx_len) };
		(id != 0).then_some(Instance { id })
	}

	/// The registry id.
	pub fn id(&self) -> u64 {
		self.id
	}

	/// The context the instance was created in.
	pub fn context(&self) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_ofx_instance_context(self.id, buf, len) })
	}

	/// Triggers a push-button parameter (the inspector's button path).
	pub fn push_button(&self, param: &str) -> bool {
		unsafe { quercus_ofx_instance_push_button(self.id, param.as_ptr(), param.len()) }
	}

	/// Sets/clears the cancel flag: a running render aborts at the
	/// next progress batch boundary.
	pub fn set_cancelled(&self, cancelled: bool) {
		unsafe { quercus_ofx_instance_set_cancelled(self.id, cancelled) }
	}

	/// Whether the cancel flag is set.
	pub fn is_cancelled(&self) -> bool {
		unsafe { quercus_ofx_instance_is_cancelled(self.id) }
	}

	/// Number of parameters.
	pub fn param_count(&self) -> Option<usize> {
		let n = unsafe { quercus_ofx_instance_param_count(self.id) };
		(n >= 0).then_some(n as usize)
	}

	/// The internal name of the parameter at `index`, in definition
	/// order; this is the key accepted by the `param_*`/`set_param_*`
	/// accessors.
	///
	/// Returns `None` when `index` is out of range.
	///
	/// # Arguments
	///
	/// * `index` - zero-based parameter index in definition order.
	pub fn param_name_at(&self, index: usize) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_ofx_param_name_at(self.id, index as i32, buf, len) })
	}

	/// The parameter's OFX type string (`kOfxParamType*`) at `index`.
	pub fn param_type_at(&self, index: usize) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_ofx_param_type_at(self.id, index as i32, buf, len) })
	}

	/// The display label (`kOfxPropLabel`) of the parameter at `index`, as
	/// shown in the inspector.
	///
	/// Returns `None` when `index` is out of range. Use
	/// [`Self::param_name_at`] for the stable internal name.
	///
	/// # Arguments
	///
	/// * `index` - zero-based parameter index in definition order.
	pub fn param_label_at(&self, index: usize) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_ofx_param_label_at(self.id, index as i32, buf, len) })
	}

	/// Reads an integer-family parameter (Integer/Integer2D/Integer3D).
	/// None on unknown name or a type mismatch.
	pub fn param_int(&self, name: &str) -> Option<Vec<i32>> {
		let dim = unsafe {
			quercus_ofx_param_get_int(self.id, name.as_ptr(), name.len(), std::ptr::null_mut(), 0)
		};
		if dim < 0 {
			return None;
		}
		let mut out = vec![0i32; dim as usize];
		unsafe {
			quercus_ofx_param_get_int(
				self.id,
				name.as_ptr(),
				name.len(),
				out.as_mut_ptr(),
				out.len(),
			)
		};
		Some(out)
	}

	/// Writes an integer-family parameter. False on unknown name or a
	/// type/arity mismatch.
	pub fn set_param_int(&self, name: &str, values: &[i32]) -> bool {
		unsafe {
			quercus_ofx_param_set_int(
				self.id,
				name.as_ptr(),
				name.len(),
				values.as_ptr(),
				values.len(),
			)
		}
	}

	/// Reads a double-family parameter (Double/Double2D/Double3D),
	/// returning one value per dimension in component order.
	///
	/// Returns `None` on an unknown parameter name or a type mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_double(&self, name: &str) -> Option<Vec<f64>> {
		let dim = unsafe {
			quercus_ofx_param_get_double(self.id, name.as_ptr(), name.len(), std::ptr::null_mut(), 0)
		};
		if dim < 0 {
			return None;
		}
		let mut out = vec![0.0f64; dim as usize];
		unsafe {
			quercus_ofx_param_get_double(
				self.id,
				name.as_ptr(),
				name.len(),
				out.as_mut_ptr(),
				out.len(),
			)
		};
		Some(out)
	}

	/// Writes a double-family parameter (Double/Double2D/Double3D) from
	/// `values`; the length must equal the parameter's arity (1..=3).
	///
	/// Returns `false` on an unknown parameter name or a type/arity
	/// mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `values` - one value per dimension, in component order.
	pub fn set_param_double(&self, name: &str, values: &[f64]) -> bool {
		unsafe {
			quercus_ofx_param_set_double(
				self.id,
				name.as_ptr(),
				name.len(),
				values.as_ptr(),
				values.len(),
			)
		}
	}

	/// Reads a boolean parameter.
	///
	/// Returns `None` on an unknown parameter name or a type mismatch
	/// (the ABI's `-1` sentinel).
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_bool(&self, name: &str) -> Option<bool> {
		match unsafe { quercus_ofx_param_get_bool(self.id, name.as_ptr(), name.len()) } {
			0 => Some(false),
			1 => Some(true),
			_ => None,
		}
	}

	/// Writes a boolean parameter.
	///
	/// Returns `false` on an unknown parameter name or a type mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `value` - new boolean state.
	pub fn set_param_bool(&self, name: &str, value: bool) -> bool {
		unsafe { quercus_ofx_param_set_bool(self.id, name.as_ptr(), name.len(), value) }
	}

	/// Reads a choice parameter as its zero-based option index.
	///
	/// Returns `None` on an unknown parameter name or a type mismatch
	/// (the underlying ABI reports `-1`).
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_choice(&self, name: &str) -> Option<i32> {
		let v = unsafe { quercus_ofx_param_get_choice(self.id, name.as_ptr(), name.len()) };
		(v >= 0).then_some(v)
	}

	/// Writes a choice parameter by option index (the position in the
	/// parameter's option menu, not its string).
	///
	/// Returns `false` on an unknown parameter name or a type mismatch;
	/// `index` is passed through without range checking here.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `index` - zero-based selected option index.
	pub fn set_param_choice(&self, name: &str, index: i32) -> bool {
		unsafe { quercus_ofx_param_set_choice(self.id, name.as_ptr(), name.len(), index) }
	}

	/// Reads a color parameter, returning 3 components for RGB and 4 for
	/// RGBA (the parameter's arity).
	///
	/// Returns `None` on an unknown parameter name or a type mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_color(&self, name: &str) -> Option<Vec<f64>> {
		let dim = unsafe {
			quercus_ofx_param_get_color(self.id, name.as_ptr(), name.len(), std::ptr::null_mut(), 0)
		};
		if dim < 0 {
			return None;
		}
		let mut out = vec![0.0f64; dim as usize];
		unsafe {
			quercus_ofx_param_get_color(
				self.id,
				name.as_ptr(),
				name.len(),
				out.as_mut_ptr(),
				out.len(),
			)
		};
		Some(out)
	}

	/// Writes a color parameter from `values` (3 components for RGB, 4 for
	/// RGBA, matching the parameter's arity).
	///
	/// Returns `false` on an unknown parameter name or a type/arity
	/// mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `values` - red, green, blue[, alpha] components.
	pub fn set_param_color(&self, name: &str, values: &[f64]) -> bool {
		unsafe {
			quercus_ofx_param_set_color(
				self.id,
				name.as_ptr(),
				name.len(),
				values.as_ptr(),
				values.len(),
			)
		}
	}

	/// Reads a string-family parameter (String/StrChoice) as UTF-8 text.
	///
	/// Returns `None` on an unknown parameter name or a type mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_string(&self, name: &str) -> Option<String> {
		string_out(|buf, len| unsafe {
			quercus_ofx_param_get_string(self.id, name.as_ptr(), name.len(), buf, len)
		})
	}

	/// Writes a string-family parameter (String/StrChoice), replacing its
	/// current text.
	///
	/// Returns `false` on an unknown parameter name or a type mismatch.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `value` - UTF-8 text to store; borrowed only for the call.
	pub fn set_param_string(&self, name: &str, value: &str) -> bool {
		unsafe {
			quercus_ofx_param_set_string(
				self.id,
				name.as_ptr(),
				name.len(),
				value.as_ptr(),
				value.len(),
			)
		}
	}

	/// Reads a parametric parameter as a curve set, parsed from the
	/// canonical curve-set JSON the plugin returns.
	///
	/// Returns `None` on an unknown parameter name, a type mismatch, or
	/// JSON that fails to parse. Counterpart of [`Self::set_param_curves`].
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	pub fn param_curves(&self, name: &str) -> Option<CurveSet> {
		let json = string_out(|buf, len| unsafe {
			quercus_ofx_param_get_curves_json(self.id, name.as_ptr(), name.len(), buf, len)
		})?;
		CurveSet::from_json(&json)
	}

	/// Writes a parametric parameter from `curves`, serialising them to
	/// the canonical curve-set JSON that the plugin's `set` action reads.
	///
	/// Returns `false` on an unknown parameter name, a type mismatch, or
	/// JSON the plugin rejects.
	///
	/// # Arguments
	///
	/// * `name` - internal parameter name (not its display label).
	/// * `curves` - curve set to store, one curve per parametric
	///   dimension; slopes are serialised too.
	pub fn set_param_curves(&self, name: &str, curves: &CurveSet) -> bool {
		let json = curves.to_json();
		unsafe {
			quercus_ofx_param_set_curves_json(
				self.id,
				name.as_ptr(),
				name.len(),
				json.as_ptr(),
				json.len(),
			)
		}
	}

	/// The persistent message count (the inspector badge data).
	pub fn persistent_message_count(&self) -> Option<usize> {
		let n = unsafe { quercus_ofx_instance_persistent_message_count(self.id) };
		(n >= 0).then_some(n as usize)
	}

	/// Clears the instance's persistent messages (the badge/message data
	/// the inspector shows), e.g. when the instance is released or the
	/// host shuts down.
	///
	/// Unknown instance ids are a silent no-op.
	pub fn clear_persistent_messages(&self) {
		unsafe { quercus_ofx_instance_clear_persistent_messages(self.id) }
	}

	/// Renders one frame through the effect (CPU path): `src` is the
	/// input frame (tight RGBA F32; `None` for sources/generators),
	/// the output is written to `dst` (tight RGBA F32, must hold
	/// `dst_w*dst_h*4` floats). False on failure (invalid buffers,
	/// cancelled, plugin error). Set parameters first; long renders
	/// report progress through the registered handler.
	pub fn render_frame(
		&self,
		time: f64,
		src: Option<(&[f32], u32, u32)>,
		dst: &mut [f32],
		dst_size: (u32, u32),
	) -> bool {
		let (dst_w, dst_h) = dst_size;
		if dst_w == 0 || dst_h == 0 || dst.len() < (dst_w * dst_h * 4) as usize {
			return false;
		}
		let (src_ptr, src_w, src_h) = match src {
			Some((pixels, w, h)) => {
				if w == 0 || h == 0 || pixels.len() < (w * h * 4) as usize {
					return false;
				}
				(pixels.as_ptr(), w as i32, h as i32)
			}
			None => (std::ptr::null(), 0, 0),
		};
		unsafe {
			quercus_ofx_instance_render_frame(
				self.id,
				time,
				src_ptr,
				src_w,
				src_h,
				dst.as_mut_ptr(),
				dst_w as i32,
				dst_h as i32,
			)
		}
	}

	/// The beginSequenceRender bracket (CPU mode): opens a sequence
	/// render over `[min, max]` seconds. Pair with
	/// [`Instance::end_sequence`].
	pub fn begin_sequence(&self, min: f64, max: f64) -> bool {
		unsafe { quercus_ofx_instance_begin_sequence(self.id, min, max) }
	}

	/// Closes the `endSequenceRender` bracket opened by
	/// [`Self::begin_sequence`], reporting the rendered `[min, max]` range so
	/// the plugin can flush per-sequence caches and resources.
	///
	/// Returns `false` on an unknown instance or a plugin error; call it
	/// once per successful `begin_sequence`.
	///
	/// # Arguments
	///
	/// * `min` - first frame time of the rendered range, in seconds.
	/// * `max` - last frame time of the rendered range, in seconds.
	pub fn end_sequence(&self, min: f64, max: f64) -> bool {
		unsafe { quercus_ofx_instance_end_sequence(self.id, min, max) }
	}

	/// The instance's region of definition at `time` — the canonical
	/// (render-scale 1:1) rectangle the plugin renders into, used to size
	/// and position the node's output.
	///
	/// Returns `None` for an unknown instance or when the plugin fails the
	/// `getRegionOfDefinition` action.
	///
	/// # Arguments
	///
	/// * `time` - timeline position in seconds at which to query.
	pub fn region_of_definition(&self, time: f64) -> Option<RectD> {
		let mut out = QuercusOfxRectD::default();
		unsafe { quercus_ofx_instance_get_rod(self.id, time, &mut out) }.then(|| out.into())
	}

	/// Creates the overlay interact (None when the plugin declares
	/// none). The interact is destroyed on drop.
	pub fn create_interact(&self) -> Option<Interact> {
		let id = unsafe { quercus_ofx_interact_create(self.id) };
		(id != 0).then_some(Interact { id })
	}
}

impl Drop for Instance {
	fn drop(&mut self) {
		unsafe { quercus_ofx_instance_release(self.id) };
	}
}

/// A rectangle in canonical coordinates.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectD {
	/// The `x1` field.
	pub x1: f64,
	/// The `y1` field.
	pub y1: f64,
	/// The `x2` field.
	pub x2: f64,
	/// The `y2` field.
	pub y2: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct QuercusOfxRectD {
	x1: f64,
	y1: f64,
	x2: f64,
	y2: f64,
}

impl From<QuercusOfxRectD> for RectD {
	fn from(r: QuercusOfxRectD) -> Self {
		Self {
			x1: r.x1,
			y1: r.y1,
			x2: r.x2,
			y2: r.y2,
		}
	}
}

// ---------------------------------------------------------------------------
// Interacts
// ---------------------------------------------------------------------------

/// Creates an overlay interact from an existing instance registry id. The
/// caller owns the returned interact; the instance remains owned by its node.
pub fn create_interact(instance: u64) -> Option<Interact> {
	let id = unsafe { quercus_ofx_interact_create(instance) };
	(id != 0).then_some(Interact { id })
}

/// A plugin's overlay interact (custom UI on the viewer). Drop
/// destroys it. Action methods return OFX status codes ([`status`]).
#[derive(Debug)]
pub struct Interact {
	id: u64,
}

impl Interact {
	/// The engine registry id.
	pub fn id(&self) -> u64 {
		self.id
	}

	/// Explicitly destroys the interact now. Other clones become inert, and
	/// their later drops are no-ops.
	pub fn destroy(&self) {
		unsafe { quercus_ofx_interact_destroy(self.id) }
	}

	/// The draw action: the plugin paints its overlay into the
	/// currently bound framebuffer (acquire the GL bridge first — see
	/// [`gl`]). `viewport` is the viewport size in pixels,
	/// `pixel_scale` the canonical→pixel ratio. No background image is
	/// passed; composite the overlay over the frame yourself.
	pub fn draw(&self, viewport: (f64, f64), pixel_scale: (f64, f64), time: f64) -> i32 {
		unsafe {
			quercus_ofx_interact_draw(
				self.id,
				viewport.0,
				viewport.1,
				pixel_scale.0,
				pixel_scale.1,
				time,
			)
		}
	}

	/// The pen motion action (`pen_down` maps to pressure 1.0/0.0).
	pub fn pen_motion(&self, position: (f64, f64), pen_down: bool, time: f64) -> i32 {
		unsafe { quercus_ofx_interact_pen_motion(self.id, position.0, position.1, pen_down, time) }
	}

	/// The pen down action: tells the plugin the pen was pressed at
	/// `position` and `time` (OFX `kOfxInteractActionPenDown`), reporting
	/// full pressure `1.0`.
	///
	/// Returns the plugin's OFX status code. Pair with [`Self::pen_up`].
	///
	/// # Arguments
	///
	/// * `position` - pen position in viewport pixels (`x`, `y`), in the
	///   coordinate space of the last [`Self::draw`] viewport.
	/// * `time` - timeline position in seconds of the event.
	pub fn pen_down(&self, position: (f64, f64), time: f64) -> i32 {
		unsafe { quercus_ofx_interact_pen_down(self.id, position.0, position.1, time) }
	}

	/// The pen up action: tells the plugin the pen was lifted at
	/// `position` and `time` (OFX `kOfxInteractActionPenUp`), closing the
	/// pair opened by [`Self::pen_down`].
	///
	/// Returns the plugin's OFX status code.
	///
	/// # Arguments
	///
	/// * `position` - pen position in viewport pixels (`x`, `y`), in the
	///   coordinate space of the last [`Self::draw`] viewport.
	/// * `time` - timeline position in seconds of the event.
	pub fn pen_up(&self, position: (f64, f64), time: f64) -> i32 {
		unsafe { quercus_ofx_interact_pen_up(self.id, position.0, position.1, time) }
	}

	/// The key down action (`key_sym` is an ofxKeySyms value — see
	/// [`key`]; `key_string` the character for printable keys).
	pub fn key_down(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		unsafe {
			quercus_ofx_interact_key_down(
				self.id,
				key_sym,
				key_string.as_ptr(),
				key_string.len(),
				time,
			)
		}
	}

	/// The key up action: tells the plugin a key was released at `time`
	/// (OFX `kOfxInteractActionKeyUp`), completing the pair opened by
	/// [`Self::key_down`].
	///
	/// Returns the plugin's OFX status code.
	///
	/// # Arguments
	///
	/// * `key_sym` - `ofxKeySyms` code of the released key, the same
	///   value passed to the matching `key_down`.
	/// * `key_string` - UTF-8 text of the printable character, empty for
	///   keys that have none.
	/// * `time` - timeline position in seconds of the event.
	pub fn key_up(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		unsafe {
			quercus_ofx_interact_key_up(
				self.id,
				key_sym,
				key_string.as_ptr(),
				key_string.len(),
				time,
			)
		}
	}

	/// The idle action (throttle to ~10 Hz).
	pub fn idle(&self) -> i32 {
		unsafe { quercus_ofx_interact_idle(self.id) }
	}

	/// Notifies the plugin that its overlay gained viewer focus at
	/// `time`; the pen and key actions are only meaningful while the
	/// interact holds focus.
	///
	/// Returns the plugin's OFX status code. Pair with [`Self::lose_focus`];
	/// the interact must already exist ([`create_interact`]).
	///
	/// # Arguments
	///
	/// * `time` - timeline position in seconds at which focus was gained.
	pub fn gain_focus(&self, time: f64) -> i32 {
		unsafe { quercus_ofx_interact_gain_focus(self.id, time) }
	}

	/// Notifies the plugin that its overlay lost viewer focus at `time`,
	/// so it can dismiss transient UI. Pair with [`Self::gain_focus`].
	///
	/// Returns the plugin's OFX status code (`kOfxStatErrBadHandle` for
	/// an unknown interact).
	///
	/// # Arguments
	///
	/// * `time` - timeline position in seconds at which focus was lost.
	pub fn lose_focus(&self, time: f64) -> i32 {
		unsafe { quercus_ofx_interact_lose_focus(self.id, time) }
	}

	/// Takes the redraw-requested flag (set by the plugin via
	/// interactRedraw).
	pub fn take_redraw(&self) -> bool {
		unsafe { quercus_ofx_interact_take_redraw(self.id) }
	}

	/// Takes the swap-buffers-requested flag.
	pub fn take_swap(&self) -> bool {
		unsafe { quercus_ofx_interact_take_swap(self.id) }
	}
}

impl Drop for Interact {
	fn drop(&mut self) {
		unsafe { quercus_ofx_interact_destroy(self.id) };
	}
}

// ---------------------------------------------------------------------------
// Parametric curves
// ---------------------------------------------------------------------------

/// A curve control point: `key` is the parametric position, `value`
/// the result, `slope` the first derivative (Hermite tangent;
/// recomputed automatically by the mutating calls).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControlPoint {
	/// The `key` field.
	pub key: f64,
	/// The `value` field.
	pub value: f64,
	/// The `slope` field.
	pub slope: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct QuercusOfxCurvePoint {
	key: f64,
	value: f64,
	slope: f64,
}

/// A set of parametric curves (one per dimension of a parametric
/// parameter) — a refcounted engine handle; cloning shares the set.
/// JSON is the interchange format with node values and project files.
pub struct CurveSet {
	ptr: *mut c_void,
}

// The engine side is mutex-guarded; the refcount is atomic.
unsafe impl Send for CurveSet {}
unsafe impl Sync for CurveSet {}

impl CurveSet {
	/// A new, empty curve set.
	pub fn new() -> Self {
		Self {
			ptr: unsafe { quercus_ofx_curveset_new() },
		}
	}

	/// Parses curve-set JSON (as produced by [`Self::to_json`] or the C++/node
	/// side) into a new refcounted set.
	///
	/// Returns `None` when the text is not valid curve-set JSON; `json`
	/// is borrowed and need not be NUL-terminated.
	pub fn from_json(json: &str) -> Option<Self> {
		let ptr = unsafe { quercus_ofx_curveset_from_json(json.as_ptr(), json.len()) };
		(!ptr.is_null()).then_some(Self { ptr })
	}

	/// Serializes the whole set — every curve and control point, slopes
	/// included — to the engine's canonical curve-set JSON, the payload
	/// node values and project files store.
	///
	/// Returns an empty string when the handle is invalid or encoding
	/// fails.
	pub fn to_json(&self) -> String {
		string_out(|buf, len| unsafe { quercus_ofx_curveset_to_json(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Number of curves.
	pub fn curve_count(&self) -> usize {
		unsafe { quercus_ofx_curveset_curve_count(self.ptr) }.max(0) as usize
	}

	/// Appends an empty curve, returning its index.
	pub fn add_curve(&self) -> Option<usize> {
		let i = unsafe { quercus_ofx_curveset_add_curve(self.ptr) };
		(i >= 0).then_some(i as usize)
	}

	/// Appends an identity curve over `[lo, hi]` (the parametric
	/// default), returning its index.
	pub fn add_identity_curve(&self, lo: f64, hi: f64) -> Option<usize> {
		let i = unsafe { quercus_ofx_curveset_add_identity_curve(self.ptr, lo, hi) };
		(i >= 0).then_some(i as usize)
	}

	/// Number of control points in `curve`, or `None` when `curve` is out
	/// of range.
	///
	/// # Arguments
	///
	/// * `curve` - zero-based curve index within this set.
	pub fn point_count(&self, curve: usize) -> Option<usize> {
		let n = unsafe { quercus_ofx_curveset_point_count(self.ptr, curve as i32) };
		(n >= 0).then_some(n as usize)
	}

	/// Copies the control point at `index` of `curve`, including its
	/// explicit Hermite `slope`.
	///
	/// Returns `None` when `curve` or `index` is out of range.
	///
	/// # Arguments
	///
	/// * `curve` - zero-based curve index within this set.
	/// * `index` - zero-based position within the curve, in key order.
	pub fn point_at(&self, curve: usize, index: usize) -> Option<ControlPoint> {
		let mut out = QuercusOfxCurvePoint::default();
		unsafe { quercus_ofx_curveset_point_at(self.ptr, curve as i32, index as i32, &mut out) }.then(
			|| ControlPoint {
				key: out.key,
				value: out.value,
				slope: out.slope,
			},
		)
	}

	/// Inserts or overwrites the control point at `key` (key order is
	/// maintained; slopes are recomputed).
	pub fn upsert_point(&self, curve: usize, key: f64, value: f64) -> bool {
		unsafe { quercus_ofx_curveset_upsert_point(self.ptr, curve as i32, key, value) }
	}

	/// Inserts `point` into `curve` at its `key` position, or overwrites
	/// the point already stored at that exact key, keeping the explicit
	/// Hermite `slope` (unlike [`Self::upsert_point`], which recomputes
	/// slopes).
	///
	/// Returns `false` when `curve` is out of range, leaving the set
	/// unchanged.
	///
	/// # Arguments
	///
	/// * `curve` - zero-based curve index within this set.
	/// * `point` - key (parametric position), value and slope to store;
	///   the slope is copied verbatim, not recalculated.
	pub fn push_point(&self, curve: usize, point: ControlPoint) -> bool {
		unsafe {
			quercus_ofx_curveset_push_point(
				self.ptr,
				curve as i32,
				QuercusOfxCurvePoint {
					key: point.key,
					value: point.value,
					slope: point.slope,
				},
			)
		}
	}

	/// Removes the control point at `index` from `curve` and recomputes
	/// the remaining slopes; later points shift down one index.
	///
	/// Returns `false` when `curve` or `index` is out of range, leaving
	/// the set unchanged.
	///
	/// # Arguments
	///
	/// * `curve` - zero-based curve index within this set.
	/// * `index` - zero-based position within the curve, in key order.
	pub fn delete_point(&self, curve: usize, index: usize) -> bool {
		unsafe { quercus_ofx_curveset_delete_point(self.ptr, curve as i32, index as i32) }
	}

	/// Removes all control points of a curve (it then evaluates as
	/// identity).
	pub fn clear_curve(&self, curve: usize) -> bool {
		unsafe { quercus_ofx_curveset_clear_curve(self.ptr, curve as i32) }
	}

	/// Evaluates a curve at `x` (cubic Hermite; out-of-range keys
	/// clamp to the endpoint values, an empty curve is identity).
	/// None on an out-of-range curve.
	pub fn evaluate(&self, curve: usize, x: f64) -> Option<f64> {
		let v = unsafe { quercus_ofx_curveset_evaluate(self.ptr, curve as i32, x) };
		(!v.is_nan()).then_some(v)
	}
}

impl Clone for CurveSet {
	fn clone(&self) -> Self {
		unsafe { quercus_ofx_curveset_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for CurveSet {
	fn drop(&mut self) {
		unsafe { quercus_ofx_curveset_release(self.ptr) };
	}
}

/// An owned parametric curve DTO used by UI editors and SDK consumers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Curve {
	/// The `points` field.
	pub points: Vec<ControlPoint>,
}

impl Curve {
	/// An empty curve (evaluates as identity).
	pub fn empty() -> Self {
		Self::default()
	}

	/// An identity curve over `[lo, hi]`, with host-computed slopes.
	pub fn identity(lo: f64, hi: f64) -> Self {
		let set = CurveSet::new();
		let Some(index) = set.add_identity_curve(lo, hi) else {
			return Self::empty();
		};
		curve_from_set(&set, index)
	}

	/// Builds a curve from key/value pairs, with host-computed slopes.
	pub fn from_pairs(pairs: &[(f64, f64)]) -> Self {
		let set = CurveSet::new();
		let Some(index) = set.add_curve() else {
			return Self::empty();
		};
		for (key, value) in pairs {
			set.upsert_point(index, *key, *value);
		}
		curve_from_set(&set, index)
	}
}

/// Copies every control point of curve `index` in `set` into an owned
/// [`Curve`], dropping points the engine reports as invalid.
///
/// An out-of-range `index` yields an empty curve rather than an error.
/// Used by [`curves_from_json`] and the `Curve` constructors.
fn curve_from_set(set: &CurveSet, index: usize) -> Curve {
	let count = set.point_count(index).unwrap_or(0);
	Curve {
		points: (0..count)
			.filter_map(|point| set.point_at(index, point))
			.collect(),
	}
}

/// Parses curve-set JSON into owned, UI-independent [`Curve`] values,
/// one per curve in the set.
///
/// Returns `None` when the JSON is malformed; otherwise points are
/// copied in key order with their slopes intact. Counterpart of
/// [`curves_to_json`].
pub fn curves_from_json(json: &str) -> Option<Vec<Curve>> {
	let set = CurveSet::from_json(json)?;
	Some(
		(0..set.curve_count())
			.map(|index| curve_from_set(&set, index))
			.collect(),
	)
}

/// Serializes owned curves to the engine's canonical curve-set JSON —
/// the same format [`CurveSet::to_json`] emits and that node values and
/// project files store.
///
/// Curves are written in slice order with their explicit slopes
/// preserved. Encoding failures collapse to an empty string (the
/// handle used internally is always valid).
pub fn curves_to_json(curves: &[Curve]) -> String {
	let set = CurveSet::new();
	for curve in curves {
		let Some(index) = set.add_curve() else {
			continue;
		};
		for point in &curve.points {
			set.push_point(index, *point);
		}
	}
	set.to_json()
}

// ---------------------------------------------------------------------------
// GL bridge
// ---------------------------------------------------------------------------

/// The offscreen GL bridge behind interact drawing and GL-mode
/// renders. Texture/FBO creation and deletion require an acquired
/// context ([`gl::acquire`]); everything fails closed when GL is
/// unavailable.
pub mod gl {
	use super::*;

	/// Whether the offscreen GL context can be created (false → fall
	/// back to CPU rendering).
	pub fn available() -> bool {
		unsafe { quercus_ofx_gl_available() }
	}

	/// An acquired GL context (current on the acquiring thread).
	/// Releases on drop. Acquire is re-entrant per thread; guards must
	/// drop in LIFO order on the same thread.
	pub struct GlGuard {
		_private: (),
	}

	impl Drop for GlGuard {
		fn drop(&mut self) {
			unsafe { quercus_ofx_gl_release() };
		}
	}

	/// Makes the GL context current on the calling thread (globally
	/// serialized). None when GL is unavailable.
	pub fn acquire() -> Option<GlGuard> {
		unsafe { quercus_ofx_gl_acquire() }.then_some(GlGuard { _private: () })
	}

	/// Creates an RGBA32F render-target texture of `width` x `height`
	/// pixels and returns its GL name. The texture is not attached to any
	/// FBO yet; use [`create_fbo`] to make it renderable and
	/// [`delete_texture`] to free it.
	///
	/// Returns `None` when GL is unavailable or texture creation fails.
	/// Requires an acquired context ([`acquire`]).
	///
	/// # Arguments
	///
	/// * `width` - texture width in pixels.
	/// * `height` - texture height in pixels.
	pub fn create_output_texture(width: i32, height: i32) -> Option<i32> {
		let t = unsafe { quercus_ofx_gl_create_output_texture(width, height) };
		(t >= 0).then_some(t)
	}

	/// Creates an RGBA32F texture and uploads `pixels` (tight RGBA
	/// F32, `width*height*4` floats), returning its GL name.
	pub fn create_input_texture(width: i32, height: i32, pixels: &[f32]) -> Option<i32> {
		if width <= 0 || height <= 0 || pixels.len() < (width * height * 4) as usize {
			return None;
		}
		let t = unsafe { quercus_ofx_gl_create_input_texture(width, height, pixels.as_ptr()) };
		(t >= 0).then_some(t)
	}

	/// Deletes the GL texture `texture`, as returned by
	/// [`create_output_texture`] or [`create_input_texture`]. The caller
	/// must first detach it from any live FBO and must not use the name
	/// afterwards.
	///
	/// Requires an acquired context ([`acquire`]); a silent no-op when GL
	/// is unavailable.
	///
	/// # Arguments
	///
	/// * `texture` - GL texture name to free; `0` names no texture, so
	///   deleting it has no effect.
	pub fn delete_texture(texture: i32) {
		unsafe { quercus_ofx_gl_delete_texture(texture) }
	}

	/// Creates an FBO with `texture` attached, returning its GL name.
	pub fn create_fbo(texture: i32, width: i32, height: i32) -> Option<i32> {
		let f = unsafe { quercus_ofx_gl_create_fbo(texture, width, height) };
		(f >= 0).then_some(f)
	}

	/// Binds `fbo` as the framebuffer used by subsequent draws and
	/// readbacks; `0` selects the default framebuffer. Pair with
	/// [`create_fbo`] and restore `0` before handing control back to the
	/// host.
	///
	/// Requires an acquired context ([`acquire`]); a silent no-op when GL
	/// is unavailable.
	pub fn bind_fbo(fbo: i32) {
		unsafe { quercus_ofx_gl_bind_fbo(fbo) }
	}

	/// Deletes a framebuffer object previously returned by
	/// [`create_fbo`]. The attached texture is not deleted; release it
	/// separately with [`delete_texture`] once no FBO references it.
	///
	/// Requires an acquired context ([`acquire`]); a silent no-op when GL
	/// is unavailable or `fbo` names nothing.
	pub fn delete_fbo(fbo: i32) {
		unsafe { quercus_ofx_gl_delete_fbo(fbo) }
	}

	/// Sets the GL viewport to the `width` x `height` pixel rectangle at
	/// the framebuffer's lower-left origin, which is the region later
	/// draws and [`read_pixels_f32`] operate on.
	///
	/// Requires an acquired context ([`acquire`]); a silent no-op when GL
	/// is unavailable.
	///
	/// # Arguments
	///
	/// * `width` - viewport width in pixels; must be positive for anything
	///   to be drawn.
	/// * `height` - viewport height in pixels; must be positive for
	///   anything to be drawn.
	pub fn set_viewport(width: i32, height: i32) {
		unsafe { quercus_ofx_gl_set_viewport(width, height) }
	}

	/// Sets the color written by [`clear`] into the currently bound
	/// framebuffer, as normalised RGBA components (`0.0..=1.0`, values
	/// outside that range are clamped by GL).
	///
	/// Requires an acquired context ([`acquire`]); a silent no-op when GL
	/// is unavailable.
	///
	/// # Arguments
	///
	/// * `r` - red component, `0.0..=1.0`.
	/// * `g` - green component, `0.0..=1.0`.
	/// * `b` - blue component, `0.0..=1.0`.
	/// * `a` - alpha component, `0.0..=1.0`.
	pub fn clear_color(r: f32, g: f32, b: f32, a: f32) {
		unsafe { quercus_ofx_gl_clear_color(r, g, b, a) }
	}

	/// Clears the color buffer.
	pub fn clear() {
		unsafe { quercus_ofx_gl_clear() }
	}

	/// Reads the current framebuffer back as tight RGBA F32 into
	/// `out` (`width*height*4` floats). None on failure.
	pub fn read_pixels_f32(width: i32, height: i32, out: &mut [f32]) -> Option<usize> {
		let needed =
			unsafe { quercus_ofx_gl_read_pixels_f32(width, height, out.as_mut_ptr(), out.len()) };
		if needed < 0 || out.len() < needed as usize {
			return None;
		}
		Some(needed as usize)
	}

	/// The version string of the offscreen GL context (the
	/// `GL_VERSION` text reported by the GL bridge), for diagnostics and
	/// logging.
	///
	/// Returns `None` when GL is unavailable or the query fails. Use
	/// [`available`] — not this string — to decide whether to fall back
	/// to CPU rendering.
	pub fn version() -> Option<String> {
		string_out(|buf, len| unsafe { quercus_ofx_gl_version(buf, len) })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The bundled minimal test plugin (`org.quercus.test-plugin` & the
	/// interact variant), located under the workspace target dir and
	/// wrapped into the OFX bundle layout for scanning.
	fn test_plugin_scan_dir() -> Option<std::path::PathBuf> {
		crate::test_support::ofx_plugin_scan_dir()
	}

	// The host is process-global; serialize the host-touching tests.
	static HOST_SERIAL: Mutex<()> = Mutex::new(());

	#[test]
	fn curveset_build_edit_json_roundtrip() {
		let set = CurveSet::new();
		assert_eq!(set.curve_count(), 0);
		assert_eq!(set.add_identity_curve(0.0, 1.0), Some(0));
		assert_eq!(set.point_count(0), Some(2));
		assert_eq!(set.evaluate(0, 0.5), Some(0.5));

		assert!(set.upsert_point(0, 0.5, 0.25));
		assert_eq!(set.point_count(0), Some(3));
		let p = set.point_at(0, 1).expect("midpoint");
		assert_eq!((p.key, p.value), (0.5, 0.25));

		let json = set.to_json();
		let copy = CurveSet::from_json(&json).expect("parse");
		assert_eq!(copy.curve_count(), 1);
		assert_eq!(copy.point_count(0), Some(3));
		assert_eq!(copy.evaluate(0, 0.5), set.evaluate(0, 0.5));

		assert!(copy.delete_point(0, 1));
		assert!(copy.clear_curve(0));
		assert_eq!(copy.point_count(0), Some(0));
		assert_eq!(
			copy.evaluate(0, 0.7),
			Some(0.7),
			"an empty curve is identity"
		);
		assert_eq!(copy.evaluate(9, 0.0), None);

		assert!(CurveSet::from_json("{").is_none());
	}

	#[test]
	fn owned_curve_dto_preserves_explicit_slopes() {
		let mut curve = Curve::from_pairs(&[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]);
		curve.points[1].slope = 0.0;
		let json = curves_to_json(&[curve.clone()]);
		let parsed = curves_from_json(&json).expect("curve DTO JSON");
		assert_eq!(parsed, vec![curve]);
		assert_eq!(Curve::identity(0.0, 1.0).points.len(), 2);
		assert!(curves_from_json("{").is_none());
	}

	#[test]
	fn host_scan_enumerate_instance_params_render() {
		let _guard = HOST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		let Some(dir) = test_plugin_scan_dir() else {
			println!("SKIP: the bundled test plugin is not built");
			return;
		};
		assert!(scan_path(&dir.to_string_lossy()));

		let info = plugins()
			.into_iter()
			.find(|p| p.identifier == "org.quercus.test-plugin")
			.expect("the test plugin is discovered");
		assert_eq!(info.label, "Quercus Test Plugin");
		assert!(
			info.contexts
				.iter()
				.any(|c| c == "OfxImageEffectContextFilter")
		);

		let inst = Instance::create("org.quercus.test-plugin", None).expect("instance");
		assert_eq!(
			inst.context().as_deref(),
			Some("OfxImageEffectContextFilter")
		);

		// Parameter introspection.
		let names: Vec<String> = (0..inst.param_count().unwrap())
			.map(|i| inst.param_name_at(i).unwrap())
			.collect();
		for expected in ["gain", "mode", "debug", "label", "button"] {
			assert!(
				names.iter().any(|n| n == expected),
				"missing param {expected}"
			);
		}
		assert_eq!(inst.param_type_at(0).as_deref(), Some("OfxParamTypeDouble"));
		assert_eq!(inst.param_label_at(0).as_deref(), Some("Gain"));

		// Typed get/set roundtrips.
		assert!(inst.set_param_double("gain", &[1.5]));
		assert_eq!(inst.param_double("gain"), Some(vec![1.5]));
		assert!(
			!inst.set_param_double("gain", &[1.0, 2.0]),
			"arity mismatch rejected"
		);
		assert!(inst.set_param_choice("mode", 1));
		assert_eq!(inst.param_choice("mode"), Some(1));
		assert!(inst.set_param_string("label", "hello"));
		assert_eq!(inst.param_string("label").as_deref(), Some("hello"));
		assert_eq!(inst.param_int("gain"), None, "type mismatch rejected");
		assert!(!inst.set_param_bool("gain", true), "type mismatch rejected");
		assert!(inst.push_button("button"));

		// Sequence bracket + RoD.
		assert!(inst.begin_sequence(0.0, 1.0));
		assert!(inst.end_sequence(0.0, 1.0));
		assert!(inst.region_of_definition(0.0).is_some());

		// Headless render: the test plugin fills the output with
		// (0.5, 0.5, 0.5, 1.0) and reports progress.
		let events = std::sync::Arc::new(Mutex::new(Vec::<(String, f64)>::new()));
		let ended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
		{
			let events = events.clone();
			let ended = ended.clone();
			set_progress_handler(
				move |label, _message, fraction| {
					events.lock().unwrap().push((label.to_string(), fraction));
					true
				},
				move || {
					ended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
				},
			);
		}
		let mut dst = vec![0.0f32; 4 * 2 * 4];
		assert!(inst.render_frame(0.0, None, &mut dst, (4, 2)));
		assert!(dst.chunks_exact(4).all(|px| px == [0.5, 0.5, 0.5, 1.0]));
		let seen = events.lock().unwrap();
		assert!(seen.iter().any(|(label, f)| label == "render" && *f == 0.5));
		assert!(ended.load(std::sync::atomic::Ordering::SeqCst) > 0);
		clear_progress_handler();
		assert!(!has_progress_handler());
	}

	#[test]
	fn interact_lifecycle() {
		let _guard = HOST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		let Some(dir) = test_plugin_scan_dir() else {
			println!("SKIP: the bundled test plugin is not built");
			return;
		};
		assert!(scan_path(&dir.to_string_lossy()));
		let inst =
			Instance::create("org.quercus.test-plugin.interact", None).expect("interact instance");
		let Some(interact) = create_interact(inst.id()) else {
			panic!("the interact variant declares an overlay interact");
		};
		assert_eq!(persistent_message_count(inst.id()), inst.persistent_message_count());
		cancel_render();
		assert_eq!(interact.idle(), status::OK);
		// The test plugin leaves the focus actions unhandled
		// (ReplyDefault = "not consumed; the host may continue").
		assert_eq!(interact.gain_focus(0.0), status::REPLY_DEFAULT);
		assert_eq!(interact.pen_down((4.0, 4.0), 0.0), status::OK);
		assert_eq!(interact.pen_motion((5.0, 5.0), true, 0.1), status::OK);
		assert_eq!(interact.pen_up((5.0, 5.0), 0.2), status::OK);
		assert_eq!(interact.key_down(key::A, "a", 0.0), status::OK);
		assert_eq!(interact.key_up(key::A, "a", 0.1), status::OK);
		assert_eq!(interact.lose_focus(0.0), status::REPLY_DEFAULT);
		let _ = interact.take_redraw();
		let _ = interact.take_swap();
		// Drop destroys the interact.

		// A plugin without an interact yields None.
		let plain = Instance::create("org.quercus.test-plugin", None).expect("plain instance");
		assert!(plain.create_interact().is_none());

		set_progress_handler(|_, _, _| true, || {});
		assert!(has_progress_handler());
		shutdown();
		assert!(!has_progress_handler());
		assert_eq!(interact.idle(), status::ERR_BAD_HANDLE);
	}
}
