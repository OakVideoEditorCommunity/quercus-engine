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

fn f32_params(w: i32, h: i32) -> VideoParams {
	let mut params = VideoParams::default();
	params.width = w;
	params.height = h;
	params.format = PIXEL_FORMAT_F32;
	params
}

/// Creates an RGBA32F output texture, returning its GL name; -1 on
/// failure. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_create_output_texture(width: i32, height: i32) -> i32 {
	gl_bridge::create_output_texture(width, height, &f32_params(width, height)).unwrap_or(-1)
}

/// Creates an RGBA32F texture and uploads `pixels` (tight RGBA F32,
/// `width*height*4` floats), returning its GL name; -1 on failure.
/// Requires an acquired context.
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

/// Deletes a GL texture. Requires an acquired context.
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

/// Deletes an FBO. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_delete_fbo(fbo: i32) {
	gl_bridge::delete_fbo(fbo);
}

/// Sets the GL viewport. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_set_viewport(width: i32, height: i32) {
	gl_bridge::set_viewport(width, height);
}

/// Sets the clear color. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_clear_color(r: f32, g: f32, b: f32, a: f32) {
	gl_bridge::gl_clear_color(r, g, b, a);
}

/// Clears the color buffer. Requires an acquired context.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_gl_clear() {
	gl_bridge::gl_clear();
}

/// Reads the current framebuffer back as tight RGBA F32
/// (`width*height*4` floats). Writes min(`out_capacity`, needed)
/// floats and returns the needed count; -1 on failure, or query the
/// size with a null buffer. Requires an acquired context.
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

/// The GL version string, snprintf-style; -1 when GL is unavailable.
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
