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
		assert!(quercus_core_colorproc_convert_color(
			proc,
			rgba.as_mut_ptr()
		));
		assert_eq!(rgba, [0.1, 0.5, 0.9, 1.0]);

		let mut px = [0.25f32, 0.5, 0.75, 1.0];
		assert!(quercus_core_colorproc_convert_f32_rgba(
			proc,
			px.as_mut_ptr(),
			1
		));
		assert_eq!(px, [0.25, 0.5, 0.75, 1.0]);

		let mut bgra = [64u8, 128, 192, 255];
		assert!(quercus_core_colorproc_convert_bgra8(
			proc,
			bgra.as_mut_ptr(),
			1
		));
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
			assert!(quercus_core_colorproc_convert_color(
				proc,
				rgba.as_mut_ptr()
			));
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
		assert_eq!(
			quercus_core_colorproc_cache_id(std::ptr::null(), std::ptr::null_mut(), 0),
			-1
		);
		assert!(!quercus_core_colorproc_convert_color(
			std::ptr::null(),
			std::ptr::null_mut()
		));
		assert!(!quercus_core_colorproc_convert_f32_rgba(
			std::ptr::null(),
			std::ptr::null_mut(),
			1
		));
		assert!(!quercus_core_colorproc_convert_bgra8(
			std::ptr::null(),
			std::ptr::null_mut(),
			1
		));
		quercus_core_colorproc_add_ref(std::ptr::null());
		quercus_core_colorproc_release(std::ptr::null());
	}
}
