//! The GL bridge (`quercus_ofx_gl_*`): the offscreen GL context and FBO
//! plumbing behind interact drawing and GL-mode renders.
//!
//! Context discipline: [`quercus_ofx_gl_acquire`] makes the process-wide
//! offscreen context current on the calling thread (globally
//! serialized; re-entrant per thread) and [`quercus_ofx_gl_release`]
//! releases it. Texture/FBO creation and deletion require an acquired
//! context. Everything fails closed (`false`/`0`/`-1`) when GL is
//! unavailable.

use std::cell::RefCell;

use quercus_ofx_plugin::gl_bridge::{self, GlGuard};
use quercus_ofx_plugin::render::{PIXEL_FORMAT_F32, VideoParams};

use crate::codec::copy_str_out;

thread_local! {
	/// Acquired guards of the calling thread, innermost last
	/// (acquire is re-entrant on the same thread; release pops).
	static GL_GUARDS: RefCell<Vec<GlGuard>> = const { RefCell::new(Vec::new()) };
}

/// Whether the offscreen GL context can be created (false → all other
/// entry points fail; fall back to CPU rendering).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_available() -> bool {
	gl_bridge::gl_available()
}

/// Makes the GL context current on the calling thread. False when GL
/// is unavailable. Every acquire must be balanced by
/// [`quercus_ofx_gl_release`] on the same thread.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_acquire() -> bool {
	match gl_bridge::acquire() {
		Ok(guard) => {
			GL_GUARDS.with(|stack| stack.borrow_mut().push(guard));
			true
		}
		Err(_) => false,
	}
}

/// Releases the calling thread's innermost acquired context. No-op
/// when none is held.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_release() {
	GL_GUARDS.with(|stack| {
		stack.borrow_mut().pop();
	});
}

/// Builds the `VideoParams` the GL bridge needs for a `w` x `h` F32 frame:
/// width and height set, format forced to `PIXEL_FORMAT_F32` (RGBA32F), and
/// every other field left at its default.
///
/// Used by the texture-creation and readback entry points so the bridge's
/// internal-format selection matches the `f32` data this module exchanges.
fn f32_params(w: i32, h: i32) -> VideoParams {
	let mut params = VideoParams::default();
	params.width = w;
	params.height = h;
	params.format = PIXEL_FORMAT_F32;
	params
}

/// Creates an RGBA32F output texture of `width` x `height` pixels (initially
/// undefined contents), returning the real GL texture name.
///
/// # Arguments
///
/// * `width` - Texture width in pixels, normally the target frame width.
/// * `height` - Texture height in pixels, normally the target frame height.
///
/// # Returns
///
/// The GL texture name, or -1 when a dimension is non-positive, GL is
/// unavailable, or texture creation fails. Requires an acquired context; the
/// texture is meant to be attached to an FBO via `quercus_ofx_gl_create_fbo`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_create_output_texture(width: i32, height: i32) -> i32 {
	gl_bridge::create_output_texture(width, height, &f32_params(width, height)).unwrap_or(-1)
}

/// Creates an RGBA32F input texture of `width` x `height` pixels and uploads
/// `pixels` into it, returning the real GL texture name.
///
/// `pixels` must hold exactly `width * height * 4` tightly packed RGBA floats
/// (row-major, `f32`). The new texture is not bound on return.
///
/// # Arguments
///
/// * `width` - Texture width in pixels; must be positive.
/// * `height` - Texture height in pixels; must be positive.
/// * `pixels` - Source samples; null is rejected.
///
/// # Returns
///
/// The GL texture name, or -1 when `pixels` is null, a dimension is
/// non-positive, GL is unavailable, or texture creation/upload fails. Requires
/// an acquired context.
///
/// # Safety
///
/// `pixels` must point to at least `width * height * 4` initialized `f32`
/// values, and an offscreen context must be acquired on the calling thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_gl_create_input_texture(
	width: i32,
	height: i32,
	pixels: *const f32,
) -> i32 {
	if pixels.is_null() || width <= 0 || height <= 0 {
		return -1;
	}
	let len = width as usize * height as usize * 4;
	let bytes = unsafe { std::slice::from_raw_parts(pixels as *const u8, len * 4) };
	gl_bridge::create_input_texture(width, height, bytes).unwrap_or(-1)
}

/// Deletes a GL texture created by `quercus_ofx_gl_create_input_texture` or
/// `quercus_ofx_gl_create_output_texture`.
///
/// `texture <= 0` is ignored; the name is invalid afterwards. A texture still
/// attached to an FBO must be detached (delete the FBO) first.
///
/// Requires an acquired offscreen context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_delete_texture(texture: i32) {
	gl_bridge::delete_gl_texture(texture);
}

/// Creates an FBO with `texture` attached, returning its GL name; -1
/// on failure. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_create_fbo(texture: i32, width: i32, height: i32) -> i32 {
	gl_bridge::create_fbo(texture, width, height).unwrap_or(-1)
}

/// Binds an FBO (0 = the default framebuffer). Requires an acquired
/// context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_bind_fbo(fbo: i32) {
	gl_bridge::bind_fbo(fbo);
}

/// Deletes an FBO created by `quercus_ofx_gl_create_fbo`.
///
/// `fbo <= 0` is ignored, and the default framebuffer is bound first so the
/// object is not deleted while bound. The attached color texture is NOT freed
/// here; release it separately with `quercus_ofx_gl_delete_texture`.
///
/// Requires an acquired offscreen context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_delete_fbo(fbo: i32) {
	gl_bridge::delete_fbo(fbo);
}

/// Sets the viewport of the bound framebuffer: width and height in pixels with
/// the origin fixed at `(0, 0)`.
///
/// # Arguments
///
/// * `width` - Viewport width in pixels (the render resolution).
/// * `height` - Viewport height in pixels.
///
/// Requires an acquired offscreen context; the render driver re-applies the
/// viewport after a plugin has changed it, before readback.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_set_viewport(width: i32, height: i32) {
	gl_bridge::set_viewport(width, height);
}

/// Sets the color the next `quercus_ofx_gl_clear` fills the bound framebuffer
/// with.
///
/// # Arguments
///
/// * `r`, `g`, `b` - Clear color components, normally normalized `0.0..=1.0`.
/// * `a` - Clear alpha, normally `0.0..=1.0`.
///
/// Requires an acquired offscreen context on the calling thread; otherwise the
/// call has no effect.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
	gl_bridge::gl_clear_color(r, g, b, a);
}

/// Clears the color buffer. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_clear() {
	gl_bridge::gl_clear();
}

/// Reads the currently bound framebuffer back as tightly packed RGBA F32
/// (`width * height * 4` floats) and copies up to `out_capacity` floats into
/// `out`.
///
/// # Arguments
///
/// * `width` - Read width in pixels; must be positive.
/// * `height` - Read height in pixels; must be positive.
/// * `out` - Caller-owned destination; null only queries the required size.
/// * `out_capacity` - Capacity of `out` in `f32` values; the copy is
///   `min(out_capacity, width*height*4)` floats.
///
/// # Returns
///
/// The needed float count (`width*height*4`, independent of `out_capacity`), or
/// -1 when `width`/`height` is non-positive or the GL readback fails. Truncation
/// is detected by comparing the return value with `out_capacity`.
///
/// # Safety
///
/// An offscreen context must be acquired on the calling thread, and `out` must
/// be valid for `out_capacity` `f32` writes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_gl_read_pixels_f32(
	width: i32,
	height: i32,
	out: *mut f32,
	out_capacity: usize,
) -> i32 {
	if width <= 0 || height <= 0 {
		return -1;
	}
	let Ok(image) = gl_bridge::read_pixels_to_image(width, height, &f32_params(width, height))
	else {
		return -1;
	};
	let needed = width as usize * height as usize * 4;
	if !out.is_null() {
		let src = image.pixels();
		let floats = needed.min(out_capacity).min(src.len() / 4);
		let src = unsafe { std::slice::from_raw_parts(src.as_ptr() as *const f32, floats) };
		unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), out, floats) };
	}
	needed as i32
}

/// Copies the current GL version string (the `glGetString(GL_VERSION)` text)
/// into `buf`, snprintf-style.
///
/// # Arguments
///
/// * `buf` - Caller-owned output buffer; null (or `buf_len == 0`) only queries
///   the required length.
/// * `buf_len` - Capacity of `buf` in bytes, including room for the NUL.
///
/// # Returns
///
/// The full byte length of the string excluding the NUL, or -1 when GL is
/// unavailable (no context could be created, or the version cannot be queried).
/// A return value `>= buf_len` means the output was truncated to `buf_len - 1`
/// bytes plus a NUL. Does not require an acquired context.
///
/// # Safety
///
/// `buf` must be valid for `buf_len` writes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_gl_version(buf: *mut u8, buf_len: usize) -> i32 {
	match gl_bridge::gl_version_string() {
		Some(v) => copy_str_out(&v, buf, buf_len),
		None => -1,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn release_without_acquire_is_a_noop() {
		quercus_ofx_gl_release();
	}

	#[test]
	fn acquire_release_pairing_on_this_host() {
		// Headless CI may lack GL entirely; when available the
		// acquire/release stack must pair up (and be re-entrant).
		if !quercus_ofx_gl_available() {
			return;
		}
		assert!(quercus_ofx_gl_acquire());
		assert!(quercus_ofx_gl_acquire(), "re-entrant on the same thread");
		quercus_ofx_gl_release();
		quercus_ofx_gl_release();
		quercus_ofx_gl_release();
	}
}
