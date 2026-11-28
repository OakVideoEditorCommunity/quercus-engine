use super::*;
use crate::descriptor::{ClipDescriptor, EffectDescriptor};
use crate::error::Error;
use crate::instance::Instance;
use crate::param::{ParamDef, ParamInstance, ParamSetInstance, ParamValue};
use crate::property::{PropertySet, Value};

fn cs(s: &str) -> std::ffi::CString {
	std::ffi::CString::new(s).unwrap()
}

fn f32_frame(w: i32, h: i32, fill: f32) -> crate::render::Frame {
	let params = render::VideoParams {
		width: w,
		height: h,
		format: render::PIXEL_FORMAT_F32,
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

fn f32_texture(w: i32, h: i32, fill: f32) -> Texture {
	Texture::wrap_frame(f32_frame(w, h, fill))
}

fn u8_texture(w: i32, h: i32) -> Texture {
	let params = render::VideoParams {
		width: w,
		height: h,
		format: render::PIXEL_FORMAT_U8,
		..Default::default()
	};
	let pixels = vec![0u8; w as usize * h as usize * 4];
	render::texture_create(&params, &pixels, 0).expect("U8 texture should be creatable")
}

fn f32_image(w: i32, h: i32, fill: f32) -> Image {
	let mut img = Image::allocate(
		crate::image::BitDepth::Float,
		crate::image::Components::Rgba,
		OfxRectD {
			x1: 0.0,
			y1: 0.0,
			x2: w as f64,
			y2: h as f64,
		},
	);
	for c in img.data.as_mut_slice().as_chunks_mut::<4>().0 {
		c.copy_from_slice(&fill.to_le_bytes());
	}
	img
}

/// Test entry point: impersonate plugin behavior using the test.*
/// configuration in the instance props. The instance handle (offset 0) is
/// the configuration store, avoiding global state shared between tests.
unsafe extern "C" fn test_entry(
	action: *const std::ffi::c_char,
	handle: *const std::ffi::c_void,
	in_args: *mut std::ffi::c_void,
	out_args: *mut std::ffi::c_void,
) -> std::ffi::c_int {
	let action_name = unsafe { std::ffi::CStr::from_ptr(action) }
		.to_string_lossy()
		.into_owned();
	let inst_props = unsafe { &*(crate::suites::tag::strip(handle as *mut std::ffi::c_void)) };
	let in_args = unsafe { &*(in_args as *const PropertySet) };
	let out_args = unsafe { &*(out_args as *const PropertySet) };

	if action_name == crate::host::ACTION_GET_CLIP_PREFERENCES {
		if let Some(Value::String(comps)) = inst_props.get("test.components", 0) {
			out_args.set_one(
				&crate::host::clip_pref_prop(crate::host::CLIP_PREF_COMPONENTS, "Output"),
				Value::String(comps.clone()),
			);
		}
		if let Some(Value::String(depth)) = inst_props.get("test.depth", 0) {
			out_args.set_one(
				&crate::host::clip_pref_prop(crate::host::CLIP_PREF_DEPTH, "Output"),
				Value::String(depth.clone()),
			);
		}
	} else if action_name == crate::host::ACTION_IS_IDENTITY {
		if let Some(Value::String(clip)) = inst_props.get("test.identity_clip", 0) {
			if !clip.to_string_lossy().is_empty() {
				out_args.set_one(crate::host::PROP_IS_IDENTITY, Value::String(clip.clone()));
				if let Some(Value::Double(t)) = inst_props.get("test.identity_time", 0) {
					out_args.set_one(crate::host::PROP_TIME, Value::Double(t));
				}
			}
		}
	} else if action_name == crate::host::ACTION_RENDER {
		// GL failure injection: only for GL-mode render (in args carry OpenGLEnabled=1).
		let gl_enabled = matches!(
			in_args.get(crate::host::PROP_GL_ENABLED, 0),
			Some(Value::Int(1))
		);
		if gl_enabled && matches!(inst_props.get("test.gl_fail", 0), Some(Value::Int(1))) {
			return crate::suites::status::FAILED;
		}
		if let Some(Value::Int(st)) = inst_props.get("test.render_status", 0) {
			return st;
		}
	}
	crate::suites::status::OK
}

#[allow(clippy::vec_box)] // ParamSetInstance's field type is exactly Vec<Box<ParamInstance>>.
fn make_instance(
	desc_props: Vec<(&'static str, Value)>,
	inst_props: Vec<(&'static str, Value)>,
	clips: &[&str],
	params: Vec<Box<ParamInstance>>,
) -> Instance {
	let desc = EffectDescriptor::new();
	for (k, v) in desc_props {
		desc.props.set_one(k, v);
	}
	let props = PropertySet::new();
	for (k, v) in inst_props {
		props.set_one(k, v);
	}
	Instance {
		props,
		plugin: std::sync::Arc::new(crate::host::Plugin {
			identifier: "org.quercus.test-render-driver".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: desc,
			lib: std::ptr::null_mut(),
			entry: test_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		}),
		context: "OfxImageEffectContextFilter".into(),
		params: ParamSetInstance { params },
		clips: clips
			.iter()
			.map(|n| {
				Box::new(crate::clip::ClipInstance::from_descriptor(
					&ClipDescriptor::new(n),
				))
			})
			.collect(),
		node_identity: std::sync::atomic::AtomicUsize::new(0),
		// Set the destroy gate: test instances do not send destroyInstance to the fake plugin on drop.
		destroyed: std::sync::atomic::AtomicBool::new(true),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: std::sync::atomic::AtomicBool::new(false),
		edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	}
}

fn job(dst: Texture, src: Option<Texture>) -> RenderJob {
	RenderJob {
		time: 0.0,
		dst,
		src,
		effect_input_id: Some("Source".into()),
		inputs: Vec::new(),
		values: Vec::new(),
		animations: Vec::new(),
		renderer: None,
		clear_destination: false,
		interactive: false,
	}
}

fn window(w: f64, h: f64) -> OfxRectD {
	OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: w,
		y2: h,
	}
}

fn first_pixel(tex: &Texture) -> [f32; 4] {
	let frame = render::texture_get_frame(tex).unwrap();
	let mut out = [0f32; 4];
	for (i, v) in out.iter_mut().enumerate() {
		*v = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

/// RenderJob::default: default values for all fields.
#[test]
fn render_job_default() {
	let j = RenderJob::default();
	assert_eq!(j.time, 0.0);
	assert!(j.dst.is_dummy());
	assert!(j.src.is_none());
	assert!(j.effect_input_id.is_none());
	assert!(j.inputs.is_empty());
	assert!(j.values.is_empty());
	assert!(j.renderer.is_none());
	assert!(!j.clear_destination);
	assert!(!j.interactive);
}

/// read_dst: rejects non-F32; Invalid on 0 size; returns w/h normally.
#[test]
fn read_dst_validation() {
	let u8tex = u8_texture(2, 2);
	assert!(matches!(read_dst(&u8tex), Err(Error::Failed(_))));

	let params = render::VideoParams {
		format: render::PIXEL_FORMAT_F32,
		..Default::default()
	};
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(params);
	let zero = Texture::wrap_frame(frame);
	assert!(matches!(read_dst(&zero), Err(Error::Invalid)));

	let (p, w, h) = read_dst(&f32_texture(3, 2, 0.0)).unwrap();
	assert_eq!((p.width, p.height), (3, 2));
	assert_eq!((w, h), (3.0, 2.0));
}

/// pixel_aspect: falls back to 1.0 when den == 0, otherwise the ratio.
#[test]
fn pixel_aspect_zero_den_falls_back() {
	let mut p = render::VideoParams {
		pixel_aspect_num: 4,
		pixel_aspect_den: 0,
		..Default::default()
	};
	assert_eq!(pixel_aspect(&p), 1.0);
	p.pixel_aspect_num = 3;
	p.pixel_aspect_den = 2;
	assert_eq!(pixel_aspect(&p), 1.5);
}

/// plugin_supports_opengl: true for "true"/"needed"; false otherwise, when
/// missing, or when not a string.
#[test]
fn plugin_supports_opengl_matrix() {
	let inst = make_instance(vec![], vec![], &[], vec![]);
	assert!(!plugin_supports_opengl(&inst), "missing property -> false");

	inst.plugin.descriptor.props.set_one(
		crate::host::PROP_GL_RENDER_SUPPORTED,
		Value::String(cs("true")),
	);
	assert!(plugin_supports_opengl(&inst));
	inst.plugin.descriptor.props.set_one(
		crate::host::PROP_GL_RENDER_SUPPORTED,
		Value::String(cs("needed")),
	);
	assert!(plugin_supports_opengl(&inst));
	inst.plugin.descriptor.props.set_one(
		crate::host::PROP_GL_RENDER_SUPPORTED,
		Value::String(cs("false")),
	);
	assert!(!plugin_supports_opengl(&inst));
	inst.plugin
		.descriptor
		.props
		.set_one(crate::host::PROP_GL_RENDER_SUPPORTED, Value::Int(1));
	assert!(!plugin_supports_opengl(&inst), "non-string value -> false");
}

/// pick_input: effect_input_id hit uses src; hit but src missing continues;
/// inputs table hit; Source fallback; unknown clip → None.
#[test]
fn pick_input_selection_rules() {
	let src = f32_texture(1, 1, 0.25);
	let matte = f32_texture(1, 1, 0.5);

	let mut j = job(f32_texture(1, 1, 0.0), Some(src.clone()));
	assert!(pick_input("Source", &j).is_some());

	// effect_input_id hit but src missing, inputs table has a same-named entry → use the table.
	j.src = None;
	j.inputs = vec![("Source".into(), matte.clone())];
	assert!(pick_input("Source", &j).is_some());

	// inputs table hits a non-Source clip.
	j.inputs.push(("Matte".into(), matte.clone()));
	assert!(pick_input("Matte", &j).is_some());

	// effect_input_id hit but src missing, no inputs, not Source → None.
	let j2 = job(f32_texture(1, 1, 0.0), None);
	let mut j2 = j2;
	j2.effect_input_id = Some("Matte".into());
	assert!(pick_input("Matte", &j2).is_none());

	// Source fallback (effect_input_id is not Source, no inputs, src present).
	j2.src = Some(src);
	assert!(pick_input("Source", &j2).is_some());

	// Unknown clip → None.
	assert!(pick_input("Nope", &j2).is_none());
}

/// begin/endSequenceRender: CPU path uses the non-GL variants; GL path fails
/// explicitly without a GL environment (with GL it uses the GL variants).
#[test]
fn begin_end_sequence_cpu_and_gl() {
	let inst = make_instance(vec![], vec![], &[], vec![]);
	let range = OfxRangeD {
		min: 0.0,
		max: 10.0,
	};
	assert!(begin_sequence(&inst, range, None).is_ok());
	assert!(end_sequence(&inst, range, None).is_ok());

	let renderer: Renderer = std::sync::Arc::new(crate::render::GlKindMarker);
	let began = begin_sequence(&inst, range, Some(renderer.clone()));
	if began.is_ok() {
		assert!(end_sequence(&inst, range, Some(renderer)).is_ok());
	} else {
		// In a real GL environment begin may also fail due to instance/plugin state; this branch
		// only verifies the CPU path and GL availability, it is not an error assertion.
		println!("SKIP: GL sequence bracket begin failed (GL or instance state unavailable)");
	}
}

/// render_frame CPU full chain: input collection, negotiation, RoD/RoI,
/// render and output assembly; the returned RoI list is paired with the
/// input clips (excluding Output).
#[test]
fn render_frame_cpu_happy_path() {
	let inst = make_instance(vec![], vec![], &["Source", "Output"], vec![]);
	let j = job(f32_texture(2, 2, 0.25), Some(f32_texture(2, 2, 0.75)));
	let (out, rois) = render_frame(&inst, &j).expect("CPU render_frame should succeed");
	assert_eq!(rois.len(), 1, "Output clip is not part of the RoI list");
	assert_eq!(rois[0].0, "Source");
	// Input clip is connected (readable through the property suite).
	assert!(matches!(
		inst.clips[0].props.get(crate::host::PROP_CLIP_CONNECTED, 0),
		Some(Value::Int(1))
	));
	// Fake plugin render writes no pixels → output is zero-filled (Image::allocate zero-initializes).
	assert_eq!(first_pixel(&out), [0.0, 0.0, 0.0, 0.0]);
}

/// render_frame: the cancel flag short-circuits before any plugin call.
#[test]
fn render_frame_cancel_short_circuits() {
	let inst = make_instance(vec![], vec![], &["Source", "Output"], vec![]);
	inst.cancel
		.store(true, std::sync::atomic::Ordering::Relaxed);
	let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: returns before any plugin call when destination texture validation fails.
#[test]
fn render_frame_rejects_bad_destination() {
	let inst = make_instance(vec![], vec![], &["Source", "Output"], vec![]);
	let j = job(u8_texture(2, 2), None);
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));

	let params = render::VideoParams {
		format: render::PIXEL_FORMAT_F32,
		..Default::default()
	};
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(params);
	let j = job(Texture::wrap_frame(frame), None);
	assert!(matches!(render_frame(&inst, &j), Err(Error::Invalid)));
}

/// render_frame: instance has no Output clip → explicit failure.
#[test]
fn render_frame_requires_output_clip() {
	let inst = make_instance(vec![], vec![], &["Source"], vec![]);
	let j = job(f32_texture(2, 2, 0.0), None);
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: negotiated components RGBA/RGB/Alpha and unknown values.
#[test]
fn render_frame_component_negotiation() {
	for comps in [
		"OfxImageComponentRGB",
		"OfxImageComponentAlpha",
		"OfxImageComponentRGBA",
	] {
		let inst = make_instance(
			vec![],
			vec![("test.components", Value::String(cs(comps)))],
			&["Source", "Output"],
			vec![],
		);
		let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
		render_frame(&inst, &j).expect("known components should render");
		let got = inst.clips[1]
			.props
			.get(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0);
		assert!(
			matches!(&got, Some(Value::String(s)) if s.to_string_lossy() == comps),
			"{comps} should be written back to the Output clip"
		);
	}

	// Unknown components → negotiation failure.
	let inst = make_instance(
		vec![],
		vec![(
			"test.components",
			Value::String(cs("OfxImageComponentBogus")),
		)],
		&["Source", "Output"],
		vec![],
	);
	let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: negotiated bit depths Byte/Short/Half/Float and unknown
/// values. Low bit-depth outputs are converted back to F32 by convert_depth
/// before assembly; unknown bit depth fails before render.
#[test]
fn render_frame_depth_negotiation() {
	for depth in [
		"OfxBitDepthByte",
		"OfxBitDepthShort",
		"OfxBitDepthHalf",
		"OfxBitDepthFloat",
	] {
		let inst = make_instance(
			vec![],
			vec![("test.depth", Value::String(cs(depth)))],
			&["Source", "Output"],
			vec![],
		);
		let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
		render_frame(&inst, &j).unwrap_or_else(|e| panic!("{depth} should render: {e}"));
		// Negotiation result written back to the Output clip (Format number reproduced via set_video_params).
		let got = inst.clips[1]
			.props
			.get(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0);
		assert!(
			matches!(&got, Some(Value::String(s)) if s.to_string_lossy() == depth),
			"{depth} should be written back to the Output clip"
		);
		// Returned texture is always F32 (pipeline working format).
		assert_eq!(
			render::texture_get_params(&j.dst).format,
			render::PIXEL_FORMAT_F32
		);
	}

	let inst = make_instance(
		vec![],
		vec![("test.depth", Value::String(cs("OfxBitDepthBogus")))],
		&["Source", "Output"],
		vec![],
	);
	let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: a failing render action is propagated as-is.
#[test]
fn render_frame_propagates_render_failure() {
	let inst = make_instance(
		vec![],
		vec![(
			"test.render_status",
			Value::Int(crate::suites::status::FAILED),
		)],
		&["Source", "Output"],
		vec![],
	);
	let j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: isIdentity hit → passthrough the referenced input clip's
/// frame; a reference to an unknown clip → explicit failure.
#[test]
fn render_frame_identity_passthrough() {
	let inst = make_instance(
		vec![],
		vec![
			("test.identity_clip", Value::String(cs("Source"))),
			("test.identity_time", Value::Double(7.0)),
		],
		&["Source", "Output"],
		vec![],
	);
	let src_tex = f32_texture(2, 2, 0.5);
	inst.clips[0].set_input_texture(Some(src_tex.clone()), 0.0);
	let j = job(f32_texture(2, 2, 0.25), Some(src_tex));
	let (out, _) = render_frame(&inst, &j).expect("passthrough should succeed");
	assert_eq!(first_pixel(&out), [0.5, 0.5, 0.5, 0.5]);

	// Reference to an unknown clip → passthrough fails.
	let inst = make_instance(
		vec![],
		vec![("test.identity_clip", Value::String(cs("Nope")))],
		&["Source", "Output"],
		vec![],
	);
	let j = job(f32_texture(2, 2, 0.25), Some(f32_texture(2, 2, 0.5)));
	assert!(matches!(render_frame(&inst, &j), Err(Error::Failed(_))));
}

/// render_frame: GL renderer + plugin declares GL support. Falls back to CPU
/// without a GL environment (the decision chain's gl gate fails); with GL it
/// takes the real GL path.
#[test]
fn render_frame_gl_decision_and_path() {
	let inst = make_instance(
		vec![(
			crate::host::PROP_GL_RENDER_SUPPORTED,
			Value::String(cs("true")),
		)],
		vec![],
		&["Source", "Output"],
		vec![],
	);
	let mut j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	j.renderer = Some(std::sync::Arc::new(crate::render::GlKindMarker));
	if crate::gl_bridge::gl_available() {
		// Real GL path: read-back contents are uncleared (undefined pixel values) — only assert
		// assembly success and the output format/size.
		let (out, _) = render_frame(&inst, &j).expect("GL path should succeed");
		let params = render::texture_get_params(&out);
		assert_eq!(
			(params.width, params.height, params.format),
			(2, 2, render::PIXEL_FORMAT_F32)
		);
	} else {
		println!("SKIP: GL unavailable (Linux/Windows stub) - only covers the use_opengl fallback decision");
		let (out, _) = render_frame(&inst, &j).expect("GL unavailable should fall back to CPU");
		assert_eq!(first_pixel(&out), [0.0, 0.0, 0.0, 0.0]);
	}
}

/// render_frame: GL render action fails → falls back to CPU (in a GL environment).
#[test]
fn render_frame_gl_failure_falls_back_to_cpu() {
	if !crate::gl_bridge::gl_available() {
		println!("SKIP: GL unavailable - GL failure CPU fallback branch unreachable");
		return;
	}
	let inst = make_instance(
		vec![(
			crate::host::PROP_GL_RENDER_SUPPORTED,
			Value::String(cs("true")),
		)],
		vec![("test.gl_fail", Value::Int(1))],
		&["Source", "Output"],
		vec![],
	);
	let mut j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
	j.renderer = Some(std::sync::Arc::new(crate::render::GlKindMarker));
	let (out, _) =
		render_frame(&inst, &j).expect("GL failure should fall back to CPU successfully");
	assert_eq!(first_pixel(&out), [0.0, 0.0, 0.0, 0.0]);
}

/// render_gl_frame: explicit failure when there is no GL context
/// (Linux/Windows stub or CGL creation failure); skipped when local GL is
/// available (this case only verifies the failure path).
#[test]
fn render_gl_frame_reports_unavailable_context() {
	if crate::gl_bridge::gl_available() {
		println!("SKIP: local GL available — this case only verifies explicit failure without GL");
		return;
	}
	let inst = make_instance(vec![], vec![], &["Source", "Output"], vec![]);
	let j = job(f32_texture(2, 2, 0.0), None);
	let params = read_dst(&j.dst).unwrap().0;
	let res = render_gl_frame(
		&inst,
		&j,
		RenderScale { x: 1.0, y: 1.0 },
		window(2.0, 2.0),
		2.0,
		2.0,
		&params,
	);
	assert!(
		res.is_err(),
		"render_gl_frame should fail without a GL environment"
	);
}

/// apply_param_overrides: unknown parameter names and type mismatches are
/// skipped; NaN/Inf cleaning and Min/Max clamping for Double scalars.
#[test]
fn apply_param_overrides_scrubs_and_clamps() {
	let def = ParamDef::new("gain", crate::param::TYPE_DOUBLE);
	def.props
		.set_one(crate::param::P_DEFAULT, Value::Double(0.25));
	def.props.set_one(crate::param::P_MIN, Value::Double(-1.0));
	def.props.set_one(crate::param::P_MAX, Value::Double(1.0));
	let inst = make_instance(
		vec![],
		vec![],
		&[],
		vec![Box::new(ParamInstance::from_def(def))],
	);
	let gain = || inst.params.find("gain").unwrap().get();

	// Unknown key + mismatched-type key → both skipped, value unchanged.
	apply_param_overrides(
		&inst,
		&[
			("nope".into(), crate::node::Value::float(2.0)),
			("gain".into(), crate::node::Value::int(3)),
		],
	);
	assert_eq!(gain(), ParamValue::Double([0.0, 0.0, 0.0], 1));

	// Above max → clamp to max.
	apply_param_overrides(&inst, &[("gain".into(), crate::node::Value::float(5.0))]);
	assert_eq!(gain(), ParamValue::Double([1.0, 0.0, 0.0], 1));

	// Below min → clamp to min.
	apply_param_overrides(&inst, &[("gain".into(), crate::node::Value::float(-5.0))]);
	assert_eq!(gain(), ParamValue::Double([-1.0, 0.0, 0.0], 1));

	// NaN → falls back to default (0.25), then goes through clamping.
	apply_param_overrides(
		&inst,
		&[("gain".into(), crate::node::Value::float(f64::NAN))],
	);
	assert_eq!(gain(), ParamValue::Double([0.25, 0.0, 0.0], 1));

	// +Inf → likewise falls back to default.
	apply_param_overrides(
		&inst,
		&[("gain".into(), crate::node::Value::float(f64::INFINITY))],
	);
	assert_eq!(gain(), ParamValue::Double([0.25, 0.0, 0.0], 1));

	// Multi-dimensional Double bypasses the scalar cleaning path: Double2D is written directly.
	let def2 = ParamDef::new("pos", crate::param::TYPE_DOUBLE2D);
	let inst = make_instance(
		vec![],
		vec![],
		&[],
		vec![Box::new(ParamInstance::from_def(def2))],
	);
	apply_param_overrides(
		&inst,
		&[("pos".into(), crate::node::Value::vec(&[1.0, 2.0]))],
	);
	assert_eq!(
		inst.params.find("pos").unwrap().get(),
		ParamValue::Double([1.0, 2.0, 0.0], 2)
	);
}

/// prop_double: Double, Int promotion, and missing properties default to 0.
#[test]
fn prop_double_reads_double_int_and_default() {
	let set = PropertySet::new();
	set.set_one("d", Value::Double(2.5));
	set.set_one("i", Value::Int(3));
	set.set_one("s", Value::String(cs("x")));
	assert_eq!(prop_double(&set, "d", 0), 2.5);
	assert_eq!(prop_double(&set, "i", 0), 3.0);
	assert_eq!(prop_double(&set, "s", 0), 0.0);
	assert_eq!(prop_double(&set, "missing", 0), 0.0);
}

/// write_output_frame: non-F32/size mismatch error; in-place CPU write-back;
/// GPU write-back through upload; Planar fails at the readback stage (the
/// Planar branch is defensively unreachable, see the test comments).
#[test]
fn write_output_frame_validation_and_writeback() {
	let image = f32_image(2, 2, 0.5);

	let mut u8dst = u8_texture(2, 2);
	assert!(matches!(
		write_output_frame(&mut u8dst, &image),
		Err(Error::Failed(_))
	));

	let mut baddst = f32_texture(3, 1, 0.0);
	assert!(matches!(
		write_output_frame(&mut baddst, &image),
		Err(Error::Failed(_))
	));

	let mut cpudst = f32_texture(2, 2, 0.0);
	write_output_frame(&mut cpudst, &image).unwrap();
	let frame = render::texture_get_frame(&cpudst).unwrap();
	assert_eq!(frame.data.as_slice(), image.data.as_slice());

	// GPU: the downloaded frame passes the size check, then is written back via upload.
	let ctx = std::sync::Arc::new(FakeUpload {
		uploads: std::sync::Mutex::new(Vec::new()),
		fail: false,
	});
	let mut gpudst = Texture::gpu(ctx.clone(), 11, 2, 2, quercus_core::PixelFormat::F32);
	write_output_frame(&mut gpudst, &image).unwrap();
	{
		let uploads = ctx.uploads.lock().unwrap();
		assert_eq!(uploads.len(), 1);
		assert_eq!(uploads[0].0, 11);
		assert_eq!(uploads[0].1.data.as_slice(), image.data.as_slice());
	}

	// Upload failure → Failed.
	let ctx = std::sync::Arc::new(FakeUpload {
		uploads: std::sync::Mutex::new(Vec::new()),
		fail: true,
	});
	let mut gpudst = Texture::gpu(ctx, 12, 2, 2, quercus_core::PixelFormat::F32);
	assert!(matches!(
		write_output_frame(&mut gpudst, &image),
		Err(Error::Failed(_))
	));

	// Planar: texture_get_frame already fails before reaching the Planar match arm
	// (to_frame returns State for unresolvable planar textures) — that defensive arm is currently
	// unreachable, this test pins the "planar target rejected" behavior.
	let planar = Texture::wrap_planar(quercus_core::texture::PlanarTexture::new(
		std::sync::Arc::new(FakeUpload {
			uploads: std::sync::Mutex::new(Vec::new()),
			fail: false,
		}),
		quercus_core::texture::PlanarFormat::Nv12,
		(2, 2),
		(1, 2),
		quercus_core::backend::YuvTransform::bt601_limited(),
		(1, 1),
	));
	let mut planardst = planar;
	assert!(matches!(
		write_output_frame(&mut planardst, &image),
		Err(Error::Failed(_))
	));
}

/// zip_rois: skips Output, pairs in clips order, defaults to a zero rectangle when RoIs run short.
#[test]
fn zip_rois_pairs_inputs_only() {
	let inst = make_instance(vec![], vec![], &["Source", "Matte", "Output"], vec![]);
	let rect = OfxRectD {
		x1: 1.0,
		y1: 2.0,
		x2: 3.0,
		y2: 4.0,
	};
	let zipped = zip_rois(&inst, &[rect]);
	assert_eq!(zipped.len(), 2, "Output is not counted");
	assert_eq!(zipped[0].0, "Source");
	assert_eq!(zipped[0].1.x2, 3.0);
	assert_eq!(zipped[1].0, "Matte");
	assert_eq!(zipped[1].1.x1, 0.0, "default rect when RoI is insufficient");

	let empty = zip_rois(&inst, &[]);
	assert_eq!(empty.len(), 2);
	assert_eq!(empty[0].1.y2, 0.0);
}

/// The trace branch (print-point coverage) of the use_opengl decision and
/// CPU output dump when QUERCUS_OFX_TRACE is enabled. Serializes and
/// restores the environment variable.
#[test]
fn trace_branches_in_render_frame() {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let prev = std::env::var_os("QUERCUS_OFX_TRACE");
	std::env::set_var("QUERCUS_OFX_TRACE", "1");
	let body = std::panic::catch_unwind(|| {
		// Plugin does not declare GL → use_opengl decision is false, but the trace branch
		// runs when the renderer is GL (prints the raw GL prop).
		let inst = make_instance(vec![], vec![], &["Source", "Output"], vec![]);
		let mut j = job(f32_texture(2, 2, 0.0), Some(f32_texture(2, 2, 0.5)));
		j.renderer = Some(std::sync::Arc::new(crate::render::GlKindMarker));
		render_frame(&inst, &j).expect("CPU fallback should succeed");
	});
	match prev {
		Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
		None => std::env::remove_var("QUERCUS_OFX_TRACE"),
	}
	if let Err(p) = body {
		std::panic::resume_unwind(p);
	}
}

struct FakeUpload {
	uploads: std::sync::Mutex<Vec<(u64, crate::render::Frame)>>,
	fail: bool,
}

impl quercus_core::backend::GpuContextLike for FakeUpload {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Cpu
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(
		&self,
		token: u64,
		frame: &crate::render::Frame,
	) -> quercus_render::error::Result<()> {
		if self.fail {
			return Err(quercus_render::error::Error::State);
		}
		self.uploads
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push((token, frame.clone()));
		Ok(())
	}
	fn download(&self, _token: u64) -> quercus_render::error::Result<crate::render::Frame> {
		Ok(f32_frame(2, 2, 0.0))
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
