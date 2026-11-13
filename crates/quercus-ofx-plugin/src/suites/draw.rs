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

//! OfxDrawSuiteV1 (OFX 1.5, vendored ofxDrawSuite.h): the host draw suite.
//!
//! During `kOfxInteractActionDraw` a plugin obtains the draw context (this
//! module's [`DrawContext`]) through `kOfxInteractPropDrawContext`, then calls
//! the suite functions to draw: getColour / setColour / setLineWidth /
//! setLineStipple / draw / drawText.
//!
//! ## Context model
//!
//! - [`DrawContext`] is created by the host before every draw action and
//!   registered through the live table [`LIVE_CONTEXTS`] (handle = heap
//!   address; the handle discipline of [`crate::suites::tag`]), then removed
//!   when draw returns. The plugin only holds a context inside a draw action
//!   - the function docs in ofxDrawSuite.h note "failure, e.g. if function is
//!   called outside kOfxInteractActionDraw" -> a handle missing from the live
//!   table means kOfxStatFailed.
//! - State (colour / lineWidth / stipple / viewport / pixelScale) lives in
//!   [`DrawContext`] and is not shared between draw actions.
//! - GL commands require a current context: before calling
//!   `kOfxInteractActionDraw` the host keeps it current through
//!   [`crate::gl_bridge::acquire`] (for the whole draw action).
//!
//! ## Drawing implementation (real GL)
//!
//! The CGL context from gl_bridge is a **3.2 core profile** (no fixed
//! pipeline, glBegin is unavailable) - this suite's draw uses a minimal
//! shader plus VAO/VBO for real rendering: an orthographic projection maps
//! canonical coordinates to NDC (canonical width = viewport / pixelScale),
//! and non-opaque colours are composited "over" (as the ofxDrawSuite.h
//! setColour docs require). Non-macOS has no GL bridge -> kOfxStatFailed.
//!
//! ## drawText
//!
//! This host has no font rasterizer (drawing glyphs in GL needs a font or
//! curve sampling, beyond Phase 1), so it faithfully returns
//! kOfxStatErrUnsupported rather than pretending to draw text.

use std::collections::HashMap;
use std::ffi::{c_char, c_float, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{LazyLock, Mutex};

use crate::suites::status;

/// `OfxStandardColour` (ofxDrawSuite.h:37-46) enum values.
pub mod std_colour {
	/// kOfxStandardColourOverlayBackground。
	pub const BACKGROUND: i32 = 0;
	/// kOfxStandardColourOverlayActive。
	pub const ACTIVE: i32 = 1;
	/// kOfxStandardColourOverlaySelected。
	pub const SELECTED: i32 = 2;
	/// kOfxStandardColourOverlayDeselected。
	pub const DESELECTED: i32 = 3;
	/// kOfxStandardColourOverlayMarqueeFG。
	pub const MARQUEE_FG: i32 = 4;
	/// kOfxStandardColourOverlayMarqueeBG。
	pub const MARQUEE_BG: i32 = 5;
	/// kOfxStandardColourOverlayText。
	pub const TEXT: i32 = 6;
}

/// `OfxDrawLineStipplePattern` (ofxDrawSuite.h:49-56) enum values.
pub mod stipple {
	/// kOfxDrawLineStipplePatternSolid。
	pub const SOLID: i32 = 0;
	/// kOfxDrawLineStipplePatternDot。
	pub const DOT: i32 = 1;
	/// kOfxDrawLineStipplePatternDash。
	pub const DASH: i32 = 2;
	/// kOfxDrawLineStipplePatternAltDash。
	pub const ALT_DASH: i32 = 3;
	/// kOfxDrawLineStipplePatternDotDash。
	pub const DOT_DASH: i32 = 4;
}

/// `OfxDrawPrimitive` (ofxDrawSuite.h:60-68) enum values.
pub mod primitive {
	/// kOfxDrawPrimitiveLines (n points draw n/2 independent line segments).
	pub const LINES: i32 = 0;
	/// kOfxDrawPrimitiveLineStrip。
	pub const LINE_STRIP: i32 = 1;
	/// kOfxDrawPrimitiveLineLoop。
	pub const LINE_LOOP: i32 = 2;
	/// kOfxDrawPrimitiveRectangle (axis-aligned filled rectangle, 2 diagonal
	/// points).
	pub const RECTANGLE: i32 = 3;
	/// kOfxDrawPrimitivePolygon (filled n-gon).
	pub const POLYGON: i32 = 4;
	/// kOfxDrawPrimitiveEllipse (axis-aligned ellipse **wireframe** inside the
	/// box of 2 diagonal points).
	pub const ELLIPSE: i32 = 5;
}

/// `OfxRGBAColourF` (ofxPixels.h): the Draw suite colour value type.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OfxRGBAColourF {
/// Red.
	pub r: f32,
/// Green.
	pub g: f32,
/// Blue.
	pub b: f32,
	/// alpha。
	pub a: f32,
}

/// `OfxPointD` (ofxCore.h:819): a canonical coordinate point.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OfxPointD {
	/// x。
	pub x: f64,
	/// y。
	pub y: f64,
}

/// Draw suite context: the plugin's drawing state for one draw action.
///
/// The host creates and registers it inside
/// [`crate::suites::interact::Interact::draw`] and removes it when draw
/// returns. `viewport`/`pixel_scale` are snapshotted at creation (used for
/// the canonical -> NDC orthographic projection); colour/line width/stipple
/// are written by the plugin through the suite functions.
pub struct DrawContext {
	/// Current drawing colour (RGBA; written by setColour, used as the shader
	/// uniform when drawing).
	pub colour: [f32; 4],
	/// Current line width (setLineWidth; glLineWidth for GL line drawing).
	pub line_width: f32,
	/// Current stipple pattern (setLineStipple; the `stipple::*` enum).
	pub stipple: i32,
	/// Viewport size (pixels; snapshotted at creation).
	pub viewport: (f64, f64),
	/// canonical -> screen-pixel ratio (snapshotted at creation).
	pub pixel_scale: (f64, f64),
}

impl DrawContext {
	/// Creates a drawing context for one draw action.
	///
	/// `viewport` is the viewport size (pixels, width and height);
	/// `pixel_scale` is the canonical -> screen-pixel scale (usually both are
	/// the same; `(1.0, 1.0)` means 1:1). Both are snapshotted at creation and
	/// never change afterwards; the canonical-space size is computed as
	/// `viewport / pixel_scale` and used by [`ortho`].
	///
	/// Initial state: opaque black `[0,0,0,1]`, line width `1.0`, solid line
	/// ([`stipple::SOLID`]). The context itself is only held by the host,
	/// boxed into the live table via [`make_context`]; the plugin only gets
	/// the handle.
	pub fn new(viewport: (f64, f64), pixel_scale: (f64, f64)) -> Self {
		Self {
			colour: [0.0, 0.0, 0.0, 1.0],
			line_width: 1.0,
			stipple: stipple::SOLID,
			viewport,
			pixel_scale,
		}
	}
}

/// Live draw-context table: registered during a draw action (address ->
/// boxed value). The plugin only holds the handle inside draw; it is removed
/// when draw returns (the same LIVE_TEXTURES pattern as
/// [`crate::suites::gl_render`]).
static LIVE_CONTEXTS: LazyLock<Mutex<HashMap<usize, Box<DrawContext>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock() -> std::sync::MutexGuard<'static, HashMap<usize, Box<DrawContext>>> {
	LIVE_CONTEXTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registers a context and returns the handle (called by the host's draw
/// action; the handle is written into the inArgs'
/// kOfxInteractPropDrawContext).
pub(crate) fn make_context(viewport: (f64, f64), pixel_scale: (f64, f64)) -> *mut c_void {
	let ctx = Box::new(DrawContext::new(viewport, pixel_scale));
	let addr = &*ctx as *const DrawContext as usize;
	lock().insert(addr, ctx);
	addr as *mut c_void
}

/// Removes the context (called after the draw action returns; later suite
/// calls -> Failed).
pub(crate) fn drop_context(handle: *mut c_void) {
	lock().remove(&(handle as usize));
}

/// Shared wrapper for the suite entry points: runs the closure and folds both
/// panics and `Err` into OFX status codes.
///
/// `f` returning `Ok(())` -> `kOfxStatOK`, `Err(code)` -> that code returned
/// unchanged; a closure panic (or any unwinding other than an abort) ->
/// `kOfxStatFailed`. This way no panic in a plugin can cross the C ABI
/// boundary (unwinding across FFI is undefined behavior). Every vtable
/// function wraps its body with it, so each function only has to deal with
/// the `Ok`/`Err` branches.
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	catch_unwind(AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	)
}

/// Resolves a context handle (reverse lookup in the live table; outside a
/// draw action -> Failed, the "outside kOfxInteractActionDraw" semantics of
/// ofxDrawSuite.h).
fn resolve(handle: *mut c_void) -> Result<&'static DrawContext, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	let addr = handle as usize;
	lock()
		.get(&addr)
		.map(|b| unsafe { &*(&**b as *const DrawContext) })
		.ok_or(status::FAILED)
}

/// Context validation on the write path: returns `Ok(())` once the handle is
/// confirmed live.
///
/// It uses the same live table and error codes as [`resolve`] (null ->
/// `kOfxStatErrBadHandle`, not found -> `kOfxStatFailed`) but only validates
/// and returns no reference; each setter then locks and `get_mut`s the field
/// itself. The two steps exist so `resolve`'s borrow ends and the lock is
/// left for the setter to hold.
///
/// The draw action is single-threaded, so the caller guarantees "live at
/// validation, still live at write" on the same thread (that is what the
/// `expect` in each setter relies on).
fn resolve_mut(handle: *mut c_void) -> Result<(), c_int> {
	resolve(handle).map(|_| ())
}

// ---- 宿主标准颜色板（getColour）----

/// Host standard colour palette: in `OfxStandardColour` enum order
/// (ofxDrawSuite.h:37-46).
const PALETTE: [[f32; 4]; 7] = [
	// BACKGROUND：宿主视口背景（本宿主 Phase 1 无合成背景，黑）。
	[0.0, 0.0, 0.0, 1.0],
	// ACTIVE：进行中/激活的 overlay 元素。
	[1.0, 0.84, 0.0, 1.0],
	// SELECTED：选中元素。
	[0.0, 1.0, 0.0, 1.0],
	// DESELECTED：未选中元素。
	[1.0, 1.0, 1.0, 0.5],
	// MARQUEE_FG：框选前景。
	[1.0, 1.0, 1.0, 1.0],
	// MARQUEE_BG：框选背景。
	[0.0, 0.0, 0.0, 0.5],
	// TEXT：overlay 文本。
	[1.0, 1.0, 1.0, 1.0],
];

/// Vtable `getColour`: writes the `std_colour`-th entry of the host standard
/// palette into `out`.
///
/// The palette is fixed at 7 entries, laid out in [`std_colour`] enum order
/// (the index is the `OfxStandardColour` value, see ofxDrawSuite.h:37-46);
/// out-of-range indexes always return kOfxStatErrValue - no modulo, no
/// fallback. Components are `f32` in the agreed 0..1 range:
/// `ACTIVE`/`SELECTED` etc. are the usual overlay colours,
/// `DESELECTED`/`MARQUEE_BG` carry an alpha of 0.5, and `BACKGROUND` is fixed
/// to black because this host has no compositing background in Phase 1.
///
/// Return codes: success -> kOfxStatOK; `out` null or index out of range ->
/// kOfxStatErrValue; `ctx` null -> kOfxStatErrBadHandle; `ctx` absent from
/// the live table (outside a draw action) -> kOfxStatFailed.
///
/// # Safety
///
/// `ctx` must be a handle returned by [`make_context`] that has not yet been
/// [`drop_context`]ed; `out` must be null or point to a writable
/// [`OfxRGBAColourF`] (on success all four components are overwritten).
/// Panics are caught by [`caught`] and never cross the ABI boundary.
unsafe extern "C" fn get_colour(
	ctx: *mut c_void,
	std_colour: c_int,
	out: *mut OfxRGBAColourF,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		let _ = resolve(ctx)?;
		let c = PALETTE
			.get(std_colour as usize)
			.ok_or(status::ERR_VALUE)?;
		unsafe {
			*out = OfxRGBAColourF {
				r: c[0],
				g: c[1],
				b: c[2],
				a: c[3],
			};
		}
		Ok(())
	})
}

/// Vtable `setColour`: sets the foreground colour used by subsequent drawing.
///
/// `colour` points to RGBA (each component `f32`, the OFX 0..1 convention);
/// the value is stored in the context as-is with no clamping, and when
/// alpha < 1 the render path enables "over" blending. The colour is used for
/// both lines and fills and stays in effect for the rest of this draw action
/// until the function is called again; the next draw action resets it to
/// opaque black.
///
/// Return codes: `colour` null or invalid `ctx` -> see below; success ->
/// kOfxStatOK.
///
/// # Safety
///
/// `ctx` must be a handle returned by [`make_context`] that has not yet been
/// [`drop_context`]ed (null -> kOfxStatErrBadHandle, not found ->
/// kOfxStatFailed); `colour` must be null or point to a readable
/// [`OfxRGBAColourF`] (null -> kOfxStatErrValue). The function only copies
/// the four components and keeps no pointer. Panics are caught by
/// [`caught`].
unsafe extern "C" fn set_colour(ctx: *mut c_void, colour: *const OfxRGBAColourF) -> c_int {
	caught(|| {
		if colour.is_null() {
			return Err(status::ERR_VALUE);
		}
		resolve_mut(ctx)?;
		let v = unsafe { &*colour };
		let addr = ctx as usize;
		let mut live = lock();
		let slot = live.get_mut(&addr).expect("resolve 已保证存活");
		slot.colour = [v.r, v.g, v.b, v.a];
		Ok(())
	})
}

/// Vtable `setLineWidth`: sets the line width (pixels) used by subsequent
/// drawing.
///
/// `width` is stored in the context as-is, **without validation or
/// clamping**: 0 means `glLineWidth` is not called (skipped by the
/// conditional branch in `gl_imp::render`, so the driver default width
/// stays), negative values are accepted too but GL ignores the call; above
/// the driver's width limit GL clamps to the nearest supported value. The
/// width only affects line primitives (LINES/STRIP/LOOP/ELLIPSE) and has no
/// effect on filled primitives (RECTANGLE/POLYGON). The state only lives for
/// this draw action.
///
/// # Safety
///
/// `ctx` must be a handle returned by [`make_context`] that has not yet been
/// [`drop_context`]ed (null -> kOfxStatErrBadHandle, not found ->
/// kOfxStatFailed); this function dereferences no other pointer. Panics are
/// caught by [`caught`].
unsafe extern "C" fn set_line_width(ctx: *mut c_void, width: c_float) -> c_int {
	caught(|| {
		resolve_mut(ctx)?;
		let addr = ctx as usize;
		let mut live = lock();
		let slot = live.get_mut(&addr).expect("resolve 已保证存活");
		slot.line_width = width;
		Ok(())
	})
}

/// Vtable `setLineStipple`: sets the stipple pattern used by subsequent
/// drawing.
///
/// `pattern` must be a [`stipple`] enum value (`SOLID`..`DOT_DASH`, i.e.
/// 0..=4); out-of-range values return [`status::ERR_VALUE`] and are not
/// clamped. It writes this draw action's context state; the next draw action
/// does not inherit it. The current real render path only reads the value and
/// does not implement stippling (`gl_imp::render` never uses `stipple`), so
/// this function does not change any output pixels yet.
///
/// # Safety
///
/// `ctx` must be a handle returned by [`make_context`] that has not yet been
/// [`drop_context`]ed (null -> kOfxStatErrBadHandle, not found ->
/// kOfxStatFailed); this function dereferences no other pointer. Panics are
/// caught by [`caught`].
unsafe extern "C" fn set_line_stipple(ctx: *mut c_void, pattern: c_int) -> c_int {
	caught(|| {
		if !(0..=stipple::DOT_DASH).contains(&pattern) {
			return Err(status::ERR_VALUE);
		}
		resolve_mut(ctx)?;
		let addr = ctx as usize;
		let mut live = lock();
		let slot = live.get_mut(&addr).expect("resolve 已保证存活");
		slot.stipple = pattern;
		Ok(())
	})
}

/// Vtable `draw`: validates the primitive and point count, then does real GL
/// drawing.
///
/// `prim` is a [`primitive`] enum value and `points`/`count` are canonical
/// coordinate points and their number. The point count is validated per
/// primitive as ofxDrawSuite.h specifies: LINES must be even and >= 2,
/// LINE_STRIP/LINE_LOOP >= 2, RECTANGLE/ELLIPSE exactly 2, POLYGON >= 3.
/// After validation the points are copied out (so no pointer into the
/// plugin's buffer is held) and handed to [`gl_imp::render`] to draw; on
/// non-macOS that path always fails.
///
/// Return codes: success -> [`status::OK`]; primitive out of range,
/// `count < 0`, `count > 0` with a null `points`, or an invalid point count ->
/// [`status::ERR_VALUE`]; a null context handle ->
/// [`status::ERR_BAD_HANDLE`]; a handle absent from the live table (outside a
/// draw action) or a failed GL draw -> [`status::FAILED`].
///
/// # Safety
///
/// `ctx` must be a handle returned by [`make_context`] that has not yet been
/// [`drop_context`]ed; `points` must be null or point to a buffer of at least
/// `count` consecutive readable [`OfxPointD`] (null is fine when `count` is
/// 0). Panics are caught by [`caught`] and never cross the ABI boundary.
unsafe extern "C" fn draw(
	ctx: *mut c_void,
	prim: c_int,
	points: *const OfxPointD,
	count: c_int,
) -> c_int {
	caught(|| {
		if !(0..=primitive::ELLIPSE).contains(&prim) {
			return Err(status::ERR_VALUE);
		}
		if count < 0 || (count > 0 && points.is_null()) {
			return Err(status::ERR_VALUE);
		}
		// 每原语的合法点数（ofxDrawSuite.h draw 文档）。
		let valid = match prim {
			primitive::LINES => count >= 2 && count % 2 == 0,
			primitive::LINE_STRIP | primitive::LINE_LOOP => count >= 2,
			primitive::RECTANGLE | primitive::ELLIPSE => count == 2,
			primitive::POLYGON => count >= 3,
			_ => false,
		};
		if !valid {
			return Err(status::ERR_VALUE);
		}
		let c = resolve(ctx)?;
		let pts: Vec<OfxPointD> = (0..count as usize)
			.map(|i| unsafe { *points.add(i) })
			.collect();
		// 真实 GL 渲染（macOS 3.2 core；非 macOS stub → Failed）。
		gl_imp::render(c, prim, &pts)
	})
}

/// Vtable `drawText`: validates the arguments and faithfully reports
/// "unsupported".
///
/// This host has no font rasterizer (drawing glyphs in GL needs a font or
/// curve sampling, beyond Phase 1), so even valid text is not drawn: `text`
/// must be NUL-terminated valid UTF-8 (CStr validation failure ->
/// kOfxStatErrValue), `pos` must be a canonical coordinate point and `ctx`
/// must be a live context; after that [`status::ERR_UNSUPPORTED`] is
/// returned. `_alignment` is ignored (with nothing drawn, alignment is
/// meaningless).
///
/// # Safety
///
/// `ctx` must be a handle returned by the host's [`make_context`] that has
/// not yet been [`drop_context`]ed; `text` must point to a readable
/// NUL-terminated string; `pos` must point to a readable [`OfxPointD`]; a
/// null `ctx` returns kOfxStatErrBadHandle, a null `text`/`pos` returns
/// kOfxStatErrValue, and a null pointer is never dereferenced; any panic is
/// caught by [`caught`] and never crosses the ABI boundary.
unsafe extern "C" fn draw_text(
	ctx: *mut c_void,
	text: *const c_char,
	pos: *const OfxPointD,
	_alignment: c_int,
) -> c_int {
	caught(|| {
		if text.is_null() || pos.is_null() {
			return Err(status::ERR_VALUE);
		}
		let _ = resolve(ctx)?;
		// 确认文本有效（UTF-8）；随后如实拒绝（无字体光栅化器）。
		let _ = unsafe { CStr::from_ptr(text) }
			.to_str()
			.map_err(|_| status::ERR_VALUE)?;
		Err(status::ERR_UNSUPPORTED)
	})
}

/// Function table layout (field-for-field identical to the SDK
/// `OfxDrawSuiteV1`; ofxDrawSuite.h:85-177).
#[repr(C)]
pub struct DrawSuiteV1 {
	/// getColour: fetches a colour from the host standard palette.
	pub get_colour: unsafe extern "C" fn(*mut c_void, c_int, *mut OfxRGBAColourF) -> c_int,
	/// setColour: sets the colour used by subsequent drawing.
	pub set_colour: unsafe extern "C" fn(*mut c_void, *const OfxRGBAColourF) -> c_int,
	/// setLineWidth: sets the subsequent line width.
	pub set_line_width: unsafe extern "C" fn(*mut c_void, c_float) -> c_int,
	/// setLineStipple: sets the subsequent stipple pattern.
	pub set_line_stipple: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
	/// draw: draws a primitive (array of points, canonical coordinates).
	pub draw: unsafe extern "C" fn(*mut c_void, c_int, *const OfxPointD, c_int) -> c_int,
	/// drawText: draws text (Phase 1 returns kOfxStatErrUnsupported).
	pub draw_text: unsafe extern "C" fn(*mut c_void, *const c_char, *const OfxPointD, c_int) -> c_int,
}

/// The function table instance.
pub fn suite_v1() -> &'static DrawSuiteV1 {
	static SUITE: std::sync::OnceLock<DrawSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| DrawSuiteV1 {
		get_colour,
		set_colour,
		set_line_width,
		set_line_stipple,
		draw,
		draw_text,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Standard palette: getColour picks host colours by enum (out of range ->
	/// ErrValue).
	#[test]
	fn get_colour_palette() {
		let ctx = make_context((640.0, 480.0), (1.0, 1.0));
		let mut out = OfxRGBAColourF { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
		unsafe {
			assert_eq!(get_colour(ctx, std_colour::ACTIVE, &mut out), status::OK);
		}
		assert_eq!((out.r, out.g, out.b, out.a), (1.0, 0.84, 0.0, 1.0));
		// 越界枚举 → ErrValue。
		unsafe {
			assert_eq!(get_colour(ctx, 99, &mut out), status::ERR_VALUE);
			assert_eq!(get_colour(ctx, -1, &mut out), status::ERR_VALUE);
			// 空输出指针 → ErrValue。
			assert_eq!(
				get_colour(ctx, std_colour::BACKGROUND, std::ptr::null_mut()),
				status::ERR_VALUE
			);
		}
		drop_context(ctx);
	}

	/// setColour really writes state: after setLineWidth/setLineStipple it is
	/// read back from the live table (the plugin's view during a draw
	/// action).
	#[test]
	fn set_colour_and_line_state() {
		let ctx = make_context((100.0, 100.0), (2.0, 2.0));
		let col = OfxRGBAColourF { r: 0.9, g: 0.1, b: 0.2, a: 1.0 };
		unsafe {
			assert_eq!(set_colour(ctx, &col), status::OK);
			assert_eq!(set_line_width(ctx, 3.5), status::OK);
			assert_eq!(set_line_stipple(ctx, stipple::DASH), status::OK);
			assert_eq!(set_line_stipple(ctx, 99), status::ERR_VALUE);
		}

		let addr = ctx as usize;
		let live = lock();
		let c = live.get(&addr).unwrap();
		assert_eq!(c.colour, [0.9, 0.1, 0.2, 1.0]);
		assert_eq!(c.line_width, 3.5);
		assert_eq!(c.stipple, stipple::DASH);
		drop(live);
		drop_context(ctx);
	}

	/// Calling after removal -> Failed (the "outside kOfxInteractActionDraw"
	/// semantics).
	#[test]
	fn draw_suite_outside_draw_fails() {
		let ctx = make_context((10.0, 10.0), (1.0, 1.0));
		drop_context(ctx);
		let col = OfxRGBAColourF { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
		let mut out = OfxRGBAColourF { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
		let s = std::ffi::CString::new("x").unwrap();
		let pos = OfxPointD { x: 0.0, y: 0.0 };
		let pts = [OfxPointD { x: 0.0, y: 0.0 }, OfxPointD { x: 10.0, y: 10.0 }];
		unsafe {
			assert_eq!(set_colour(ctx, &col), status::FAILED);
			assert_eq!(get_colour(ctx, std_colour::TEXT, &mut out), status::FAILED);
			assert_eq!(set_line_width(ctx, 1.0), status::FAILED);
			assert_eq!(set_line_stipple(ctx, stipple::SOLID), status::FAILED);
			// 合法参数（避开空指针检查）→ 摘除后 resolve 失败 → Failed。
			assert_eq!(draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2), status::FAILED);
			// drawText 同样 Failed（先 resolve 失败）。
			assert_eq!(draw_text(ctx, s.as_ptr(), &pos, 0), status::FAILED);
		}
	}

	/// draw primitive argument validation (does not touch GL): invalid
	/// primitive/point count -> ErrValue.
	#[test]
	fn draw_primitive_argument_validation() {
		let ctx = make_context((10.0, 10.0), (1.0, 1.0));
		let pts = [OfxPointD { x: 0.0, y: 0.0 }, OfxPointD { x: 10.0, y: 10.0 }];
		unsafe {
			// 非法原语枚举。
			assert_eq!(draw(ctx, 99, pts.as_ptr(), 2), status::ERR_VALUE);
			assert_eq!(draw(ctx, -1, pts.as_ptr(), 2), status::ERR_VALUE);
			// 点数不合法：LINES 奇数点。
			assert_eq!(draw(ctx, primitive::LINES, pts.as_ptr(), 3), status::ERR_VALUE);
			// RECTANGLE 需恰好 2 点。
			assert_eq!(draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 3), status::ERR_VALUE);
			// POLYGON 需 ≥3 点。
			assert_eq!(draw(ctx, primitive::POLYGON, pts.as_ptr(), 2), status::ERR_VALUE);
			// 空指针 + 正点数。
			assert_eq!(draw(ctx, primitive::LINES, std::ptr::null(), 2), status::ERR_VALUE);
			// 合法参数：环境相关的 GL 路径（无 current 上下文时宿主可能
			// 失败）——参数校验已过即可，具体状态不在此断言。
			let st = draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2);
			assert!(
				st == status::OK || st == status::FAILED,
				"合法参数应 OK（有 GL）或 Failed（无 GL 上下文），got {st}"
			);
		}
		drop_context(ctx);
	}

	/// drawText: valid arguments -> Unsupported (faithful refusal); null
	/// pointer -> ErrValue.
	#[test]
	fn draw_text_is_honest_unsupported() {
		let ctx = make_context((10.0, 10.0), (1.0, 1.0));
		let s = std::ffi::CString::new("hello").unwrap();
		let pos = OfxPointD { x: 1.0, y: 2.0 };
		unsafe {
			assert_eq!(draw_text(ctx, s.as_ptr(), &pos, 0), status::ERR_UNSUPPORTED);
			assert_eq!(draw_text(ctx, std::ptr::null(), &pos, 0), status::ERR_VALUE);
		}
		drop_context(ctx);
	}
}

// ---- GL 绘制实现（平台分派）--------------------------------------------

#[cfg(target_os = "macos")]
mod gl_imp {
	use super::{DrawContext, OfxPointD};
	use crate::suites::status;
	use std::ffi::c_void;

	// ---- GL 常量（GL 规范值）----
	const GL_FALSE: i32 = 0;
	const GL_FLOAT: u32 = 0x1406;
	const GL_ARRAY_BUFFER: u32 = 0x8892;
	const GL_STATIC_DRAW: u32 = 0x88E4;
	const GL_FRAGMENT_SHADER: u32 = 0x8B30;
	const GL_VERTEX_SHADER: u32 = 0x8B31;
	const GL_COMPILE_STATUS: u32 = 0x8B81;
	const GL_LINK_STATUS: u32 = 0x8B82;
	const GL_TRIANGLES: u32 = 0x0004;
	const GL_LINES: u32 = 0x0001;
	const GL_LINE_STRIP: u32 = 0x0003;
	const GL_LINE_LOOP: u32 = 0x0002;
	const GL_BLEND: u32 = 0x0BE2;
	const GL_SRC_ALPHA: u32 = 0x0302;
	const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
	const GL_NO_ERROR: u32 = 0;

	// # Safety: 全部是 OpenGL.framework 导出函数；参数语义见各函数
	// 注释。调用前提：GL 上下文 current（宿主 draw action 已 acquire）。
	#[link(name = "OpenGL", kind = "framework")]
	unsafe extern "C" {
		fn glGenVertexArrays(n: i32, arrays: *mut u32);
		fn glBindVertexArray(array: u32);
		fn glGenBuffers(n: i32, buffers: *mut u32);
		fn glBindBuffer(target: u32, buffer: u32);
		fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32);
		fn glCreateShader(type_: u32) -> u32;
		fn glShaderSource(shader: u32, count: i32, string: *const *const i8, length: *const i32);
		fn glCompileShader(shader: u32);
		fn glGetShaderiv(shader: u32, pname: u32, params: *mut i32);
		fn glDeleteShader(shader: u32);
		fn glCreateProgram() -> u32;
		fn glAttachShader(program: u32, shader: u32);
		fn glLinkProgram(program: u32);
		fn glGetProgramiv(program: u32, pname: u32, params: *mut i32);
		fn glUseProgram(program: u32);
		fn glGetAttribLocation(program: u32, name: *const i8) -> i32;
		fn glVertexAttribPointer(
			index: u32,
			size: i32,
			type_: u32,
			normalized: u8,
			stride: i32,
			pointer: *const c_void,
		);
		fn glEnableVertexAttribArray(index: u32);
		fn glGetUniformLocation(program: u32, name: *const i8) -> i32;
		fn glUniformMatrix4fv(location: i32, count: i32, transpose: u8, value: *const f32);
		fn glUniform4f(location: i32, x: f32, y: f32, z: f32, w: f32);
		fn glDrawArrays(mode: u32, first: i32, count: i32);
		fn glLineWidth(width: f32);
		fn glEnable(cap: u32);
		fn glBlendFunc(sfactor: u32, dfactor: u32);
		fn glGetError() -> u32;
	}

	/// Vertex shader (canonical coordinates + colour uniform; projection maps
	/// to NDC).
	const VERT_SRC: &[u8] = b"#version 150\n\
		in vec2 a_pos;\n\
		uniform mat4 u_proj;\n\
		uniform vec4 u_col;\n\
		out vec4 v_col;\n\
		void main() {\n\
		  v_col = u_col;\n\
		  gl_Position = u_proj * vec4(a_pos, 0.0, 1.0);\n\
		}\n\0";

	const FRAG_SRC: &[u8] = b"#version 150\n\
		in vec4 v_col;\n\
		out vec4 frag_color;\n\
		void main() { frag_color = v_col; }\n\0";

	/// Compiles one GLSL shader and returns the shader object name on success.
	///
	/// `kind` is `GL_VERTEX_SHADER`/`GL_FRAGMENT_SHADER`; `src` must be a
	/// **NUL-terminated** source byte string (this module's
	/// `VERT_SRC`/`FRAG_SRC` all end with `\0`), with the length argument
	/// passed as null, so the source must not contain another NUL.
	///
	/// On failure returns [`status::FAILED`]: `glCreateShader` returned 0, or
	/// the compile status is `GL_FALSE` (in that case the newly created
	/// object is deleted first, leaving no leak). On success the caller is
	/// responsible for `glDeleteShader` after linking / on the failure path.
	/// The GL context must be current when called; this function caches no
	/// result.
	fn compile_shader(kind: u32, src: &[u8]) -> Result<u32, i32> {
		// SAFETY: glCreateShader/glShaderSource/glCompileShader 均为当前
		// 上下文的合法调用；src 是 NUL 结尾串（b"...\0"）。
		let sh = unsafe { glCreateShader(kind) };
		if sh == 0 {
			return Err(status::FAILED);
		}
		unsafe {
			let ptr = src.as_ptr() as *const i8;
			glShaderSource(sh, 1, &ptr, std::ptr::null());
			glCompileShader(sh);
		}
		let mut ok: i32 = 0;
		// SAFETY: glGetShaderiv 写 ok。
		unsafe { glGetShaderiv(sh, GL_COMPILE_STATUS, &mut ok) };
		if ok == GL_FALSE {
			// SAFETY: sh 是本函数 glCreateShader 的产物。
			unsafe { glDeleteShader(sh) };
			return Err(status::FAILED);
		}
		Ok(sh)
	}

	/// Lazily initialized process-wide draw resource set `(program, vao, vbo)`.
	///
	/// On first call it compiles the vertex/fragment shaders, links the
	/// program, generates and binds one set of fixed VAO/VBO, and caches the
	/// result in a `OnceLock`; afterwards (including the failure result) it is
	/// reused directly: on success each draw only re-uploads vertices with
	/// `glBufferData`, the VAO/VBO are not reallocated, avoiding per-call
	/// creation/leaks; on failure later calls immediately return the same
	/// error code and do not retry.
	///
	/// Returns `Err(status::FAILED)` when: any shader fails to compile
	/// (already-compiled objects are deleted), `glCreateProgram` returns 0, or
	/// linking fails. After linking the shader objects are deleted (the
	/// program keeps the executable). The caller must ensure the GL context is
	/// current, and this is the only place in the process that uses these GL
	/// objects.
	fn resources() -> Result<(u32, u32, u32), i32> {
		static RES: std::sync::OnceLock<Result<(u32, u32, u32), i32>> = std::sync::OnceLock::new();
		*RES.get_or_init(|| {
			let vs = match compile_shader(GL_VERTEX_SHADER, VERT_SRC) {
				Ok(s) => s,
				Err(e) => return Err(e),
			};
			let fs = match compile_shader(GL_FRAGMENT_SHADER, FRAG_SRC) {
				Ok(s) => s,
				Err(e) => {
					// SAFETY: vs 是本闭包编译成功的着色器。
					unsafe { glDeleteShader(vs) };
					return Err(e);
				}
			};
			let prog = unsafe { glCreateProgram() };
			if prog == 0 {
				// SAFETY: vs/fs 均有效。
				unsafe {
					glDeleteShader(vs);
					glDeleteShader(fs);
				}
				return Err(status::FAILED);
			}
			unsafe {
				glAttachShader(prog, vs);
				glAttachShader(prog, fs);
				glLinkProgram(prog);
			}
			let mut ok: i32 = 0;
			// SAFETY: glGetProgramiv 写 ok。
			unsafe { glGetProgramiv(prog, GL_LINK_STATUS, &mut ok) };
			// 链接后可删着色器对象（程序保留可执行版本）。
			unsafe {
				glDeleteShader(vs);
				glDeleteShader(fs);
			}
			if ok == GL_FALSE {
				return Err(status::FAILED);
			}
			// SAFETY: 生成并绑定固定 VAO/VBO（3.2 core）。
			let mut vao: u32 = 0;
			let mut vbo: u32 = 0;
			unsafe {
				glGenVertexArrays(1, &mut vao);
				glBindVertexArray(vao);
				glGenBuffers(1, &mut vbo);
				glBindBuffer(GL_ARRAY_BUFFER, vbo);
			}
			Ok((prog, vao, vbo))
		})
	}

	/// Builds the canonical -> NDC orthographic projection matrix
	/// (column-major 4x4).
	///
	/// `cw`/`ch` are the canonical-space width and height (computed by the
	/// host as `viewport / pixel_scale` and passed in) mapped to `[-1, 1]` in
	/// NDC; both are first clamped with `max(1.0)`, avoiding division by zero
	/// or flipping for 0/negative sizes. The matrix puts `(0,0)` at the
	/// bottom-left with the y axis up (matching GL NDC) and z is always 0. It
	/// returns 16 `f32` values that can be passed straight to
	/// `glUniformMatrix4fv` (`transpose = 0`).
	///
	/// Pairs with [`vertices`]: vertices stay in canonical coordinates and the
	/// projection happens in the vertex shader's `u_proj * vec4(a_pos, 0, 1)`.
	fn ortho(cw: f64, ch: f64) -> [f32; 16] {
		let (cw, ch) = (cw.max(1.0) as f32, ch.max(1.0) as f32);
		[
			2.0 / cw, 0.0, 0.0, 0.0,
			0.0, 2.0 / ch, 0.0, 0.0,
			0.0, 0.0, -1.0, 0.0,
			-1.0, -1.0, 0.0, 1.0,
		]
	}

	/// Expands an OFX primitive and canonical point list into GL vertices.
	///
	/// `prim` is a [`super::primitive`] enum value and `pts` is that
	/// primitive's canonical point count (`draw` has already validated by
	/// type). Returns `(GL primitive mode, interleaved xy vertices)`:
	/// LINES/LINE_STRIP/LINE_LOOP use the point list directly; RECTANGLE takes
	/// the two diagonal points and splits them into two triangles (filled);
	/// POLYGON builds a triangle fan around `pts[0]` (by convex-polygon
	/// convention; a concave polygon will draw wrong coverage); ELLIPSE takes
	/// the centre/radius of the box of the diagonal points and draws a
	/// **wireframe** with 64 `GL_LINE_LOOP` segments.
	/// An unknown `prim` returns empty vertices (the caller reports
	/// `ERR_VALUE`).
	///
	/// Coordinates are canonical (the input space relative to the viewport,
	/// before dividing by the pixel scale) and are converted to `f32` as-is;
	/// no clipping or wrapping. The function only reads `pts` and returns a
	/// freshly allocated vertex vector, regenerated on every draw (the vertex
	/// buffer is re-uploaded with `glBufferData`).
	fn vertices(prim: i32, pts: &[OfxPointD]) -> (u32, Vec<f32>) {
		match prim {
			super::primitive::LINES => (
				GL_LINES,
				pts.iter().flat_map(|p| [p.x as f32, p.y as f32]).collect(),
			),
			super::primitive::LINE_STRIP => (
				GL_LINE_STRIP,
				pts.iter().flat_map(|p| [p.x as f32, p.y as f32]).collect(),
			),
			super::primitive::LINE_LOOP => (
				GL_LINE_LOOP,
				pts.iter().flat_map(|p| [p.x as f32, p.y as f32]).collect(),
			),
			super::primitive::RECTANGLE => {
				let (x1, y1) = (pts[0].x as f32, pts[0].y as f32);
				let (x2, y2) = (pts[1].x as f32, pts[1].y as f32);
				// 两个三角形组成实心矩形（对角点定义）。
				(
					GL_TRIANGLES,
					vec![
						x1, y1, x2, y1, x1, y2,
						x1, y2, x2, y1, x2, y2,
					],
				)
			}
			super::primitive::POLYGON => {
				// 三角扇：0,i,i+1（凸多边形约定）。
				let mut v = Vec::new();
				for i in 1..pts.len() - 1 {
					v.extend_from_slice(&[
						pts[0].x as f32, pts[0].y as f32,
						pts[i].x as f32, pts[i].y as f32,
						pts[i + 1].x as f32, pts[i + 1].y as f32,
					]);
				}
				(GL_TRIANGLES, v)
			}
			super::primitive::ELLIPSE => {
				// 椭圆**线框**（文档），包围盒内 64 段线环。
				let (cx, cy) = ((pts[0].x + pts[1].x) / 2.0, (pts[0].y + pts[1].y) / 2.0);
				let (rx, ry) = (
					(pts[1].x - pts[0].x).abs() / 2.0,
					(pts[1].y - pts[0].y).abs() / 2.0,
				);
				let n = 64;
				let mut v = Vec::with_capacity(n * 2);
				for i in 0..n {
					let a = std::f64::consts::TAU * i as f64 / n as f64;
					v.push((cx + rx * a.cos()) as f32);
					v.push((cy + ry * a.sin()) as f32);
				}
				(GL_LINE_LOOP, v)
			}
			_ => (GL_TRIANGLES, Vec::new()),
		}
	}

	/// Real GL rendering: binds the fixed VAO/VBO, re-uploads vertices, sets
	/// the uniforms and draws.
	///
	/// `ctx` provides the snapshotted viewport/pixel scale, current colour,
	/// line width and stipple pattern; `prim` is a [`primitive`] enum value
	/// (the caller has already validated it); `pts` are canonical coordinate
	/// points whose length/meaning depends on the primitive, expanded into GL
	/// vertices by [`vertices`]. When `ctx.colour` has alpha < 1 it enables
	/// `GL_SRC_ALPHA/GL_ONE_MINUS_SRC_ALPHA` blending ("over" compositing);
	/// when the line width is > 0 it uses `glLineWidth`; the stipple pattern
	/// (`stipple`) is currently **not** implemented and only affects state
	/// reads.
	///
	/// Precondition: the GL context must be current (the host keeps it for
	/// the whole draw action). With no current context it issues no GL command
	/// and fails immediately - calling GL without a current context on macOS
	/// is undefined behavior.
	///
	/// Returns `Ok(())` on success (`draw` turns it into [`status::OK`]). The
	/// error codes it returns: [`status::FAILED`] when the context is not
	/// current, shader/resource initialization fails, `vertices` produces no
	/// vertices, the `a_pos` attribute is missing, or `glGetError` is not
	/// `GL_NO_ERROR`; empty vertices themselves map to
	/// [`status::ERR_VALUE`].
	pub fn render(ctx: &DrawContext, prim: i32, pts: &[OfxPointD]) -> Result<(), i32> {
		if !crate::gl_bridge::is_current() {
			return Err(status::FAILED);
		}
		let (prog, vao, vbo) = resources().map_err(|_| status::FAILED)?;
		let (mode, verts) = vertices(prim, pts);
		if verts.is_empty() {
			return Err(status::ERR_VALUE);
		}
		// 合成：非不透明色 "over"（ofxDrawSuite.h setColour 文档）。
		if ctx.colour[3] < 1.0 {
			// SAFETY: 开启混合并设因子（上下文 current）。
			unsafe {
				glEnable(GL_BLEND);
				glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
			}
		}
		// SAFETY: 以下 GL 调用均要求上下文 current（宿主已保证）；对象
		// 名来自 resources()（本进程唯一 GL 使用方）。
		unsafe {
			glUseProgram(prog);
			glBindVertexArray(vao);
			glBindBuffer(GL_ARRAY_BUFFER, vbo);
			glBufferData(
				GL_ARRAY_BUFFER,
				(verts.len() * 4) as isize,
				verts.as_ptr() as *const c_void,
				GL_STATIC_DRAW,
			);

			let a_pos = glGetAttribLocation(prog, b"a_pos\0".as_ptr() as *const i8);
			if a_pos < 0 {
				return Err(status::FAILED);
			}
			glVertexAttribPointer(a_pos as u32, 2, GL_FLOAT, 0, 0, std::ptr::null());
			glEnableVertexAttribArray(a_pos as u32);

			let u_proj = glGetUniformLocation(prog, b"u_proj\0".as_ptr() as *const i8);
			let u_col = glGetUniformLocation(prog, b"u_col\0".as_ptr() as *const i8);
			let cw = ctx.viewport.0 / ctx.pixel_scale.0.max(1e-6);
			let ch = ctx.viewport.1 / ctx.pixel_scale.1.max(1e-6);
			let m = ortho(cw, ch);
			glUniformMatrix4fv(u_proj, 1, 0, m.as_ptr());
			glUniform4f(u_col, ctx.colour[0], ctx.colour[1], ctx.colour[2], ctx.colour[3]);

			if ctx.line_width > 0.0 {
				glLineWidth(ctx.line_width);
			}
			glDrawArrays(mode, 0, (verts.len() / 2) as i32);

			// 解除绑定（对象保留复用）。
			glBindVertexArray(0);
			glBindBuffer(GL_ARRAY_BUFFER, 0);
			glUseProgram(0);
		}
		let err = unsafe { glGetError() };
		if err != GL_NO_ERROR {
			return Err(status::FAILED);
		}
		Ok(())
	}
}

/// Non-macOS: the Draw suite's GL drawing stub (no GL bridge -> Failed).
#[cfg(not(target_os = "macos"))]
mod gl_imp {
	use super::{DrawContext, OfxPointD};
	use crate::suites::status;

	/// GL drawing stub for non-macOS platforms: always [`status::FAILED`],
	/// producing no drawing at all.
	///
	/// This host only has the macOS CGL 3.2 core context bridge
	/// ([`crate::gl_bridge`]); other platforms have no usable GL context, so
	/// here it faithfully returns a generic failure per OFX semantics (the
	/// "failure" returns in the ofxDrawSuite.h functions) instead of
	/// pretending to draw anything. All arguments are ignored - `_pts` is not
	/// validated because the caller (`draw`) already checked primitive/point
	/// validity before entering.
	pub fn render(_ctx: &DrawContext, _prim: i32, _pts: &[OfxPointD]) -> Result<(), i32> {
		Err(status::FAILED)
	}
}
