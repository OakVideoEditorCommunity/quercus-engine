//! C ABI for `quercus_core::color::ColorProcessor` (prefix
//! `quercus_core_colorproc_`): color-space conversions for plugins.
//!
//! The handle is Arc-backed (add_ref/release pair from
//! `export_handle!`). Processors are immutable once created, so a shared
//! handle is safe to use from any thread. `create`/`create_lut`/
//! `create_display_icc` return null when the default OCIO config is not
//! loaded (see `quercus_core_color_set_up_default_config`).

use quercus_core::color::{ColorProcessor, Direction};

use crate::codec::{copy_str_out, str_arg};
use crate::handle::into_ffi;

crate::export_handle!(
	ColorProcessor,
	quercus_core_colorproc_add_ref,
	quercus_core_colorproc_release
);

fn direction(inverse: bool) -> Direction {
	if inverse { Direction::Inverse } else { Direction::Normal }
}

/// A processor from two colorspace names on the default config.
///
/// `(src_space, src_space_len)` is the source colorspace or role name and
/// `(dst_transform, dst_transform_len)` the destination colorspace name; both are
/// UTF-8 byte pairs, not NUL-terminated, and are only borrowed for the call.
/// Roles (e.g. "scene_linear") are resolved to their canonical names. `inverse`
/// selects dst → src: `false` runs src → dst, `true` swaps the endpoints.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null on null / non-UTF-8 input or when no
/// default OCIO config is loaded. OCIO failures are non-fatal and yield a
/// pass-through processor (check [`quercus_core_colorproc_is_valid`]).
///
/// # Safety
///
/// Each `(ptr, len)` pair must point to `len` readable bytes for the whole call;
/// the strings are not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create(
	src_space: *const u8,
	src_space_len: usize,
	dst_transform: *const u8,
	dst_transform_len: usize,
	inverse: bool,
) -> *mut ColorProcessor {
	let (Some(src), Some(dst)) = (
		unsafe { str_arg(src_space, src_space_len) },
		unsafe { str_arg(dst_transform, dst_transform_len) },
	) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create(src, dst, direction(inverse)) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// A processor applying the LUT file at `path`; `inverse` runs it backwards.
///
/// `(path, path_len)` is a UTF-8 byte pair with the LUT file path (not
/// NUL-terminated, borrowed for the call only). `inverse` selects the direction:
/// `false` = forward (src → dst), `true` = reversed (dst → src); the file
/// transform uses linear interpolation.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null when the path is null / not valid
/// UTF-8, when no default config is loaded, or when the file transform cannot be
/// created. An OCIO processor-build failure yields a pass-through processor
/// (see [`quercus_core_colorproc_is_valid`]).
///
/// # Safety
///
/// `path` must point to `path_len` readable bytes for the whole call; the path
/// is not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create_lut(
	path: *const u8,
	path_len: usize,
	inverse: bool,
) -> *mut ColorProcessor {
	let Some(path) = (unsafe { str_arg(path, path_len) }) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create_lut(path, direction(inverse)) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// A processor converting `src_space` to the display ICC profile at `icc_path`.
///
/// `(src_space, src_space_len)` names a colorspace or role of the default config
/// (the pipeline reference); `(icc_path, icc_path_len)` is the display-class ICC
/// profile path. Both are UTF-8 byte pairs, not NUL-terminated, and are only
/// borrowed for the call. The chain is `<src_space> → linear Rec.709 → CIE XYZ
/// D65 → ICC forward`.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null on null / non-UTF-8 input or when
/// no default OCIO config is loaded. An OCIO failure yields a pass-through
/// processor (see [`quercus_core_colorproc_is_valid`]).
///
/// # Safety
///
/// Each `(ptr, len)` pair must point to `len` readable bytes for the whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create_display_icc(
	src_space: *const u8,
	src_space_len: usize,
	icc_path: *const u8,
	icc_path_len: usize,
) -> *mut ColorProcessor {
	let (Some(src), Some(icc)) = (
		unsafe { str_arg(src_space, src_space_len) },
		unsafe { str_arg(icc_path, icc_path_len) },
	) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create_display_icc(src, icc) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// A BGRA8 processor converting `src_space` to the display ICC profile.
///
/// `(src_space, src_space_len)` names a colorspace or role of the default config
/// (the pipeline reference, e.g. "scene_linear"); `(icc_path, icc_path_len)` is
/// the display-class ICC profile path. Both are UTF-8 byte pairs, not
/// NUL-terminated, and are only borrowed for the call. The chain is
/// `<src_space> → linear Rec.709 → CIE XYZ D65 → ICC`, with R/B-swapping
/// matrices around it for BGRA buffers.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null on null / non-UTF-8 input or when
/// no default OCIO config is loaded. An OCIO failure yields a pass-through
/// processor (see [`quercus_core_colorproc_is_valid`]).
///
/// # Safety
///
/// Each `(ptr, len)` pair must point to `len` readable bytes for the whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create_display_icc_bgra8(
	src_space: *const u8,
	src_space_len: usize,
	icc_path: *const u8,
	icc_path_len: usize,
) -> *mut ColorProcessor {
	let (Some(src), Some(icc)) = (
		unsafe { str_arg(src_space, src_space_len) },
		unsafe { str_arg(icc_path, icc_path_len) },
	) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create_display_icc_bgra8(src, icc) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// A processor converting CIE XYZ (D65) to the display ICC profile.
///
/// `(icc_path, icc_path_len)` is a UTF-8 byte pair (not NUL-terminated) with the
/// path of a display-class ICC profile. Unlike
/// [`quercus_core_colorproc_create_display_icc`] the source is not a named
/// colorspace: the caller has already linearized and gamut-mapped its content to
/// CIE XYZ (D65), so only the ICC half of the chain runs.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null when the path is null / not valid
/// UTF-8, when no default OCIO config is loaded, or when the chain cannot be
/// built.
///
/// # Safety
///
/// `icc_path` must point to `icc_path_len` readable bytes for the whole call;
/// the path is not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create_display_icc_xyz(
	icc_path: *const u8,
	icc_path_len: usize,
) -> *mut ColorProcessor {
	let Some(icc) = (unsafe { str_arg(icc_path, icc_path_len) }) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create_display_icc_xyz(icc) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// The BGRA8 variant of [`quercus_core_colorproc_create_display_icc_xyz`].
///
/// `(icc_path, icc_path_len)` is a UTF-8 byte pair (not NUL-terminated) with the
/// path of a display-class ICC profile. The chain has R/B-swapping matrices
/// baked around it, for buffers whose bytes are in BGRA order.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colorproc_release`, or null when the path is null / not valid
/// UTF-8, when no default OCIO config is loaded, or when the XYZ chain cannot be
/// built — callers then fall back to the sRGB chain instead of feeding XYZ
/// values through a no-op.
///
/// # Safety
///
/// `icc_path` must point to `icc_path_len` readable bytes for the whole call;
/// the path is not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_create_display_icc_xyz_bgra8(
	icc_path: *const u8,
	icc_path_len: usize,
) -> *mut ColorProcessor {
	let Some(icc) = (unsafe { str_arg(icc_path, icc_path_len) }) else {
		return std::ptr::null_mut();
	};
	match ColorProcessor::create_display_icc_xyz_bgra8(icc) {
		Some(p) => into_ffi(p) as *mut ColorProcessor,
		None => std::ptr::null_mut(),
	}
}

/// A pass-through processor (no config needed).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_colorproc_pass_through() -> *mut ColorProcessor {
	into_ffi(ColorProcessor::pass_through()) as *mut ColorProcessor
}

/// Whether the processor has a real OCIO transform.
///
/// Returns `false` for a null handle and for the pass-through fallback produced
/// when OCIO cannot build a transform, `true` otherwise. Callers use it to tell
/// a real conversion from a no-op.
///
/// # Safety
///
/// `this` must be null or a live processor handle from this ABI. The handle is
/// borrowed, not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_is_valid(this: *const ColorProcessor) -> bool {
	!this.is_null() && unsafe { &*this }.is_valid()
}

/// The processor's OCIO cache id, snprintf-style.
///
/// `buf` / `buf_len` describe the caller's output buffer: the id is copied
/// NUL-terminated, truncated to `buf_len - 1` bytes when it does not fit, and
/// the return value is the full length excluding the NUL — so `ret >= buf_len`
/// tells the caller it was truncated, and a null buffer (or `buf_len == 0`)
/// only queries the length. A pass-through processor has an empty id, so the
/// call returns `0`.
///
/// Returns `-1` when `this` is null (nothing is written).
///
/// # Safety
///
/// `this` must be null or a live processor handle from this ABI; `buf` must be
/// null or point to `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_cache_id(
	this: *const ColorProcessor,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if this.is_null() {
		return -1;
	}
	copy_str_out(&unsafe { &*this }.cache_id(), buf, buf_len)
}

/// Converts one RGBA color in place.
///
/// `rgba` points to 4 `f64` values (RGBA, normally `0..=1`) that are read
/// before the conversion and written back after it; a pass-through processor
/// writes the same values back.
///
/// Returns `false` on a null `this` or a null `rgba` (nothing is written).
///
/// # Safety
///
/// `this` must be null or a live processor handle from this ABI; `rgba` must be
/// null or point to a writable array of 4 `f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_convert_color(
	this: *const ColorProcessor,
	rgba: *mut f64,
) -> bool {
	if this.is_null() || rgba.is_null() {
		return false;
	}
	let mut color: [f64; 4] = unsafe { std::ptr::read(rgba.cast()) };
	color = unsafe { &*this }.convert_color(color);
	unsafe { std::ptr::write(rgba.cast(), color) };
	true
}

/// Converts `pixels * 4` f32 RGBA samples in place.
///
/// `data` points to tightly packed RGBA f32 samples (4 floats per pixel). A
/// pass-through processor leaves them untouched, and `pixels == 0` with a null
/// `data` is a valid no-op.
///
/// Returns `false` on a null `this`, a null `data` with `pixels > 0`, or an
/// engine error from the OCIO apply.
///
/// # Safety
///
/// `this` must be null or a live processor handle from this ABI; `data` must be
/// null or point to a writable buffer of `pixels * 4` f32 values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_convert_f32_rgba(
	this: *const ColorProcessor,
	data: *mut f32,
	pixels: i64,
) -> bool {
	if this.is_null() || (data.is_null() && pixels > 0) {
		return false;
	}
	let samples = if data.is_null() {
		&mut []
	} else {
		unsafe { std::slice::from_raw_parts_mut(data, pixels.max(0) as usize * 4) }
	};
	unsafe { &*this }.convert_f32_rgba(samples, pixels).is_ok()
}

/// Converts `pixels * 4` u8 BGRA samples in place.
///
/// `data` points to a tightly packed BGRA8 buffer of at least `pixels * 4`
/// bytes; the processor must have been built by a `*_bgra8` constructor, whose
/// chain has the R/B swizzle baked in. A pass-through processor leaves the bytes
/// untouched, and `pixels == 0` with a null `data` is a valid no-op.
///
/// Returns `false` on a null `this`, a null `data` with `pixels > 0`, or an
/// engine error from the OCIO apply.
///
/// # Safety
///
/// `this` must be null or a live processor handle from this ABI; `data` must be
/// null or point to a writable buffer of `pixels * 4` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colorproc_convert_bgra8(
	this: *const ColorProcessor,
	data: *mut u8,
	pixels: i64,
) -> bool {
	if this.is_null() || (data.is_null() && pixels > 0) {
		return false;
	}
	let buf = if data.is_null() {
		&mut []
	} else {
		unsafe { std::slice::from_raw_parts_mut(data, pixels.max(0) as usize * 4) }
	};
	unsafe { &*this }.convert_bgra8(buf, pixels).is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	#[test]
	fn pass_through_is_identity() {
		unsafe {
			let proc = quercus_core_colorproc_pass_through();
			assert!(!proc.is_null());

			let mut rgba = [0.1f64, 0.5, 0.9, 1.0];
			assert!(quercus_core_colorproc_convert_color(proc, rgba.as_mut_ptr()));
			assert_eq!(rgba, [0.1, 0.5, 0.9, 1.0]);

			let mut px = [0.25f32, 0.5, 0.75, 1.0];
			assert!(quercus_core_colorproc_convert_f32_rgba(proc, px.as_mut_ptr(), 1));
			assert_eq!(px, [0.25, 0.5, 0.75, 1.0]);

			let mut bgra = [64u8, 128, 192, 255];
			assert!(quercus_core_colorproc_convert_bgra8(proc, bgra.as_mut_ptr(), 1));
			assert_eq!(bgra, [64, 128, 192, 255]);

			quercus_core_colorproc_release(proc);
		}
	}

	#[test]
	fn create_with_default_config() {
		// Loads the bundled OCIO config (idempotent, headless-safe).
		assert!(crate::corestate::quercus_core_color_set_up_default_config());
		unsafe {
			let (src, dst) = ("acescg", "srgb");
			let proc =
				quercus_core_colorproc_create(arg(src).0, src.len(), arg(dst).0, dst.len(), false);
			assert!(!proc.is_null(), "create after config load");

			if quercus_core_colorproc_is_valid(proc) {
				// A real transform changes mid gray.
				let mut rgba = [0.18f64, 0.18, 0.18, 1.0];
				assert!(quercus_core_colorproc_convert_color(proc, rgba.as_mut_ptr()));
				assert_ne!(rgba[0], 0.18);
			}
			quercus_core_colorproc_release(proc);
		}
	}

	#[test]
	fn null_and_invalid_input_is_safe() {
		unsafe {
			assert!(quercus_core_colorproc_create(std::ptr::null(), 0, arg("x").0, 1, false).is_null());
			assert!(!quercus_core_colorproc_is_valid(std::ptr::null()));
			assert_eq!(quercus_core_colorproc_cache_id(std::ptr::null(), std::ptr::null_mut(), 0), -1);
			assert!(!quercus_core_colorproc_convert_color(std::ptr::null(), std::ptr::null_mut()));
			assert!(!quercus_core_colorproc_convert_f32_rgba(std::ptr::null(), std::ptr::null_mut(), 1));
			assert!(!quercus_core_colorproc_convert_bgra8(std::ptr::null(), std::ptr::null_mut(), 1));
			quercus_core_colorproc_add_ref(std::ptr::null());
			quercus_core_colorproc_release(std::ptr::null());
		}
	}
}

// ---------------------------------------------------------------------------
// ColorTransform (the plain-data display/output transform descriptor)
// ---------------------------------------------------------------------------

use quercus_core::colortransform::ColorTransform;

crate::export_handle!(
	ColorTransform,
	quercus_core_colortransform_add_ref,
	quercus_core_colortransform_release
);

/// An output-colorspace transform descriptor; null on invalid input.
///
/// `(output, output_len)` is a UTF-8 byte pair (not NUL-terminated) naming the
/// destination colorspace. The name is copied into the descriptor, so the
/// buffer is only borrowed for the call.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colortransform_release`, or null when `output` is null or not
/// valid UTF-8.
///
/// # Safety
///
/// `output` must point to `output_len` readable bytes for the whole call; the
/// bytes are not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colortransform_new_output(
	output: *const u8,
	output_len: usize,
) -> *mut ColorTransform {
	let Some(output) = (unsafe { str_arg(output, output_len) }) else {
		return std::ptr::null_mut();
	};
	into_ffi(ColorTransform::new_output(output)) as *mut ColorTransform
}

/// A display/view/look transform descriptor; null on invalid input.
///
/// `(display, display_len)`, `(view, view_len)` and `(look, look_len)` are UTF-8
/// byte pairs (not NUL-terminated) naming the display, the display view and an
/// optional look (the empty string is allowed). The names are copied into the
/// descriptor, so the buffers are only borrowed for the call.
///
/// Returns a refcounted handle to release with
/// `quercus_core_colortransform_release`, or null when a pointer is null or a
/// slice is not valid UTF-8.
///
/// # Safety
///
/// Each `(ptr, len)` pair must point to `len` readable bytes for the whole call;
/// the bytes are not retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colortransform_new_display(
	display: *const u8,
	display_len: usize,
	view: *const u8,
	view_len: usize,
	look: *const u8,
	look_len: usize,
) -> *mut ColorTransform {
	let (Some(display), Some(view), Some(look)) = (
		unsafe { str_arg(display, display_len) },
		unsafe { str_arg(view, view_len) },
		unsafe { str_arg(look, look_len) },
	) else {
		return std::ptr::null_mut();
	};
	into_ffi(ColorTransform::new_display(display, view, look)) as *mut ColorTransform
}

/// Whether this is a display/view/look transform.
///
/// Returns `false` for a null handle and for an output-colorspace transform
/// (created by `quercus_core_colortransform_new_output`); a display transform
/// from `quercus_core_colortransform_new_display` is `true` even when its
/// display name is empty.
///
/// # Safety
///
/// `this` must be null or a live `ColorTransform` handle obtained from this
/// ABI. The handle is borrowed, not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_colortransform_is_display(
	this: *const ColorTransform,
) -> bool {
	!this.is_null() && unsafe { &*this }.is_display()
}

macro_rules! colortransform_getter {
	($( $name:ident => $native:ident ),* $(,)?) => {
		$(
			#[doc = concat!("`ColorTransform::", stringify!($native), "`, snprintf-style; -1 on null.")]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(
				this: *const ColorTransform,
				buf: *mut u8,
				buf_len: usize,
			) -> i32 {
				if this.is_null() {
					return -1;
				}
				copy_str_out(unsafe { &*this }.$native(), buf, buf_len)
			}
		)*
	};
}

colortransform_getter! {
	quercus_core_colortransform_output => output,
	quercus_core_colortransform_display => display,
	quercus_core_colortransform_view => view,
	quercus_core_colortransform_look => look,
}
