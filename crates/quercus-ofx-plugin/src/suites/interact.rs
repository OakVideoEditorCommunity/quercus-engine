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

//! OfxInteractSuiteV1 + the [`Interact`] host object.
//!
//! Interact is the host-side object for a plugin-defined UI (the main-process
//! UI event host): it coexists with the render instances in the worker process
//! (OFX allows several instances of the same plugin) and is created through
//! [`crate::instance::Instance::new_interact`], bound to an effect instance.
//!
//! ## Lifecycle
//!
//! ```text
//! new_interact   → kOfxActionNewInteract (create; plugin refusal → None)
//! describe       → kOfxActionDescribe (kOfxActionDescribeInteract)
//! create_instance→ kOfxActionCreateInstance (kOfxActionCreateInstanceInteract)
//! draw/pen/key/idle → kOfxInteractAction* (host → plugin)
//! destroy        → kOfxActionDestroyInstance (kOfxActionDestroyInstanceInteract;
//!                  also triggered automatically when the Instance is destroyed)
//! ```
//!
//! The plugin entry receives every action through `Interact::call` (the
//! overlay interact V2/V1 entry, or the plugin's main entry when undeclared;
//! task contract: new_interact sends kOfxActionNewInteract to the plugin
//! entry). The return value (OfxStatus) is passed through faithfully.
//!
//! ## Semantics (ofxInteract.h)
//!
//! - Return of the pen/key actions: kOfxStatOK = the plugin handled the
//!   event, so the host must not pass it on to other interact objects in the
//!   view; kOfxStatReplyDefault = the plugin did not handle it, so the host
//!   may act on it itself.
//! - Pen coordinates: the caller supplies viewport pixels; inArgs also
//!   carries canonical coordinates (kOfxInteractPropPenPosition = viewport
//!   pixels / pixelScale) as well as pixelScale and pressure (a two-state pen
//!   maps to 0/1 per ofxInteract.h).
//! - key: kOfxPropKeySym (a key code from ofxKeySyms.h) + kOfxPropKeyString
//!   (UTF-8; keys without a UTF-8 encoding are the empty string).
//! - draw: before calling, the host keeps GL current for the whole action via
//!   [`crate::gl_bridge::acquire`] (the plugin issues raw GL commands or goes
//!   through the Draw suite; ofxInteract.h "the openGL context for this interact
//!   has been set"); inArgs carries
//!   viewport/pixelScale/time/backgroundImage/backgroundColour/drawContext.

use crate::gl_bridge;
use std::ffi::{c_int, c_void, CString};
use std::panic;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::host::{EntryPoint, Plugin};
use crate::property::{PropertySet, Value};
use crate::suites::{status, tag};

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// An Interact instance. `Arc<Interact>` manages the lifetime (the app side
/// holds the handle plus [`crate::instance::Instance`]'s interact field).
///
/// `#[repr(C)]` with props at offset 0 (the handle convention: interact
/// handle = `&props` | INTERACT tag; the property suite strips the tag and
/// reads the property set directly).
#[repr(C)]
pub struct Interact {
	/// The interact instance-level property set (offset 0, handle convention).
	pub props: PropertySet,
	/// The owning plugin.
	pub plugin: Arc<Plugin>,
	/// The interact entry (the overlay interact V2/V1 entry, or the plugin's
	/// main entry when undeclared). All interact actions are dispatched here.
	pub entry: EntryPoint,
	/// The effect instance handle (tagged INSTANCE; the interact action's
	/// inArgs kOfxPropEffectInstance). An opaque token, never dereferenced.
	pub effect_handle: *mut c_void,
	/// A buffer swap requested by the plugin through interactSwapBuffers
	/// (cleared after the app side polls it).
	pub swap_requested: AtomicBool,
	/// A redraw requested by the plugin through interactRedraw (cleared after
	/// the app side polls it).
	pub redraw_requested: AtomicBool,
	/// Whether destroy has been signaled (idempotence gate; also triggered by
	/// Instance destruction).
	pub destroyed: AtomicBool,
}

// SAFETY: effect_handle 是不透明令牌（同 [`crate::property::Value::Pointer`]，
// 从不在此对象上解引用）；entry 是函数指针，调用永远发生在宿主控制的
// action 分发点；其余字段（Arc/Mutex/AtomicBool/PropertySet）均线程安全。
// Interact 经 Instance.interact 字段跨线程可达，因此 Send/Sync 成立。
unsafe impl Send for Interact {}
unsafe impl Sync for Interact {}

impl Interact {
	/// Constructs an interact (host-internal; called by
	/// [`crate::instance::Instance::new_interact`]).
	/// `effect_handle` is a tagged INSTANCE handle (an opaque token).
	pub(crate) fn new(
		plugin: Arc<Plugin>,
		entry: EntryPoint,
		effect_handle: *mut c_void,
	) -> Arc<Interact> {
		let i = Arc::new(Interact {
			props: PropertySet::new(),
			plugin,
			entry,
			effect_handle,
			swap_requested: AtomicBool::new(false),
			redraw_requested: AtomicBool::new(false),
			destroyed: AtomicBool::new(false),
		});
		init_interact_props(&i.props);
		i
	}

	/// The interact handle (tagged INTERACT).
	pub(crate) fn handle(&self) -> *mut c_void {
		tag::make(&self.props as *const PropertySet, tag::INTERACT)
	}

	/// Dispatches one interact action to the plugin entry.
	///
	/// # Safety
	/// The handle matches the action (the Interact lifetime is controlled by the
	/// host; it is not called again after destroy).
	pub(crate) fn call(&self, action: &str, in_args: &PropertySet, out_args: &PropertySet) -> i32 {
		unsafe {
			self.plugin
				.call_entry(self.entry, action, self.handle(), in_args, out_args)
		}
	}

	/// Returns the effect instance handle (the inArgs kOfxPropEffectInstance).
	fn effect_instance_prop(&self) -> Value {
		Value::Pointer(self.effect_handle)
	}

	/// The current pixelScale (from the interact props, default 1,1).
	fn pixel_scale(&self) -> (f64, f64) {
		read_double2(&self.props, crate::host::PROP_INTERACT_PIXEL_SCALE).unwrap_or((1.0, 1.0))
	}

	/// describe (kOfxActionDescribe == kOfxActionDescribeInteract,
	/// ofxInteract.h:171). inArgs/outArgs are redundantly NULL.
	pub fn describe(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_DESCRIBE, &empty, &empty)
	}

	/// createInstance (kOfxActionCreateInstance ==
	/// kOfxActionCreateInstanceInteract, ofxInteract.h:198).
	/// A prerequisite for draw/pen/key (the \pre of the actions in ofxInteract.h).
	pub fn create_instance(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_CREATE_INSTANCE, &empty, &empty)
	}

	/// The draw action (ofxInteract.h:265). Before calling, the host keeps the
	/// GL context current for the whole action via
	/// [`crate::gl_bridge::acquire`]; inArgs carries
	/// kOfxInteractPropPixelScale/ViewportSize/BackgroundColour/
	/// BackgroundImage/Time/RenderScale/DrawContext + kOfxPropEffectInstance.
	///
	/// `background_image` is the background image handle held by the host (this
	/// host has no composited background in Phase 1, so None means empty — the
	/// task contract's "may be null").
	///
	/// Return value: the plugin's OfxStatus is passed through faithfully; when the
	/// GL context is unavailable (not macOS / no context) it returns
	/// kOfxStatErrMissingHostFeature (a host-side failure that never reaches the
	/// plugin).
	pub fn draw(
		&self,
		viewport_size: (f64, f64),
		pixel_scale: (f64, f64),
		time: f64,
		background_image: Option<*mut c_void>,
	) -> i32 {
		// 1. GL current 整个 draw action（插件发原生 GL 命令或经 Draw
		//    suite；失败即无法绘制 → 不触达插件）。
		let _guard = match gl_bridge::acquire() {
			Ok(g) => g,
			Err(_) => return status::ERR_MISSING_HOST_FEATURE,
		};

		// 2. Draw suite 上下文（存活注册；draw 返回后摘除）。
		let draw_ctx = crate::suites::draw::make_context(viewport_size, pixel_scale);

		let in_args = PropertySet::new();
		in_args.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(pixel_scale.0), Value::Double(pixel_scale.1)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_VIEWPORT_SIZE,
			vec![
				Value::Double(viewport_size.0),
				Value::Double(viewport_size.1),
			],
		);
		in_args.set_one(
			crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
			Value::Pointer(background_image.unwrap_or(ptr::null_mut())),
		);
		in_args.define(
			crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
			vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
		);
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.set_one(
			crate::host::PROP_INTERACT_DRAW_CONTEXT,
			Value::Pointer(draw_ctx),
		);
		in_args.set_one(
			crate::host::PROP_EFFECT_INSTANCE,
			self.effect_instance_prop(),
		);

		// 3. interact 实例属性同步（kOfxInteractPropPixelScale 等是实例
		//    属性集上只读的"当前状态"，ofxInteract.h:58）。
		self.props.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(pixel_scale.0), Value::Double(pixel_scale.1)],
		);
		self.props.define(
			crate::host::PROP_INTERACT_VIEWPORT_SIZE,
			vec![
				Value::Double(viewport_size.0),
				Value::Double(viewport_size.1),
			],
		);
		self.props.set_one(
			crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
			Value::Pointer(background_image.unwrap_or(ptr::null_mut())),
		);

		let out = PropertySet::new();
		let st = self.call(crate::host::ACTION_INTERACT_DRAW, &in_args, &out);
		crate::suites::draw::drop_context(draw_ctx);
		st
	}

	/// Builds the inArgs for pen-family actions (shared by PenMotion/PenDown/
	/// PenUp and gain/lose focus).
	///
	/// Contents: the effect instance handle, pixelScale, background colour (black),
	/// time in seconds, renderScale=1, canonical pen coordinates (viewport pixels /
	/// pixelScale, protected with 1e-9 when scale is 0), plus rounded integer
	/// viewport coordinates and the pressure value.
	///
	/// # Arguments
	///
	/// * `pen_viewport` - viewport pixel coordinates `(x, y)` (top-left origin).
	/// * `pressure` - pen pressure, 0.0..=1.0 (a two-state pen uses 1.0/0.0).
	/// * `time` - current render time, in seconds.
	fn pen_in_args(&self, pen_viewport: (f64, f64), pressure: f64, time: f64) -> PropertySet {
		let (ps_x, ps_y) = self.pixel_scale();
		// canonical = viewport 像素 / pixelScale（pixelScale 是
		// canonical→屏幕像素的换算比例）。
		let in_args = PropertySet::new();
		in_args.set_one(
			crate::host::PROP_EFFECT_INSTANCE,
			self.effect_instance_prop(),
		);
		in_args.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(ps_x), Value::Double(ps_y)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
			vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
		);
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_PEN_POSITION,
			vec![
				Value::Double(pen_viewport.0 / ps_x.max(1e-9)),
				Value::Double(pen_viewport.1 / ps_y.max(1e-9)),
			],
		);
		in_args.define(
			crate::host::PROP_INTERACT_PEN_VIEWPORT_POSITION,
			vec![
				Value::Int(pen_viewport.0.round() as i32),
				Value::Int(pen_viewport.1.round() as i32),
			],
		);
		in_args.set_one(
			crate::host::PROP_INTERACT_PEN_PRESSURE,
			Value::Double(pressure),
		);
		in_args
	}

	/// pen_motion (ofxInteract.h:302). `pen_viewport` is in viewport pixels;
	/// `pen_down` says whether the pen is down (a two-state pen maps to pressure
	/// 1.0/0.0 per ofxInteract.h:114).
	pub fn pen_motion(&self, pen_viewport: (f64, f64), pen_down: bool, time: f64) -> i32 {
		let pressure = if pen_down { 1.0 } else { 0.0 };
		let in_args = self.pen_in_args(pen_viewport, pressure, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_MOTION, &in_args, &out)
	}

	/// Dispatches kOfxInteractActionPenDown (ofxInteract.h:340) with the
	/// pressure fixed at 1.0.
	///
	/// # Arguments
	///
	/// * `pen_viewport` - viewport pixel coordinates `(x, y)`.
	/// * `time` - current time, in seconds.
	///
	/// # Returns
	///
	/// The plugin's OfxStatus, passed through as-is.
	pub fn pen_down(&self, pen_viewport: (f64, f64), time: f64) -> i32 {
		let in_args = self.pen_in_args(pen_viewport, 1.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_DOWN, &in_args, &out)
	}

	/// Dispatches kOfxInteractActionPenUp (ofxInteract.h:376) with the pressure
	/// fixed at 0.0.
	///
	/// # Arguments
	///
	/// * `pen_viewport` - viewport pixel coordinates `(x, y)`.
	/// * `time` - current time, in seconds.
	///
	/// # Returns
	///
	/// The plugin's OfxStatus, passed through as-is.
	pub fn pen_up(&self, pen_viewport: (f64, f64), time: f64) -> i32 {
		let in_args = self.pen_in_args(pen_viewport, 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_UP, &in_args, &out)
	}

	/// Builds the inArgs for key-family actions (shared by KeyDown/KeyUp,
	/// ofxInteract.h:384-442).
	///
	/// Contents: the effect instance handle, `kOfxPropKeySym`, `kOfxPropKeyString`
	/// (a UTF-8 `CString`), time in seconds and renderScale=1.
	///
	/// # Arguments
	///
	/// * `key_sym` - a key code from ofxKeySyms.h.
	/// * `key_string` - the UTF-8 character for the key; keys without a printable
	///   character are the empty string.
	/// * `time` - current time, in seconds.
	fn key_in_args(&self, key_sym: i32, key_string: &str, time: f64) -> PropertySet {
		let in_args = PropertySet::new();
		in_args.set_one(
			crate::host::PROP_EFFECT_INSTANCE,
			self.effect_instance_prop(),
		);
		in_args.set_one(crate::host::PROP_KEY_SYM, Value::Int(key_sym));
		in_args.set_one(crate::host::PROP_KEY_STRING, Value::String(cs(key_string)));
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args
	}

	/// key_down (ofxInteract.h:410). `key_sym` is a key code from ofxKeySyms.h;
	/// `key_string` is the UTF-8 character (keys without a UTF-8 encoding are the
	/// empty string).
	pub fn key_down(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		let in_args = self.key_in_args(key_sym, key_string, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_KEY_DOWN, &in_args, &out)
	}

	/// Dispatches kOfxInteractActionKeyUp (ofxInteract.h:443).
	///
	/// The inArgs are assembled by the shared key inArgs builder and sent to the
	/// plugin through `Interact::call`.
	///
	/// # Arguments
	///
	/// * `key_sym` - a key code from ofxKeySyms.h.
	/// * `key_string` - the UTF-8 character for the key; keys with no UTF-8
	///   representation pass the empty string.
	/// * `time` - current time, in seconds.
	///
	/// # Returns
	///
	/// The plugin's OfxStatus (kOfxStatOK = handled, kOfxStatReplyDefault = not
	/// handled).
	pub fn key_up(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		let in_args = self.key_in_args(key_sym, key_string, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_KEY_UP, &in_args, &out)
	}

	/// idle (the host's idle pump; kOfxInteractActionIdle per the task contract,
	/// an extension of this host). Returns kOfxStatReplyDefault when the plugin
	/// does not implement it.
	pub fn idle(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_IDLE, &empty, &empty)
	}

	/// gain_focus (ofxInteract.h:501): a prerequisite for pen/key events (the
	/// \pre of the actions in ofxInteract.h requires the interact to have gained
	/// focus).
	pub fn gain_focus(&self, time: f64) -> i32 {
		let in_args = self.pen_in_args((0.0, 0.0), 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_GAIN_FOCUS, &in_args, &out)
	}

	/// Dispatches kOfxInteractActionLoseFocus (ofxInteract.h:526).
	///
	/// Uses empty pen inArgs (coordinates 0,0, pressure 0) to tell the plugin that
	/// the interact lost focus; pairs with [`Interact::gain_focus`].
	///
	/// # Arguments
	///
	/// * `time` - current time, in seconds.
	///
	/// # Returns
	///
	/// The plugin's OfxStatus, passed through as-is.
	pub fn lose_focus(&self, time: f64) -> i32 {
		let in_args = self.pen_in_args((0.0, 0.0), 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_LOSE_FOCUS, &in_args, &out)
	}

	/// destroy (kOfxActionDestroyInstance == kOfxActionDestroyInstanceInteract,
	/// ofxInteract.h:227). Idempotent: the plugin is notified only once. The
	/// returned status is ignored (destruction cannot be rolled back; ofxInteract.h
	/// "what is returned is moot").
	pub fn destroy(&self) {
		if self.destroyed.swap(true, Ordering::Relaxed) {
			return;
		}
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_DESTROY_INSTANCE, &empty, &empty);
	}
}

/// Initializes the default values of the interact instance property table.
///
/// Defines/sets pixelScale=(1,1), viewportSize=(0,0), an empty background image
/// and slaveToParam, a white suggested colour, a black background colour,
/// bitDepth=32 and hasAlpha=1. Called once by [`Interact::new`] when the
/// instance is created.
///
/// # Arguments
///
/// * `props` - the property set to initialize (usually `Interact::props`).
fn init_interact_props(props: &PropertySet) {
	props.define(
		crate::host::PROP_INTERACT_PIXEL_SCALE,
		vec![Value::Double(1.0), Value::Double(1.0)],
	);
	props.define(
		crate::host::PROP_INTERACT_VIEWPORT_SIZE,
		vec![Value::Double(0.0), Value::Double(0.0)],
	);
	props.set_one(
		crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
		Value::Pointer(ptr::null_mut()),
	);
	props.define(
		crate::host::PROP_INTERACT_SUGGESTED_COLOUR,
		vec![Value::Double(1.0), Value::Double(1.0), Value::Double(1.0)],
	);
	props.define(crate::host::PROP_INTERACT_SLAVE_TO_PARAM, vec![]);
	props.define(
		crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
		vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
	);
	// 帧缓冲位深：离屏 FBO 是 RGBA32F → 每分量 32 bit，含 alpha。
	props.set_one(crate::host::PROP_INTERACT_BIT_DEPTH, Value::Int(32));
	props.set_one(crate::host::PROP_INTERACT_HAS_ALPHA, Value::Int(1));
}

/// Reads a Double×2 property from a property set.
///
/// Elements 0 and 1 must both be [`Value::Double`]; returns `None` when the
/// property is missing, has too few elements or has a type mismatch. Used to
/// read two-element properties such as pixelScale.
///
/// # Arguments
///
/// * `props` - the property set.
/// * `name` - the property name (e.g. `kOfxInteractPropPixelScale`).
///
/// # Returns
///
/// `(x, y)`, or `None`.
fn read_double2(props: &PropertySet, name: &str) -> Option<(f64, f64)> {
	let x = match props.get(name, 0)? {
		Value::Double(d) => d,
		_ => return None,
	};
	let y = match props.get(name, 1)? {
		Value::Double(d) => d,
		_ => return None,
	};
	Some((x, y))
}

// ---- OfxInteractSuiteV1（ofxInteract.h:534-544）------------------------

/// Resolves a C handle into an `Interact` reference.
///
/// The handle must be non-null with the low 3 tag bits equal to `tag::INTERACT`;
/// after validation the tag is stripped to yield `&Interact` (whose `props` sits
/// at offset 0). Whether the object has been freed is not checked — the lifetime
/// is guaranteed by the host and [`crate::instance::Instance`].
///
/// # Arguments
///
/// * `handle` - the interact handle the host handed to the plugin (tagged
///   INTERACT).
///
/// # Returns
///
/// `Ok(&Interact)`; a null pointer or a wrong tag yields
/// `Err(status::ERR_BAD_HANDLE)`.
///
/// # Safety
///
/// When non-null and correctly tagged, the Interact `handle` points to must stay
/// alive for the duration of the call.
fn resolve_interact(handle: *mut c_void) -> Result<&'static Interact, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		if tag::kind(handle) != tag::INTERACT {
			return Err(status::ERR_BAD_HANDLE);
		}
		// Interact 的 props 在偏移 0：剥标签后的地址即 &Interact。
		Ok(&*(tag::strip(handle) as *const Interact))
	}
}

/// Runs a closure and normalizes its result into an OFX status code (the panic
/// fallback for suite entries).
///
/// The closure returning `Ok(())` → kOfxStatOK; `Err(code)` → `code` passed
/// through; a panic → kOfxStatFailed. All interact C entries go through it.
///
/// # Arguments
///
/// * `f` - the actual implementation, returning `Ok(())` or the OFX status code
///   to propagate.
///
/// # Returns
///
/// kOfxStatOK(0), the error code given by the closure, or kOfxStatFailed(1).
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	panic::catch_unwind(panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	)
}

/// interactSwapBuffers: records the plugin's GL buffer swap request.
///
/// The host has no UI view and cannot actually swap GL buffers; this sets
/// `swap_requested` (cleared after the app side polls it) and returns OK.
///
/// # Arguments
///
/// * `handle` - a tagged INTERACT handle.
///
/// # Returns
///
/// kOfxStatOK; kOfxStatErrBadHandle when the handle is null or wrongly tagged;
/// kOfxStatFailed on panic.
///
/// # Safety
///
/// `handle` must be null or point to a still-live host Interact; when non-null,
/// its memory must be readable for the duration of the call.
unsafe extern "C" fn interact_swap_buffers(handle: *mut c_void) -> c_int {
	caught(|| {
		let i = resolve_interact(handle)?;
		i.swap_requested.store(true, Ordering::Relaxed);
		Ok(())
	})
}

/// interactRedraw: records the plugin's redraw request.
///
/// This host has no separate interact UI view, so it sets `redraw_requested`
/// (cleared after the app side polls it) and returns OK, letting the UI consume
/// the request.
///
/// # Arguments
///
/// * `handle` - a tagged INTERACT handle.
///
/// # Returns
///
/// kOfxStatOK; kOfxStatErrBadHandle when the handle is null or wrongly tagged;
/// kOfxStatFailed on panic.
///
/// # Safety
///
/// `handle` must be null or point to a still-live host Interact; when non-null,
/// its memory must be readable for the duration of the call.
unsafe extern "C" fn interact_redraw(handle: *mut c_void) -> c_int {
	caught(|| {
		let i = resolve_interact(handle)?;
		i.redraw_requested.store(true, Ordering::Relaxed);
		Ok(())
	})
}

/// interactGetPropertySet: returns the interact's property set handle.
///
/// After validating that `property` is non-null and `handle` is valid, tags
/// `&Interact.props` with `INTERACT` and writes it into `*property`. The written
/// handle borrows the Interact's lifetime.
///
/// # Arguments
///
/// * `handle` - a tagged INTERACT handle.
/// * `property` - the output location receiving the tagged INTERACT property set
///   handle.
///
/// # Returns
///
/// kOfxStatOK; kOfxStatErrValue when `property` is null; kOfxStatErrBadHandle
/// when `handle` is invalid; kOfxStatFailed on panic.
///
/// # Safety
///
/// `handle` must be null or point to a still-live host Interact object;
/// `property` must be null or point to a writable `*mut c_void`. The written
/// property set handle is valid only while the Interact is alive.
unsafe extern "C" fn interact_get_property_set(
	handle: *mut c_void,
	property: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property.is_null() {
			return Err(status::ERR_VALUE);
		}
		let i = resolve_interact(handle)?;
		unsafe { *property = tag::make(&i.props as *const PropertySet, tag::INTERACT) };
		Ok(())
	})
}

/// Function table layout (field-for-field identical to the SDK
/// `OfxInteractSuiteV1`; ofxInteract.h:534).
#[repr(C)]
pub struct InteractSuiteV1 {
	/// interactSwapBuffers.
	pub interact_swap_buffers: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// interactRedraw.
	pub interact_redraw: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// interactGetPropertySet.
	pub interact_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
}

/// The function table instance.
pub fn suite_v1() -> &'static InteractSuiteV1 {
	static SUITE: std::sync::OnceLock<InteractSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| InteractSuiteV1 {
		interact_swap_buffers,
		interact_redraw,
		interact_get_property_set,
	})
}

#[cfg(test)]
mod tests;
