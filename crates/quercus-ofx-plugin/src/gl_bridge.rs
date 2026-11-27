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

//! GL interop bridge (approach B implemented: offscreen GL context + texture/FBO + readback).
//!
//! ## Background (phase 6a spike conclusion, implemented)
//!
//! OFX's `OpenGLRender` extension requires the host to hand clip textures
//! to the plugin as **GL texture names**
//! (`kOfxImageEffectPropOpenGLTextureIndex`), and the plugin draws
//! directly into the output texture. quercus's render backend is wgpu
//! (Metal on macOS), and Metal and GL texture names share no namespace
//! (approach A is not feasible; see the 0.1 spike document). This module
//! implements **approach B**:
//!
//! ```text
//! the host creates its own native offscreen GL context (macOS = CGL)
//!   → create an output GL texture + FBO per frame (the plugin renders into the attached texture)
//!   → the plugin obtains the real texture name through the GL suite (OpenGLTextureIndex)
//!   → after render returns, glReadPixels reads it back as a CPU frame
//!   → convert to the target frame format (F32/RGBA8) and assemble the frame
//! ```
//!
//! Rendering runs inside the quercus-worker process (M15 process
//! isolation); the GL context only needs to be created inside the worker
//! process, and the main process needs no GL.
//!
//! ## Context lifecycle and threading model
//!
//! - **Process-wide shared singleton**: lazily created by `OnceLock`
//!   (`imp::context`), created once and shared by the whole process (a CGL
//!   context can be made current on any thread, but not on two at once).
//! - **Serialization**: [`acquire`] takes the global `Mutex` and makes the
//!   context current on the calling thread; it returns a [`GlGuard`] that
//!   clears current and releases the lock on drop. OFX plugin rendering is
//!   already serialized by the instance lock (`Instance::render_lock`), but
//!   different instances may render on different threads — the GL side
//!   must serialize once more so only one thread holds a current context
//!   at a time.
//! - **Keeping current across calls**: ofxGPURender.h requires the host to
//!   keep the context current during Render/Begin/EndSequenceRender (the
//!   plugin issues GL commands directly between suite calls). The render
//!   driver therefore acquires **once** for the whole render action
//!   (including suite callbacks), holding the [`GlGuard`] across the
//!   action; the remaining functions in this module assume the context is
//!   already current (a guard is held) and take no further lock.
//! - **Re-entrant on the same thread**: when [`acquire`] is called while
//!   the same thread already holds an outer guard, it only increments the
//!   thread-local nesting depth (the context is already current, so the
//!   lock is not re-taken); only the outermost guard drop clears current
//!   and releases the lock. The host test for interact draw needs "one
//!   acquire covering FBO setup → plugin drawing → readback" while
//!   `Interact::draw` acquires again internally (the host must guarantee
//!   current before every action) — nested acquire avoids the deadlock.
//! - **Cleanup**: on process exit, the [`GlContext`] in the `OnceLock` is
//!   dropped, destroying the CGL context and pixel format.
//!
//! ## Platforms
//!
//! - **macOS (real implementation)**: Core OpenGL (`OpenGL.framework`,
//!   `#[link(name = "OpenGL", kind = "framework")]`, no new crate
//!   needed). Pixel format: 3.2 core profile, offline renderer allowed
//!   (headless environments without a GPU can fall back to software
//!   rendering), 24-bit color + 8-bit alpha. Offscreen rendering (FBO)
//!   needs no drawable and does not swap buffers.
//! - **Linux (EGL reserved)**: `cfg(target_os = "linux")` goes through the
//!   stub, everything returns failure/`false` (the use_opengl decision
//!   falls back to CPU based on this). Wiring point: replace `imp` with
//!   EGL (`eglGetDisplay/eglCreateContext` + GLES3); the FFI is
//!   isomorphic and the GL commands are the same.
//! - **Windows (WGL reserved)**: same, stub. Wiring point:
//!   `wglCreateContext` + `wglMakeCurrent`; the FFI is isomorphic.
//!
//! ## Readback format conversion (real and correct)
//!
//! The internal format of the output GL texture is chosen from the target
//! frame format (pipeline F32 → `GL_RGBA32F`, U8 target → `GL_RGBA8`).
//! `glReadPixels` reads back according to the texture type (`GL_FLOAT` or
//! `GL_UNSIGNED_BYTE`), flipping vertically (GL origin bottom-left vs
//! framebuffer origin top-left), and normalizes U8 to F32 by `v/255.0` —
//! matching the CPU path output format (F32 RGBA) and assembled via
//! `write_output_frame`.

use crate::error;
use quercus_core::backend;
use std::env;
use std::sync::Arc;

use crate::image::Image;
use crate::render::VideoParams;

// ---- Public API (platform-independent signatures; impl dispatched by platform cfg) ----

/// Whether the TEST build may touch the real CGL context on macOS. The
/// headless CI runner cannot exercise Apple's GL reliably (parallel unit
/// tests have segfaulted inside the driver), so the test build behaves as
/// if GL were unavailable unless `QUERCUS_GPU_TESTS` asks for it — the same
/// gate the integration GL tests use. Release builds are untouched.
///
/// Every public entry point below obeys the gate: the suites fabricate
/// contexts and texture names when they drive the GL paths without a
/// real driver, so even a "context is current" call must not reach CGL.
#[cfg(all(test, target_os = "macos"))]
fn real_gl_gated() -> bool {
	env::var_os("QUERCUS_GPU_TESTS").is_none()
}

/// The error gated entry points report in tests (see [`real_gl_gated`]).
#[cfg(all(test, target_os = "macos"))]
fn gl_gated_error() -> error::Error {
	error::Error::Failed(
		"gl_bridge: real GL is gated behind QUERCUS_GPU_TESTS in tests".to_string(),
	)
}

/// Whether a GL context is available (an offscreen context can be
/// created). The use_opengl decision uses it as the
/// "target texture has a valid GL name" gate: the bridge can create a
/// real GL texture for the target frame ⟺ the context is available.
/// Lazily created and idempotent.
pub fn gl_available() -> bool {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return false;
	}
	imp::gl_available()
}

/// Whether the current GL implementation exposes the memory-object
/// extension needed for zero-copy (Linux `GL_EXT_memory_object_fd`,
/// Windows `GL_EXT_memory_object_win32`). Useful as a one-line probe on a
/// target machine before enabling the path.
pub fn gl_memory_object_available() -> bool {
	#[cfg(target_os = "macos")]
	{
		false
	}
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	{
		imp::gl_memory_object_available()
	}
}

/// Linux: prefer the EGL device whose renderer matches the engine's
/// Vulkan adapter (multi-GPU machines). Must be called before the GL
/// context is first created; later calls are ignored.
#[cfg(target_os = "linux")]
pub fn set_preferred_renderer(name: &str) {
	imp::set_preferred_renderer(name);
}

/// Windows: kept for call-site parity. WGL device selection is currently
/// implicit (the hidden window's display); the probe logs GL_RENDERER.
#[cfg(target_os = "windows")]
pub fn set_preferred_renderer(_name: &str) {}

/// Acquires the process-wide GL context and makes it current on this
/// thread (globally serialized). The returned guard clears current and
/// releases the lock on drop. On failure (no GL / context creation
/// failed) → the caller falls back to CPU.
pub fn acquire() -> error::Result<GlGuard> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return Err(gl_gated_error());
	}
	imp::acquire()
}

/// Creates an output GL texture (size = target frame; internal format
/// from `params.format`: F32 → RGBA32F, U8 → RGBA8) and returns the real
/// GL texture name.
///
/// # Safety
///
/// Requires the GL context to be current (called while a [`GlGuard`] is
/// held).
pub fn create_output_texture(w: i32, h: i32, params: &VideoParams) -> error::Result<i32> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return Err(gl_gated_error());
	}
	imp::create_output_texture(w, h, params)
}

/// Creates an input GL texture (RGBA32F) and uploads the F32 pixel data,
/// returning the real GL texture name. `pixels` must be `w*h*4*4` bytes
/// (tightly packed RGBA F32, row-major).
///
/// # Safety
///
/// Requires the GL context to be current (called while a [`GlGuard`] is
/// held).
pub fn create_input_texture(w: i32, h: i32, pixels: &[u8]) -> error::Result<i32> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return Err(gl_gated_error());
	}
	imp::create_input_texture(w, h, pixels)
}

/// Deletes a GL texture (a product of [`create_output_texture`] /
/// [`create_input_texture`]).
///
/// Ignored when `name <= 0`. The name must not be used afterwards; a
/// texture still attached to an FBO must have that FBO deleted first.
/// Linux/Windows zero-copy textures do not go through here, see
/// `delete_memory_texture`.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn delete_gl_texture(name: i32) {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::delete_gl_texture(name);
}

/// Creates an FBO and attaches `tex` as the color attachment (binds it
/// and checks completeness). Returns the FBO name (currently bound).
/// Cleaned up on failure.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn create_fbo(tex: i32, w: i32, h: i32) -> error::Result<i32> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return Err(gl_gated_error());
	}
	imp::create_fbo(tex, w, h)
}

/// Binds an FBO (0 = system framebuffer).
///
/// `fbo` is a name returned by [`create_fbo`]; after binding, subsequent
/// GL draw/readback operations target that framebuffer until it is bound
/// again. The render driver calls this before and after the render action
/// to keep the plugin from re-binding it.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn bind_fbo(fbo: i32) {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::bind_fbo(fbo);
}

/// Deletes an FBO (a product of [`create_fbo`]).
///
/// Ignored when `fbo <= 0`; the default framebuffer is bound first so a
/// still-bound object is not deleted. The color-attachment texture is not
/// released here, use [`delete_gl_texture`] for it.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn delete_fbo(fbo: i32) {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::delete_fbo(fbo);
}

/// Sets the viewport (pixel coordinates).
///
/// `w`, `h` are the viewport width/height (render resolution); the origin
/// is fixed at `(0, 0)`. The render driver calls this after binding the
/// output FBO; the plugin may change the viewport, so it is set again
/// before readback.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn set_viewport(w: i32, h: i32) {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::set_viewport(w, h);
}

/// Process-wide `glReadPixels` counter (tests prove the zero-copy path
/// never calls it; the explicit fallback/readback path still does).
static GL_READBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of `glReadPixels` calls made through this bridge.
pub fn gl_readback_count() -> u64 {
	GL_READBACKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reads back the currently bound FBO and assembles it into an F32 RGBA
/// image (vertical flip, U8 normalization). The returned Image uses the
/// same format as the CPU path output and can be passed directly to
/// [`crate::render_driver::write_output_frame`].
///
/// `w`, `h` are the pixel dimensions to read, which must be positive and
/// match the FBO color attachment; `params.format` selects the readback
/// type (F32 → `GL_FLOAT`, U8 → `GL_UNSIGNED_BYTE`, normalized by
/// `1/255.0`). The image origin is top-left.
///
/// # Safety
///
/// Requires the GL context to be current and the target FBO still bound.
pub fn read_pixels_to_image(w: i32, h: i32, params: &VideoParams) -> error::Result<Arc<Image>> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return Err(gl_gated_error());
	}
	GL_READBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	imp::read_pixels_to_image(w, h, params)
}

/// Sets the clear color (for FBO tests / the failure purple screen).
///
/// `r`, `g`, `b`, `a` are RGBA components in `0.0..=1.0` (out-of-range
/// values are clamped by GL); this sets the current context's clear-color
/// state for the next [`gl_clear`] and does not clear anything itself.
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::gl_clear_color(r, g, b, a);
}

/// Clears the color buffer (for FBO tests; the plugin's normal path
/// issues its own GL commands).
///
/// # Safety
///
/// Requires the GL context to be current.
pub fn gl_clear() {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return;
	}
	imp::gl_clear();
}

/// The version string of the current GL implementation (real-context
/// evidence; for tests).
pub fn gl_version_string() -> Option<String> {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return None;
	}
	imp::gl_version_string()
}

/// Opaque GL context handle (holds the global lock + the context is
/// current; clears current and releases the lock on drop).
pub struct GlGuard {
	_inner: imp::GuardInner,
}

/// Whether this thread holds GL current (including inside a nested guard;
/// true after a successful [`acquire`]). Callers without GL (e.g. the Draw
/// suite invoked outside a draw action) use this to avoid issuing GL
/// commands — calling GL without a current context on macOS is undefined
/// behavior.
pub fn is_current() -> bool {
	#[cfg(all(test, target_os = "macos"))]
	if real_gl_gated() {
		return false;
	}
	imp::is_current()
}

// ---- Platform implementations ------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
	use super::*;
	use crate::image::{BitDepth, Components};
	use std::ffi::{c_char, c_void};
	use std::sync::{Arc, Mutex, OnceLock};

	// ---- OpenGL constants (GL spec values, not OFX macros) ----
	const GL_TEXTURE_2D: u32 = 0x0DE1;
	const GL_RGBA: u32 = 0x1908;
	const GL_UNSIGNED_BYTE: u32 = 0x1401;
	const GL_FLOAT: u32 = 0x1406;
	const GL_RGBA32F: u32 = 0x8814;
	const GL_RGBA8: u32 = 0x8058;
	const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
	const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
	const GL_NEAREST: u32 = 0x2600;
	const GL_TEXTURE_WRAP_S: u32 = 0x2802;
	const GL_TEXTURE_WRAP_T: u32 = 0x2803;
	const GL_CLAMP_TO_EDGE: u32 = 0x812F;
	const GL_FRAMEBUFFER: u32 = 0x8D40;
	const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
	const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
	const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
	const GL_PACK_ALIGNMENT: u32 = 0x0D05;
	const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
	const GL_VERSION: u32 = 0x1F02;
	const GL_NO_ERROR: u32 = 0;

	// CGL pixel format attributes (CGLPixelFormatAttribute, CGL.h).
	const K_CGL_PFA_OPENGL_PROFILE: i32 = 99;
	const K_CGLOGLP_VERSION_3_2_CORE: i32 = 0x3200;
	const K_CGL_PFA_ALLOW_OFFLINE_RENDERERS: i32 = 96;
	const K_CGL_PFA_COLOR_SIZE: i32 = 8;
	const K_CGL_PFA_ALPHA_SIZE: i32 = 11;

	// macOS: FFI declarations for CGL + OpenGL (linked directly against OpenGL.framework).
	//
	// # Safety
	//
	// All are functions exported by the system framework; for pointer-argument
	// semantics see each function's comment. CGL* types are represented as
	// `*mut c_void` opaque object pointers (CGLContextObj / CGLPixelFormatObj).
	#[link(name = "OpenGL", kind = "framework")]
	unsafe extern "C" {
		/// Selects a pixel format from an attribute list (returns kCGLNoError=0
		/// on success; `npix` is the pixel-format count CGL is required to return
		/// and may be discarded since this bridge only takes the format object —
		/// but the argument must still be provided, otherwise it is undefined
		/// behavior).
		fn CGLChoosePixelFormat(attrs: *const i32, pix: *mut *mut c_void, npix: *mut i32) -> i32;
		/// Creates a context (share = NULL: no share group). Returns
		/// kCGLNoError=0.
		fn CGLCreateContext(pix: *mut c_void, share: *mut c_void, ctx: *mut *mut c_void) -> i32;
		fn CGLDestroyPixelFormat(pix: *mut c_void);
		fn CGLDestroyContext(ctx: *mut c_void);
		/// Makes it the calling thread's current context (NULL = clear). Returns
		/// kCGLNoError=0.
		fn CGLSetCurrentContext(ctx: *mut c_void) -> i32;

		fn glGenTextures(n: i32, textures: *mut u32);
		fn glDeleteTextures(n: i32, textures: *const u32);
		fn glBindTexture(target: u32, texture: u32);
		fn glTexParameteri(target: u32, pname: u32, param: i32);
		fn glPixelStorei(pname: u32, param: i32);
		/// Allocates/uploads a 2D texture image. Allocates only when `pixels` is
		/// NULL.
		fn glTexImage2D(
			target: u32,
			level: i32,
			internalformat: i32,
			width: i32,
			height: i32,
			border: i32,
			format: u32,
			type_: u32,
			pixels: *const c_void,
		);
		fn glGenFramebuffers(n: i32, framebuffers: *mut u32);
		fn glDeleteFramebuffers(n: i32, framebuffers: *const u32);
		fn glBindFramebuffer(target: u32, framebuffer: u32);
		fn glFramebufferTexture2D(
			target: u32,
			attachment: u32,
			textarget: u32,
			texture: u32,
			level: i32,
		);
		fn glCheckFramebufferStatus(target: u32) -> u32;
		fn glViewport(x: i32, y: i32, width: i32, height: i32);
		fn glClearColor(r: f32, g: f32, b: f32, a: f32);
		fn glClear(mask: u32);
		fn glReadPixels(
			x: i32,
			y: i32,
			width: i32,
			height: i32,
			format: u32,
			type_: u32,
			pixels: *mut c_void,
		);
		fn glGetError() -> u32;
		/// Returns an info string for the current context (non-empty = the
		/// context is really usable).
		fn glGetString(name: u32) -> *const c_char;
	}

	/// CGL context singleton (lazily created via OnceLock; see below for
	/// Send+Sync).
	pub struct GlContext {
		ctx: *mut c_void,
		pixel_format: *mut c_void,
	}

	// SAFETY: a CGL context object can be made current on any thread via
	// CGLSetCurrentContext, but not on two threads at once — this module
	// serializes every make-current and GL call under a global Mutex; the
	// pointer is only passed between threads (Arc-shared) and never
	// dereferenced concurrently; Drop only runs at process exit (single
	// threaded). Send/Sync are therefore sound.
	unsafe impl Send for GlContext {}
	unsafe impl Sync for GlContext {}

	impl Drop for GlContext {
		fn drop(&mut self) {
			// SAFETY: single-threaded during process exit; current is cleared
			// before destroying the objects, the order CGL documents requires
			// (a context must be unset current before it is destroyed).
			unsafe {
				CGLSetCurrentContext(std::ptr::null_mut());
				if !self.ctx.is_null() {
					CGLDestroyContext(self.ctx);
				}
				if !self.pixel_format.is_null() {
					CGLDestroyPixelFormat(self.pixel_format);
				}
			}
		}
	}

	static GL_CONTEXT: OnceLock<Result<Arc<GlContext>, String>> = OnceLock::new();

	/// Global GL serialization lock: only one thread at a time makes the
	/// shared context current.
	static GL_LOCK: Mutex<()> = Mutex::new(());

	fn lock() -> std::sync::MutexGuard<'static, ()> {
		GL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn err(msg: &str) -> crate::error::Error {
		crate::error::Error::Failed(format!("gl_bridge: {msg}"))
	}

	/// Lazily creates the process-wide CGL context (idempotent).
	fn context() -> crate::error::Result<&'static Arc<GlContext>> {
		let res = GL_CONTEXT.get_or_init(create_context);
		match res {
			Ok(c) => Ok(c),
			Err(e) => Err(err(&format!("CGL context creation failed: {e}"))),
		}
	}

	/// Lazily creates the process-wide CGL context (called by the `OnceLock`
	/// in `context`, executed once).
	///
	/// Pixel-format attributes: OpenGL 3.2 core profile, offline renderers
	/// allowed (headless environments without a GPU can fall back to software
	/// rendering), 24-bit color + 8-bit alpha; the context joins no share
	/// group (`share = NULL`, this process has no other GL user). Returns
	/// `Arc<GlContext>` on success.
	///
	/// # Errors
	///
	/// Returns `String` (containing the CGL return code) when
	/// `CGLChoosePixelFormat` or `CGLCreateContext` fails; the failure path
	/// destroys the already selected pixel format. `context` wraps that error
	/// as `Error::Failed` and caches it.
	fn create_context() -> Result<Arc<GlContext>, String> {
		// SAFETY: CGLChoosePixelFormat writes the pixel-format object into pix;
		// the attribute list is 0-terminated (the profile value follows
		// kCGLPFAOpenGLProfile and 1 follows kCGLPFAAllowOfflineRenderers; see
		// the attr pairing convention in CGL.h).
		let attrs = [
			K_CGL_PFA_OPENGL_PROFILE,
			K_CGLOGLP_VERSION_3_2_CORE,
			K_CGL_PFA_ALLOW_OFFLINE_RENDERERS,
			1,
			K_CGL_PFA_COLOR_SIZE,
			24,
			K_CGL_PFA_ALPHA_SIZE,
			8,
			0,
		];
		unsafe {
			let mut pix: *mut c_void = std::ptr::null_mut();
			// npix is a CGL out parameter (pixel-format count); this bridge does
			// not need it, but the argument must still be supplied (CGL writes it).
			let mut npix: i32 = 0;
			let st = CGLChoosePixelFormat(attrs.as_ptr(), &mut pix, &mut npix);
			if st != 0 || pix.is_null() {
				return Err(format!("CGLChoosePixelFormat failed: {st}"));
			}
			let mut ctx: *mut c_void = std::ptr::null_mut();
			// share = NULL: no share group (this process is the only GL user and
			// needs no namespace shared with other contexts).
			let st = CGLCreateContext(pix, std::ptr::null_mut(), &mut ctx);
			if st != 0 || ctx.is_null() {
				CGLDestroyPixelFormat(pix);
				return Err(format!("CGLCreateContext failed: {st}"));
			}
			Ok(Arc::new(GlContext {
				ctx,
				pixel_format: pix,
			}))
		}
	}

	/// guard lifetime: global lock + context current.
	pub struct GuardInner {
		/// The outermost guard holds the global lock; same-thread nested guards
		/// hold None (only the depth is incremented, see [`acquire`]).
		_lock: Option<std::sync::MutexGuard<'static, ()>>,
		_ctx: Arc<GlContext>,
	}

	// This thread's acquire nesting depth. >0 = this thread already holds an
	// outer guard (context already current); nested acquire only bumps the
	// depth, it does not re-take the lock.
	thread_local! {
		static ACQUIRE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
	}

	/// Reports whether this thread holds GL current (including nested
	/// [`acquire`] guards).
	///
	/// Reads the thread-local nesting depth and issues no GL commands. Callers
	/// without GL (for example the draw suite invoked outside a draw action)
	/// use this to skip GL commands — calling GL without a current context on
	/// macOS is undefined behavior.
	pub fn is_current() -> bool {
		ACQUIRE_DEPTH.with(|d| d.get() > 0)
	}

	/// Acquires the process-wide CGL context, makes it current on this
	/// thread, and returns a [`GlGuard`].
	///
	/// The first call creates the context; on success the guard holds the
	/// global `GL_LOCK` (only one thread at a time has a current context), and
	/// dropping the guard clears current and releases the lock. Re-entrant on
	/// the same thread: when an outer guard is already held, only the
	/// thread-local depth is incremented and no lock is re-taken; only the
	/// outermost drop clears current.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when context creation fails or
	/// `CGLSetCurrentContext` returns non-zero (the latter releases the lock
	/// first); the caller falls back to CPU accordingly.
	pub fn acquire() -> crate::error::Result<GlGuard> {
		let ctx = context()?.clone();
		let depth = ACQUIRE_DEPTH.with(|d| d.get());
		if depth > 0 {
			// Nested: the context is already current (outer guard held); only bump the depth.
			ACQUIRE_DEPTH.with(|d| d.set(depth + 1));
			return Ok(GlGuard {
				_inner: GuardInner {
					_lock: None,
					_ctx: ctx,
				},
			});
		}
		let guard = lock();
		// SAFETY: ctx is a valid CGLContextObj; once current, this thread may
		// issue GL commands. Returns non-zero on failure.
		let st = unsafe { CGLSetCurrentContext(ctx.ctx) };
		if st != 0 {
			drop(guard);
			return Err(err(&format!("CGLSetCurrentContext failed: {st}")));
		}
		ACQUIRE_DEPTH.with(|d| d.set(1));
		Ok(GlGuard {
			_inner: GuardInner {
				_lock: Some(guard),
				_ctx: ctx,
			},
		})
	}

	impl Drop for GuardInner {
		fn drop(&mut self) {
			let depth = ACQUIRE_DEPTH.with(|d| d.get());
			if depth <= 1 {
				// Outermost: clear current (NULL) and release the global lock.
				// LIFO stack discipline guarantees every nested guard is already
				// dropped (depth back to 1), so _lock is always Some.
				ACQUIRE_DEPTH.with(|d| d.set(0));
				// SAFETY: clear current (NULL) then release the lock; see the module docs.
				unsafe {
					CGLSetCurrentContext(std::ptr::null_mut());
				}
				drop(self._lock.take());
			} else {
				// Nested guard drop: decrement depth only; do not clear current or release the lock.
				ACQUIRE_DEPTH.with(|d| d.set(depth - 1));
			}
		}
	}

	/// Reports whether the process-wide CGL context is available (can be
	/// created).
	///
	/// Lazily created (`OnceLock`, idempotent, failures are cached too); the
	/// first call may actually create the context. The render decision uses it
	/// as the "target texture can get a valid GL name" gate: on `false` it
	/// falls back to CPU.
	pub fn gl_available() -> bool {
		context().is_ok()
	}

	/// Reports whether the current CGL context carries an imported memory
	/// object.
	///
	/// macOS's CGL path does not implement zero-copy; the public entry point
	/// [`super::gl_memory_object_available`] always returns `false`. This
	/// function only probes the context's memory slot internally, and the CGL
	/// implementation never fills it. Lazily creates the context.
	pub fn gl_memory_object_available() -> bool {
		let _ = context();
		false
	}

	/// Reads and clears the current context's GL error, converting an
	/// unhandled error into `Error::Failed`.
	///
	/// `glGetError` reports one error per call; `what` is used to build the
	/// error message (the message starts with `what` and is followed by the GL
	/// error code). Used for the closing check of creation/upload paths and
	/// for test assertions. Requires the context to be current (`glGetError`
	/// acts on the current context).
	fn check_error(what: &str) -> crate::error::Result<()> {
		// SAFETY: glGetError takes no arguments; it reads the current context's error state.
		let e = unsafe { glGetError() };
		if e != GL_NO_ERROR {
			Err(err(&format!("{what} failed: GL error 0x{e:x}")))
		} else {
			Ok(())
		}
	}

	/// Target frame format → output GL texture format.
	/// Returns (internal format, readback type, bytes per channel).
	fn gl_texture_format(params: &VideoParams) -> crate::error::Result<(u32, u32, usize)> {
		match params.format {
			f if f == crate::render::PIXEL_FORMAT_F32 => Ok((GL_RGBA32F, GL_FLOAT, 4)),
			f if f == crate::render::PIXEL_FORMAT_U8 => Ok((GL_RGBA8, GL_UNSIGNED_BYTE, 1)),
			f => Err(err(&format!(
				"target format {f} unsupported for GL (F32/U8 only)"
			))),
		}
	}

	/// Creates a `w`×`h` 2D texture with internal format `internal` and sets
	/// up the sampling state.
	///
	/// The sampling parameters must be a combination that keeps the FBO
	/// complete: both the min and mag filters are `GL_NEAREST` (a mipmap min
	/// filter would make the level-0 FBO incomplete), and S/T are both
	/// `GL_CLAMP_TO_EDGE`. Allocation uploads no pixels (`glTexImage2D` with
	/// `pixels` = NULL); `UNPACK_ALIGNMENT` is temporarily set to 1 and
	/// restored to 4 afterwards, and the texture is unbound at the end.
	/// Returns the texture name on success.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when `w`/`h` are non-positive, `glGenTextures`
	/// returns 0, or `glTexImage2D` reports a GL error. Requires the context
	/// to be current.
	fn new_texture(w: i32, h: i32, internal: u32) -> crate::error::Result<u32> {
		if w <= 0 || h <= 0 {
			return Err(err("invalid texture size"));
		}
		unsafe {
			let mut tex: u32 = 0;
			glGenTextures(1, &mut tex);
			if tex == 0 {
				return Err(err("glGenTextures failed"));
			}
			glBindTexture(GL_TEXTURE_2D, tex);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as i32);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
			glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
			glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
			// internalformat is a GLenum (unsigned); in the C signature
			// glTexImage2D's internalformat is really GLint — safe here since
			// the value is < 2^31.
			glTexImage2D(
				GL_TEXTURE_2D,
				0,
				internal as i32,
				w,
				h,
				0,
				GL_RGBA,
				GL_FLOAT,
				std::ptr::null(),
			);
			glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
			glBindTexture(GL_TEXTURE_2D, 0);
			check_error("glTexImage2D")?;
			Ok(tex)
		}
	}

	/// Creates an output texture according to the target frame format
	/// (F32 → `GL_RGBA32F`, U8 → `GL_RGBA8`) and returns the real GL texture
	/// name.
	///
	/// `w`, `h` are the pixel dimensions (must be > 0); `params.format`
	/// selects the internal format and the remaining fields are unused. The
	/// sampling state is set by [`new_texture`] to NEAREST + clamp-to-edge, so
	/// the texture can be attached to an FBO and handed to the plugin for
	/// rendering.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when `params.format` is neither F32 nor U8, the
	/// size is invalid, or GL creation fails. Requires the context to be
	/// current.
	pub fn create_output_texture(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<i32> {
		let (internal, _, _) = gl_texture_format(params)?;
		new_texture(w, h, internal).map(|t| t as i32)
	}

	/// Creates an RGBA32F input texture and uploads the CPU pixels into it,
	/// returning the real GL texture name.
	///
	/// `w`, `h` are the pixel dimensions (must be > 0); `pixels` must be
	/// exactly `w*h*4*4` bytes of tightly packed RGBA `f32`, row-major.
	/// Upload uses `UNPACK_ALIGNMENT = 1`; the sampling state is set by
	/// [`new_texture`] to NEAREST + clamp-to-edge, so the texture can also be
	/// attached to an FBO.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when the size is non-positive, the `pixels`
	/// length does not match (the just-created texture is deleted), or
	/// `glGetError` is non-zero after the upload. Requires the context to be
	/// current.
	pub fn create_input_texture(w: i32, h: i32, pixels: &[u8]) -> crate::error::Result<i32> {
		let tex = new_texture(w, h, GL_RGBA32F)?;
		// Upload F32 RGBA pixels (tightly packed, row-major; ensure unpack alignment=1 first).
		let expected = (w as usize) * (h as usize) * 4 * 4;
		if pixels.len() != expected {
			let _ = delete_gl_texture(tex as i32);
			return Err(err(&format!(
				"input pixel length {} does not match w*h*4*4={expected}",
				pixels.len()
			)));
		}
		unsafe {
			glBindTexture(GL_TEXTURE_2D, tex);
			glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
			glTexImage2D(
				GL_TEXTURE_2D,
				0,
				GL_RGBA32F as i32,
				w,
				h,
				0,
				GL_RGBA,
				GL_FLOAT,
				pixels.as_ptr() as *const c_void,
			);
			glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
			glBindTexture(GL_TEXTURE_2D, 0);
		}
		check_error("input texture upload")?;
		Ok(tex as i32)
	}

	/// Deletes the texture name `name` (`glDeleteTextures`).
	///
	/// Returns immediately when `name <= 0`. The name must not be used
	/// afterwards; a texture still attached to an FBO must have that FBO
	/// deleted first. Requires the context to be current.
	pub fn delete_gl_texture(name: i32) {
		if name <= 0 {
			return;
		}
		// SAFETY: name is a texture name previously produced by glGenTextures;
		// it must not be used after deletion (this bridge guarantees no further
		// references). Requires a current context.
		unsafe {
			let t = name as u32;
			glDeleteTextures(1, &t);
		}
	}

	/// Creates an FBO and attaches `tex` as color attachment 0
	/// (`GL_COLOR_ATTACHMENT0`).
	///
	/// `tex` must be a live 2D texture name; `w`, `h` are the expected pixel
	/// dimensions, which this implementation only validates (`> 0`) and does
	/// not reallocate storage for (the texture was already allocated by
	/// [`create_output_texture`]). On success the FBO stays bound and its name
	/// is returned.
	///
	/// # Errors
	///
	/// Returns an error when an argument is non-positive, `glGenFramebuffers`
	/// fails, or the completeness check is not `GL_FRAMEBUFFER_COMPLETE`; the
	/// failure path deletes the created FBO and binds back to 0. Requires the
	/// context to be current.
	pub fn create_fbo(tex: i32, _w: i32, _h: i32) -> crate::error::Result<i32> {
		if tex <= 0 {
			return Err(err("invalid FBO attachment texture"));
		}
		unsafe {
			let mut fbo: u32 = 0;
			glGenFramebuffers(1, &mut fbo);
			if fbo == 0 {
				return Err(err("glGenFramebuffers failed"));
			}
			glBindFramebuffer(GL_FRAMEBUFFER, fbo);
			glFramebufferTexture2D(
				GL_FRAMEBUFFER,
				GL_COLOR_ATTACHMENT0,
				GL_TEXTURE_2D,
				tex as u32,
				0,
			);
			let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
			if status != GL_FRAMEBUFFER_COMPLETE {
				glDeleteFramebuffers(1, &fbo);
				glBindFramebuffer(GL_FRAMEBUFFER, 0);
				return Err(err(&format!("FBO incomplete: 0x{status:x}")));
			}
			Ok(fbo as i32)
		}
	}

	/// Binds `fbo` as the draw/read target of `GL_FRAMEBUFFER`.
	///
	/// `fbo == 0` means the system (default) framebuffer; a negative value
	/// becomes an invalid name after the unsigned conversion and GL reports an
	/// error (this implementation does not validate it). Requires the context
	/// to be current. The render driver binds it once before and once after
	/// the render action, ensuring the plugin has not re-bound the output FBO.
	pub fn bind_fbo(fbo: i32) {
		// SAFETY: binds 0 (system framebuffer) or an FBO name created by this bridge.
		unsafe {
			glBindFramebuffer(GL_FRAMEBUFFER, fbo as u32);
		}
	}

	/// Deletes the FBO created by [`create_fbo`], binding the default
	/// framebuffer (0) first to avoid deleting an object that is still bound.
	///
	/// Returns immediately when `fbo <= 0`. Requires the context to be
	/// current; the color-attachment texture is not released by this function,
	/// call [`delete_gl_texture`] separately.
	pub fn delete_fbo(fbo: i32) {
		if fbo <= 0 {
			return;
		}
		// SAFETY: fbo is a product of this bridge's glGenFramebuffers; unbind before deleting.
		unsafe {
			glBindFramebuffer(GL_FRAMEBUFFER, 0);
			let f = fbo as u32;
			glDeleteFramebuffers(1, &f);
		}
	}

	/// Sets the GL viewport to origin `(0, 0)` with width/height `w`×`h`
	/// pixels.
	///
	/// `w`, `h` are the render resolution (offscreen FBO size), not the window
	/// size, and should be positive. The render driver calls this after
	/// binding the output FBO; the plugin may change the viewport, so it is set
	/// again before readback. Requires the context to be current.
	pub fn set_viewport(w: i32, h: i32) {
		// SAFETY: the viewport is GL context state; write it directly.
		unsafe {
			glViewport(0, 0, w, h);
		}
	}

	/// Reads back the raw pixels of the currently bound framebuffer with
	/// `glReadPixels`.
	///
	/// Returns a tightly packed, row-major byte buffer with the GL origin
	/// (bottom-left): 4 channels per pixel, `format`/`type_` supplied by the
	/// caller (this module uses `GL_RGBA` + `GL_FLOAT` or `GL_UNSIGNED_BYTE`),
	/// `bpc` is the bytes per channel (4 or 1), and the buffer size is
	/// `w*h*4*bpc`. Sets `PACK_ALIGNMENT` to 1 before the read and restores it
	/// to 4 afterwards. The caller must guarantee `w`, `h` are positive.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when `glGetError` is not `GL_NO_ERROR` after
	/// `glReadPixels` (the error message uses `what` to say which call it
	/// was).
	fn read_pixels(
		w: i32,
		h: i32,
		format: u32,
		type_: u32,
		bpc: usize,
	) -> crate::error::Result<Vec<u8>> {
		let size = (w as usize) * (h as usize) * 4 * bpc;
		let mut buf = vec![0u8; size];
		unsafe {
			glPixelStorei(GL_PACK_ALIGNMENT, 1);
			glReadPixels(0, 0, w, h, format, type_, buf.as_mut_ptr() as *mut c_void);
			glPixelStorei(GL_PACK_ALIGNMENT, 4);
		}
		check_error("glReadPixels")?;
		Ok(buf)
	}

	/// Reads back the currently bound FBO and assembles it into an F32 RGBA
	/// image.
	///
	/// `w`, `h` are the pixel dimensions to read, which must be positive and
	/// match the texture size of the FBO color attachment (this implementation
	/// does not validate them; a negative value is interpreted as a huge
	/// `usize` and fails during allocation). `params.format` selects the
	/// `glReadPixels` type and conversion: F32 → `GL_FLOAT` copied as-is, U8 →
	/// `GL_UNSIGNED_BYTE` normalized by `1/255.0`. The GL origin is bottom-left
	/// while the framebuffer origin is top-left, so rows are flipped
	/// vertically; the returned image is `BitDepth::Float` +
	/// `Components::Rgba`, matching the CPU path output format, and can be
	/// passed directly to
	/// [`crate::render_driver::write_output_frame`].
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when `params.format` is neither F32 nor U8, or
	/// when `glReadPixels` reports a GL error. Requires the context to be
	/// current and the target FBO still bound.
	pub fn read_pixels_to_image(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<Arc<Image>> {
		let (_, gl_type, bpc) = gl_texture_format(params)?;
		let raw = read_pixels(w, h, GL_RGBA, gl_type, bpc)?;
		let (w, h) = (w as usize, h as usize);
		let mut img = Image::allocate(
			BitDepth::Float,
			Components::Rgba,
			crate::instance::OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w as f64,
				y2: h as f64,
			},
		);
		let dst = img.data.as_mut_slice();
		let src_row = w * 4 * bpc;
		let dst_row = w * 4 * 4;
		for y in 0..h {
			// Vertical flip: GL readback origin is bottom-left, framebuffer origin top-left.
			let src_y = h - 1 - y;
			let src = &raw[src_y * src_row..(src_y + 1) * src_row];
			let d = &mut dst[y * dst_row..(y + 1) * dst_row];
			for x in 0..w {
				let (r, g, b, a): (f32, f32, f32, f32) = if bpc == 4 {
					let p = &src[x * 16..x * 16 + 16];
					(
						f32::from_ne_bytes(p[0..4].try_into().unwrap()),
						f32::from_ne_bytes(p[4..8].try_into().unwrap()),
						f32::from_ne_bytes(p[8..12].try_into().unwrap()),
						f32::from_ne_bytes(p[12..16].try_into().unwrap()),
					)
				} else {
					let p = &src[x * 4..x * 4 + 4];
					(
						p[0] as f32 / 255.0,
						p[1] as f32 / 255.0,
						p[2] as f32 / 255.0,
						p[3] as f32 / 255.0,
					)
				};
				let o = x * 16;
				d[o..o + 4].copy_from_slice(&r.to_ne_bytes());
				d[o + 4..o + 8].copy_from_slice(&g.to_ne_bytes());
				d[o + 8..o + 12].copy_from_slice(&b.to_ne_bytes());
				d[o + 12..o + 16].copy_from_slice(&a.to_ne_bytes());
			}
		}
		Ok(Arc::new(img))
	}

	/// Sets the clear color used by the next [`gl_clear`].
	///
	/// `r`, `g`, `b`, `a` are RGBA components in `0.0..=1.0` (out-of-range
	/// values are clamped by GL); this only changes the current context's GL
	/// state and does not clear anything itself. Used by FBO tests and the
	/// failure purple screen. Requires the context to be current.
	pub fn gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
		// SAFETY: writes the current context's clear-color state.
		unsafe {
			glClearColor(r, g, b, a);
		}
	}

	/// Clears the color buffer of the currently bound framebuffer with the
	/// clear color set by [`gl_clear_color`]
	/// (`glClear(GL_COLOR_BUFFER_BIT)`).
	///
	/// Used by FBO setup/readback tests and the failure purple screen; the
	/// plugin's normal render path issues its own GL commands. Requires the
	/// context to be current.
	pub fn gl_clear() {
		// SAFETY: clears the color buffer of the currently bound framebuffer.
		unsafe {
			glClear(GL_COLOR_BUFFER_BIT);
		}
	}

	/// Returns the `GL_VERSION` string of the process-wide CGL context (on
	/// macOS it looks like `"4.1 Metal - 90.5"`).
	///
	/// Requires the context to be current (during an [`acquire`] guard); this
	/// function does not make it current. `glGetString` returns a pointer into
	/// the context-resident string; it is copied into a `String` immediately
	/// and the pointer is not retained. Returns [`None`] when unavailable (no
	/// context or a null pointer); the `gl_context_is_real` test uses it to
	/// prove that a real driver context exists.
	pub fn gl_version_string() -> Option<String> {
		// SAFETY: glGetString returns a string resident in the current context;
		// it is copied into a Rust String before returning, no pointer is kept.
		unsafe {
			let p = glGetString(GL_VERSION);
			if p.is_null() {
				None
			} else {
				Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
			}
		}
	}
}

// ---- Linux: EGL + GL_EXT_memory_object_fd (true GPU zero-copy) ---------

/// A GL texture created from a Vulkan `OPAQUE_FD` memory object.
///
/// The GL memory object owns the duplicated fd; deleting the texture and
/// the memory object releases both. `name` is the real texture name OFX
/// plugins receive through `kOfxImageEffectPropOpenGLTextureIndex`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[derive(Clone)]
pub struct GlMemoryTexture {
	/// Real GL texture name.
	pub name: i32,
	/// GL memory object name (deleted with the texture).
	pub(crate) memory: u32,
	/// Width in pixels.
	pub width: i32,
	/// Height in pixels.
	pub height: i32,
}

/// Imports a wgpu/Vulkan GPU allocation (an `OPAQUE_FD` on Linux, an
/// `OPAQUE_WIN32` handle on Windows) as a real GL texture that aliases
/// the same memory, so an OFX plugin can render into the shared output
/// texture without a `glReadPixels` copy.
///
/// `export` carries the platform handle and allocation size from
/// `GpuContext::export_gl_memory`; `width`/`height` are the pixel
/// dimensions and must match the export exactly; `params.format` must be
/// F32 or U8 and selects `GL_RGBA32F`/`GL_RGBA8` (other fields are
/// unused). The handle is duplicated, so the caller may keep using the
/// wgpu texture. Pairs with [`delete_memory_texture`], which releases
/// both the texture and the GL memory object;
/// [`gl_memory_object_available`] probes support on a machine.
///
/// # Errors
///
/// Returns `Error::Failed` when the context is not current, the
/// memory-object extension is unavailable, the size does not match the
/// export, or the GL import/allocation calls fail.
///
/// # Safety
///
/// Requires the GL context to be current ([`GlGuard`] held). The
/// returned texture must be destroyed via [`delete_memory_texture`]
/// while a guard is still held.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn import_memory_texture(
	export: &backend::GlMemoryExport,
	width: i32,
	height: i32,
	params: &VideoParams,
) -> error::Result<GlMemoryTexture> {
	imp::import_memory_texture(export, width, height, params)
}

/// Destroy a zero-copy GL texture and its imported memory object.
///
/// # Safety
///
/// Requires the GL context to be current.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn delete_memory_texture(texture: &GlMemoryTexture) {
	imp::delete_memory_texture(texture);
}

/// Flush all GL commands (cross-API visibility boundary for the first
/// zero-copy version; later replaced by external semaphores).
pub fn gl_finish() {
	#[cfg(target_os = "macos")]
	{
		// macOS has no zero-copy GL path yet; the CGL implementation does
		// not expose glFinish. Nothing to do.
	}
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	imp::gl_finish();
}

/// Issue a GL memory barrier covering texture reads/writes.
pub fn gl_memory_barrier() {
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	imp::gl_memory_barrier();
}

#[cfg(target_os = "linux")]
mod imp {
	use super::*;
	use crate::image::{BitDepth, Components};
	use crate::instance::OfxRectD;
	use libloading::Library;
	use std::ffi::{c_char, c_void, CStr};
	use std::os::fd::IntoRawFd;
	use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

	const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();
	const EGL_NO_CONTEXT: *mut c_void = std::ptr::null_mut();
	const EGL_NO_SURFACE: *mut c_void = std::ptr::null_mut();
	const EGL_TRUE: u32 = 1;
	const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313F;
	const EGL_SURFACE_TYPE: i32 = 0x3033;
	const EGL_PBUFFER_BIT: i32 = 0x0001;
	const EGL_RENDERABLE_TYPE: i32 = 0x3040;
	const EGL_OPENGL_BIT: i32 = 0x0008;
	const EGL_RED_SIZE: i32 = 0x3024;
	const EGL_GREEN_SIZE: i32 = 0x3023;
	const EGL_BLUE_SIZE: i32 = 0x3022;
	const EGL_ALPHA_SIZE: i32 = 0x3021;
	const EGL_NONE: i32 = 0x3038;
	const EGL_WIDTH: i32 = 0x3057;
	const EGL_HEIGHT: i32 = 0x3056;
	const EGL_OPENGL_API: u32 = 0x30A2;
	const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;
	const EGL_CONTEXT_MINOR_VERSION: i32 = 0x30FB;
	const EGL_CONTEXT_OPENGL_PROFILE_MASK: i32 = 0x30FD;
	const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: i32 = 0x00000001;
	const EGL_VENDOR: i32 = 0x3053;
	const EGL_VERSION: i32 = 0x3054;
	const EGL_RENDERER_EXT: i32 = 0x335F;
	const EGL_DRM_RENDER_NODE_FILE_EXT: i32 = 0x3377;

	const GL_TEXTURE_2D: u32 = 0x0DE1;
	const GL_RGBA: u32 = 0x1908;
	const GL_UNSIGNED_BYTE: u32 = 0x1401;
	const GL_FLOAT: u32 = 0x1406;
	const GL_RGBA32F: u32 = 0x8814;
	const GL_RGBA8: u32 = 0x8058;
	const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
	const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
	const GL_NEAREST: u32 = 0x2600;
	const GL_TEXTURE_WRAP_S: u32 = 0x2802;
	const GL_TEXTURE_WRAP_T: u32 = 0x2803;
	const GL_CLAMP_TO_EDGE: u32 = 0x812F;
	const GL_FRAMEBUFFER: u32 = 0x8D40;
	const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
	const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
	const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
	const GL_PACK_ALIGNMENT: u32 = 0x0D05;
	const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
	const GL_VERSION: u32 = 0x1F02;
	const GL_NO_ERROR: u32 = 0;
	const GL_TEXTURE_FETCH_BARRIER_BIT: u32 = 0x00000008;
	const GL_SHADER_IMAGE_ACCESS_BARRIER_BIT: u32 = 0x00000020;
	const GL_HANDLE_TYPE_OPAQUE_FD_EXT: u32 = 0x9586;

	type EGLDisplay = *mut c_void;
	type EGLConfig = *mut c_void;
	type EGLContext = *mut c_void;
	type EGLSurface = *mut c_void;
	type EGLDeviceEXT = *mut c_void;
	type EGLGetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;
	type EGLGetDisplay = unsafe extern "C" fn(*mut c_void) -> EGLDisplay;
	type EGLGetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const i32) -> EGLDisplay;
	type EGLQueryDevices = unsafe extern "C" fn(i32, *mut EGLDeviceEXT, *mut i32) -> u32;
	type EGLQueryDeviceString = unsafe extern "C" fn(EGLDeviceEXT, i32) -> *const c_char;
	type EGLInitialize = unsafe extern "C" fn(EGLDisplay, *mut i32, *mut i32) -> u32;
	type EGLBindAPI = unsafe extern "C" fn(u32) -> u32;
	type EGLChooseConfig =
		unsafe extern "C" fn(EGLDisplay, *const i32, *mut EGLConfig, i32, *mut i32) -> u32;
	type EGLCreateContext =
		unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const i32) -> EGLContext;
	type EGLCreatePbufferSurface =
		unsafe extern "C" fn(EGLDisplay, EGLConfig, *const i32) -> EGLSurface;
	type EGLMakeCurrent =
		unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> u32;
	type EGLDestroySurface = unsafe extern "C" fn(EGLDisplay, EGLSurface);
	type EGLDestroyContext = unsafe extern "C" fn(EGLDisplay, EGLContext);
	type EGLTerminate = unsafe extern "C" fn(EGLDisplay) -> u32;
	type EGLQueryString = unsafe extern "C" fn(EGLDisplay, i32) -> *const c_char;
	type EGLGetError = unsafe extern "C" fn() -> i32;

	struct EglFns {
		get_proc: EGLGetProcAddress,
		get_display: EGLGetDisplay,
		get_platform_display: Option<EGLGetPlatformDisplay>,
		query_devices: Option<EGLQueryDevices>,
		query_device_string: Option<EGLQueryDeviceString>,
		initialize: EGLInitialize,
		bind_api: EGLBindAPI,
		choose_config: EGLChooseConfig,
		create_context: EGLCreateContext,
		create_pbuffer_surface: EGLCreatePbufferSurface,
		make_current: EGLMakeCurrent,
		destroy_surface: EGLDestroySurface,
		destroy_context: EGLDestroyContext,
		terminate: EGLTerminate,
		query_string: EGLQueryString,
		get_error: EGLGetError,
	}

	impl EglFns {
		/// Resolves the EGL entry points this bridge needs from an already
		/// loaded `libEGL`.
		///
		/// Functions that every EGL 1.4 library exports are required
		/// ([`load_sym`]); the device-enumeration and platform-display functions
		/// (`eglGetPlatformDisplayEXT`, `eglQueryDevicesEXT`,
		/// `eglQueryDeviceStringEXT`) are optional ([`ext_sym`]) and stored as
		/// `Option`s, which is what makes the multi-GPU path in [`pick_display`]
		/// best-effort. Returns an error naming the first missing required
		/// symbol.
		fn load(lib: &Library) -> Result<Self, String> {
			unsafe {
				let get_proc: EGLGetProcAddress = load_sym(lib, b"eglGetProcAddress\0")?;
				Ok(Self {
					get_proc,
					get_display: load_sym(lib, b"eglGetDisplay\0")?,
					get_platform_display: ext_sym(get_proc, b"eglGetPlatformDisplayEXT\0"),
					query_devices: ext_sym(get_proc, b"eglQueryDevicesEXT\0"),
					query_device_string: ext_sym(get_proc, b"eglQueryDeviceStringEXT\0"),
					initialize: load_sym(lib, b"eglInitialize\0")?,
					bind_api: load_sym(lib, b"eglBindAPI\0")?,
					choose_config: load_sym(lib, b"eglChooseConfig\0")?,
					create_context: load_sym(lib, b"eglCreateContext\0")?,
					create_pbuffer_surface: load_sym(lib, b"eglCreatePbufferSurface\0")?,
					make_current: load_sym(lib, b"eglMakeCurrent\0")?,
					destroy_surface: load_sym(lib, b"eglDestroySurface\0")?,
					destroy_context: load_sym(lib, b"eglDestroyContext\0")?,
					terminate: load_sym(lib, b"eglTerminate\0")?,
					query_string: load_sym(lib, b"eglQueryString\0")?,
					get_error: load_sym(lib, b"eglGetError\0")?,
				})
			}
		}
	}

	type GLGetString = unsafe extern "C" fn(u32) -> *const c_char;
	type GLGenTextures = unsafe extern "C" fn(i32, *mut u32);
	type GLDeleteTextures = unsafe extern "C" fn(i32, *const u32);
	type GLBindTexture = unsafe extern "C" fn(u32, u32);
	type GLTexParameteri = unsafe extern "C" fn(u32, u32, i32);
	type GLTexImage2D = unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
	type GLGenFramebuffers = unsafe extern "C" fn(i32, *mut u32);
	type GLDeleteFramebuffers = unsafe extern "C" fn(i32, *const u32);
	type GLBindFramebuffer = unsafe extern "C" fn(u32, u32);
	type GLFramebufferTexture2D = unsafe extern "C" fn(u32, u32, u32, u32, i32);
	type GLCheckFramebufferStatus = unsafe extern "C" fn(u32) -> u32;
	type GLViewport = unsafe extern "C" fn(i32, i32, i32, i32);
	type GLClearColor = unsafe extern "C" fn(f32, f32, f32, f32);
	type GLClear = unsafe extern "C" fn(u32);
	type GLReadPixels = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void);
	type GLGetError = unsafe extern "C" fn() -> u32;
	type GLFinish = unsafe extern "C" fn();
	type GLMemoryBarrier = unsafe extern "C" fn(u32);
	type GLPixelStorei = unsafe extern "C" fn(u32, i32);
	type GLCreateMemoryObjects = unsafe extern "C" fn(i32, *mut u32);
	type GLDeleteMemoryObjects = unsafe extern "C" fn(i32, *const u32);
	type GLImportMemoryFd = unsafe extern "C" fn(u32, u64, u32, i32);
	type GLTexStorageMem2D = unsafe extern "C" fn(u32, i32, u32, i32, i32, u32, u64);

	type GLMemoryBarrierFn = GLMemoryBarrier;

	struct GlFns {
		get_string: GLGetString,
		gen_textures: GLGenTextures,
		delete_textures: GLDeleteTextures,
		bind_texture: GLBindTexture,
		tex_parameter_i: GLTexParameteri,
		tex_image_2d: GLTexImage2D,
		gen_framebuffers: GLGenFramebuffers,
		delete_framebuffers: GLDeleteFramebuffers,
		bind_framebuffer: GLBindFramebuffer,
		framebuffer_texture_2d: GLFramebufferTexture2D,
		check_framebuffer_status: GLCheckFramebufferStatus,
		viewport: GLViewport,
		clear_color: GLClearColor,
		clear: GLClear,
		read_pixels: GLReadPixels,
		get_error: GLGetError,
		finish: GLFinish,
		memory_barrier: Option<GLMemoryBarrierFn>,
		pixel_store_i: GLPixelStorei,
	}

	impl GlFns {
		/// Resolves every core GL function this bridge calls from `libGL` and
		/// stores them as typed function pointers.
		///
		/// All these entry points are required ([`load_sym`]); `glMemoryBarrier`
		/// is optional ([`ext_sym_from_lib`]) because some drivers only expose
		/// it with GL 4.2+ / `ARB_shader_image_load_store`. Returns an error
		/// naming the first missing symbol. Called from [`create_context`].
		fn load(lib: &Library) -> Result<Self, String> {
			unsafe {
				Ok(Self {
					get_string: load_sym(lib, b"glGetString\0")?,
					gen_textures: load_sym(lib, b"glGenTextures\0")?,
					delete_textures: load_sym(lib, b"glDeleteTextures\0")?,
					bind_texture: load_sym(lib, b"glBindTexture\0")?,
					tex_parameter_i: load_sym(lib, b"glTexParameteri\0")?,
					tex_image_2d: load_sym(lib, b"glTexImage2D\0")?,
					gen_framebuffers: load_sym(lib, b"glGenFramebuffers\0")?,
					delete_framebuffers: load_sym(lib, b"glDeleteFramebuffers\0")?,
					bind_framebuffer: load_sym(lib, b"glBindFramebuffer\0")?,
					framebuffer_texture_2d: load_sym(lib, b"glFramebufferTexture2D\0")?,
					check_framebuffer_status: load_sym(lib, b"glCheckFramebufferStatus\0")?,
					viewport: load_sym(lib, b"glViewport\0")?,
					clear_color: load_sym(lib, b"glClearColor\0")?,
					clear: load_sym(lib, b"glClear\0")?,
					read_pixels: load_sym(lib, b"glReadPixels\0")?,
					get_error: load_sym(lib, b"glGetError\0")?,
					finish: load_sym(lib, b"glFinish\0")?,
					memory_barrier: ext_sym_from_lib(lib, b"glMemoryBarrier\0"),
					pixel_store_i: load_sym(lib, b"glPixelStorei\0")?,
				})
			}
		}
	}

	struct GlMemoryFns {
		create: GLCreateMemoryObjects,
		delete: GLDeleteMemoryObjects,
		import: GLImportMemoryFd,
		tex_storage: GLTexStorageMem2D,
	}

	impl GlMemoryFns {
		fn load(egl: &EglFns) -> Option<Self> {
			unsafe {
				Some(Self {
					create: ext_sym(egl.get_proc, b"glCreateMemoryObjectsEXT\0")?,
					delete: ext_sym(egl.get_proc, b"glDeleteMemoryObjectsEXT\0")?,
					import: ext_sym(egl.get_proc, b"glImportMemoryFdEXT\0")?,
					tex_storage: ext_sym(egl.get_proc, b"glTexStorageMem2DEXT\0")?,
				})
			}
		}
	}

	/// Resolves a required symbol from a loaded library and copies it out as
	/// `T`.
	///
	/// `name` must be the NUL-terminated ASCII symbol name; the error
	/// message strips the terminator. Returns `Err` with the symbol name and
	/// the loader's message when the library does not export it. Used for
	/// the EGL and GL functions that `libEGL`/`libGL` export directly.
	///
	/// # Safety
	///
	/// `T` must be the exact signature and calling convention of `name`;
	/// calling through a mismatched pointer is undefined behavior. The
	/// library must stay loaded while the returned pointer is used
	/// ([`GlContext`](GlContext) keeps both `Library` handles alive).
	unsafe fn load_sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
		let sym = unsafe { lib.get::<T>(name) }.map_err(|e| {
			format!(
				"{}: {e}",
				String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
			)
		})?;
		Ok(*sym)
	}

	/// Resolves an optional EGL/GL extension entry point through
	/// `eglGetProcAddress`, returning `None` when it is not exported.
	///
	/// `name` must be NUL-terminated (the `CStr` conversion rejects names
	/// without a terminator, returning `None`). Used for extension functions
	/// and for core entry points newer than the ones `libEGL`/`libGL` export
	/// directly.
	///
	/// # Safety
	///
	/// `get_proc` must be a valid `eglGetProcAddress`, and `T` must be the
	/// exact signature and calling convention of `name`; calling through a
	/// mismatched pointer is undefined behavior. The pointer is only valid
	/// while the context that exported it is alive.
	unsafe fn ext_sym<T: Copy>(get_proc: EGLGetProcAddress, name: &[u8]) -> Option<T> {
		let c = CStr::from_bytes_with_nul(name).ok()?;
		let p = unsafe { get_proc(c.as_ptr()) };
		if p.is_null() {
			None
		} else {
			Some(unsafe { std::mem::transmute_copy(&p) })
		}
	}

	/// Looks up an optional symbol in an already loaded library, returning
	/// `None` when it is absent.
	///
	/// Unlike [`ext_sym`], this goes through the dynamic linker
	/// (`libloading::Library::get`) rather than `eglGetProcAddress`, and is
	/// used for entry points such as `glMemoryBarrier` that some drivers
	/// export from `libGL` directly instead of through the EGL dispatch.
	///
	/// # Safety
	///
	/// `T` must be the exact signature and calling convention of `name`;
	/// calling through a mismatched pointer is undefined behavior. `lib`
	/// must stay loaded for as long as the returned pointer is used.
	unsafe fn ext_sym_from_lib<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
		unsafe { lib.get::<T>(name) }.ok().map(|s| *s)
	}

	fn cstr(p: *const c_char) -> String {
		if p.is_null() {
			String::new()
		} else {
			unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
		}
	}

	struct GlContext {
		display: EGLDisplay,
		context: EGLContext,
		surface: EGLSurface,
		egl: EglFns,
		gl: GlFns,
		memory: Option<GlMemoryFns>,
		_egl_lib: Library,
		_gl_lib: Library,
	}

	// SAFETY: the global GL_LOCK serializes every make-current/GL call;
	// the pointers are only dereferenced by the thread holding the guard.
	unsafe impl Send for GlContext {}
	unsafe impl Sync for GlContext {}

	impl GlContext {
		fn drop_current(&self) {
			unsafe {
				(self.egl.make_current)(
					self.display,
					EGL_NO_SURFACE,
					EGL_NO_SURFACE,
					EGL_NO_CONTEXT,
				);
			}
		}
	}

	impl Drop for GlContext {
		fn drop(&mut self) {
			unsafe {
				(self.egl.make_current)(
					self.display,
					EGL_NO_SURFACE,
					EGL_NO_SURFACE,
					EGL_NO_CONTEXT,
				);
				if !self.surface.is_null() {
					(self.egl.destroy_surface)(self.display, self.surface);
				}
				if !self.context.is_null() {
					(self.egl.destroy_context)(self.display, self.context);
				}
				if !self.display.is_null() {
					(self.egl.terminate)(self.display);
				}
			}
		}
	}

	static GL_CONTEXT: OnceLock<Result<Arc<GlContext>, String>> = OnceLock::new();
	static GL_LOCK: Mutex<()> = Mutex::new(());

	fn lock() -> MutexGuard<'static, ()> {
		GL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn err(msg: &str) -> crate::error::Error {
		crate::error::Error::Failed(format!("gl_bridge: {msg}"))
	}

	fn context() -> crate::error::Result<&'static Arc<GlContext>> {
		match GL_CONTEXT.get_or_init(create_context) {
			Ok(c) => Ok(c),
			Err(e) => Err(err(e)),
		}
	}

	/// Renderer name preference (set from the engine's Vulkan adapter).
	static PREFERRED_RENDERER: Mutex<Option<String>> = Mutex::new(None);

	/// Records the GL renderer name that [`pick_display`] should prefer.
	///
	/// The engine passes the Vulkan adapter name so multi-GPU machines pick
	/// the EGL device driving the same GPU as wgpu. Only the first non-empty
	/// name is kept: later calls are ignored, because the GL context (and
	/// therefore the display choice) is created once. `name` is compared
	/// case-insensitively as a substring in either direction. No-op for an
	/// empty name.
	pub fn set_preferred_renderer(name: &str) {
		let mut slot = PREFERRED_RENDERER.lock().unwrap_or_else(|e| e.into_inner());
		if slot.is_none() && !name.is_empty() {
			*slot = Some(name.to_string());
		}
	}

	/// Chooses the EGL display to create the GL context on.
	///
	/// When `EGL_EXT_platform_device` plus `eglQueryDevicesEXT` /
	/// `eglQueryDeviceStringEXT` are available, enumerates up to 8 devices,
	/// initializes each, reads vendor/renderer/DRM node (logged when
	/// `QUERCUS_OFX_GL_TRACE` is set), and returns the first device whose
	/// renderer matches the preferred renderer (case-insensitive substring
	/// in either direction, set by [`set_preferred_renderer`]). Otherwise it
	/// prefers the first non-software device (not llvmpipe, vendor not Mesa),
	/// then the first enumerated device. Falls back to
	/// `eglGetDisplay(EGL_DEFAULT_DISPLAY)` plus `eglInitialize` when device
	/// enumeration is unavailable or yields nothing.
	///
	/// # Errors
	///
	/// Returns `Err` when the default display is `EGL_NO_DISPLAY` or
	/// `eglInitialize` fails on it. Devices that fail to initialize are
	/// skipped.
	fn pick_display(egl: &EglFns) -> Result<EGLDisplay, String> {
		let preferred = PREFERRED_RENDERER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.unwrap_or_default()
			.to_ascii_lowercase();
		let mut first: Option<EGLDisplay> = None;
		let mut first_hardware: Option<EGLDisplay> = None;
		if let (Some(get_platform), Some(query_devices), Some(query_device_string)) = (
			egl.get_platform_display,
			egl.query_devices,
			egl.query_device_string,
		) {
			let mut devices = [std::ptr::null_mut(); 8];
			let mut count = 0i32;
			let ok = unsafe { query_devices(8, devices.as_mut_ptr(), &mut count) };
			if ok == EGL_TRUE {
				for (i, dev) in devices.iter().copied().enumerate() {
					if i >= count as usize {
						break;
					}
					let dpy =
						unsafe { get_platform(EGL_PLATFORM_DEVICE_EXT, dev, std::ptr::null()) };
					if dpy.is_null() {
						continue;
					}
					let mut major = 0i32;
					let mut minor = 0i32;
					if unsafe { (egl.initialize)(dpy, &mut major, &mut minor) } != EGL_TRUE {
						continue;
					}
					let vendor = cstr(unsafe { (egl.query_string)(dpy, EGL_VENDOR) });
					let renderer = cstr(unsafe { query_device_string(dev, EGL_RENDERER_EXT) });
					let node =
						cstr(unsafe { query_device_string(dev, EGL_DRM_RENDER_NODE_FILE_EXT) });
					if std::env::var_os("QUERCUS_OFX_GL_TRACE").is_some() {
						eprintln!("[ofx-gl] egl device {i}: vendor={vendor} renderer={renderer} node={node}");
					}
					let software = renderer.to_ascii_lowercase().contains("llvmpipe")
						|| vendor.to_ascii_lowercase().contains("mesa");
					if !preferred.is_empty() {
						let r = renderer.to_ascii_lowercase();
						if r.contains(&preferred) || preferred.contains(&r) {
							return Ok(dpy);
						}
					}
					if !software && first_hardware.is_none() {
						first_hardware = Some(dpy);
					}
					if first.is_none() {
						first = Some(dpy);
					}
				}
			}
		}
		if let Some(dpy) = first_hardware {
			return Ok(dpy);
		}
		if let Some(dpy) = first {
			return Ok(dpy);
		}
		let dpy = unsafe { (egl.get_display)(EGL_DEFAULT_DISPLAY) };
		if dpy.is_null() {
			return Err("eglGetDisplay(EGL_DEFAULT_DISPLAY) returned EGL_NO_DISPLAY".into());
		}
		let mut major = 0i32;
		let mut minor = 0i32;
		if unsafe { (egl.initialize)(dpy, &mut major, &mut minor) } != EGL_TRUE {
			return Err(format!("eglInitialize(default) failed: 0x{:x}", unsafe {
				(egl.get_error)()
			}));
		}
		Ok(dpy)
	}

	/// Builds the process-wide EGL context once (called through the
	/// `OnceLock` in `context`).
	///
	/// Loads `libEGL.so.1` (or `libEGL.so`) and `libGL.so.1` (or
	/// `libGL.so`), resolves the EGL and GL entry point tables
	/// ([`EglFns::load`], [`GlFns::load`]), picks a display with
	/// [`pick_display`], chooses a pbuffer-capable OpenGL config, binds the
	/// desktop GL API, creates a 3.2 core-profile context and a 16×16
	/// pbuffer surface, makes them current, and verifies that `GL_VERSION`
	/// is non-empty. Also probes the optional memory-object entry points
	/// ([`GlMemoryFns::load`]).
	///
	/// # Errors
	///
	/// Returns a human-readable `String` naming the failed step (missing
	/// library, EGL error code, null version string); every partial EGL
	/// object is destroyed/terminated on the error paths. `context` caches
	/// the result and wraps it as `Error::Failed`.
	fn create_context() -> Result<Arc<GlContext>, String> {
		let egl_lib = unsafe { Library::new("libEGL.so.1") }
			.or_else(|_| unsafe { Library::new("libEGL.so") })
			.map_err(|e| format!("load libEGL: {e}"))?;
		let gl_lib = unsafe { Library::new("libGL.so.1") }
			.or_else(|_| unsafe { Library::new("libGL.so") })
			.map_err(|e| format!("load libGL: {e}"))?;
		let egl = EglFns::load(&egl_lib)?;
		let gl = GlFns::load(&gl_lib)?;
		let memory = GlMemoryFns::load(&egl);
		let display = pick_display(&egl)?;
		let vendor = cstr(unsafe { (egl.query_string)(display, EGL_VENDOR) });
		let version = cstr(unsafe { (egl.query_string)(display, EGL_VERSION) });
		if std::env::var_os("QUERCUS_OFX_GL_TRACE").is_some() {
			eprintln!("[ofx-gl] EGL vendor={vendor} version={version}");
		}

		let config_attrs = [
			EGL_SURFACE_TYPE,
			EGL_PBUFFER_BIT,
			EGL_RENDERABLE_TYPE,
			EGL_OPENGL_BIT,
			EGL_RED_SIZE,
			8,
			EGL_GREEN_SIZE,
			8,
			EGL_BLUE_SIZE,
			8,
			EGL_ALPHA_SIZE,
			8,
			EGL_NONE,
		];
		let mut config: EGLConfig = std::ptr::null_mut();
		let mut n = 0i32;
		if unsafe { (egl.choose_config)(display, config_attrs.as_ptr(), &mut config, 1, &mut n) }
			!= EGL_TRUE
			|| n == 0
		{
			unsafe { (egl.terminate)(display) };
			return Err("eglChooseConfig(pbuffer, OpenGL) failed".into());
		}
		if unsafe { (egl.bind_api)(EGL_OPENGL_API) } != EGL_TRUE {
			unsafe { (egl.terminate)(display) };
			return Err("eglBindAPI(OpenGL) failed".into());
		}
		let context_attrs = [
			EGL_CONTEXT_MAJOR_VERSION,
			3,
			EGL_CONTEXT_MINOR_VERSION,
			2,
			EGL_CONTEXT_OPENGL_PROFILE_MASK,
			EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
			EGL_NONE,
		];
		let context = unsafe {
			(egl.create_context)(display, config, EGL_NO_CONTEXT, context_attrs.as_ptr())
		};
		if context.is_null() {
			unsafe { (egl.terminate)(display) };
			return Err(format!("eglCreateContext failed: 0x{:x}", unsafe {
				(egl.get_error)()
			}));
		}
		let pbuffer_attrs = [EGL_WIDTH, 16, EGL_HEIGHT, 16, EGL_NONE];
		let surface =
			unsafe { (egl.create_pbuffer_surface)(display, config, pbuffer_attrs.as_ptr()) };
		if surface.is_null() {
			unsafe {
				(egl.destroy_context)(display, context);
				(egl.terminate)(display);
			}
			return Err(format!("eglCreatePbufferSurface failed: 0x{:x}", unsafe {
				(egl.get_error)()
			}));
		}
		if unsafe { (egl.make_current)(display, surface, surface, context) } != EGL_TRUE {
			unsafe {
				(egl.destroy_surface)(display, surface);
				(egl.destroy_context)(display, context);
				(egl.terminate)(display);
			}
			return Err(format!("eglMakeCurrent failed: 0x{:x}", unsafe {
				(egl.get_error)()
			}));
		}
		let gl_version = cstr(unsafe { (gl.get_string)(GL_VERSION) });
		if gl_version.is_empty() {
			unsafe {
				(egl.make_current)(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
				(egl.destroy_surface)(display, surface);
				(egl.destroy_context)(display, context);
				(egl.terminate)(display);
			}
			return Err("glGetString(GL_VERSION) returned null".into());
		}
		if std::env::var_os("QUERCUS_OFX_GL_TRACE").is_some() {
			eprintln!("[ofx-gl] GL_VERSION={gl_version}");
		}
		Ok(Arc::new(GlContext {
			display,
			context,
			surface,
			egl,
			gl,
			memory,
			_egl_lib: egl_lib,
			_gl_lib: gl_lib,
		}))
	}

	pub struct GuardInner {
		_lock: Option<MutexGuard<'static, ()>>,
		_ctx: Arc<GlContext>,
	}

	thread_local! {
		static ACQUIRE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
	}

	/// Returns whether the calling thread currently holds the GL context
	/// current, including nested [`acquire`] guards.
	///
	/// Reads a thread-local depth counter and issues no GL calls. The draw
	/// suite uses it to avoid issuing GL commands outside a render action,
	/// where nothing guarantees a current context.
	pub fn is_current() -> bool {
		ACQUIRE_DEPTH.with(|d| d.get() > 0)
	}

	/// Makes the process-wide EGL context current on the calling thread and
	/// returns a guard that undoes that on drop.
	///
	/// Holds the global `GL_LOCK` for the whole guard, so only one thread at
	/// a time has the shared context current; the render action is expected
	/// to hold one guard across all OFX suite callbacks. Re-entrant on the
	/// same thread: if a guard is already held, only `ACQUIRE_DEPTH` is
	/// incremented and no lock is taken, and only the outermost drop clears
	/// current and releases the lock.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when the context cannot be created or
	/// `eglMakeCurrent` fails (the EGL error code is included).
	pub fn acquire() -> crate::error::Result<GlGuard> {
		let ctx = context()?.clone();
		let depth = ACQUIRE_DEPTH.with(|d| d.get());
		if depth > 0 {
			ACQUIRE_DEPTH.with(|d| d.set(depth + 1));
			return Ok(GlGuard {
				_inner: GuardInner {
					_lock: None,
					_ctx: ctx,
				},
			});
		}
		let guard = lock();
		let ok =
			unsafe { (ctx.egl.make_current)(ctx.display, ctx.surface, ctx.surface, ctx.context) };
		if ok != EGL_TRUE {
			drop(guard);
			return Err(err(&format!("eglMakeCurrent failed: 0x{:x}", unsafe {
				(ctx.egl.get_error)()
			})));
		}
		ACQUIRE_DEPTH.with(|d| d.set(1));
		Ok(GlGuard {
			_inner: GuardInner {
				_lock: Some(guard),
				_ctx: ctx,
			},
		})
	}

	impl Drop for GuardInner {
		fn drop(&mut self) {
			let depth = ACQUIRE_DEPTH.with(|d| d.get());
			if depth <= 1 {
				ACQUIRE_DEPTH.with(|d| d.set(0));
				self._ctx.drop_current();
				drop(self._lock.take());
			} else {
				ACQUIRE_DEPTH.with(|d| d.set(depth - 1));
			}
		}
	}

	/// Reports whether the process-wide EGL context is available.
	///
	/// Creates it lazily on first call (`OnceLock`, idempotent; failures are
	/// cached too), so this may load `libEGL`/`libGL` and create a context.
	/// The render decision uses it as the "target texture can have a real GL
	/// name" gate; `false` means the caller must fall back to the CPU path.
	pub fn gl_available() -> bool {
		context().is_ok()
	}

	/// Reports whether the current context exposes the
	/// `GL_EXT_memory_object_fd` extension used by
	/// [`import_memory_texture`].
	///
	/// True only when all four entry points (`glCreateMemoryObjectsEXT`,
	/// `glDeleteMemoryObjectsEXT`, `glImportMemoryFdEXT`,
	/// `glTexStorageMem2DEXT`) resolved through `eglGetProcAddress`. Lazily
	/// creates the context, so this doubles as a probe; `false` when the
	/// context cannot be created or the extension is missing.
	pub fn gl_memory_object_available() -> bool {
		context().map(|c| c.memory.is_some()).unwrap_or(false)
	}

	fn check_error(what: &str) -> crate::error::Result<()> {
		let ctx = context()?;
		let e = unsafe { (ctx.gl.get_error)() };
		if e != GL_NO_ERROR {
			Err(err(&format!("{what} failed: GL error 0x{e:x}")))
		} else {
			Ok(())
		}
	}

	fn gl_texture_format(params: &VideoParams) -> crate::error::Result<(u32, u32, usize)> {
		match params.format {
			f if f == crate::render::PIXEL_FORMAT_F32 => Ok((GL_RGBA32F, GL_FLOAT, 4)),
			f if f == crate::render::PIXEL_FORMAT_U8 => Ok((GL_RGBA8, GL_UNSIGNED_BYTE, 1)),
			f => Err(err(&format!(
				"target format {f} unsupported for GL (F32/U8 only)"
			))),
		}
	}

	/// Binds `name` and applies the sampling state the offscreen FBO
	/// requires.
	///
	/// The min/mag filters are `GL_NEAREST` (a mipmap min filter would make
	/// the level-0 FBO incomplete) and both wrap modes are
	/// `GL_CLAMP_TO_EDGE`, so sampling at the texture edge never pulls in
	/// border texels. `ctx` must be the live context and `name` a texture
	/// created by it; the texture is left bound to `GL_TEXTURE_2D`.
	fn set_texture_params(ctx: &GlContext, name: u32) {
		unsafe {
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
		}
	}

	/// Creates an unwritten output texture of `w`×`h` whose internal format
	/// follows `params.format` (`PIXEL_FORMAT_F32` → `GL_RGBA32F`,
	/// `PIXEL_FORMAT_U8` → `GL_RGBA8`).
	///
	/// Sampling is set by [`set_texture_params`] (nearest, clamp-to-edge) so
	/// the texture is usable as an FBO color attachment, and the real GL
	/// texture name is returned. The plugin receives that name through
	/// `kOfxImageEffectPropOpenGLTextureIndex`.
	///
	/// # Errors
	///
	/// Fails on a non-positive size, an unsupported `params.format`, or a GL
	/// error reported after creation. The context must be current.
	pub fn create_output_texture(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<i32> {
		let ctx = context()?;
		if w <= 0 || h <= 0 {
			return Err(err("invalid texture size"));
		}
		let (internal, upload_type, _) = gl_texture_format(params)?;
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_image_2d)(
				GL_TEXTURE_2D,
				0,
				internal as i32,
				w,
				h,
				0,
				GL_RGBA,
				upload_type,
				std::ptr::null(),
			);
		}
		set_texture_params(ctx, name);
		check_error("create_output_texture")?;
		Ok(name as i32)
	}

	/// Creates an RGBA32F input texture of `w`×`h` and uploads `pixels`.
	///
	/// `pixels` must hold tightly packed RGBA `f32` samples, row-major with
	/// `w * h * 16` bytes or more (the sampler reads exactly that prefix).
	/// The upload uses `UNPACK_ALIGNMENT = 1`, and the sampling state is set
	/// by [`set_texture_params`] (nearest, clamp-to-edge) so the texture can
	/// also be attached to an FBO. Returns the real GL texture name.
	///
	/// # Errors
	///
	/// Fails on a non-positive size, a too-short `pixels` slice, or a GL
	/// error reported right after the upload. The context must be current.
	pub fn create_input_texture(w: i32, h: i32, pixels: &[u8]) -> crate::error::Result<i32> {
		let ctx = context()?;
		if w <= 0 || h <= 0 || pixels.len() < (w as usize * h as usize * 16) {
			return Err(err("invalid input texture size/data"));
		}
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.pixel_store_i)(GL_UNPACK_ALIGNMENT, 1);
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_image_2d)(
				GL_TEXTURE_2D,
				0,
				GL_RGBA32F as i32,
				w,
				h,
				0,
				GL_RGBA,
				GL_FLOAT,
				pixels.as_ptr().cast(),
			);
		}
		set_texture_params(ctx, name);
		check_error("create_input_texture")?;
		Ok(name as i32)
	}

	/// Deletes a GL texture name created by this module
	/// (`glDeleteTextures`).
	///
	/// `name <= 0` is ignored, and nothing happens when the context cannot
	/// be created. The caller must not use `name` afterwards; a texture
	/// still attached to a live FBO must not be deleted before that FBO.
	pub fn delete_gl_texture(name: i32) {
		if name <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let name = name as u32;
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
			}
		}
	}

	/// Imports a wgpu/Vulkan `OPAQUE_FD` allocation as a real GL texture
	/// that aliases the same GPU memory, so the OFX plugin can render in
	/// place instead of readback.
	///
	/// `export` is the Vulkan export produced by
	/// `GpuContext::export_gl_memory`; `width` and `height` must equal
	/// `export.width()` and `export.height()` (pixels), and `params` selects
	/// the internal format from `params.format` (`PIXEL_FORMAT_F32` →
	/// `GL_RGBA32F`, `PIXEL_FORMAT_U8` → `GL_RGBA8`; other fields are
	/// unused). The fd is duplicated and ownership transfers to the GL
	/// memory object (`GL_EXT_memory_object_fd` with
	/// `GL_HANDLE_TYPE_OPAQUE_FD_EXT`), which the returned
	/// [`GlMemoryTexture`] owns; destroy it with [`delete_memory_texture`]
	/// while the guard is held.
	///
	/// # Errors
	///
	/// Returns an error when the context is not current, the
	/// `GL_EXT_memory_object_fd` entry points are missing, the requested
	/// size disagrees with the export, `dup` fails, or `glImportMemoryFdEXT`
	/// / `glTexStorageMem2DEXT` fail (partial texture/memory objects are
	/// cleaned up and the fd closed on import failure).
	pub fn import_memory_texture(
		export: &quercus_core::backend::GlMemoryExport,
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<GlMemoryTexture> {
		if !is_current() {
			return Err(err("import_memory_texture requires a current GL context"));
		}
		let ctx = context()?;
		let memory_fns = ctx
			.memory
			.as_ref()
			.ok_or_else(|| err("GL_EXT_memory_object_fd is not available"))?;
		let (internal, _, _) = gl_texture_format(params)?;
		if (w as u32) != export.width() || (h as u32) != export.height() {
			return Err(err("exported texture size mismatch"));
		}
		let fd = export
			.try_clone_fd()
			.map_err(|e| err(&format!("dup exported fd: {e}")))?;
		let raw_fd = fd.into_raw_fd();
		let mut memory = 0u32;
		unsafe {
			(memory_fns.create)(1, &mut memory);
			let _ = (ctx.gl.get_error)();
			(memory_fns.import)(memory, export.size(), GL_HANDLE_TYPE_OPAQUE_FD_EXT, raw_fd);
		}
		let import_err = unsafe { (ctx.gl.get_error)() };
		if import_err != GL_NO_ERROR {
			unsafe {
				libc::close(raw_fd);
				(memory_fns.delete)(1, &memory);
			}
			return Err(err(&format!(
				"glImportMemoryFdEXT failed: 0x{import_err:x}"
			)));
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(memory_fns.tex_storage)(GL_TEXTURE_2D, 1, internal, w, h, memory, 0);
		}
		set_texture_params(ctx, name);
		if let Err(e) = check_error("glTexStorageMem2DEXT") {
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
				(memory_fns.delete)(1, &memory);
			}
			return Err(e);
		}
		Ok(GlMemoryTexture {
			name: name as i32,
			memory,
			width: w,
			height: h,
		})
	}

	/// Destroys a zero-copy texture returned by [`import_memory_texture`]:
	/// deletes the GL texture, then the GL memory object that owns the
	/// dup'ed `OPAQUE_FD` file descriptor.
	///
	/// `texture.name <= 0` is ignored, as is a missing context. Must be
	/// called with a [`GlGuard`] held and before the exporting wgpu/Vulkan
	/// texture is destroyed.
	pub fn delete_memory_texture(texture: &GlMemoryTexture) {
		if texture.name <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let name = texture.name as u32;
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
				if let Some(m) = ctx.memory.as_ref() {
					(m.delete)(1, &texture.memory);
				}
			}
		}
	}

	/// Creates a framebuffer object and attaches `tex` as color attachment 0.
	///
	/// `tex` must be a live 2D texture name; `w` and `h` are the expected
	/// dimensions in pixels and are only validated (`> 0`), not used to
	/// allocate storage (the texture already owns it). The new FBO is left
	/// bound and its name is returned.
	///
	/// # Errors
	///
	/// Returns an error when any argument is non-positive, when
	/// `glGenFramebuffers` fails, or when `glCheckFramebufferStatus` reports
	/// anything other than `GL_FRAMEBUFFER_COMPLETE`; a partially created
	/// FBO is deleted on the error paths. The context must be current.
	pub fn create_fbo(tex: i32, w: i32, h: i32) -> crate::error::Result<i32> {
		let ctx = context()?;
		if tex <= 0 || w <= 0 || h <= 0 {
			return Err(err("invalid FBO parameters"));
		}
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut fbo = 0u32;
		unsafe {
			(ctx.gl.gen_framebuffers)(1, &mut fbo);
			(ctx.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
			(ctx.gl.framebuffer_texture_2d)(
				GL_FRAMEBUFFER,
				GL_COLOR_ATTACHMENT0,
				GL_TEXTURE_2D,
				tex as u32,
				0,
			);
		}
		let status = unsafe { (ctx.gl.check_framebuffer_status)(GL_FRAMEBUFFER) };
		if status != GL_FRAMEBUFFER_COMPLETE {
			unsafe {
				(ctx.gl.delete_framebuffers)(1, &fbo);
			}
			return Err(err(&format!("FBO incomplete: 0x{status:x}")));
		}
		Ok(fbo as i32)
	}

	/// Binds `fbo` as the current draw/read framebuffer
	/// (`GL_FRAMEBUFFER`).
	///
	/// `fbo == 0` selects the default (system) framebuffer; negative values
	/// are clamped to 0. No-op when the context is unavailable. The render
	/// driver binds the output FBO before the GL render action and re-binds
	/// it before readback in case the plugin changed it.
	pub fn bind_fbo(fbo: i32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo.max(0) as u32);
			}
		}
	}

	/// Deletes a framebuffer created by [`create_fbo`].
	///
	/// `fbo <= 0` is ignored. If `fbo` is currently bound, GL reverts the
	/// binding to the default framebuffer. No-op when the context is
	/// unavailable. The color-attachment texture is not destroyed here; use
	/// [`delete_gl_texture`] for it after the FBO is gone.
	pub fn delete_fbo(fbo: i32) {
		if fbo <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let fbo = fbo as u32;
			unsafe {
				(ctx.gl.delete_framebuffers)(1, &fbo);
			}
		}
	}

	/// Sets the GL viewport to the rectangle at origin `(0, 0)` with size
	/// `w`×`h` pixels.
	///
	/// `w` and `h` must be positive; they are the render resolution, not the
	/// window size (rendering is offscreen into an FBO). Called by
	/// `render_driver` after binding the output FBO and again after the
	/// render action, because a plugin may leave a different viewport
	/// behind.
	pub fn set_viewport(w: i32, h: i32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.viewport)(0, 0, w, h);
			}
		}
	}

	/// Reads the currently bound framebuffer back into a CPU image.
	///
	/// `w` and `h` are the pixel dimensions to read; they must be positive
	/// and match the bound FBO's color attachment. `params.format` selects
	/// the `glReadPixels` type and the conversion: F32 → `GL_FLOAT` (copied
	/// as-is), U8 → `GL_UNSIGNED_BYTE` scaled by `1/255.0`. The GL origin is
	/// bottom-left, so rows are flipped vertically to produce a
	/// top-left-origin [`Image`] with `BitDepth::Float` and
	/// `Components::Rgba`, matching the CPU path's output format (ready for
	/// `render_driver::write_output_frame`).
	///
	/// # Errors
	///
	/// Fails when the format is neither F32 nor U8, when `w`/`h` are not
	/// positive, or when `glReadPixels` reports a GL error. The context must
	/// be current and the target FBO still bound.
	pub fn read_pixels_to_image(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<Arc<crate::image::Image>> {
		let ctx = context()?;
		let (_, read_type, bpc) = gl_texture_format(params)?;
		if w <= 0 || h <= 0 {
			return Err(err("invalid readback size"));
		}
		let count = (w as usize)
			.saturating_mul(h as usize)
			.saturating_mul(4)
			.saturating_mul(bpc);
		let mut raw = vec![0u8; count];
		unsafe {
			(ctx.gl.pixel_store_i)(GL_PACK_ALIGNMENT, 1);
			(ctx.gl.read_pixels)(0, 0, w, h, GL_RGBA, read_type, raw.as_mut_ptr().cast());
		}
		check_error("glReadPixels")?;
		let mut img = crate::image::Image::allocate(
			BitDepth::Float,
			Components::Rgba,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w as f64,
				y2: h as f64,
			},
		);
		let dst = img.data.as_mut_slice();
		let src_row = w as usize * 4 * bpc;
		let dst_row = w as usize * 16;
		for y in 0..h as usize {
			let src_y = h as usize - 1 - y;
			let src = &raw[src_y * src_row..(src_y + 1) * src_row];
			let d = &mut dst[y * dst_row..(y + 1) * dst_row];
			for x in 0..w as usize {
				let (r, g, b, a) = if bpc == 4 {
					let p = &src[x * 16..x * 16 + 16];
					(
						f32::from_ne_bytes(p[0..4].try_into().unwrap()),
						f32::from_ne_bytes(p[4..8].try_into().unwrap()),
						f32::from_ne_bytes(p[8..12].try_into().unwrap()),
						f32::from_ne_bytes(p[12..16].try_into().unwrap()),
					)
				} else {
					let p = &src[x * 4..x * 4 + 4];
					(
						p[0] as f32 / 255.0,
						p[1] as f32 / 255.0,
						p[2] as f32 / 255.0,
						p[3] as f32 / 255.0,
					)
				};
				let o = x * 16;
				d[o..o + 4].copy_from_slice(&r.to_ne_bytes());
				d[o + 4..o + 8].copy_from_slice(&g.to_ne_bytes());
				d[o + 8..o + 12].copy_from_slice(&b.to_ne_bytes());
				d[o + 12..o + 16].copy_from_slice(&a.to_ne_bytes());
			}
		}
		Ok(Arc::new(img))
	}

	/// Sets the clear color used by the next [`gl_clear`].
	///
	/// `r`, `g`, `b` and `a` are the RGBA components in `0.0..=1.0` (values
	/// outside that range are clamped by GL). Used by the FBO round-trip
	/// tests and the failure/placeholder fill; it only changes GL state and
	/// does not clear anything by itself. The context must be current.
	pub fn gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.clear_color)(r, g, b, a);
			}
		}
	}

	/// Clears the color buffer of the currently bound framebuffer with the
	/// color last set by [`gl_clear_color`] (`glClear(GL_COLOR_BUFFER_BIT)`).
	///
	/// Used by the FBO tests and the failure/placeholder fill; the plugin's
	/// normal render path issues its own GL commands. The context must be
	/// current.
	pub fn gl_clear() {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.clear)(GL_COLOR_BUFFER_BIT);
			}
		}
	}

	/// Blocks until every previously issued GL command has completed
	/// (`glFinish`).
	///
	/// This is the cross-API visibility boundary of the zero-copy path: the
	/// caller then hands the shared allocation back to wgpu/Vulkan. A
	/// cheaper `glFlush` is not sufficient because the other API reads the
	/// memory directly. No-op when the context is unavailable; called by
	/// `render_driver` right after the plugin's GL render.
	pub fn gl_finish() {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.finish)();
			}
		}
	}

	/// Issues `glMemoryBarrier` for the texture-fetch and shader-image-access
	/// stages so the plugin's GL writes become visible to later texture
	/// reads (and imported writes become visible to the plugin).
	///
	/// Used by the zero-copy path between the plugin's render action and the
	/// wgpu/Vulkan consumer, paired with [`gl_finish`]. No-op when the entry
	/// point was not resolved (some drivers do not export it from `libGL`)
	/// or the context is unavailable.
	pub fn gl_memory_barrier() {
		if let Ok(ctx) = context() {
			if let Some(barrier) = ctx.gl.memory_barrier {
				unsafe {
					barrier(GL_TEXTURE_FETCH_BARRIER_BIT | GL_SHADER_IMAGE_ACCESS_BARRIER_BIT);
				}
			}
		}
	}

	/// Returns the `GL_VERSION` string of the process-wide EGL context.
	///
	/// The context must already be current (a [`GlGuard`] is held); this
	/// does not acquire it. Returns `None` when the context cannot be
	/// created or `glGetString(GL_VERSION)` returns null. Tests and the
	/// `QUERCUS_OFX_GL_TRACE` output use it as evidence that a real driver
	/// context was created.
	pub fn gl_version_string() -> Option<String> {
		let ctx = context().ok()?;
		let p = unsafe { (ctx.gl.get_string)(GL_VERSION) };
		if p.is_null() {
			None
		} else {
			Some(cstr(p))
		}
	}
}

// ---- Windows: WGL + GL_EXT_memory_object_win32 ------------------------

#[cfg(target_os = "windows")]
mod imp {
	use super::*;
	use crate::image::{BitDepth, Components};
	use crate::instance::OfxRectD;
	use std::ffi::{c_char, c_void, CStr};
	use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
	use windows_sys::Win32::Foundation::{CloseHandle, HINSTANCE, HWND};
	use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC, HDC};
	use windows_sys::Win32::Graphics::OpenGL::{
		wglCreateContext, wglDeleteContext, wglGetProcAddress, wglMakeCurrent, ChoosePixelFormat,
		SetPixelFormat, HGLRC, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_SUPPORT_OPENGL,
		PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR,
	};
	use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
	use windows_sys::Win32::UI::WindowsAndMessaging::{
		CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW, CS_OWNDC,
		WNDCLASSW, WS_OVERLAPPEDWINDOW,
	};

	const GL_TEXTURE_2D: u32 = 0x0DE1;
	const GL_RGBA: u32 = 0x1908;
	const GL_UNSIGNED_BYTE: u32 = 0x1401;
	const GL_FLOAT: u32 = 0x1406;
	const GL_RGBA32F: u32 = 0x8814;
	const GL_RGBA8: u32 = 0x8058;
	const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
	const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
	const GL_NEAREST: u32 = 0x2600;
	const GL_TEXTURE_WRAP_S: u32 = 0x2802;
	const GL_TEXTURE_WRAP_T: u32 = 0x2803;
	const GL_CLAMP_TO_EDGE: u32 = 0x812F;
	const GL_FRAMEBUFFER: u32 = 0x8D40;
	const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
	const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
	const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
	const GL_PACK_ALIGNMENT: u32 = 0x0D05;
	const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
	const GL_VERSION: u32 = 0x1F02;
	const GL_NO_ERROR: u32 = 0;
	const GL_TEXTURE_FETCH_BARRIER_BIT: u32 = 0x00000008;
	const GL_SHADER_IMAGE_ACCESS_BARRIER_BIT: u32 = 0x00000020;
	const GL_HANDLE_TYPE_OPAQUE_WIN32_EXT: u32 = 0x9587;

	const WGL_CONTEXT_MAJOR_VERSION_ARB: i32 = 0x2091;
	const WGL_CONTEXT_MINOR_VERSION_ARB: i32 = 0x2092;
	const WGL_CONTEXT_PROFILE_MASK_ARB: i32 = 0x9126;
	const WGL_CONTEXT_CORE_PROFILE_BIT_ARB: i32 = 0x0000_0001;

	type WglCreateContextAttribsArb = unsafe extern "system" fn(HDC, HGLRC, *const i32) -> HGLRC;
	type GLGetString = unsafe extern "system" fn(u32) -> *const c_char;
	type GLGenTextures = unsafe extern "system" fn(i32, *mut u32);
	type GLDeleteTextures = unsafe extern "system" fn(i32, *const u32);
	type GLBindTexture = unsafe extern "system" fn(u32, u32);
	type GLTexParameteri = unsafe extern "system" fn(u32, u32, i32);
	type GLTexImage2D =
		unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
	type GLGenFramebuffers = unsafe extern "system" fn(i32, *mut u32);
	type GLDeleteFramebuffers = unsafe extern "system" fn(i32, *const u32);
	type GLBindFramebuffer = unsafe extern "system" fn(u32, u32);
	type GLFramebufferTexture2D = unsafe extern "system" fn(u32, u32, u32, u32, i32);
	type GLCheckFramebufferStatus = unsafe extern "system" fn(u32) -> u32;
	type GLViewport = unsafe extern "system" fn(i32, i32, i32, i32);
	type GLClearColor = unsafe extern "system" fn(f32, f32, f32, f32);
	type GLClear = unsafe extern "system" fn(u32);
	type GLReadPixels = unsafe extern "system" fn(i32, i32, i32, i32, u32, u32, *mut c_void);
	type GLGetError = unsafe extern "system" fn() -> u32;
	type GLFinish = unsafe extern "system" fn();
	type GLMemoryBarrier = unsafe extern "system" fn(u32);
	type GLPixelStorei = unsafe extern "system" fn(u32, i32);
	type GLCreateMemoryObjects = unsafe extern "system" fn(i32, *mut u32);
	type GLDeleteMemoryObjects = unsafe extern "system" fn(i32, *const u32);
	type GLImportMemoryWin32Handle = unsafe extern "system" fn(u32, u64, u32, *mut c_void);
	type GLTexStorageMem2D = unsafe extern "system" fn(u32, i32, u32, i32, i32, u32, u64);

	struct GlFns {
		get_string: GLGetString,
		gen_textures: GLGenTextures,
		delete_textures: GLDeleteTextures,
		bind_texture: GLBindTexture,
		tex_parameter_i: GLTexParameteri,
		tex_image_2d: GLTexImage2D,
		gen_framebuffers: GLGenFramebuffers,
		delete_framebuffers: GLDeleteFramebuffers,
		bind_framebuffer: GLBindFramebuffer,
		framebuffer_texture_2d: GLFramebufferTexture2D,
		check_framebuffer_status: GLCheckFramebufferStatus,
		viewport: GLViewport,
		clear_color: GLClearColor,
		clear: GLClear,
		read_pixels: GLReadPixels,
		get_error: GLGetError,
		finish: GLFinish,
		memory_barrier: Option<GLMemoryBarrier>,
		pixel_store_i: GLPixelStorei,
	}

	impl GlFns {
		/// Resolves every GL function this bridge calls and stores them as typed
		/// function pointers.
		///
		/// All core entry points are required ([`load_gl`]); `glMemoryBarrier`
		/// is optional ([`load_gl_optional`]) because it only exists with
		/// GL 4.2+ / `ARB_shader_image_load_store`. Returns an error naming the
		/// first missing symbol. Must run with a current context so
		/// `wglGetProcAddress` can resolve context-dependent entry points.
		fn load() -> Result<Self, String> {
			unsafe {
				Ok(Self {
					get_string: load_gl(b"glGetString\0")?,
					gen_textures: load_gl(b"glGenTextures\0")?,
					delete_textures: load_gl(b"glDeleteTextures\0")?,
					bind_texture: load_gl(b"glBindTexture\0")?,
					tex_parameter_i: load_gl(b"glTexParameteri\0")?,
					tex_image_2d: load_gl(b"glTexImage2D\0")?,
					gen_framebuffers: load_gl(b"glGenFramebuffers\0")?,
					delete_framebuffers: load_gl(b"glDeleteFramebuffers\0")?,
					bind_framebuffer: load_gl(b"glBindFramebuffer\0")?,
					framebuffer_texture_2d: load_gl(b"glFramebufferTexture2D\0")?,
					check_framebuffer_status: load_gl(b"glCheckFramebufferStatus\0")?,
					viewport: load_gl(b"glViewport\0")?,
					clear_color: load_gl(b"glClearColor\0")?,
					clear: load_gl(b"glClear\0")?,
					read_pixels: load_gl(b"glReadPixels\0")?,
					get_error: load_gl(b"glGetError\0")?,
					finish: load_gl(b"glFinish\0")?,
					memory_barrier: load_gl_optional(b"glMemoryBarrier\0"),
					pixel_store_i: load_gl(b"glPixelStorei\0")?,
				})
			}
		}
	}

	struct GlMemoryFns {
		create: GLCreateMemoryObjects,
		delete: GLDeleteMemoryObjects,
		import: GLImportMemoryWin32Handle,
		tex_storage: GLTexStorageMem2D,
	}

	impl GlMemoryFns {
		fn load() -> Option<Self> {
			unsafe {
				Some(Self {
					create: load_gl_optional(b"glCreateMemoryObjectsEXT\0")?,
					delete: load_gl_optional(b"glDeleteMemoryObjectsEXT\0")?,
					import: load_gl_optional(b"glImportMemoryWin32HandleEXT\0")?,
					tex_storage: load_gl_optional(b"glTexStorageMem2DEXT\0")?,
				})
			}
		}
	}

	/// Resolves a required GL entry point by name, wrapping
	/// [`load_gl_optional`].
	///
	/// `name` must be the NUL-terminated ASCII symbol (the trailing NUL is
	/// stripped in the error message). Returns an error mentioning the
	/// missing function when neither `wglGetProcAddress` nor
	/// `opengl32.dll` exports it.
	///
	/// # Safety
	///
	/// As for [`load_gl_optional`]: `T` must match the real signature and
	/// calling convention of `name`, and the context that exported the
	/// pointer must stay alive while it is used.
	unsafe fn load_gl<T: Copy>(name: &[u8]) -> Result<T, String> {
		unsafe { load_gl_optional(name) }.ok_or_else(|| {
			format!(
				"missing GL function {}",
				String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
			)
		})
	}

	/// Resolves a GL entry point by name, returning `None` instead of an
	/// error when it is unavailable.
	///
	/// Tries `wglGetProcAddress` first (for extensions and core entry points
	/// newer than 1.1), then falls back to `GetProcAddress` on
	/// `opengl32.dll`. `name` must be the NUL-terminated ASCII symbol name.
	///
	/// # Safety
	///
	/// The caller must instantiate `T` with the exact signature and calling
	/// convention (`extern "system"`) of the named function; calling through
	/// a wrong type is undefined behavior. The returned pointer is only
	/// valid while the context that exported it is alive.
	unsafe fn load_gl_optional<T: Copy>(name: &[u8]) -> Option<T> {
		let p = unsafe { wglGetProcAddress(name.as_ptr()) };
		if let Some(f) = p {
			return Some(unsafe { std::mem::transmute_copy(&f) });
		}
		let module = unsafe { GetModuleHandleW(windows_sys::w!("opengl32.dll")) };
		if module.is_null() {
			return None;
		}
		let p = unsafe { GetProcAddress(module, name.as_ptr()) };
		p.map(|f| unsafe { std::mem::transmute_copy(&f) })
	}

	fn cstr(p: *const c_char) -> String {
		if p.is_null() {
			String::new()
		} else {
			unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
		}
	}

	struct GlContext {
		hwnd: HWND,
		hdc: HDC,
		hrc: HGLRC,
		gl: GlFns,
		memory: Option<GlMemoryFns>,
	}

	// SAFETY: the global GL_LOCK serializes every make-current/GL call;
	// window/context handles are only used by the thread holding the guard.
	unsafe impl Send for GlContext {}
	unsafe impl Sync for GlContext {}

	impl GlContext {
		fn drop_current(&self) {
			unsafe {
				wglMakeCurrent(self.hdc, std::ptr::null_mut());
			}
		}
	}

	impl Drop for GlContext {
		fn drop(&mut self) {
			unsafe {
				wglMakeCurrent(self.hdc, std::ptr::null_mut());
				if !self.hrc.is_null() {
					wglDeleteContext(self.hrc);
				}
				if !self.hdc.is_null() {
					ReleaseDC(self.hwnd, self.hdc);
				}
				if !self.hwnd.is_null() {
					DestroyWindow(self.hwnd);
				}
				UnregisterClassW(windows_sys::w!("QuercusOfxGlWindow"), std::ptr::null_mut());
			}
		}
	}

	static GL_CONTEXT: OnceLock<Result<Arc<GlContext>, String>> = OnceLock::new();
	static GL_LOCK: Mutex<()> = Mutex::new(());

	fn lock() -> MutexGuard<'static, ()> {
		GL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn err(msg: &str) -> crate::error::Error {
		crate::error::Error::Failed(format!("gl_bridge: {msg}"))
	}

	fn context() -> crate::error::Result<&'static Arc<GlContext>> {
		match GL_CONTEXT.get_or_init(create_context) {
			Ok(c) => Ok(c),
			Err(e) => Err(err(e)),
		}
	}

	/// Builds the process-wide WGL context once (called through the
	/// `OnceLock` in `context`).
	///
	/// Registers a `CS_OWNDC` hidden window class, creates an 8×8 window and
	/// its DC, selects and sets a 32-bit RGBA pixel format, then creates a
	/// throw-away legacy context to resolve `wglCreateContextAttribsARB`.
	/// With that entry point it tries a 4.5 and then a 3.2 core-profile
	/// context (first success wins) and verifies that `GL_VERSION` is
	/// non-empty. Loads the core GL entry points ([`GlFns::load`]) and the
	/// optional memory-object ones ([`GlMemoryFns::load`]).
	///
	/// # Errors
	///
	/// Returns a human-readable `String` naming the failed Win32/GL call;
	/// every partially created window, DC and context is released on the
	/// error paths. `context` caches the result and wraps it as
	/// `Error::Failed`.
	fn create_context() -> Result<Arc<GlContext>, String> {
		let hinstance: HINSTANCE = unsafe { GetModuleHandleW(std::ptr::null()) };
		if hinstance.is_null() {
			return Err("GetModuleHandleW(NULL) failed".into());
		}
		let class_name = windows_sys::w!("QuercusOfxGlWindow");
		let wc = WNDCLASSW {
			style: CS_OWNDC,
			lpfnWndProc: Some(DefWindowProcW),
			hInstance: hinstance,
			lpszClassName: class_name,
			..Default::default()
		};
		unsafe {
			RegisterClassW(&wc);
		}
		let hwnd = unsafe {
			CreateWindowExW(
				0,
				class_name,
				windows_sys::w!("Quercus OFX GL"),
				WS_OVERLAPPEDWINDOW,
				0,
				0,
				8,
				8,
				std::ptr::null_mut(),
				std::ptr::null_mut(),
				hinstance,
				std::ptr::null(),
			)
		};
		if hwnd.is_null() {
			return Err("CreateWindowExW failed".into());
		}
		let hdc = unsafe { GetDC(hwnd) };
		if hdc.is_null() {
			unsafe { DestroyWindow(hwnd) };
			return Err("GetDC failed".into());
		}
		let mut pfd: PIXELFORMATDESCRIPTOR = unsafe { std::mem::zeroed() };
		pfd.nSize = std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16;
		pfd.nVersion = 1;
		pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
		pfd.iPixelType = PFD_TYPE_RGBA;
		pfd.cColorBits = 32;
		pfd.cAlphaBits = 8;
		pfd.cDepthBits = 24;
		let format = unsafe { ChoosePixelFormat(hdc, &pfd) };
		if format == 0 {
			unsafe {
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("ChoosePixelFormat failed".into());
		}
		if unsafe { SetPixelFormat(hdc, format, &pfd) } == 0 {
			unsafe {
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("SetPixelFormat failed".into());
		}

		let legacy = unsafe { wglCreateContext(hdc) };
		if legacy.is_null() {
			unsafe {
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("wglCreateContext failed".into());
		}
		if unsafe { wglMakeCurrent(hdc, legacy) } == 0 {
			unsafe {
				wglDeleteContext(legacy);
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("wglMakeCurrent(legacy) failed".into());
		}
		let create_attribs: Option<WglCreateContextAttribsArb> =
			unsafe { load_gl_optional(b"wglCreateContextAttribsARB\0") };
		let Some(create_attribs) = create_attribs else {
			unsafe {
				wglMakeCurrent(hdc, std::ptr::null_mut());
				wglDeleteContext(legacy);
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("wglCreateContextAttribsARB missing".into());
		};
		let mut hrc = std::ptr::null_mut();
		for (major, minor) in [(4, 5), (3, 2)] {
			let attrs = [
				WGL_CONTEXT_MAJOR_VERSION_ARB,
				major,
				WGL_CONTEXT_MINOR_VERSION_ARB,
				minor,
				WGL_CONTEXT_PROFILE_MASK_ARB,
				WGL_CONTEXT_CORE_PROFILE_BIT_ARB,
				0,
			];
			hrc = unsafe { create_attribs(hdc, std::ptr::null_mut(), attrs.as_ptr()) };
			if !hrc.is_null() {
				break;
			}
		}
		unsafe { wglDeleteContext(legacy) };
		if hrc.is_null() {
			unsafe {
				wglMakeCurrent(hdc, std::ptr::null_mut());
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("wglCreateContextAttribsARB(core) failed".into());
		}
		if unsafe { wglMakeCurrent(hdc, hrc) } == 0 {
			unsafe {
				wglDeleteContext(hrc);
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("wglMakeCurrent(core) failed".into());
		}
		let gl = GlFns::load()?;
		let version = cstr(unsafe { (gl.get_string)(GL_VERSION) });
		if version.is_empty() {
			unsafe {
				wglMakeCurrent(hdc, std::ptr::null_mut());
				wglDeleteContext(hrc);
				ReleaseDC(hwnd, hdc);
				DestroyWindow(hwnd);
			}
			return Err("glGetString(GL_VERSION) returned null".into());
		}
		if std::env::var_os("QUERCUS_OFX_GL_TRACE").is_some() {
			eprintln!("[ofx-gl] WGL GL_VERSION={version}");
		}
		let memory = GlMemoryFns::load();
		Ok(Arc::new(GlContext {
			hwnd,
			hdc,
			hrc,
			gl,
			memory,
		}))
	}

	pub struct GuardInner {
		_lock: Option<MutexGuard<'static, ()>>,
		_ctx: Arc<GlContext>,
	}

	thread_local! {
		static ACQUIRE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
	}

	/// Returns whether the calling thread currently holds the GL context
	/// current, including nested [`acquire`] guards.
	///
	/// Reads a thread-local depth counter and issues no GL calls. The draw
	/// suite uses it to avoid issuing GL commands outside a render action,
	/// where nothing guarantees a current context.
	pub fn is_current() -> bool {
		ACQUIRE_DEPTH.with(|d| d.get() > 0)
	}

	/// Makes the process-wide WGL context current on the calling thread and
	/// returns a guard that undoes that on drop.
	///
	/// Holds the global `GL_LOCK` for the whole guard, so only one thread at
	/// a time has the shared context current; the render action is expected
	/// to hold one guard across all OFX suite callbacks. Re-entrant on the
	/// same thread: if a guard is already held, only `ACQUIRE_DEPTH` is
	/// incremented and no lock is taken, and only the outermost drop clears
	/// current and releases the lock.
	///
	/// # Errors
	///
	/// Returns `Error::Failed` when the context cannot be created or
	/// `wglMakeCurrent` fails.
	pub fn acquire() -> crate::error::Result<GlGuard> {
		let ctx = context()?.clone();
		let depth = ACQUIRE_DEPTH.with(|d| d.get());
		if depth > 0 {
			ACQUIRE_DEPTH.with(|d| d.set(depth + 1));
			return Ok(GlGuard {
				_inner: GuardInner {
					_lock: None,
					_ctx: ctx,
				},
			});
		}
		let guard = lock();
		if unsafe { wglMakeCurrent(ctx.hdc, ctx.hrc) } == 0 {
			drop(guard);
			return Err(err("wglMakeCurrent failed"));
		}
		ACQUIRE_DEPTH.with(|d| d.set(1));
		Ok(GlGuard {
			_inner: GuardInner {
				_lock: Some(guard),
				_ctx: ctx,
			},
		})
	}

	impl Drop for GuardInner {
		fn drop(&mut self) {
			let depth = ACQUIRE_DEPTH.with(|d| d.get());
			if depth <= 1 {
				ACQUIRE_DEPTH.with(|d| d.set(0));
				self._ctx.drop_current();
				drop(self._lock.take());
			} else {
				ACQUIRE_DEPTH.with(|d| d.set(depth - 1));
			}
		}
	}

	/// Reports whether the process-wide WGL context is available.
	///
	/// Creates it lazily on first call (`OnceLock`, idempotent; failures are
	/// cached too), so this may create the hidden window and GL context. The
	/// render decision uses it as the "target texture can have a real GL
	/// name" gate; `false` means the caller must fall back to the CPU path.
	pub fn gl_available() -> bool {
		context().is_ok()
	}

	/// Reports whether the current context exposes the Windows memory-object
	/// extension used by [`import_memory_texture`].
	///
	/// True only when all four entry points (`glCreateMemoryObjectsEXT`,
	/// `glDeleteMemoryObjectsEXT`, `glImportMemoryWin32HandleEXT`,
	/// `glTexStorageMem2DEXT`) resolved. Lazily creates the context (cached),
	/// so this works as a one-line probe; `false` when the context cannot be
	/// created or the extension is missing.
	pub fn gl_memory_object_available() -> bool {
		context().map(|c| c.memory.is_some()).unwrap_or(false)
	}

	fn check_error(what: &str) -> crate::error::Result<()> {
		let ctx = context()?;
		let e = unsafe { (ctx.gl.get_error)() };
		if e != GL_NO_ERROR {
			Err(err(&format!("{what} failed: GL error 0x{e:x}")))
		} else {
			Ok(())
		}
	}

	fn gl_texture_format(params: &VideoParams) -> crate::error::Result<(u32, u32, usize)> {
		match params.format {
			f if f == crate::render::PIXEL_FORMAT_F32 => Ok((GL_RGBA32F, GL_FLOAT, 4)),
			f if f == crate::render::PIXEL_FORMAT_U8 => Ok((GL_RGBA8, GL_UNSIGNED_BYTE, 1)),
			f => Err(err(&format!(
				"target format {f} unsupported for GL (F32/U8 only)"
			))),
		}
	}

	/// Binds `name` and applies the sampling state the offscreen FBO
	/// requires.
	///
	/// The min/mag filters are `GL_NEAREST` (a mipmap min filter would make
	/// the level-0 FBO incomplete) and both wrap modes are
	/// `GL_CLAMP_TO_EDGE`, so sampling at the texture edge never pulls in
	/// border texels. `ctx` must be the live context and `name` a texture
	/// created by it; the texture is left bound to `GL_TEXTURE_2D`.
	fn set_texture_params(ctx: &GlContext, name: u32) {
		unsafe {
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
			(ctx.gl.tex_parameter_i)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
		}
	}

	/// Creates an unwritten output texture of `w`×`h` whose internal format
	/// follows `params.format` (`PIXEL_FORMAT_F32` → `GL_RGBA32F`,
	/// `PIXEL_FORMAT_U8` → `GL_RGBA8`).
	///
	/// Sampling is set by [`set_texture_params`] (nearest, clamp-to-edge) so
	/// the texture is usable as an FBO color attachment, and the real GL
	/// texture name is returned. The plugin receives that name through
	/// `kOfxImageEffectPropOpenGLTextureIndex`.
	///
	/// # Errors
	///
	/// Fails on a non-positive size, an unsupported `params.format`, or a GL
	/// error reported after creation. The context must be current.
	pub fn create_output_texture(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<i32> {
		let ctx = context()?;
		if w <= 0 || h <= 0 {
			return Err(err("invalid texture size"));
		}
		let (internal, upload_type, _) = gl_texture_format(params)?;
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_image_2d)(
				GL_TEXTURE_2D,
				0,
				internal as i32,
				w,
				h,
				0,
				GL_RGBA,
				upload_type,
				std::ptr::null(),
			);
		}
		set_texture_params(ctx, name);
		check_error("create_output_texture")?;
		Ok(name as i32)
	}

	/// Creates an RGBA32F input texture of `w`×`h` and uploads `pixels`.
	///
	/// `pixels` must hold tightly packed RGBA `f32` samples, row-major with
	/// `w * h * 16` bytes or more (the sampler reads exactly that prefix).
	/// The upload uses `UNPACK_ALIGNMENT = 1`, and the sampling state is set
	/// by [`set_texture_params`] (nearest, clamp-to-edge) so the texture can
	/// also be attached to an FBO. Returns the real GL texture name.
	///
	/// # Errors
	///
	/// Fails on a non-positive size, a too-short `pixels` slice, or a GL
	/// error reported right after the upload. The context must be current.
	pub fn create_input_texture(w: i32, h: i32, pixels: &[u8]) -> crate::error::Result<i32> {
		let ctx = context()?;
		if w <= 0 || h <= 0 || pixels.len() < (w as usize * h as usize * 16) {
			return Err(err("invalid input texture size/data"));
		}
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.pixel_store_i)(GL_UNPACK_ALIGNMENT, 1);
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(ctx.gl.tex_image_2d)(
				GL_TEXTURE_2D,
				0,
				GL_RGBA32F as i32,
				w,
				h,
				0,
				GL_RGBA,
				GL_FLOAT,
				pixels.as_ptr().cast(),
			);
		}
		set_texture_params(ctx, name);
		check_error("create_input_texture")?;
		Ok(name as i32)
	}

	/// Deletes a GL texture name created by this module
	/// (`glDeleteTextures`).
	///
	/// `name <= 0` is ignored, and nothing happens when the context cannot
	/// be created. The caller must not use `name` afterwards; a texture
	/// still attached to a live FBO must not be deleted before that FBO.
	pub fn delete_gl_texture(name: i32) {
		if name <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let name = name as u32;
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
			}
		}
	}

	/// Imports a wgpu/Vulkan `OPAQUE_WIN32` allocation as a real GL texture
	/// that aliases the same GPU memory, so the OFX plugin can render in
	/// place instead of readback.
	///
	/// `export` is the Vulkan export produced by
	/// `GpuContext::export_gl_memory`; `width` and `height` must equal
	/// `export.width()` and `export.height()` (pixels), and `params` selects
	/// the internal format from `params.format` (`PIXEL_FORMAT_F32` →
	/// `GL_RGBA32F`, `PIXEL_FORMAT_U8` → `GL_RGBA8`; other fields are
	/// unused). The NT handle is duplicated and ownership transfers to the
	/// GL memory object, which the returned [`GlMemoryTexture`] owns;
	/// destroy it with [`delete_memory_texture`] while the guard is held.
	///
	/// # Errors
	///
	/// Returns an error when the context is not current, the
	/// `GL_EXT_memory_object_win32` entry points are missing, the requested
	/// size disagrees with the export, the handle cannot be duplicated, or
	/// `glImportMemoryWin32HandleEXT` / `glTexStorageMem2DEXT` fail (partial
	/// texture/memory objects are cleaned up).
	pub fn import_memory_texture(
		export: &quercus_core::backend::GlMemoryExport,
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<GlMemoryTexture> {
		if !is_current() {
			return Err(err("import_memory_texture requires a current GL context"));
		}
		let ctx = context()?;
		let memory_fns = ctx
			.memory
			.as_ref()
			.ok_or_else(|| err("GL_EXT_memory_object_win32 is not available"))?;
		let (internal, _, _) = gl_texture_format(params)?;
		if (w as u32) != export.width() || (h as u32) != export.height() {
			return Err(err("exported texture size mismatch"));
		}
		let handle = export
			.try_clone_handle()
			.map_err(|e| err(&format!("duplicate exported handle: {e}")))?;
		let handle_ptr = handle as *mut c_void;
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut memory = 0u32;
		unsafe {
			(memory_fns.create)(1, &mut memory);
			(memory_fns.import)(
				memory,
				export.size(),
				GL_HANDLE_TYPE_OPAQUE_WIN32_EXT,
				handle_ptr,
			);
		}
		let import_err = unsafe { (ctx.gl.get_error)() };
		if import_err != GL_NO_ERROR {
			unsafe {
				CloseHandle(handle_ptr);
				(memory_fns.delete)(1, &memory);
			}
			return Err(err(&format!(
				"glImportMemoryWin32HandleEXT failed: 0x{import_err:x}"
			)));
		}
		let mut name = 0u32;
		unsafe {
			(ctx.gl.gen_textures)(1, &mut name);
			(ctx.gl.bind_texture)(GL_TEXTURE_2D, name);
			(memory_fns.tex_storage)(GL_TEXTURE_2D, 1, internal, w, h, memory, 0);
		}
		set_texture_params(ctx, name);
		if let Err(e) = check_error("glTexStorageMem2DEXT") {
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
				(memory_fns.delete)(1, &memory);
			}
			return Err(e);
		}
		Ok(GlMemoryTexture {
			name: name as i32,
			memory,
			width: w,
			height: h,
		})
	}

	/// Destroys a zero-copy texture returned by [`import_memory_texture`]:
	/// deletes the GL texture, then the GL memory object that owns the
	/// duplicated Win32 handle.
	///
	/// `texture.name <= 0` is ignored, as is a missing context. Must be
	/// called with a [`GlGuard`] held and before the exporting wgpu/Vulkan
	/// texture is destroyed; the imported memory object is the only GL-side
	/// owner of the handle.
	pub fn delete_memory_texture(texture: &GlMemoryTexture) {
		if texture.name <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let name = texture.name as u32;
			unsafe {
				(ctx.gl.delete_textures)(1, &name);
				if let Some(m) = ctx.memory.as_ref() {
					(m.delete)(1, &texture.memory);
				}
			}
		}
	}

	/// Creates a framebuffer object and attaches `tex` as color attachment 0.
	///
	/// `tex` must be a live 2D texture name; `w` and `h` are the expected
	/// dimensions in pixels and are only validated (`> 0`), not used to
	/// allocate storage (the texture already owns it). The new FBO is left
	/// bound and its name is returned.
	///
	/// # Errors
	///
	/// Returns an error when any argument is non-positive, when
	/// `glGenFramebuffers` fails, or when `glCheckFramebufferStatus` reports
	/// anything other than `GL_FRAMEBUFFER_COMPLETE`; a partially created
	/// FBO is deleted on the error paths. The context must be current.
	pub fn create_fbo(tex: i32, w: i32, h: i32) -> crate::error::Result<i32> {
		let ctx = context()?;
		if tex <= 0 || w <= 0 || h <= 0 {
			return Err(err("invalid FBO parameters"));
		}
		unsafe {
			let _ = (ctx.gl.get_error)();
		}
		let mut fbo = 0u32;
		unsafe {
			(ctx.gl.gen_framebuffers)(1, &mut fbo);
			(ctx.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo);
			(ctx.gl.framebuffer_texture_2d)(
				GL_FRAMEBUFFER,
				GL_COLOR_ATTACHMENT0,
				GL_TEXTURE_2D,
				tex as u32,
				0,
			);
		}
		let status = unsafe { (ctx.gl.check_framebuffer_status)(GL_FRAMEBUFFER) };
		if status != GL_FRAMEBUFFER_COMPLETE {
			unsafe {
				(ctx.gl.delete_framebuffers)(1, &fbo);
			}
			return Err(err(&format!("FBO incomplete: 0x{status:x}")));
		}
		Ok(fbo as i32)
	}

	/// Binds `fbo` as the current draw/read framebuffer
	/// (`GL_FRAMEBUFFER`).
	///
	/// `fbo == 0` selects the default (system) framebuffer; negative values
	/// are clamped to 0. No-op when the context is unavailable. The render
	/// driver binds the output FBO before the GL render action and re-binds
	/// it before readback in case the plugin changed it.
	pub fn bind_fbo(fbo: i32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo.max(0) as u32);
			}
		}
	}

	/// Deletes a framebuffer created by [`create_fbo`].
	///
	/// `fbo <= 0` is ignored. If `fbo` is currently bound, GL reverts the
	/// binding to the default framebuffer. No-op when the context is
	/// unavailable. The color-attachment texture is not destroyed here; use
	/// [`delete_gl_texture`] for it after the FBO is gone.
	pub fn delete_fbo(fbo: i32) {
		if fbo <= 0 {
			return;
		}
		if let Ok(ctx) = context() {
			let fbo = fbo as u32;
			unsafe {
				(ctx.gl.delete_framebuffers)(1, &fbo);
			}
		}
	}

	/// Sets the GL viewport to the rectangle at origin `(0, 0)` with size
	/// `w`×`h` pixels.
	///
	/// `w` and `h` must be positive; they are the render resolution, not the
	/// window size (rendering is offscreen into an FBO). Called by
	/// `render_driver` after binding the output FBO and again after the
	/// render action, because a plugin may leave a different viewport
	/// behind.
	pub fn set_viewport(w: i32, h: i32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.viewport)(0, 0, w, h);
			}
		}
	}

	/// Reads the currently bound framebuffer back into a CPU image.
	///
	/// `w` and `h` are the pixel dimensions to read; they must be positive
	/// and match the bound FBO's color attachment. `params.format` selects
	/// the `glReadPixels` type and the conversion: F32 → `GL_FLOAT` (copied
	/// as-is), U8 → `GL_UNSIGNED_BYTE` scaled by `1/255.0`. The GL origin is
	/// bottom-left, so rows are flipped vertically to produce a
	/// top-left-origin [`Image`] with `BitDepth::Float` and
	/// `Components::Rgba`, matching the CPU path's output format (ready for
	/// `render_driver::write_output_frame`).
	///
	/// # Errors
	///
	/// Fails when the format is neither F32 nor U8, when `w`/`h` are not
	/// positive, or when `glReadPixels` reports a GL error. The context must
	/// be current and the target FBO still bound.
	pub fn read_pixels_to_image(
		w: i32,
		h: i32,
		params: &VideoParams,
	) -> crate::error::Result<Arc<crate::image::Image>> {
		let ctx = context()?;
		let (_, read_type, bpc) = gl_texture_format(params)?;
		if w <= 0 || h <= 0 {
			return Err(err("invalid readback size"));
		}
		let count = (w as usize)
			.saturating_mul(h as usize)
			.saturating_mul(4)
			.saturating_mul(bpc);
		let mut raw = vec![0u8; count];
		unsafe {
			(ctx.gl.pixel_store_i)(GL_PACK_ALIGNMENT, 1);
			(ctx.gl.read_pixels)(0, 0, w, h, GL_RGBA, read_type, raw.as_mut_ptr().cast());
		}
		check_error("glReadPixels")?;
		let mut img = crate::image::Image::allocate(
			BitDepth::Float,
			Components::Rgba,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w as f64,
				y2: h as f64,
			},
		);
		let dst = img.data.as_mut_slice();
		let src_row = w as usize * 4 * bpc;
		let dst_row = w as usize * 16;
		for y in 0..h as usize {
			let src_y = h as usize - 1 - y;
			let src = &raw[src_y * src_row..(src_y + 1) * src_row];
			let d = &mut dst[y * dst_row..(y + 1) * dst_row];
			for x in 0..w as usize {
				let (r, g, b, a) = if bpc == 4 {
					let p = &src[x * 16..x * 16 + 16];
					(
						f32::from_ne_bytes(p[0..4].try_into().unwrap()),
						f32::from_ne_bytes(p[4..8].try_into().unwrap()),
						f32::from_ne_bytes(p[8..12].try_into().unwrap()),
						f32::from_ne_bytes(p[12..16].try_into().unwrap()),
					)
				} else {
					let p = &src[x * 4..x * 4 + 4];
					(
						p[0] as f32 / 255.0,
						p[1] as f32 / 255.0,
						p[2] as f32 / 255.0,
						p[3] as f32 / 255.0,
					)
				};
				let o = x * 16;
				d[o..o + 4].copy_from_slice(&r.to_ne_bytes());
				d[o + 4..o + 8].copy_from_slice(&g.to_ne_bytes());
				d[o + 8..o + 12].copy_from_slice(&b.to_ne_bytes());
				d[o + 12..o + 16].copy_from_slice(&a.to_ne_bytes());
			}
		}
		Ok(Arc::new(img))
	}

	/// Sets the clear color used by the next [`gl_clear`].
	///
	/// `r`, `g`, `b` and `a` are the RGBA components in `0.0..=1.0` (values
	/// outside that range are clamped by GL). Used by the FBO round-trip
	/// tests and the failure/placeholder fill; it only changes GL state and
	/// does not clear anything by itself. The context must be current.
	pub fn gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.clear_color)(r, g, b, a);
			}
		}
	}

	/// Clears the color buffer of the currently bound framebuffer with the
	/// color last set by [`gl_clear_color`] (`glClear(GL_COLOR_BUFFER_BIT)`).
	///
	/// Used by the FBO tests and the failure/placeholder fill; the plugin's
	/// normal render path issues its own GL commands. The context must be
	/// current.
	pub fn gl_clear() {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.clear)(GL_COLOR_BUFFER_BIT);
			}
		}
	}

	/// Blocks until every previously issued GL command has completed
	/// (`glFinish`).
	///
	/// This is the cross-API visibility boundary of the zero-copy path: the
	/// caller then hands the shared allocation back to wgpu/Vulkan. A
	/// cheaper `glFlush` is not sufficient because the other API reads the
	/// memory directly. No-op when the context is unavailable; called by
	/// `render_driver` right after the plugin's GL render.
	pub fn gl_finish() {
		if let Ok(ctx) = context() {
			unsafe {
				(ctx.gl.finish)();
			}
		}
	}

	/// Issues `glMemoryBarrier` for the texture-fetch and shader-image-access
	/// stages so the plugin's GL writes become visible to later texture
	/// reads (and imported writes become visible to the plugin).
	///
	/// Used by the zero-copy path between the plugin's render action and the
	/// wgpu/Vulkan consumer, paired with [`gl_finish`]. No-op when the entry
	/// point was not resolved (it is loaded optionally) or the context is
	/// unavailable.
	pub fn gl_memory_barrier() {
		if let Ok(ctx) = context() {
			if let Some(barrier) = ctx.gl.memory_barrier {
				unsafe {
					barrier(GL_TEXTURE_FETCH_BARRIER_BIT | GL_SHADER_IMAGE_ACCESS_BARRIER_BIT);
				}
			}
		}
	}

	/// Returns the `GL_VERSION` string of the process-wide WGL context.
	///
	/// The context must already be current (a [`GlGuard`] is held); this
	/// does not acquire it. Returns `None` when the context cannot be
	/// created or `glGetString(GL_VERSION)` returns null. Tests and the
	/// `QUERCUS_OFX_GL_TRACE` output use it as evidence that a real driver
	/// context exists (the string looks like `"4.6.0 ..."`).
	pub fn gl_version_string() -> Option<String> {
		let ctx = context().ok()?;
		let p = unsafe { (ctx.gl.get_string)(GL_VERSION) };
		if p.is_null() {
			None
		} else {
			Some(cstr(p))
		}
	}
}

/// Not macOS/Linux/Windows: GL bridge stub

/// All fail/false, the use_opengl decision falls back to CPU).
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
	use super::*;

	/// Linux wiring point: replace with EGL (eglGetDisplay → eglInitialize →
	/// eglChooseConfig → eglCreateContext → eglMakeCurrent); Windows is
	/// replaced with WGL (wglCreateContext/wglMakeCurrent). The signatures are
	/// isomorphic to the macOS implementation.
	pub struct GuardInner;

	/// Reports whether a GL context is available; the stub platform always
	/// returns `false`.
	///
	/// This platform is neither macOS nor Linux/Windows; the render decision
	/// falls back to CPU on `false`, so no context creation is ever attempted
	/// here.
	pub fn gl_available() -> bool {
		false
	}

	/// Reports whether this thread holds GL current; the stub platform has no
	/// context and always returns `false`.
	///
	/// Same meaning as the real implementations: on real platforms it is
	/// `true` when the thread-local nesting depth is greater than 0; callers
	/// use it to skip GL commands when there is no current context.
	pub fn is_current() -> bool {
		false
	}

	/// Acquires a GL context guard; the stub platform always returns
	/// [`crate::error::Error::Failed`].
	///
	/// The error message says the current platform has no GL implementation;
	/// the caller (render driver) falls back to CPU accordingly and should not
	/// treat it as fatal.
	pub fn acquire() -> crate::error::Result<GlGuard> {
		Err(crate::error::Error::Failed(
			"gl_bridge: no platform GL implementation (Linux/Windows stub)".into(),
		))
	}

	/// Creates an output texture (internal format from the target frame
	/// format); the stub platform always returns
	/// [`crate::error::Error::Failed`].
	///
	/// `_w`, `_h` are the pixel dimensions and `_p` provides the target pixel
	/// format (F32 → RGBA32F, U8 → RGBA8); the stub creates no texture and
	/// touches no GL state.
	pub fn create_output_texture(_w: i32, _h: i32, _p: &VideoParams) -> crate::error::Result<i32> {
		Err(crate::error::Error::Failed(
			"gl_bridge: no platform GL implementation".into(),
		))
	}

	/// Creates an input texture and uploads pixels; the stub platform always
	/// returns [`crate::error::Error::Failed`].
	///
	/// `_w`, `_h` are the pixel dimensions; `_p` is tightly packed RGBA F32
	/// pixels (the real implementation requires exactly `w*h*4*4` bytes); the
	/// stub validates nothing, uploads nothing, and just errors.
	pub fn create_input_texture(_w: i32, _h: i32, _p: &[u8]) -> crate::error::Result<i32> {
		Err(crate::error::Error::Failed(
			"gl_bridge: no platform GL implementation".into(),
		))
	}

	/// Deletes a GL texture; the stub platform has no GL context, so this is a
	/// no-op.
	///
	/// `_name` is the texture name the real implementation would return from
	/// [`super::create_output_texture`] / [`super::create_input_texture`];
	/// those functions always fail under the stub, so there is no real texture
	/// to release and this only keeps the cleanup path's signature consistent.
	pub fn delete_gl_texture(_name: i32) {}

	/// Creates an FBO and attaches a color attachment; the stub platform
	/// always returns [`crate::error::Error::Failed`].
	///
	/// `_tex`, `_w`, `_h` have the same meaning as the real implementation
	/// (attachment texture name, width/height in pixels), but the stub creates
	/// no objects; callers fall back to CPU or the GL readback path as usual.
	pub fn create_fbo(_tex: i32, _w: i32, _h: i32) -> crate::error::Result<i32> {
		Err(crate::error::Error::Failed(
			"gl_bridge: no platform GL implementation".into(),
		))
	}

	/// Binds an FBO; the stub platform has no GL context, so this is a no-op.
	///
	/// `_fbo` has the same meaning as the real implementation (0 = system
	/// framebuffer) and is ignored.
	pub fn bind_fbo(_fbo: i32) {}

	/// Deletes an FBO; the stub platform has no GL context, so this is a
	/// no-op.
	///
	/// `_fbo` is the FBO name [`super::create_fbo`] would have returned; under
	/// the stub that function always fails, so there is no real object and
	/// this only keeps the cleanup path's signature consistent.
	pub fn delete_fbo(_fbo: i32) {}

	/// Sets the GL viewport; the stub platform has no GL context, so this is a
	/// no-op.
	///
	/// `_w`, `_h` are the viewport width/height (pixels), same meaning as the
	/// real implementation but ignored.
	pub fn set_viewport(_w: i32, _h: i32) {}

	/// Reads back the current framebuffer and assembles it into an image; the
	/// stub platform always returns [`crate::error::Error::Failed`].
	///
	/// `_w`, `_h` are the pixel dimensions to read and `_p` provides the
	/// target pixel format (F32/U8); both exist only to keep the signature
	/// consistent with the real implementation, and the stub allocates no
	/// image and reads no pixels.
	pub fn read_pixels_to_image(
		_w: i32,
		_h: i32,
		_p: &VideoParams,
	) -> crate::error::Result<Arc<Image>> {
		Err(crate::error::Error::Failed(
			"gl_bridge: no platform GL implementation".into(),
		))
	}

	/// Sets the clear color; the stub platform has no GL context, so this is a
	/// no-op.
	///
	/// `_r`, `_g`, `_b`, `_a` have the same meaning as the real implementation
	/// (RGBA components in `0.0..=1.0`) but are all ignored by this stub; for
	/// the real platform's clear see [`super::gl_clear`].
	pub fn gl_clear_color(_r: f32, _g: f32, _b: f32, _a: f32) {}

	/// Clears the currently bound color buffer; the stub platform has no GL
	/// context, so this is a no-op.
	///
	/// The real implementation clears with the color set by
	/// [`super::gl_clear_color`]; this stub touches no GL state, so callers
	/// must not rely on the clear taking effect.
	pub fn gl_clear() {}

	/// Returns the GL version string; the stub platform has no GL
	/// implementation, so it is always [`None`].
	///
	/// Signature-compatible with the real platform implementations, so tests
	/// and diagnostics can call it unconditionally; callers first use
	/// [`super::gl_available`] to decide whether the GL path should be taken
	/// and then whether to read it.
	pub fn gl_version_string() -> Option<String> {
		None
	}
}

#[cfg(test)]
mod tests;
