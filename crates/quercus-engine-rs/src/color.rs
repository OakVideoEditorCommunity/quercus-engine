//! Safe wrapper over the engine's `ColorProcessor` C ABI
//! (`quercus_core_colorproc_*`): color-space conversions for plugins.

use std::ffi::c_void;

use thiserror::Error;

use crate::codec::string_out;

#[cfg_attr(all(windows, feature = "engine-cdylib"), link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim"))]
#[cfg_attr(all(not(windows), feature = "engine-cdylib"), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_core_colorproc_create(
		src_space: *const u8,
		src_space_len: usize,
		dst_transform: *const u8,
		dst_transform_len: usize,
		inverse: bool,
	) -> *mut c_void;
	fn quercus_core_colorproc_create_lut(path: *const u8, path_len: usize, inverse: bool)
	-> *mut c_void;
	fn quercus_core_colorproc_create_display_icc(
		src_space: *const u8,
		src_space_len: usize,
		icc_path: *const u8,
		icc_path_len: usize,
	) -> *mut c_void;
	fn quercus_core_colorproc_create_display_icc_bgra8(
		src_space: *const u8,
		src_space_len: usize,
		icc_path: *const u8,
		icc_path_len: usize,
	) -> *mut c_void;
	fn quercus_core_colorproc_create_display_icc_xyz(
		icc_path: *const u8,
		icc_path_len: usize,
	) -> *mut c_void;
	fn quercus_core_colorproc_create_display_icc_xyz_bgra8(
		icc_path: *const u8,
		icc_path_len: usize,
	) -> *mut c_void;
	fn quercus_core_colorproc_pass_through() -> *mut c_void;
	fn quercus_core_colorproc_is_valid(this: *const c_void) -> bool;
	fn quercus_core_colorproc_cache_id(this: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_colorproc_convert_color(this: *const c_void, rgba: *mut f64) -> bool;
	fn quercus_core_colorproc_convert_f32_rgba(this: *const c_void, data: *mut f32, pixels: i64) -> bool;
	fn quercus_core_colorproc_convert_bgra8(this: *const c_void, data: *mut u8, pixels: i64) -> bool;
	fn quercus_core_colorproc_add_ref(this: *const c_void);
	fn quercus_core_colorproc_release(this: *const c_void);

	// colortransform
	fn quercus_core_colortransform_new_output(output: *const u8, output_len: usize) -> *mut c_void;
	fn quercus_core_colortransform_new_display(
		display: *const u8,
		display_len: usize,
		view: *const u8,
		view_len: usize,
		look: *const u8,
		look_len: usize,
	) -> *mut c_void;
	fn quercus_core_colortransform_is_display(this: *const c_void) -> bool;
	fn quercus_core_colortransform_output(this: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_colortransform_display(this: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_colortransform_view(this: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_colortransform_look(this: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_core_colortransform_add_ref(this: *const c_void);
	fn quercus_core_colortransform_release(this: *const c_void);
}

/// A failed color-processor call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the engine color processor call failed")]
pub struct Error;

/// A color-space converter. Reference-counted in the engine: `Clone`
/// adds a reference, `Drop` releases. Immutable once created, so clones
/// are safe to use from any thread.
pub struct ColorProcessor {
	ptr: *mut c_void,
}

unsafe impl Send for ColorProcessor {}
unsafe impl Sync for ColorProcessor {}

impl ColorProcessor {
	fn from_raw(ptr: *mut c_void) -> Option<Self> {
		if ptr.is_null() { None } else { Some(Self { ptr }) }
	}

	/// A processor from two colorspace names on the default OCIO config
	/// (`inverse` runs dst → src). `None` when no default config is
	/// loaded (see [`crate::state::color_set_up_default_config`]). OCIO
	/// failures yield a pass-through processor — check with
	/// [`ColorProcessor::is_valid`].
	pub fn create(src_space: &str, dst_transform: &str, inverse: bool) -> Option<Self> {
		Self::from_raw(unsafe {
			quercus_core_colorproc_create(
				src_space.as_ptr(),
				src_space.len(),
				dst_transform.as_ptr(),
				dst_transform.len(),
				inverse,
			)
		})
	}

	/// A processor applying the LUT file at `path` (`inverse` runs it
	/// backwards).
	pub fn create_lut(path: &str, inverse: bool) -> Option<Self> {
		Self::from_raw(unsafe { quercus_core_colorproc_create_lut(path.as_ptr(), path.len(), inverse) })
	}

	/// A processor converting `src_space` to the display ICC profile at
	/// `icc_path`.
	pub fn create_display_icc(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::from_raw(unsafe {
			quercus_core_colorproc_create_display_icc(
				src_space.as_ptr(),
				src_space.len(),
				icc_path.as_ptr(),
				icc_path.len(),
			)
		})
	}

	/// A BGRA8 processor converting `src_space` to the display ICC profile.
	pub fn create_display_icc_bgra8(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::from_raw(unsafe {
			quercus_core_colorproc_create_display_icc_bgra8(
				src_space.as_ptr(),
				src_space.len(),
				icc_path.as_ptr(),
				icc_path.len(),
			)
		})
	}

	/// A processor converting CIE XYZ (D65) to the display ICC profile.
	pub fn create_display_icc_xyz(icc_path: &str) -> Option<Self> {
		Self::from_raw(unsafe {
			quercus_core_colorproc_create_display_icc_xyz(icc_path.as_ptr(), icc_path.len())
		})
	}

	/// The BGRA8 variant of [`ColorProcessor::create_display_icc_xyz`].
	pub fn create_display_icc_xyz_bgra8(icc_path: &str) -> Option<Self> {
		Self::from_raw(unsafe {
			quercus_core_colorproc_create_display_icc_xyz_bgra8(icc_path.as_ptr(), icc_path.len())
		})
	}

	/// A pass-through processor (no config needed).
	pub fn pass_through() -> Self {
		Self {
			ptr: unsafe { quercus_core_colorproc_pass_through() },
		}
	}

	/// Whether the processor has a real OCIO transform (false for the
	/// pass-through fallback produced on OCIO failures).
	pub fn is_valid(&self) -> bool {
		unsafe { quercus_core_colorproc_is_valid(self.ptr) }
	}

	/// Returns the engine's cache id for this processor's OCIO transform.
	///
	/// The id identifies the OCIO processor in the engine cache, so equal ids mean the
	/// same conversion. It is empty for a pass-through processor (which carries no
	/// OCIO transform) and `None` when the engine reports a failure, e.g. for an
	/// invalid handle. Each call copies the string out of the engine, so the result
	/// is owned.
	pub fn cache_id(&self) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_core_colorproc_cache_id(self.ptr, buf, len) })
	}

	/// Converts one RGBA color through the processor.
	///
	/// `rgba` holds straight RGBA values (normally `0..=1`, alpha last) and is passed
	/// by value, so the caller's array is untouched. Returns the converted color as a
	/// new array; a pass-through processor (or an invalid handle) returns the input
	/// unchanged.
	pub fn convert_color(&self, rgba: [f64; 4]) -> [f64; 4] {
		let mut out = rgba;
		unsafe { quercus_core_colorproc_convert_color(self.ptr, out.as_mut_ptr()) };
		out
	}

	/// Converts `pixels` RGBA f32 pixels in place to the processor's output.
	///
	/// `samples` is a tightly packed RGBA f32 buffer (4 floats per pixel) and
	/// `pixels` is the pixel count — `samples.len()` must be at least `pixels * 4`,
	/// since the engine converts `pixels * 4` elements. A pass-through processor
	/// leaves the samples unchanged. Returns `Err(Error)` when the engine reports an
	/// OCIO apply failure; the buffer contents are then unspecified.
	pub fn convert_f32_rgba(&self, samples: &mut [f32], pixels: i64) -> Result<(), Error> {
		if unsafe { quercus_core_colorproc_convert_f32_rgba(self.ptr, samples.as_mut_ptr(), pixels) } {
			Ok(())
		} else {
			Err(Error)
		}
	}

	/// Converts `pixels` BGRA8 pixels in place to the processor's output.
	///
	/// `data` is a tightly packed BGRA8 buffer (4 bytes per pixel; the R/B swizzle
	/// belongs to the processor chain, so it must have been built by a `*_bgra8`
	/// constructor) and `pixels` is the pixel count — `data.len()` must be at least
	/// `pixels * 4`, since the engine derives the length from `pixels`. A pass-through
	/// processor leaves the bytes unchanged. Returns `Err(Error)` when the engine
	/// reports an OCIO apply failure; the buffer contents are then unspecified.
	pub fn convert_bgra8(&self, data: &mut [u8], pixels: i64) -> Result<(), Error> {
		if unsafe { quercus_core_colorproc_convert_bgra8(self.ptr, data.as_mut_ptr(), pixels) } {
			Ok(())
		} else {
			Err(Error)
		}
	}
}

impl Clone for ColorProcessor {
	fn clone(&self) -> Self {
		unsafe { quercus_core_colorproc_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for ColorProcessor {
	fn drop(&mut self) {
		unsafe { quercus_core_colorproc_release(self.ptr) };
	}
}

/// A display/output transform descriptor. Reference-counted in the
/// engine: `Clone` adds a reference, `Drop` releases.
pub struct ColorTransform {
	ptr: *mut c_void,
}

unsafe impl Send for ColorTransform {}
unsafe impl Sync for ColorTransform {}

impl ColorTransform {
	/// Creates an output-colorspace transform descriptor.
	///
	/// `output` names the destination colorspace (copied into the engine, so the
	/// argument is only borrowed for the call); `is_display()` is `false` and the
	/// display/view/look getters read back empty. Because the name is a `&str` the
	/// returned handle is always valid. `Clone` adds a reference and `Drop` releases
	/// it.
	pub fn new_output(output: &str) -> Self {
		Self {
			ptr: unsafe { quercus_core_colortransform_new_output(output.as_ptr(), output.len()) },
		}
	}

	/// Creates a display/view/look transform descriptor.
	///
	/// `display` is the engine's display name (e.g. `"sRGB"`), `view` the display
	/// view (e.g. `"Standard"`) and `look` the optional look (empty for none). The
	/// names are copied into the engine, so the arguments are only borrowed for the
	/// call; the result reports `is_display() == true` and `output()` reads back
	/// empty. Because all three arguments are `&str` the returned handle is always
	/// valid. `Clone` adds a reference and `Drop` releases it.
	pub fn new_display(display: &str, view: &str, look: &str) -> Self {
		Self {
			ptr: unsafe {
				quercus_core_colortransform_new_display(
					display.as_ptr(),
					display.len(),
					view.as_ptr(),
					view.len(),
					look.as_ptr(),
					look.len(),
				)
			},
		}
	}

	/// Whether this is a display/view/look transform.
	pub fn is_display(&self) -> bool {
		unsafe { quercus_core_colortransform_is_display(self.ptr) }
	}

	/// The output colorspace name ("" for display transforms).
	pub fn output(&self) -> String {
		string_out(|buf, len| unsafe { quercus_core_colortransform_output(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// The display name ("" for output transforms).
	pub fn display(&self) -> String {
		string_out(|buf, len| unsafe { quercus_core_colortransform_display(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Returns the transform's view name.
	///
	/// The view is the second component of a display/view/look transform (e.g.
	/// `"Standard"`); an output-colorspace transform returns an empty string. The
	/// name is copied out of the engine handle, so the result is owned; an invalid
	/// handle also reads back as empty.
	pub fn view(&self) -> String {
		string_out(|buf, len| unsafe { quercus_core_colortransform_view(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Returns the transform's look name.
	///
	/// The look is the optional third component of a display/view/look transform
	/// (`"none"` or empty when no look is applied); an output-colorspace transform
	/// returns an empty string. The name is copied out of the engine handle, so the
	/// result is owned; an invalid handle also reads back as empty.
	pub fn look(&self) -> String {
		string_out(|buf, len| unsafe { quercus_core_colortransform_look(self.ptr, buf, len) })
			.unwrap_or_default()
	}
}

impl Clone for ColorTransform {
	fn clone(&self) -> Self {
		unsafe { quercus_core_colortransform_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for ColorTransform {
	fn drop(&mut self) {
		unsafe { quercus_core_colortransform_release(self.ptr) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pass_through_is_identity() {
		let proc = ColorProcessor::pass_through();
		assert!(!proc.is_valid(), "pass-through carries no OCIO transform");

		assert_eq!(proc.convert_color([0.1, 0.5, 0.9, 1.0]), [0.1, 0.5, 0.9, 1.0]);

		let mut px = [0.25f32, 0.5, 0.75, 1.0];
		proc.convert_f32_rgba(&mut px, 1).expect("convert");
		assert_eq!(px, [0.25, 0.5, 0.75, 1.0]);

		let mut bgra = [64u8, 128, 192, 255];
		proc.convert_bgra8(&mut bgra, 1).expect("convert");
		assert_eq!(bgra, [64, 128, 192, 255]);

		// Clones share the processor.
		let clone = proc.clone();
		assert_eq!(clone.convert_color([1.0, 0.0, 0.0, 1.0]), [1.0, 0.0, 0.0, 1.0]);
	}

	#[test]
	fn colortransform_roundtrip() {
		let output = ColorTransform::new_output("srgb");
		assert!(!output.is_display());
		assert_eq!(output.output(), "srgb");
		assert_eq!(output.display(), "");

		let display = ColorTransform::new_display("sRGB", "Standard", "none");
		assert!(display.is_display());
		assert_eq!(display.display(), "sRGB");
		assert_eq!(display.view(), "Standard");
		assert_eq!(display.look(), "none");

		let clone = display.clone();
		drop(display);
		assert_eq!(clone.view(), "Standard");
	}

	#[test]
	fn create_with_default_config() {
		crate::state::color_set_up_default_config(None).expect("load config");
		let proc = ColorProcessor::create("acescg", "srgb", false).expect("create");
		if proc.is_valid() {
			let gray = proc.convert_color([0.18, 0.18, 0.18, 1.0]);
			assert_ne!(gray[0], 0.18, "a real transform changes mid gray");
			assert!(proc.cache_id().is_some_and(|id| !id.is_empty()));
		}
	}
}
