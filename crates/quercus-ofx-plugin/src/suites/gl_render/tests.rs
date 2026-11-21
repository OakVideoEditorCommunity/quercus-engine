use super::*;
use crate::property::{PropertySet, Value};

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// The GL render suite's live table is process-global state: every test in
/// this module that touches it is serialized through this lock (otherwise
/// purge_leftovers would step on other tests' registered entries).
fn suite_lock() -> std::sync::MutexGuard<'static, ()> {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn clip(name: &str) -> ClipInstance {
	ClipInstance::from_descriptor(&crate::descriptor::ClipDescriptor::new(name))
}

fn clip_handle(c: &ClipInstance) -> *mut c_void {
	tag::make(&c.props as *const PropertySet, tag::CLIP)
}

fn f32_frame(w: i32, h: i32, fill: f32) -> crate::render::Frame {
	let params = crate::render::VideoParams {
		width: w,
		height: h,
		format: crate::render::PIXEL_FORMAT_F32,
		..Default::default()
	};
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(params);
	assert!(frame.allocate());
	for c in frame.data.as_chunks_mut::<4>().0 {
		c.copy_from_slice(&fill.to_le_bytes());
	}
	frame
}

fn f32_texture(w: i32, h: i32, fill: f32) -> crate::render::Texture {
	crate::render::Texture::wrap_frame(f32_frame(w, h, fill))
}

struct FakeGpu;

impl quercus_core::backend::GpuContextLike for FakeGpu {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Cpu
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(
		&self,
		_token: u64,
		_frame: &quercus_core::texture::Frame,
	) -> quercus_render::error::Result<()> {
		Ok(())
	}
	fn download(&self, _token: u64) -> quercus_render::error::Result<quercus_core::texture::Frame> {
		Ok(quercus_core::texture::Frame::new())
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&quercus_core::color::ColorProcessor>,
	) -> quercus_render::error::Result<()> {
		Ok(())
	}
}

fn gl_ctx(output: crate::render::Texture, index: Option<i32>) -> crate::suites::GlCtx {
	crate::suites::GlCtx {
		renderer: std::sync::Arc::new(FakeGpu),
		output_texture: output,
		gl_pixel_depth: "OfxBitDepthFloat",
		output_gl_texture: index,
	}
}

/// Reads the property set from a handle written by clipLoadTexture.
fn handle_props(handle: *mut c_void) -> PropertySet {
	assert_eq!(tag::kind(handle), tag::PROPERTY_SET, "texture handle tag 0");
	unsafe { (*(tag::strip(handle))).clone() }
}

// `Value` does not implement PartialEq (Pointer is not comparable) — assert by unwrapping per kind.

fn prop_int(props: &PropertySet, name: &str, index: usize) -> i32 {
	match props.get(name, index) {
		Some(Value::Int(i)) => i,
		other => panic!("{name}[{index}] should be Int, got {other:?}"),
	}
}

fn prop_double(props: &PropertySet, name: &str, index: usize) -> f64 {
	match props.get(name, index) {
		Some(Value::Double(d)) => d,
		other => panic!("{name}[{index}] should be Double, got {other:?}"),
	}
}

fn prop_string(props: &PropertySet, name: &str, index: usize) -> String {
	match props.get(name, index) {
		Some(Value::String(s)) => s.to_string_lossy().into_owned(),
		other => panic!("{name}[{index}] should be String, got {other:?}"),
	}
}

/// GL pixel-depth negotiation matrix (ofxGPURender.h
/// kOfxOpenGLPropPixelDepth): undeclared/contains Float → pipeline F32
/// feasible; declared without Float → None (GL mode not feasible, fall back
/// to CPU).
#[test]
fn pick_gl_pixel_depth_matrix() {
	let props = PropertySet::new();
	assert_eq!(pick_gl_pixel_depth(&props), Some("OfxBitDepthFloat"));

	let props2 = PropertySet::new();
	props2.define(
		crate::host::PROP_GL_PIXEL_DEPTH,
		vec![
			Value::String(cs("OfxBitDepthHalf")),
			Value::String(cs("OfxBitDepthByte")),
		],
	);
	assert_eq!(pick_gl_pixel_depth(&props2), None);

	let props3 = PropertySet::new();
	props3.define(
		crate::host::PROP_GL_PIXEL_DEPTH,
		vec![
			Value::String(cs("OfxBitDepthByte")),
			Value::String(cs("OfxBitDepthFloat")),
		],
	);
	assert_eq!(pick_gl_pixel_depth(&props3), Some("OfxBitDepthFloat"));

	let props4 = PropertySet::new();
	props4.define(
		crate::host::PROP_GL_PIXEL_DEPTH,
		vec![Value::String(cs("OfxBitDepthFloat"))],
	);
	assert_eq!(pick_gl_pixel_depth(&props4), Some("OfxBitDepthFloat"));

	// List present but its elements are not strings → skipped; no Float → None.
	let props5 = PropertySet::new();
	props5.define(crate::host::PROP_GL_PIXEL_DEPTH, vec![Value::Int(4)]);
	assert_eq!(pick_gl_pixel_depth(&props5), None);
}

/// Texture request format → expected components matrix (NULL/unknown →
/// host's choice).
#[test]
fn format_components_matrix() {
	use crate::image::Components;
	assert_eq!(
		format_components(Some(GL_FORMAT_RGBA)),
		Some(Components::Rgba)
	);
	assert_eq!(format_components(None), Some(Components::Rgba));
	assert_eq!(
		format_components(Some(GL_FORMAT_RGB)),
		Some(Components::Rgb)
	);
	assert_eq!(
		format_components(Some(GL_FORMAT_ALPHA)),
		Some(Components::Alpha)
	);
	assert_eq!(format_components(Some(GL_FORMAT_LUMINANCE)), None);
	assert_eq!(format_components(Some(GL_FORMAT_LUMINANCE_ALPHA)), None);
	assert_eq!(format_components(Some("bogus")), None);
}

/// make_texture_props: all 12 properties prescribed by ofxGPURender.h are
/// present and have the right values.
#[test]
fn make_texture_props_fields() {
	let props = make_texture_props(
		4.0,
		2.0,
		crate::image::Components::Rgb,
		"OfxBitDepthFloat",
		PREMULT_NONE,
		1.5,
		crate::instance::RenderScale { x: 2.0, y: 3.0 },
		48,
		7,
	);
	assert_eq!(prop_int(&props, GL_TEXTURE_INDEX, 0), 7);
	assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
	assert_eq!(
		prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0),
		"OfxBitDepthFloat"
	);
	assert_eq!(
		prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
		"OfxImageComponentRGB"
	);
	assert_eq!(
		prop_string(&props, "OfxImageEffectPropPreMultiplication", 0),
		PREMULT_NONE
	);
	assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 0), 2.0);
	assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 1), 3.0);
	assert_eq!(prop_double(&props, "OfxImagePropPixelAspectRatio", 0), 1.5);
	for (i, want) in [0, 0, 4, 2].iter().enumerate() {
		assert_eq!(
			prop_int(&props, crate::image::K_IMAGE_PROP_BOUNDS, i),
			*want,
			"Bounds[{i}]"
		);
		assert_eq!(
			prop_int(&props, crate::image::K_IMAGE_PROP_ROD, i),
			*want,
			"RoD[{i}]"
		);
	}
	assert_eq!(
		prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
		48
	);
	assert_eq!(prop_string(&props, "OfxImagePropField", 0), "OfxFieldNone");
	assert!(matches!(
		props.get(crate::image::K_IMAGE_PROP_UNIQUE_ID, 0),
		Some(Value::String(_))
	));
}

/// rect_props: a Double rectangle rounds to Int×4 (negative half away from
/// zero).
#[test]
fn rect_props_rounds_to_int() {
	let props = PropertySet::new();
	rect_props(
		&props,
		"r",
		crate::instance::OfxRectD {
			x1: 1.4,
			y1: -0.6,
			x2: 3.5,
			y2: 2.49,
		},
	);
	for (i, want) in [1, -1, 4, 2].iter().enumerate() {
		assert_eq!(prop_int(&props, "r", i), *want, "r[{i}]");
	}
}

/// clip_string / clip_par: string and non-string fallbacks, PAR defaults to
/// 1.0 when missing.
#[test]
fn clip_string_and_par_fallbacks() {
	let c = clip("Source");
	assert_eq!(clip_string(&c, "missing", "def"), "def");
	assert_eq!(clip_par(&c), 1.0);

	c.props.set_one("s", Value::String(cs("xyz")));
	assert_eq!(clip_string(&c, "s", "def"), "xyz");
	c.props.set_one("s2", Value::Int(3));
	assert_eq!(clip_string(&c, "s2", "def"), "def");

	c.props
		.set_one("OfxImagePropPixelAspectRatio", Value::Double(2.0));
	assert_eq!(clip_par(&c), 2.0);
	c.props
		.set_one("OfxImagePropPixelAspectRatio", Value::Int(2));
	assert_eq!(clip_par(&c), 1.0, "non-Double falls back to 1.0");
}

/// texture_size / image_width / image_height.
#[test]
fn texture_size_and_image_dims() {
	assert_eq!(texture_size(&crate::render::Texture::dummy()), (0.0, 0.0));
	assert_eq!(texture_size(&f32_texture(3, 2, 0.0)), (3.0, 2.0));

	let img = Image::allocate(
		crate::image::BitDepth::Float,
		crate::image::Components::Rgba,
		crate::instance::OfxRectD {
			x1: 0.0,
			y1: 0.0,
			x2: 5.0,
			y2: 4.0,
		},
	);
	assert_eq!(image_width(&img), 5.0);
	assert_eq!(image_height(&img), 4.0);
}

/// resolve_clip: null handle / non-CLIP tag → BadHandle; a CLIP tag is
/// valid.
#[test]
fn resolve_clip_handle_rules() {
	assert!(matches!(
		resolve_clip(std::ptr::null_mut()),
		Err(status::ERR_BAD_HANDLE)
	));
	let props = PropertySet::new();
	let wrong = tag::make(&props as *const PropertySet, tag::PROPERTY_SET);
	assert!(matches!(resolve_clip(wrong), Err(status::ERR_BAD_HANDLE)));
	let c = clip("Source");
	assert!(resolve_clip(clip_handle(&c)).is_ok());
}

/// clipLoadTexture: entry guards (null out / null clip / non-clip handle /
/// no GL context).
#[test]
fn clip_load_texture_entry_guards() {
	let _lock = suite_lock();
	crate::suites::set_gl_ctx(None);
	let s = suite_v1();
	let c = clip("Source");
	let name = cs(GL_FORMAT_RGBA);
	let mut sentinel = 0u8;
	let mut out: *mut c_void = &mut sentinel as *mut u8 as *mut c_void;
	unsafe {
		// Null out → BadHandle (and it is not dereferenced).
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				name.as_ptr(),
				std::ptr::null(),
				std::ptr::null_mut(),
			),
			status::ERR_BAD_HANDLE
		);
		// Null clip handle → BadHandle.
		assert_eq!(
			(s.clip_load_texture)(
				std::ptr::null_mut(),
				0.0,
				name.as_ptr(),
				std::ptr::null(),
				&mut out,
			),
			status::ERR_BAD_HANDLE
		);
		assert!(out.is_null(), "entry clears out first");
		// Non-clip tag → BadHandle.
		let wrong = tag::make(&c.props as *const PropertySet, tag::PROPERTY_SET);
		assert_eq!(
			(s.clip_load_texture)(wrong, 0.0, name.as_ptr(), std::ptr::null(), &mut out),
			status::ERR_BAD_HANDLE
		);
		// No GL context → MissingHostFeature (the GL suite is only reachable during GL rendering).
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				name.as_ptr(),
				std::ptr::null(),
				&mut out,
			),
			status::ERR_MISSING_HOST_FEATURE
		);
	}
}

/// clipLoadTexture(Output): ignores format and returns the handle of the
/// attached output texture (the real GL name is reported through
/// OpenGLTextureIndex); free only removes the handle.
#[test]
fn clip_load_texture_output_clip() {
	let _lock = suite_lock();
	let s = suite_v1();
	let out_clip = clip("Output");
	let tex = f32_texture(4, 2, 0.25);
	crate::suites::set_gl_ctx(Some(gl_ctx(tex, Some(9))));
	let rgb = cs(GL_FORMAT_RGB); // Output ignores the requested format.
	let mut handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&out_clip),
				0.0,
				rgb.as_ptr(),
				std::ptr::null(),
				&mut handle,
			),
			status::OK
		);
	}
	let props = handle_props(handle);
	assert_eq!(prop_int(&props, GL_TEXTURE_INDEX, 0), 9);
	assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
	assert_eq!(
		prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
		"OfxImageComponentRGBA"
	);
	assert_eq!(prop_int(&props, crate::image::K_IMAGE_PROP_BOUNDS, 2), 4);
	// Row bytes = w × RGBA × F32; OpenGLTextureIndex=0 variant (CPU fallback).
	assert_eq!(
		prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
		4 * 4 * 4
	);
	assert!(LIVE_TEXTURES
		.lock()
		.unwrap()
		.contains_key(&(tag::strip(handle) as usize)));

	// free: the output clip releases the handle only, not the texture (the host reads it back).
	unsafe {
		assert_eq!((s.clip_free_texture)(handle), status::OK);
		// Double release / null handle → BadHandle.
		assert_eq!((s.clip_free_texture)(handle), status::ERR_BAD_HANDLE);
		assert_eq!(
			(s.clip_free_texture)(std::ptr::null_mut()),
			status::ERR_BAD_HANDLE
		);
	}
	assert!(!LIVE_TEXTURES
		.lock()
		.unwrap()
		.contains_key(&(tag::strip(handle) as usize)));

	// output_gl_texture = None → index 0 (CPU fallback semantics).
	crate::suites::set_gl_ctx(Some(gl_ctx(f32_texture(2, 2, 0.0), None)));
	let mut h0: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&out_clip),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut h0,
			),
			status::OK
		);
	}
	assert_eq!(prop_int(&handle_props(h0), GL_TEXTURE_INDEX, 0), 0);
	unsafe {
		assert_eq!((s.clip_free_texture)(h0), status::OK);
	}
	crate::suites::set_gl_ctx(None);
}

/// clipLoadTexture(Output): attached texture is dummy/0-sized → BadHandle.
#[test]
fn clip_load_texture_output_rejects_empty_texture() {
	let _lock = suite_lock();
	let s = suite_v1();
	let out_clip = clip("Output");
	crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
	let mut handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&out_clip),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut handle,
			),
			status::ERR_BAD_HANDLE
		);
	}
	crate::suites::set_gl_ctx(None);
}

/// clipLoadTexture(input clip): full-frame restriction, format matching,
/// negotiated properties and release; OpenGLTextureIndex=0 when GL is
/// unavailable (CPU fallback).
#[test]
fn clip_load_texture_input_clip() {
	let _lock = suite_lock();
	// Real GL bridge on Linux/macOS: clipLoadTexture's GL upload requires the
	// context current (the production path guarantees it via render_driver's GlGuard).
	let _gl = crate::gl_bridge::acquire().ok();
	let s = suite_v1();
	let c = clip("Source");
	c.set_input_texture(Some(f32_texture(2, 2, 0.5)), 0.0);
	crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));

	// Sub-region request (non-null region) → Failed (phase 2 is full-frame only).
	let region = crate::instance::OfxRectD::default();
	unsafe {
		let mut h: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				std::ptr::null(),
				&region as *const crate::instance::OfxRectD as *const c_void,
				&mut h,
			),
			status::FAILED
		);
	}

	// No input texture attached → Failed (fetch_image NotFound mapped).
	let empty = clip("Source");
	unsafe {
		let mut h: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&empty),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut h,
			),
			status::FAILED
		);
	}

	// Component mismatch: request RGB but the image is RGBA (negotiated default) → Failed.
	let rgb = cs(GL_FORMAT_RGB);
	unsafe {
		let mut h: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			(s.clip_load_texture)(clip_handle(&c), 0.0, rgb.as_ptr(), std::ptr::null(), &mut h,),
			status::FAILED
		);
	}

	// Success (format=NULL): the texture-handle properties are complete.
	let mut handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut handle,
			),
			status::OK
		);
	}
	let props = handle_props(handle);
	assert_eq!(
		prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
		"OfxImageComponentRGBA"
	);
	assert_eq!(
		prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0),
		"OfxBitDepthFloat"
	);
	assert_eq!(
		prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
		2 * 4 * 4
	);
	assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
	// Not GL (Linux/Windows stub) → index 0; on macOS with GL it is > 0.
	let index = match props.get(GL_TEXTURE_INDEX, 0) {
		Some(Value::Int(i)) => i,
		other => panic!("OpenGLTextureIndex should be Int, got {other:?}"),
	};
	if crate::gl_bridge::gl_available() {
		assert!(
			index > 0,
			"input texture should have a real GL name when GL is available"
		);
	} else {
		assert_eq!(index, 0, "index is 0 without GL");
	}

	// Requesting RGBA (explicit) and the Luminance family (None = host's choice) both succeed.
	let rgba = cs(GL_FORMAT_RGBA);
	let lum = cs(GL_FORMAT_LUMINANCE);
	let lum_a = cs(GL_FORMAT_LUMINANCE_ALPHA);
	let mut h2: *mut c_void = std::ptr::null_mut();
	let mut h3: *mut c_void = std::ptr::null_mut();
	let mut h4: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				rgba.as_ptr(),
				std::ptr::null(),
				&mut h2
			),
			status::OK
		);
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				lum.as_ptr(),
				std::ptr::null(),
				&mut h3
			),
			status::OK
		);
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				lum_a.as_ptr(),
				std::ptr::null(),
				&mut h4
			),
			status::OK
		);
		// A non-UTF-8 format string is treated as None (host's choice) → OK.
		let bad = CString::new(vec![0xFFu8]).unwrap();
		let mut h5: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				bad.as_ptr(),
				std::ptr::null(),
				&mut h5,
			),
			status::OK
		);
		// free input texture: drops the table entry (GL mode also deletes the real GL texture).
		assert_eq!((s.clip_free_texture)(h2), status::OK);
		assert_eq!((s.clip_free_texture)(h3), status::OK);
		assert_eq!((s.clip_free_texture)(h4), status::OK);
		assert_eq!((s.clip_free_texture)(h5), status::OK);
		assert_eq!((s.clip_free_texture)(handle), status::OK);
	}
	crate::suites::set_gl_ctx(None);
}

/// clipLoadTexture: the render context's scale flows into the texture
/// properties; a 0-sized image → Failed.
#[test]
fn clip_load_texture_uses_render_scale_and_rejects_zero_image() {
	let _lock = suite_lock();
	let s = suite_v1();

	// With a render ctx: scale is written into the handle's RenderScale property.
	let c = clip("Source");
	c.set_input_texture(Some(f32_texture(2, 2, 0.0)), 0.0);
	crate::suites::set_render_ctx(Some(crate::suites::RenderCtx {
		time: 1.0,
		scale: crate::instance::RenderScale { x: 2.0, y: 2.0 },
		range: crate::instance::OfxRangeD { min: 0.0, max: 1.0 },
	}));
	crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
	let mut handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&c),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut handle,
			),
			status::OK
		);
	}
	let props = handle_props(handle);
	assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 0), 2.0);
	unsafe {
		assert_eq!((s.clip_free_texture)(handle), status::OK);
	}
	crate::suites::set_render_ctx(None);

	// 0×0 frame (not dummy: data is non-empty) → image_width == 0 → Failed.
	let zero = clip("Source");
	let params = crate::render::VideoParams {
		format: crate::render::PIXEL_FORMAT_F32,
		..Default::default()
	};
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(params);
	frame.data = vec![0u8; 16];
	zero.set_input_texture(Some(crate::render::Texture::wrap_frame(frame)), 0.0);
	let mut h: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&zero),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut h,
			),
			status::FAILED
		);
	}
	crate::suites::set_gl_ctx(None);
}

/// purge_leftovers: releases leftover input handles and keeps the output
/// handle (the host still has to read it back).
#[test]
fn purge_leftovers_keeps_outputs() {
	let _lock = suite_lock();
	let s = suite_v1();
	let input = clip("Source");
	input.set_input_texture(Some(f32_texture(2, 2, 0.0)), 0.0);
	let out_clip = clip("Output");
	crate::suites::set_gl_ctx(Some(gl_ctx(f32_texture(2, 2, 0.0), Some(1))));
	let mut input_handle: *mut c_void = std::ptr::null_mut();
	let mut out_handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&input),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut input_handle,
			),
			status::OK
		);
		assert_eq!(
			(s.clip_load_texture)(
				clip_handle(&out_clip),
				0.0,
				std::ptr::null(),
				std::ptr::null(),
				&mut out_handle,
			),
			status::OK
		);
	}
	assert!(LIVE_TEXTURES
		.lock()
		.unwrap()
		.contains_key(&(tag::strip(input_handle) as usize)));
	assert!(LIVE_TEXTURES
		.lock()
		.unwrap()
		.contains_key(&(tag::strip(out_handle) as usize)));

	purge_leftovers();
	let live = LIVE_TEXTURES.lock().unwrap();
	assert!(
		!live.contains_key(&(tag::strip(input_handle) as usize)),
		"input handle should be released by cleanup"
	);
	assert!(
		live.contains_key(&(tag::strip(out_handle) as usize)),
		"output handle should be kept"
	);
	drop(live);
	unsafe {
		assert_eq!((s.clip_free_texture)(out_handle), status::OK);
	}
	crate::suites::set_gl_ctx(None);
}

/// delete_gl_texture_if_gl: no-op without a GL context; with a context it
/// deletes through the bridge (the bridge is a stub when GL is unavailable,
/// so this only verifies it does not panic).
#[test]
fn delete_gl_texture_if_gl_branches() {
	let _lock = suite_lock();
	crate::suites::set_gl_ctx(None);
	delete_gl_texture_if_gl(3);
	crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
	delete_gl_texture_if_gl(4);
	crate::suites::set_gl_ctx(None);
}

/// flushResources: the host caches no GPU resources → ReplyDefault.
#[test]
fn flush_resources_replies_default() {
	unsafe {
		assert_eq!((suite_v1().flush_resources)(), status::REPLY_DEFAULT);
	}
}
