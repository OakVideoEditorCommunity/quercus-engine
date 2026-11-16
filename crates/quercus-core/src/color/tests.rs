use super::*;
use std::sync::{Mutex, MutexGuard};

/// Serializes config-dependent color tests (the process-wide default
/// config is global).
static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());

fn config_lock() -> MutexGuard<'static, ()> {
	CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A display-class ICC profile from the usual system locations, if any
/// (macOS ColorSync, Linux colord/ghostscript, Debian/Ubuntu
/// `icc-profiles-free` — the CI runner installs the latter).
fn system_icc() -> Option<&'static str> {
	[
		"/System/Library/ColorSync/Profiles/sRGB Profile.icc",
		"/System/Library/ColorSync/Profiles/Display P3.icc",
		"/usr/share/color/icc/colord/sRGB.icc",
		"/usr/share/color/icc/ghostscript/srgb.icc",
		"/usr/share/color/icc/sRGB.icc",
		"/usr/local/share/color/icc/colord/sRGB.icc",
	]
	.into_iter()
	.find(|p| std::path::Path::new(p).exists())
}

#[test]
fn lut_extensions_case_insensitive_and_dot_optional() {
	assert!(is_supported_lut_extension("cube"));
	assert!(is_supported_lut_extension(".CUBE"));
	assert!(is_supported_lut_extension("clf"));
	assert!(!is_supported_lut_extension("exr"));
	assert!(!is_supported_lut_extension(""));
	assert_eq!(SUPPORTED_LUT_EXTENSIONS.len(), 9);
}

#[test]
fn pass_through_processor_identity() {
	let p = ColorProcessor::pass_through();
	assert!(!p.is_valid());
	assert_eq!(
		p.convert_color([0.25, 0.5, 0.75, 1.0]),
		[0.25, 0.5, 0.75, 1.0]
	);
	let mut f = Frame::dummy();
	assert!(p.convert_frame(&mut f).is_ok());
}

#[test]
fn convert_frame_rejects_non_f32() {
	// A pass-through processor accepts anything (no conversion needed).
	let p = ColorProcessor::pass_through();
	let mut f = Frame::new();
	f.format = PixelFormat::U8;
	assert!(
		p.convert_frame(&mut f).is_ok(),
		"pass-through converts nothing"
	);
	// A *valid* processor requires the F32 pipeline format.
	if set_up_default_config().is_err() {
		return;
	}
	let valid = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
		.and_then(|p| p.is_valid().then_some(p));
	if let Some(valid) = valid {
		let mut f = Frame::new();
		f.format = PixelFormat::U8;
		assert_eq!(
			valid.convert_frame(&mut f).unwrap_err().code(),
			crate::error::QUERCUSCORE_E_INVALID
		);
	}
}

#[test]
fn create_without_config_yields_none() {
	let _lock = config_lock();
	// When no default config has been set up, creation is None.
	let saved = default_config();
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = None;
	assert!(ColorProcessor::create("scene_linear", "sdr-video", Direction::Normal).is_none());
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = saved;
}

#[test]
fn default_config_setup_and_role_resolution() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		// Bundled OCIO missing (e.g. stub build): skip.
		return;
	}
	let config = default_config().expect("config set up");
	// The built-in default config carries a scene_linear role.
	assert!(config.has_role("scene_linear") || config.has_role("aces_interchange"));
}

#[test]
fn processor_from_builtin_converts() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	} // Create via the built-in config; a valid processor must exist for
   // the ACES scene→display-encoded pairing and must preserve alpha.
	let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal);
	let p = p.expect("processor handle always returned");
	assert!(
		p.is_valid(),
		"ACEScg→sRGB Encoded must be a valid processor"
	);
	let out = p.convert_color([0.18, 0.18, 0.18, 1.0]);
	assert!((out[3] - 1.0).abs() < 1e-6, "alpha passes through");
	assert!(out[0].is_finite() && out[1].is_finite() && out[2].is_finite());
	assert!(
		(out[0] - 0.18).abs() > 1e-3,
		"the transform must change the value"
	);
}

#[test]
fn unknown_colorspaces_yield_invalid_processor() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let p = ColorProcessor::create("not_a_real_colorspace", "also_not_real", Direction::Normal)
		.unwrap();
	assert!(!p.is_valid(), "lookup failure is non-fatal (pass-through)");
	assert_eq!(
		p.convert_color([1.0, 0.5, 0.25, 0.0]),
		[1.0, 0.5, 0.25, 0.0]
	);
}

#[test]
fn display_transform_queries() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let config = default_config().unwrap();
	if config.num_displays_all() <= 0 {
		return;
	}
	let display = config.display_all(0).unwrap();
	let n = config.num_views_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, &display);
	assert!(n >= 0);
	if n > 0 {
		let view = config
			.view_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, &display, 0)
			.unwrap();
		let id = display_transform(&display, &view);
		assert!(id.is_some());
		assert!(!id.unwrap().is_empty());
		// Querying a later view iterates past the non-matching ones
		// (the loop's pass-through arm); an unknown view is Ok(None).
		let last = config
			.view_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, &display, n - 1)
			.unwrap();
		assert!(display_transform_result(&display, &last).is_ok());
		assert_eq!(
			display_transform_result(&display, "no-such-view").unwrap(),
			None
		);
	}
	assert!(display_transform("no-such-display", "x").is_none());
}

#[test]
fn convert_frame_with_valid_processor_changes_pixels() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
		.expect("handle always returned");
	if !p.is_valid() {
		return;
	}
	let mut f = Frame::new();
	let mut pod = crate::frame::VideoParamsPod::default();
	pod.width = 4;
	pod.height = 2;
	f.set_video_params(pod);
	f.allocate();
	// Fill with 0.18 grey via f32 view.
	let f32s: &mut [f32] = unsafe {
		std::slice::from_raw_parts_mut(f.data.as_mut_ptr() as *mut f32, f.pixel_count() * 4)
	};
	for px in f32s.as_chunks_mut::<4>().0 {
		px[0] = 0.18;
		px[1] = 0.18;
		px[2] = 0.18;
		px[3] = 1.0;
	}
	p.convert_frame(&mut f).unwrap();
	let out: &[f32] =
		unsafe { std::slice::from_raw_parts(f.data.as_ptr() as *const f32, f.pixel_count() * 4) };
	assert!(
		(out[0] - 0.18).abs() > 1e-4,
		"scene_linear→sdr-video must change 0.18 grey (got {})",
		out[0]
	);
	assert!((out[3] - 1.0).abs() < 1e-5, "alpha preserved");
}

#[test]
fn display_icc_processor_applies_srgb_profile() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	// A display-class ICC is required; the usual system profile
	// locations are probed (CI runners may have none — skip then).
	let Some(icc) = system_icc() else {
		eprintln!("no system ICC profile; skipping");
		return;
	};
	let p =
		ColorProcessor::create_display_icc("scene_linear", icc).expect("handle always returned");
	assert!(p.is_valid(), "ICC processor builds from {icc}");
	// 0.18 scene-linear grey -> ~0.5 sRGB device grey (the sRGB system
	// profile's device space is sRGB-encoded).
	let out = p.convert_color([0.18, 0.18, 0.18, 1.0]);
	assert!(
		(out[0] - 0.5).abs() < 0.08,
		"0.18 linear grey should land near 0.5 sRGB (got {})",
		out[0]
	);
	assert!(
		(out[0] - out[1]).abs() < 1e-3 && (out[1] - out[2]).abs() < 1e-3,
		"grey stays grey: {out:?}"
	);
	assert!((out[3] - 1.0).abs() < 1e-5, "alpha preserved");
}

/// The non-sRGB project output gamut display path: content converted to
/// CIE XYZ (D65, unit luminance) by
/// `crate::colormath::output_spec_to_xyz_d65` must flow through the
/// display ICC. Builds by running the classic builder with the config's
/// `cie_xyz_d65_interchange` role as the source space — the feasibility
/// question this test answers is whether OCIO accepts the role name as a
/// `ColorSpaceTransform` source (it is a role, not a bare colorspace, in
/// the OCIO 2.2+ builtin configs).
#[test]
fn display_icc_xyz_accepts_interchange_role() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	// Any display-class ICC (CI runners may have none — skip then).
	let Some(icc) = system_icc() else {
		eprintln!("no system ICC profile; skipping");
		return;
	};
	let p = ColorProcessor::create_display_icc_xyz(icc)
		.expect("handle always returned or explicit None");
	assert!(
		p.is_valid(),
		"the cie_xyz_d65_interchange role must build the XYZ→ICC chain from {icc}"
	);
	// Numeric sanity: the XYZ chain fed with `output_spec_to_xyz_d65` of
	// an sRGB-encoded mid-grey must match the classic chain applied to
	// the same encoded values — legs 1+2 (XYZ→lin709→XYZ) are the inverse
	// round trip of the classic chain's lin709→XYZ leg, so both must land
	// on the same device values.
	let spec = crate::colormath::OutputColorSpec::default();
	let encoded = [0.5f32, 0.5, 0.5, 1.0];
	let mut xyz_in = encoded;
	crate::colormath::output_spec_to_xyz_d65(&mut xyz_in, spec);
	let mut via_xyz = xyz_in;
	let _ = p.convert_f32_rgba(&mut via_xyz, 1);
	let srgb = ColorProcessor::create_display_icc("sRGB Encoded Rec.709 (sRGB)", icc)
		.expect("handle always returned");
	let mut via_srgb = encoded;
	let _ = srgb.convert_f32_rgba(&mut via_srgb, 1);
	for c in 0..3 {
		assert!(
			(via_xyz[c] - via_srgb[c]).abs() < 0.02,
			"channel {c}: XYZ chain {} vs sRGB chain {} (round trip must be identity)",
			via_xyz[c],
			via_srgb[c]
		);
	}
	// Grey stays grey; alpha preserved.
	assert!(
		(via_xyz[0] - via_xyz[1]).abs() < 1e-3 && (via_xyz[1] - via_xyz[2]).abs() < 1e-3,
		"grey stays grey: {via_xyz:?}"
	);
	assert!((via_xyz[3] - 1.0).abs() < 1e-5, "alpha preserved");
}

/// The exact chain the viewers use (BGRA8, display-class ICC from
/// `QUERCUS_DISPLAY_ICC`): a mid-grey frame must NOT collapse to black —
/// the viewer-black-screen regression guard. Falls back to a system
/// profile when the env var is unset (point it at the display profile
/// under investigation to override); skipped only when neither exists.
/// An empty env value is treated as unset, same as
/// `displayicc::env_override_icc`.
#[test]
fn display_icc_bgra8_never_outputs_black() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let env_icc = std::env::var("QUERCUS_DISPLAY_ICC").unwrap_or_default();
	let icc = if env_icc.is_empty() {
		let Some(system) = system_icc() else {
			eprintln!("QUERCUS_DISPLAY_ICC unset and no system ICC profile; skipping");
			return;
		};
		system.to_string()
	} else {
		env_icc
	};
	let p = ColorProcessor::create_display_icc_bgra8("sRGB Encoded Rec.709 (sRGB)", &icc)
		.expect("handle always returned");
	assert!(p.is_valid(), "BGRA8 ICC processor builds from {icc}");
	// BGRA bytes: 0.5 grey, 0.75 red, 0.25 green, 0.6 blue.
	let mut data: Vec<u8> = vec![
		128, 128, 128, 255, // grey
		0, 0, 191, 255, // red
		0, 64, 0, 255, // green
		153, 0, 0, 255, // blue
	];
	let bgra_result = p.convert_bgra8(&mut data, 4);
	eprintln!("bgra8 convert: {bgra_result:?} -> {data:?}");
	// The F32 RGBA leg (the CpuF32 display path) must not crush to
	// black either — and unlike the u8 leg it must not even fail.
	let p32 = ColorProcessor::create_display_icc("sRGB Encoded Rec.709 (sRGB)", &icc)
		.expect("handle always returned");
	assert!(p32.is_valid(), "F32 ICC processor builds from {icc}");
	let mut f32s: Vec<f32> = vec![
		0.5, 0.5, 0.5, 1.0, // grey
		0.75, 0.0, 0.0, 1.0, // red
		0.0, 0.25, 0.0, 1.0, // green
		0.0, 0.0, 0.6, 1.0, // blue
	];
	let f32_result = p32.convert_f32_rgba(&mut f32s, 4);
	eprintln!("f32 convert: {f32_result:?} -> {f32s:?}");
	bgra_result.expect("convert");
	for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
		assert_eq!(px[3], 255, "pixel {i}: alpha preserved");
		let rgb: u32 = px[0] as u32 + px[1] as u32 + px[2] as u32;
		assert!(rgb > 0, "pixel {i} must not be crushed to black: {px:?}");
	}
	// Grey stays greyish (a display profile must not tint wildly).
	let (b, g, r) = (data[0] as i32, data[1] as i32, data[2] as i32);
	assert!(
		(b - g).abs() < 24 && (g - r).abs() < 24,
		"grey stays grey through the display ICC: {b} {g} {r}"
	);
	f32_result.expect("convert f32");
	for (i, px) in f32s.as_chunks::<4>().0.iter().enumerate() {
		assert!((px[3] - 1.0).abs() < 1e-3, "pixel {i}: alpha preserved");
		let rgb = px[0] + px[1] + px[2];
		assert!(rgb > 0.0, "pixel {i} must not be crushed to black: {px:?}");
	}
}

#[test]
fn inverse_direction_reverses() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let fwd = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
		.expect("handle");
	let inv = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Inverse)
		.expect("handle");
	if !fwd.is_valid() || !inv.is_valid() {
		return;
	}
	let x = fwd.convert_color([0.18, 0.18, 0.18, 1.0]);
	let back = inv.convert_color(x);
	assert!(
		(back[0] - 0.18).abs() < 0.05,
		"inverse undoes forward ({} -> {})",
		x[0],
		back[0]
	);
}

#[test]
fn create_lut_from_real_file() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	// A tiny 1D .cube LUT (identity-ish with a slight lift).
	let lut = "TITLE quercusrender test\nLUT_1D_SIZE 2\n0.0 0.0 0.0\n1.0 1.0 1.0\n";
	let path = std::env::temp_dir().join("quercusrender-test-lut.cube");
	std::fs::write(&path, lut).unwrap();
	let p = ColorProcessor::create_lut(path.to_string_lossy().as_ref(), Direction::Normal)
		.expect("processor handle always returned");
	assert!(
		p.is_valid(),
		"a readable .cube must produce a valid processor"
	);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn grading_primary_and_from_processor() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping grading-primary/from-processor assertions");
		return;
	}
	// A grading-primary processor is valid and converts on the live
	// config (the shader test below relies on the same construction).
	let p = ColorProcessor::create_grading_primary(GradingStyle::Log)
		.expect("grading-primary processor on a live config");
	assert!(p.is_valid(), "grading-primary processor must be valid");
	let out = p.convert_color([0.18, 0.5, 0.7, 1.0]);
	assert!(out.iter().all(|v| v.is_finite()));

	let config = default_config().expect("config set up");
	let proc = config
		.processor("ACEScg", "sRGB Encoded Rec.709 (sRGB)")
		.expect("ACEScg→sRGB Encoded processor on the default config");
	let p = ColorProcessor::from_processor(proc);
	assert!(p.is_valid());
	assert!(!p.cache_id().is_empty(), "OCIO cache id present");
}

#[test]
fn grading_primary_shader_generates_analytic_glsl() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return; // Bundled OCIO missing (e.g. stub build): skip.
	}
	for style in [GradingStyle::Lin, GradingStyle::Log] {
		let stub = grading_primary_function_shader(style)
			.expect("default config generates an analytic grading shader");
		assert!(
			stub.contains("ove_grading_primary"),
			"function name present"
		);
		assert!(
			!stub.contains("sampler"),
			"no LUT upload expected for grading"
		);
		// Cache hit: a repeated call returns the same text.
		let again = grading_primary_function_shader(style).unwrap();
		assert_eq!(stub, again);
	}
}

#[test]
fn create_lut_missing_file_is_invalid() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let p = ColorProcessor::create_lut("/nonexistent/never.cube", Direction::Normal).unwrap();
	assert!(!p.is_valid(), "unreadable LUT → pass-through processor");
}

#[test]
fn ocio_function_shader_generates_glsl_for_chromakey() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return; // Bundled OCIO missing (e.g. stub build): skip.
	}
	let stub = ocio_function_shader(
		"SceneLinearToCIEXYZ_d65",
		"scene_linear",
		"cie_xyz_d65_interchange",
	)
	.expect("default config generates an analytic shader");
	assert!(
		stub.contains("SceneLinearToCIEXYZ_d65"),
		"function name present"
	);
	assert!(
		!stub.contains("sampler"),
		"no LUT upload expected in the default config"
	);
	// Cache hit: a repeated call returns the same text.
	let again = ocio_function_shader(
		"SceneLinearToCIEXYZ_d65",
		"scene_linear",
		"cie_xyz_d65_interchange",
	)
	.unwrap();
	assert_eq!(stub, again);
}

#[test]
fn ocio_function_shader_rejects_lut_processors() {
	let _lock = config_lock();
	// studio-config's Rec.709 display is a CLF LUT chain (unlike the
	// analytic sRGB one); without a LUT upload path it must be refused.
	let Ok(cfg) =
		ocio_rs::Config::create_from_builtin_config("studio-config-v2.1.0_aces-v1.3_ocio-v2.3")
	else {
		return;
	};
	let cfg = SafeConfig(cfg);
	assert!(
		build_ocio_function_shader(&cfg, "probe", "ACEScg", "Rec.709 - Display").is_none(),
		"LUT-based processor must be refused"
	);
	// A pure-matrix pairing on the same config still generates.
	assert!(
		build_ocio_function_shader(&cfg, "probe", "ACEScg", "ACES2065-1").is_some(),
		"analytic processor still generates"
	);
}

// ---- Branch-coverage fill-ins ------------------------------------------

#[test]
fn rb_swap_matrix_builds_from_ocio() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	assert!(rb_swap_matrix().is_some(), "matrix transform available");
}

#[test]
fn pass_through_processor_accessors_and_conversions() {
	let p = ColorProcessor::pass_through();
	assert!(p.processor().is_none());
	assert_eq!(p.cache_id(), "", "pass-through has no cache id");
	// BGRA8 and F32 conversions are no-ops without a CPU processor.
	let mut data = [1u8, 2, 3, 4];
	p.convert_bgra8(&mut data, 1).unwrap();
	assert_eq!(data, [1, 2, 3, 4]);
	let mut samples = [0.25f32, 0.5, 0.75, 1.0];
	p.convert_f32_rgba(&mut samples, 1).unwrap();
	assert_eq!(samples, [0.25, 0.5, 0.75, 1.0]);
	// Negative/zero pixel counts stay harmless on the no-op path too.
	p.convert_bgra8(&mut data, -3).unwrap();
}

#[test]
fn create_lut_on_config_and_inverse_direction() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	let config = default_config().expect("config set up");
	let lut = "TITLE quercus create_lut_on test\nLUT_1D_SIZE 2\n0.0 0.0 0.0\n1.0 1.0 1.0\n";
	let path =
		std::env::temp_dir().join(format!("quercus-color-lut-on-{}.cube", std::process::id()));
	std::fs::write(&path, lut).unwrap();
	let path = path.to_string_lossy().into_owned();

	let fwd = ColorProcessor::create_lut_on(&config, &path, Direction::Normal)
		.expect("processor handle always returned");
	assert!(fwd.is_valid(), "readable LUT on a specific config");
	// Direction::Inverse exercises the inverse mapping (`to_ocio`) on
	// the transform and the processor.
	let inv = ColorProcessor::create_lut_on(&config, &path, Direction::Inverse)
		.expect("processor handle always returned");
	assert!(inv.is_valid(), "inverse LUT processor");
	// create_lut (default config) with the inverse direction too.
	let inv_default = ColorProcessor::create_lut(&path, Direction::Inverse)
		.expect("processor handle always returned");
	assert!(inv_default.is_valid());

	// An unreadable LUT yields a pass-through handle, not None.
	let missing =
		ColorProcessor::create_lut_on(&config, "/nonexistent/quercus.cube", Direction::Normal)
			.expect("processor handle always returned");
	assert!(!missing.is_valid(), "unreadable LUT → pass-through");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn valid_processor_accessor_and_cache_id() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping processor accessor assertions");
		return;
	}
	let config = default_config().expect("config set up");
	let proc = config
		.processor("ACEScg", "sRGB Encoded Rec.709 (sRGB)")
		.expect("ACEScg→sRGB Encoded processor on the default config");
	let p = ColorProcessor::from_processor(proc);
	assert!(
		p.processor().is_some(),
		"valid processor exposes its handle"
	);
	assert!(!p.cache_id().is_empty());
}

#[test]
fn convert_frame_rejects_non_f32_and_unaligned_buffers() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
		.expect("handle always returned");
	if !p.is_valid() {
		eprintln!("processor unavailable in this config; skipping");
		return;
	}
	// A valid processor only accepts the F32 pipeline format.
	let mut f = Frame::new();
	f.format = PixelFormat::U8;
	assert_eq!(
		p.convert_frame(&mut f).unwrap_err().code(),
		crate::error::QUERCUSCORE_E_INVALID
	);
	// F32 but a byte length that is not a multiple of 4.
	let mut f = Frame::new();
	f.format = PixelFormat::F32;
	f.data = vec![0u8; 3];
	assert!(
		p.convert_frame(&mut f).is_err(),
		"unaligned buffer rejected"
	);
	// A well-formed F32 buffer converts.
	let mut f = Frame::new();
	f.width = 1;
	f.height = 1;
	f.channels = 4;
	f.format = PixelFormat::F32;
	f.data = vec![0u8; 16];
	p.convert_frame(&mut f).unwrap();
}

#[test]
fn display_icc_bgra8_and_xyz_bgra8_chains() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	let Some(icc) = system_icc() else {
		eprintln!("no system ICC profile; skipping");
		return;
	};
	let p = ColorProcessor::create_display_icc_bgra8("sRGB Encoded Rec.709 (sRGB)", icc)
		.expect("handle always returned");
	assert!(p.is_valid(), "BGRA8 ICC chain builds from {icc}");
	let mut data: Vec<u8> = vec![128, 128, 128, 255, 0, 0, 191, 255];
	assert!(p.convert_bgra8(&mut data, 2).is_ok());
	assert_eq!(data[3], 255, "alpha preserved");
	assert!(data[..3].iter().any(|&b| b != 0), "grey survives");
	// A short buffer is rejected before converting anything.
	let mut short = vec![0u8; 3];
	assert_eq!(
		p.convert_bgra8(&mut short, 2).unwrap_err().code(),
		crate::error::QUERCUSCORE_E_INVALID
	);
	// Zero/negative counts clamp to nothing to convert.
	let mut unchanged = vec![7u8; 4];
	let _ = p.convert_bgra8(&mut unchanged, 0);
	let _ = p.convert_bgra8(&mut unchanged, -3);
	// The F32 entry point on the same chain.
	let mut samples: Vec<f32> = vec![0.5, 0.5, 0.5, 1.0];
	assert!(p.convert_f32_rgba(&mut samples, 1).is_ok());
	assert!((samples[3] - 1.0).abs() < 1e-3, "alpha preserved");
	// The XYZ BGRA8 wrapper (both R/B swizzles baked in).
	let xyz = ColorProcessor::create_display_icc_xyz_bgra8(icc).expect("handle");
	assert!(xyz.is_valid(), "XYZ BGRA8 ICC chain builds from {icc}");
}

/// Captures the process-wide pipeline color settings and restores them
/// on drop, so a panicking assertion cannot leak the legacy setting
/// into the tests that run after it.
struct PipelineColorGuard {
	working: crate::colormath::WorkingColorSpace,
	output: crate::colormath::OutputColorSpec,
}

impl PipelineColorGuard {
	fn capture() -> Self {
		Self {
			working: pipeline_working_space(),
			output: pipeline_output_spec(),
		}
	}

	fn restore(&mut self) {
		set_pipeline_color_settings(self.working, self.output);
	}
}

impl Drop for PipelineColorGuard {
	fn drop(&mut self) {
		self.restore();
	}
}

#[test]
fn pipeline_working_ofx_name_legacy_mode() {
	use crate::colormath::WorkingColorSpace;
	// The pipeline settings are process-global: take the config lock
	// like the other tests that touch global color state.
	let _lock = config_lock();
	let mut restore = PipelineColorGuard::capture();
	set_pipeline_color_settings(WorkingColorSpace::SrgbLegacy, restore.output);
	assert_eq!(pipeline_working_ofx_name(), "sRGB");
	restore.restore();
	let expected = match restore.working {
		WorkingColorSpace::AcesCg => "ACEScg",
		WorkingColorSpace::SrgbLegacy => "sRGB",
	};
	assert_eq!(pipeline_working_ofx_name(), expected, "settings restored");
}

#[test]
fn config_path_env_and_bundled_fallback() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	let saved = std::env::var_os("OCIO");
	struct EnvGuard(Option<std::ffi::OsString>);
	impl Drop for EnvGuard {
		fn drop(&mut self) {
			match self.0.take() {
				Some(v) => std::env::set_var("OCIO", v),
				None => std::env::remove_var("OCIO"),
			}
		}
	}
	let _env_guard = EnvGuard(saved);

	std::env::set_var("OCIO", "/tmp/quercus-config-path-test.ocio");
	assert_eq!(
		config_path().as_deref(),
		Some("/tmp/quercus-config-path-test.ocio"),
		"a non-empty $OCIO wins"
	);
	// An empty $OCIO is treated as unset and falls back to the bundled
	// extraction location (a config must exist for that branch).
	std::env::set_var("OCIO", "");
	let fallback = config_path().expect("bundled fallback when a config exists");
	assert!(fallback.ends_with("ocioconf/config.ocio"), "got {fallback}");
}

#[test]
fn set_up_default_config_from_bad_path_errors_and_keeps_config() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		return;
	}
	let before = default_config();
	let err = set_up_default_config_from(Some("/nonexistent/quercus-config.ocio"));
	assert!(err.is_err(), "a bad config path is a load error");
	assert!(
		default_config().is_some(),
		"the previous default config survives a failed load"
	);
	assert_eq!(
		default_config().map(|c| c.cache_id()),
		before.map(|c| c.cache_id())
	);
}

#[test]
fn display_transform_scene_linear_role_fallback() {
	let _lock = config_lock();
	// A minimal config with a scene_linear role but no reference /
	// aces_interchange bindings: the last-resort fallback chain.
	let cfg_text = r#"
ocio_profile_version: 2
name: quercus-fallback-test
search_path: ""
roles:
  scene_linear: Raw
displays:
  quercusdisplay:
    - !<View> {name: quercusview, colorspace: Raw}
colorspaces:
  - !<ColorSpace>
    name: Raw
    family: ""
    isdata: false
    allocation: uniform
"#;
	let Ok(cfg) = ocio_rs::Config::from_stream(cfg_text) else {
		eprintln!("OCIO config-from-stream unavailable; skipping");
		return;
	};
	// The fallback chain only runs when the first three lookups fail.
	assert!(cfg
		.color_space("reference")
		.and_then(|cs| cs.name())
		.filter(|s| !s.is_empty())
		.is_none());
	assert!(cfg
		.role_color_space("reference")
		.filter(|s| !s.is_empty())
		.is_none());
	assert!(cfg
		.role_color_space("aces_interchange")
		.filter(|s| !s.is_empty())
		.is_none());
	assert_eq!(cfg.role_color_space("scene_linear").as_deref(), Some("Raw"));
	let saved = default_config();
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) =
		Some(std::sync::Arc::new(SafeConfig(cfg)));
	let found = display_transform_result("quercusdisplay", "quercusview");
	let unknown_view = display_transform_result("quercusdisplay", "nope");
	let unknown_display = display_transform_result("nope", "quercusview");
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = saved;

	assert!(
		found.is_ok(),
		"scene_linear fallback resolves the reference space: {found:?}"
	);
	assert_eq!(unknown_view.unwrap(), None);
	assert_eq!(unknown_display.unwrap(), None);
}

// ---- Additional branch coverage: no-config and explicit-config paths ----

/// Temporarily clear the process-wide default config and restore it on
/// drop (used by the no-config tests below; the caller must hold
/// `config_lock`).
struct DefaultConfigGuard(Option<std::sync::Arc<SafeConfig>>);

impl DefaultConfigGuard {
	fn clear() -> Self {
		let saved = default_config();
		*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = None;
		Self(saved)
	}
}

impl Drop for DefaultConfigGuard {
	fn drop(&mut self) {
		*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = self.0.take();
	}
}

/// Temporarily set `$OCIO`, restoring the previous value on drop.
struct OcioEnvGuard(Option<std::ffi::OsString>);

impl OcioEnvGuard {
	fn set(value: &str) -> Self {
		let saved = std::env::var_os("OCIO");
		std::env::set_var("OCIO", value);
		Self(saved)
	}
}

impl Drop for OcioEnvGuard {
	fn drop(&mut self) {
		match self.0.take() {
			Some(v) => std::env::set_var("OCIO", v),
			None => std::env::remove_var("OCIO"),
		}
	}
}

/// Every `default_config()?` guard's `None` arm: without a process
/// default config each factory returns `None` instead of panicking,
/// and `display_transform_result` reports the missing-config state.
#[test]
fn factories_without_a_default_config_return_none() {
	let _lock = config_lock();
	let _env = OcioEnvGuard::set("");
	let _guard = DefaultConfigGuard::clear();

	assert!(ColorProcessor::create("scene_linear", "sdr-video", Direction::Normal).is_none());
	assert!(ColorProcessor::create_lut("/tmp/quercus-none.cube", Direction::Normal).is_none());
	assert!(ColorProcessor::create_grading_primary(GradingStyle::Lin).is_none());
	assert!(ColorProcessor::create_grading_primary(GradingStyle::Log).is_none());
	assert!(ColorProcessor::create_display_icc("scene_linear", "/tmp/quercus-none.icc").is_none());
	assert!(
		ColorProcessor::create_display_icc_bgra8("scene_linear", "/tmp/quercus-none.icc").is_none()
	);
	assert!(ColorProcessor::create_display_icc_xyz("/tmp/quercus-none.icc").is_none());
	assert!(ColorProcessor::create_display_icc_xyz_bgra8("/tmp/quercus-none.icc").is_none());
	assert!(ocio_function_shader("quercus_fn", "a", "b").is_none());
	assert!(grading_primary_function_shader(GradingStyle::Lin).is_none());
	assert!(grading_primary_function_shader(GradingStyle::Log).is_none());
	assert_eq!(
		display_transform_result("display", "view")
			.unwrap_err()
			.code(),
		crate::error::QUERCUSCORE_E_STATE
	);
	assert!(display_transform("display", "view").is_none());
	assert!(config_path().is_none(), "no config and empty $OCIO");
}

/// `set_up_default_config_from(Some(path))` loads an explicit config
/// file and installs it as the process default (the project-properties
/// OCIO override path), replacing the previous default.
#[test]
fn set_up_default_config_from_explicit_file_replaces_default() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no bundled OCIO config; skipping");
		return;
	}
	let path = std::env::temp_dir().join(format!(
		"quercus-color-explicit-{}.ocio",
		std::process::id()
	));
	let cfg_text = r#"
ocio_profile_version: 2
name: quercus-explicit-test
search_path: ""
roles:
  scene_linear: Raw
  default: Raw
displays:
  quercusdisplay:
    - !<View> {name: quercusview, colorspace: Raw}
colorspaces:
  - !<ColorSpace>
    name: Raw
    family: ""
    isdata: false
    allocation: uniform
"#;
	std::fs::write(&path, cfg_text).unwrap();
	let result = set_up_default_config_from(Some(path.to_string_lossy().as_ref()));
	let installed_name = default_config().and_then(|c| c.name());
	let _ = std::fs::remove_file(&path);
	// Restore the bundled default config installed above.
	let restored = set_up_default_config_from(None);

	assert!(result.is_ok(), "an explicit valid config loads: {result:?}");
	assert_eq!(installed_name.as_deref(), Some("quercus-explicit-test"));
	assert!(
		restored.is_ok(),
		"the bundled config restores: {restored:?}"
	);
}

/// The XYZ display chain's build failure surfaces as `None` (not a
/// pass-through), so the caller can fall back to the sRGB chain: a
/// config that cannot produce the processor (unreadable ICC file)
/// yields `Some(invalid)` from the classic builder but `None` from
/// the XYZ wrapper.
#[test]
fn display_icc_xyz_returns_none_when_chain_is_invalid() {
	let _lock = config_lock();
	let cfg_text = r#"
ocio_profile_version: 2
name: quercus-xyz-test
search_path: ""
roles:
  scene_linear: Raw
  cie_xyz_d65_interchange: Raw
displays:
  quercusdisplay:
    - !<View> {name: quercusview, colorspace: Raw}
colorspaces:
  - !<ColorSpace>
    name: Raw
    family: ""
    isdata: false
    allocation: uniform
  - !<ColorSpace>
    name: Linear Rec.709 (sRGB)
    family: ""
    isdata: false
    allocation: uniform
"#;
	let Ok(cfg) = ocio_rs::Config::from_stream(cfg_text) else {
		eprintln!("OCIO config-from-stream unavailable; skipping");
		return;
	};
	// An unreadable ICC file must fail processor creation but not the
	// transform-chain assembly.
	let icc = std::env::temp_dir().join(format!("quercus-xyz-junk-{}.icc", std::process::id()));
	std::fs::write(&icc, b"this is not an ICC profile").unwrap();
	let icc = icc.to_string_lossy().into_owned();

	let _guard = DefaultConfigGuard::clear();
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) =
		Some(std::sync::Arc::new(SafeConfig(cfg)));

	let classic =
		ColorProcessor::create_display_icc("cie_xyz_d65_interchange", &icc).expect("handle");
	let classic_bgra8 =
		ColorProcessor::create_display_icc_bgra8("cie_xyz_d65_interchange", &icc).expect("handle");
	let xyz = ColorProcessor::create_display_icc_xyz(&icc);
	let xyz_bgra8 = ColorProcessor::create_display_icc_xyz_bgra8(&icc);

	assert!(
		!classic.is_valid(),
		"the ICC leg fails against a junk profile (non-fatal pass-through)"
	);
	assert!(!classic_bgra8.is_valid());
	assert!(
		xyz.is_none(),
		"an invalid chain must surface as None for the XYZ wrapper"
	);
	assert!(xyz_bgra8.is_none());
	let _ = std::fs::remove_file(&icc);
}

/// A config whose display/view exist but whose roles do not resolve
/// makes `display_transform_result` report `Error::State` (the
/// `ok_or` arm of the role fallback chain).
#[test]
fn display_transform_result_errors_when_no_reference_role_resolves() {
	let _lock = config_lock();
	let cfg_text = r#"
ocio_profile_version: 2
name: quercus-no-roles-test
search_path: ""
displays:
  quercusdisplay:
    - !<View> {name: quercusview, colorspace: Raw}
colorspaces:
  - !<ColorSpace>
    name: Raw
    family: ""
    isdata: false
    allocation: uniform
"#;
	let Ok(cfg) = ocio_rs::Config::from_stream(cfg_text) else {
		eprintln!("OCIO config-from-stream unavailable; skipping");
		return;
	};
	let _guard = DefaultConfigGuard::clear();
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) =
		Some(std::sync::Arc::new(SafeConfig(cfg)));

	let err = display_transform_result("quercusdisplay", "quercusview")
		.expect_err("no reference role resolves to a source space");
	assert_eq!(err.code(), crate::error::QUERCUSCORE_E_STATE);
	assert!(display_transform("quercusdisplay", "quercusview").is_none());
}

/// `set_up_default_config` honours `$OCIO` (non-empty → that file;
/// empty → the bundled config) and swaps the process default.
#[test]
fn set_up_default_config_honours_the_ocio_environment() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no bundled OCIO config; skipping");
		return;
	}
	let path = std::env::temp_dir().join(format!("quercus-color-env-{}.ocio", std::process::id()));
	let cfg_text = r#"
ocio_profile_version: 2
name: quercus-env-test
search_path: ""
roles:
  scene_linear: Raw
  default: Raw
displays:
  quercusdisplay:
    - !<View> {name: quercusview, colorspace: Raw}
colorspaces:
  - !<ColorSpace>
    name: Raw
    family: ""
    isdata: false
    allocation: uniform
"#;
	std::fs::write(&path, cfg_text).unwrap();
	let path_str = path.to_string_lossy().into_owned();

	let loaded = {
		let _env = OcioEnvGuard::set(&path_str);
		set_up_default_config()
	};
	let installed_name = default_config().and_then(|c| c.name());
	// An empty $OCIO is treated as unset: the bundled config loads.
	let bundled = {
		let _env = OcioEnvGuard::set("");
		set_up_default_config()
	};
	let _ = std::fs::remove_file(&path);

	assert!(loaded.is_ok(), "a non-empty $OCIO loads: {loaded:?}");
	assert_eq!(installed_name.as_deref(), Some("quercus-env-test"));
	assert!(bundled.is_ok(), "an empty $OCIO falls back: {bundled:?}");
}

/// A valid processor rejects a short F32 buffer (the `try_apply`
/// error mapping) and the `convert_bgra8` entry point guards its
/// staging buffer length.
#[test]
fn conversion_error_arms_from_buffer_size_mismatches() {
	let _lock = config_lock();
	if set_up_default_config().is_err() {
		eprintln!("no default OCIO config; skipping");
		return;
	}
	let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
		.expect("handle always returned");
	if !p.is_valid() {
		eprintln!("processor unavailable in this config; skipping");
		return;
	}
	// Fewer samples than pixels * 4: the OCIO apply fails and is
	// mapped to a named error.
	let mut short = [0f32; 4];
	let err = p.convert_f32_rgba(&mut short, 2).unwrap_err();
	assert!(
		err.to_string().contains("OCIO f32 apply"),
		"the apply error is named: {err}"
	);
	// BGRA8: the staging buffer must cover pixels * 4 bytes.
	let mut bytes = [9u8; 4];
	assert_eq!(
		p.convert_bgra8(&mut bytes, 2).unwrap_err().code(),
		crate::error::QUERCUSCORE_E_INVALID
	);
}
