use super::*;
use crate::descriptor::{ClipDescriptor, EffectDescriptor};
use crate::image::{BitDepth, Components, Image};
use crate::param::{ChangeReason, ParamDef, ParamInstance};
use crate::property::Value as PropValue;
use crate::suites::status;
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

fn action_is(action: *const c_char, name: &str) -> bool {
	!action.is_null() && unsafe { std::ffi::CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
}

fn prop_string(props: &PropertySet, name: &str) -> Option<String> {
	match props.get(name, 0) {
		Some(PropValue::String(s)) => Some(s.to_string_lossy().into_owned()),
		_ => None,
	}
}

unsafe extern "C" fn entry_ok(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::OK
}

unsafe extern "C" fn entry_default(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::REPLY_DEFAULT
}

unsafe extern "C" fn entry_err(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::ERR_FATAL
}

fn param_set(defs: Vec<ParamDef>) -> ParamSetInstance {
	ParamSetInstance {
		params: defs
			.into_iter()
			.map(|d| Box::new(ParamInstance::from_def(d)))
			.collect(),
	}
}

fn clip(name: &str) -> ClipInstance {
	ClipInstance::from_descriptor(&ClipDescriptor::new(name))
}

fn make_instance_with(
	entry: crate::host::EntryPoint,
	params: ParamSetInstance,
	clips: Vec<ClipInstance>,
) -> Arc<Instance> {
	let plugin = Arc::new(Plugin {
		identifier: "test.instance".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::new(),
		contexts: vec![],
		descriptor: EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: AtomicBool::new(false),
	});
	Arc::new(Instance {
		props: PropertySet::new(),
		plugin,
		context: "OfxImageEffectContextFilter".into(),
		params,
		clips: clips.into_iter().map(Box::new).collect(),
		node_identity: AtomicUsize::new(0),
		destroyed: AtomicBool::new(false),
		sequence_range: Mutex::new(None),
		progress_cb: Mutex::new(None),
		cancel: AtomicBool::new(false),
		edit: Mutex::new(EditTransaction::new()),
		render_lock: Mutex::new(()),
		interact: Mutex::new(None),
	})
}

fn make_instance(entry: crate::host::EntryPoint, clips: Vec<ClipInstance>) -> Arc<Instance> {
	make_instance_with(entry, param_set(vec![]), clips)
}

fn scale() -> RenderScale {
	RenderScale { x: 1.0, y: 1.0 }
}

fn window() -> OfxRectD {
	OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: 2.0,
		y2: 2.0,
	}
}

fn out_image() -> Arc<Image> {
	Arc::new(Image::allocate(BitDepth::Float, Components::Rgba, window()))
}

// ---- parameter edit transaction --------------------------------------

#[test]
fn edit_transaction_nesting_and_multi() {
	use quercus_undo::undocommand::UndoCommand;
	let inst = make_instance(entry_ok, vec![]);
	assert!(!inst.in_edit());

	// Nested begin: the first entry resets the transaction state.
	inst.edit_begin();
	inst.edit_begin();
	inst.submit_undo_command(UndoCommand::from_closures(|| {}, || {}), "one");
	{
		let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
		assert_eq!(e.depth, 2);
		assert_eq!(e.param_count, 1);
		assert_eq!(e.first_label, "one");
		assert_eq!(e.multi.as_ref().expect("multi").multi_child_count(), 1);
	}
	inst.submit_undo_command(UndoCommand::from_closures(|| {}, || {}), "two");
	{
		let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
		assert_eq!(e.multi.as_ref().expect("multi").multi_child_count(), 2);
		assert_eq!(e.first_label, "one", "first label preserved");
	}
	// Inner end: still inside the transaction, the multi is kept.
	inst.edit_end();
	assert!(inst.in_edit());
	// Outer end: redo and clear.
	inst.edit_end();
	assert!(!inst.in_edit());
	{
		let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
		assert!(e.multi.is_none());
		assert_eq!(e.param_count, 0);
		assert!(e.first_label.is_empty());
	}
	// end outside a transaction: depth does not go out of bounds.
	inst.edit_end();

	// A new transaction counts afresh (the first entry takes the reset branch).
	inst.edit_begin();
	{
		let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
		assert!(e.multi.is_none());
		assert_eq!(e.param_count, 0);
	}
	inst.edit_end();

	// Submit outside a transaction → redo immediately.
	let ran = Arc::new(AtomicBool::new(false));
	let flag = ran.clone();
	inst.submit_undo_command(
		UndoCommand::from_closures(
			move || flag.store(true, std::sync::atomic::Ordering::Relaxed),
			|| {},
		),
		"solo",
	);
	assert!(ran.load(std::sync::atomic::Ordering::Relaxed));
}

// ---- clip preferences -------------------------------------------------

unsafe extern "C" fn entry_prefs_wrong_types(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_CLIP_PREFERENCES) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.define(
			&crate::host::clip_pref_prop(crate::host::CLIP_PREF_COMPONENTS, "Output"),
			vec![PropValue::Int(1)],
		);
		out.define(
			&crate::host::clip_pref_prop(crate::host::CLIP_PREF_DEPTH, "Output"),
			vec![PropValue::Int(1)],
		);
		out.define(
			&crate::host::clip_pref_prop(crate::host::CLIP_PREF_PAR, "Output"),
			vec![PropValue::String(cs("x"))],
		);
		out.set_one(crate::host::PROP_FRAME_RATE, PropValue::Int(30));
		out.set_one(crate::host::PROP_FIELD_ORDER, PropValue::Int(1));
	}
	status::OK
}

#[test]
fn clip_preferences_default_and_reply_default_paths() {
	let inst = make_instance(entry_ok, vec![clip("Source"), clip("Output")]);
	let prefs = inst.get_clip_preferences().expect("default negotiation");
	assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
	assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
	assert_eq!(prefs.pixel_aspect_ratio, 1.0);
	// The plugin writes nothing back → the out args preset frame rate 1.0.
	assert_eq!(prefs.frame_rate, 1.0);
	assert_eq!(prefs.field, "");
	// per-clip feed-back (components/bit depth/PAR).
	let source = inst.clips.iter().find(|c| c.name == "Source").unwrap();
	assert_eq!(
		prop_string(&source.props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS).as_deref(),
		Some("OfxImageComponentRGBA")
	);
	assert_eq!(
		prop_string(&source.props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH).as_deref(),
		Some("OfxBitDepthFloat")
	);
	assert!(matches!(
		source.props.get("OfxImagePropPixelAspectRatio", 0),
		Some(PropValue::Double(d)) if d == 1.0
	));

	// ReplyDefault is accepted as well.
	let inst = make_instance(entry_default, vec![clip("Output")]);
	assert!(inst.get_clip_preferences().is_ok());
	// Failure status → Err.
	let inst = make_instance(entry_err, vec![clip("Output")]);
	assert!(inst.get_clip_preferences().is_err());
}

#[test]
fn clip_preferences_wrong_out_types_fall_back() {
	let inst = make_instance(entry_prefs_wrong_types, vec![clip("Output")]);
	let prefs = inst
		.get_clip_preferences()
		.expect("wrong types should fall back");
	assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
	assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
	assert_eq!(prefs.pixel_aspect_ratio, 1.0);
	assert_eq!(prefs.frame_rate, 24.0);
	assert_eq!(prefs.field, "");
}

// ---- region of definition / interest ---------------------------------

unsafe extern "C" fn entry_rod_ok(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_ROD) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.define(
			crate::host::PROP_ROD,
			vec![
				PropValue::Double(1.0),
				PropValue::Double(2.0),
				PropValue::Double(3.0),
				PropValue::Double(4.0),
			],
		);
	}
	status::OK
}

unsafe extern "C" fn entry_roi_source(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_ROI) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.define(
			"OfxImageEffectPropRegionOfInterest_Source",
			vec![
				PropValue::Double(1.0),
				PropValue::Double(2.0),
				PropValue::Double(3.0),
				PropValue::Double(4.0),
			],
		);
	}
	status::OK
}

#[test]
fn region_of_definition_and_roi_paths() {
	let inst = make_instance(entry_rod_ok, vec![]);
	let rod = inst.get_region_of_definition(0.5, scale()).expect("rod");
	assert_eq!((rod.x1, rod.y1, rod.x2, rod.y2), (1.0, 2.0, 3.0, 4.0));
	let inst = make_instance(entry_err, vec![]);
	assert!(inst.get_region_of_definition(0.0, scale()).is_err());

	let inst = make_instance(entry_roi_source, vec![clip("Source"), clip("Output")]);
	let region = OfxRectD {
		x1: 10.0,
		y1: 11.0,
		x2: 12.0,
		y2: 13.0,
	};
	let rois = inst
		.get_regions_of_interest(0.0, scale(), region)
		.expect("rois");
	assert_eq!(rois.len(), 2);
	assert_eq!(
		(rois[0].x1, rois[0].y1, rois[0].x2, rois[0].y2),
		(1.0, 2.0, 3.0, 4.0)
	);
	// A clip the plugin does not write back keeps the preset zero values from
	// the out args (per-clip predefinition).
	assert_eq!(
		(rois[1].x1, rois[1].y1, rois[1].x2, rois[1].y2),
		(0.0, 0.0, 0.0, 0.0)
	);
	let inst = make_instance(entry_err, vec![clip("Source")]);
	assert!(inst.get_regions_of_interest(0.0, scale(), region).is_err());
}

// ---- isIdentity -------------------------------------------------------

unsafe extern "C" fn entry_identity_source(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_IS_IDENTITY) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.set_one(
			crate::host::PROP_IS_IDENTITY,
			PropValue::String(cs("Source")),
		);
		out.set_one(crate::host::PROP_TIME, PropValue::Double(2.5));
	}
	status::OK
}

unsafe extern "C" fn entry_identity_time_int(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_IS_IDENTITY) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.set_one(
			crate::host::PROP_IS_IDENTITY,
			PropValue::String(cs("Source")),
		);
		out.set_one(crate::host::PROP_TIME, PropValue::Int(3));
	}
	status::OK
}

#[test]
fn is_identity_paths() {
	let inst = make_instance(entry_identity_source, vec![]);
	let (time, name) = inst.is_identity(0.0).unwrap().expect("identity");
	assert_eq!(time, 2.5);
	assert_eq!(name, "Source");

	// Pass-through time is not a Double → falls back to the input time.
	let inst = make_instance(entry_identity_time_int, vec![]);
	let (time, name) = inst.is_identity(1.25).unwrap().expect("identity");
	assert_eq!(time, 1.25);
	assert_eq!(name, "Source");

	// Empty identity → None.
	let inst = make_instance(entry_ok, vec![]);
	assert!(inst.is_identity(0.0).unwrap().is_none());
	// Plugin failure → treated as not identity (not fatal).
	let inst = make_instance(entry_err, vec![]);
	assert!(inst.is_identity(0.0).unwrap().is_none());
}

// ---- render / render_gl ----------------------------------------------

unsafe extern "C" fn progress_cb(_: f64, _: *mut c_void) -> c_int {
	0
}

struct NoopUi;
impl crate::progress::UiProgressReporter for NoopUi {
	fn update(&mut self, _progress: f64) -> bool {
		true
	}
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

fn fake_renderer() -> crate::render::Renderer {
	Arc::new(FakeGpu)
}

#[test]
fn render_paths_and_progress_reporters() {
	// facade cancel flag → the entry short-circuits immediately.
	let inst = make_instance(entry_err, vec![]);
	inst.cancel
		.store(true, std::sync::atomic::Ordering::Relaxed);
	assert!(inst.render(0.0, scale(), window(), out_image()).is_err());

	// facade progress-callback branch.
	let inst = make_instance(entry_ok, vec![]);
	*inst.progress_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some((progress_cb, 0));
	inst.render(0.0, scale(), window(), out_image())
		.expect("render with callback");

	// UI factory branch (no callback + factory registered).
	static FACTORY_LOCK: Mutex<()> = Mutex::new(());
	let _g = FACTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	crate::progress::set_reporter_factory(Some(Arc::new(|_, _| Box::new(NoopUi))));
	let inst = make_instance(entry_ok, vec![]);
	let result = inst.render(0.0, scale(), window(), out_image());
	crate::progress::set_reporter_factory(None);
	result.expect("render with UI factory");

	// Plugin render fails → Err.
	let inst = make_instance(entry_err, vec![]);
	assert!(inst.render(0.0, scale(), window(), out_image()).is_err());

	// in-args: both GL switch states.
	let gl = Instance::render_in_args(0.0, scale(), window(), true);
	assert!(matches!(
		gl.get(crate::host::PROP_GL_ENABLED, 0),
		Some(PropValue::Int(1))
	));
	let cpu = Instance::render_in_args(0.0, scale(), window(), false);
	assert!(cpu.get(crate::host::PROP_GL_ENABLED, 0).is_none());
}

unsafe extern "C" fn entry_gl_attach_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GL_CONTEXT_ATTACHED) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

unsafe extern "C" fn entry_gl_render_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_RENDER) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

#[test]
fn render_gl_paths() {
	// Success: attach → render → detach; empty bit-depth list → default F32.
	let inst = make_instance(entry_ok, vec![]);
	inst.render_gl(
		0.0,
		scale(),
		window(),
		fake_renderer(),
		crate::render::Texture::dummy(),
		Some(7),
	)
	.expect("GL render");
	assert!(crate::suites::gl_ctx().is_none(), "GL context cleaned up");

	// Plugin declares an F32 bit-depth list → pick takes the hit branch.
	let inst = make_instance(entry_ok, vec![]);
	inst.plugin.descriptor.props.define(
		crate::host::PROP_GL_PIXEL_DEPTH,
		vec![PropValue::String(cs("OfxBitDepthFloat"))],
	);
	inst.render_gl(
		0.0,
		scale(),
		window(),
		fake_renderer(),
		crate::render::Texture::dummy(),
		None,
	)
	.expect("GL render with declared bit depth");

	// attach fails → cleanup + Err.
	let inst = make_instance(entry_gl_attach_fails, vec![]);
	assert!(inst
		.render_gl(
			0.0,
			scale(),
			window(),
			fake_renderer(),
			crate::render::Texture::dummy(),
			None,
		)
		.is_err());
	assert!(crate::suites::gl_ctx().is_none());

	// render fails → detach is still sent + Err.
	let inst = make_instance(entry_gl_render_fails, vec![]);
	assert!(inst
		.render_gl(
			0.0,
			scale(),
			window(),
			fake_renderer(),
			crate::render::Texture::dummy(),
			None,
		)
		.is_err());

	// Cancel → the entry short-circuits immediately.
	let inst = make_instance(entry_ok, vec![]);
	inst.cancel
		.store(true, std::sync::atomic::Ordering::Relaxed);
	assert!(inst
		.render_gl(
			0.0,
			scale(),
			window(),
			fake_renderer(),
			crate::render::Texture::dummy(),
			None,
		)
		.is_err());
}

// ---- output colourspace ----------------------------------------------

unsafe extern "C" fn entry_colourspace_value(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.set_one(
			crate::host::PROP_CLIP_COLOURSPACE,
			PropValue::String(cs("ACEScg")),
		);
	}
	status::OK
}

unsafe extern "C" fn entry_colourspace_cross(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.set_one(
			crate::host::PROP_CLIP_COLOURSPACE,
			PropValue::String(cs("OfxColourspace_Source")),
		);
	}
	status::OK
}

unsafe extern "C" fn entry_colourspace_int(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	out_args: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
		let out = unsafe { &*(out_args as *const PropertySet) };
		out.set_one(crate::host::PROP_CLIP_COLOURSPACE, PropValue::Int(1));
	}
	status::OK
}

#[test]
fn output_colourspace_paths() {
	// OK + a direct value.
	let inst = make_instance(
		entry_colourspace_value,
		vec![clip("Source"), clip("Output")],
	);
	assert_eq!(
		inst.get_output_colourspace(&["sRGB".to_string()]).unwrap(),
		"ACEScg"
	);
	// Write back to the Output clip.
	inst.set_output_colourspace("sRGB");
	assert_eq!(
		prop_string(
			&inst
				.clips
				.iter()
				.find(|c| c.name == "Output")
				.unwrap()
				.props,
			crate::host::PROP_CLIP_COLOURSPACE
		)
		.as_deref(),
		Some("sRGB")
	);

	// A cross-reference resolves to the referenced clip's actual colour space.
	let inst = make_instance(
		entry_colourspace_cross,
		vec![clip("Source"), clip("Output")],
	);
	let expected = prop_string(&inst.clips[0].props, crate::host::PROP_CLIP_COLOURSPACE).unwrap();
	assert_eq!(inst.get_output_colourspace(&[]).unwrap(), expected);
	// The referenced clip's colour-space type mismatches → the reference string
	// is returned unchanged.
	inst.clips[0]
		.props
		.set_one(crate::host::PROP_CLIP_COLOURSPACE, PropValue::Int(1));
	assert_eq!(
		inst.get_output_colourspace(&[]).unwrap(),
		"OfxColourspace_Source"
	);
	// Unknown referenced clip / non-reference returned unchanged.
	assert_eq!(
		inst.resolve_colourspace("OfxColourspace_Nope".into()),
		"OfxColourspace_Nope"
	);
	assert_eq!(inst.resolve_colourspace("sRGB".into()), "sRGB");

	// OK but empty value → Err; OK but not a String → Err.
	let inst = make_instance(entry_ok, vec![clip("Output")]);
	assert!(inst.get_output_colourspace(&[]).is_err());
	let inst = make_instance(entry_colourspace_int, vec![clip("Output")]);
	assert!(inst.get_output_colourspace(&[]).is_err());

	// ReplyDefault: the first input clip's colour space.
	let inst = make_instance(entry_default, vec![clip("Source"), clip("Output")]);
	let expected = prop_string(&inst.clips[0].props, crate::host::PROP_CLIP_COLOURSPACE).unwrap();
	assert_eq!(inst.get_output_colourspace(&[]).unwrap(), expected);
	// No input clip → empty string.
	let inst = make_instance(entry_default, vec![clip("Output")]);
	assert_eq!(inst.get_output_colourspace(&[]).unwrap(), "");
	// The input clip has no colourspace property → empty string.
	let inst = make_instance(entry_default, vec![clip("Source")]);
	inst.clips[0]
		.props
		.remove(crate::host::PROP_CLIP_COLOURSPACE);
	assert_eq!(inst.get_output_colourspace(&[]).unwrap(), "");

	// Any other status code → Err.
	let inst = make_instance(entry_err, vec![clip("Output")]);
	assert!(inst.get_output_colourspace(&[]).is_err());

	// No Output clip → set_output_colourspace is a no-op.
	let inst = make_instance(entry_ok, vec![clip("Source")]);
	inst.set_output_colourspace("x");
}

// ---- sequence render --------------------------------------------------

unsafe extern "C" fn entry_begin_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_BEGIN_SEQUENCE) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

unsafe extern "C" fn entry_end_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_END_SEQUENCE) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

#[test]
fn sequence_render_paths() {
	let range = OfxRangeD {
		min: 1.0,
		max: 10.0,
	};

	let inst = make_instance(entry_ok, vec![]);
	inst.begin_sequence_render(range).expect("begin");
	assert_eq!(
		inst.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.map(|r| r.min),
		Some(1.0)
	);
	inst.end_sequence_render(range).expect("end");
	assert!(inst
		.sequence_range
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.is_none());

	// GL variant (in args carry OpenGLEnabled).
	let inst = make_instance(entry_ok, vec![]);
	inst.begin_sequence_render_gl(range).expect("gl begin");
	inst.end_sequence_render_gl(range).expect("gl end");

	// Failure status → Err (end still clears the range).
	let inst = make_instance(entry_begin_fails, vec![]);
	assert!(inst.begin_sequence_render(range).is_err());
	let inst = make_instance(entry_end_fails, vec![]);
	assert!(inst.end_sequence_render(range).is_err());
	assert!(inst
		.sequence_range
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.is_none());

	// in-args: both GL switch states.
	let gl = Instance::sequence_in_args(range, true);
	assert!(matches!(
		gl.get(crate::host::PROP_GL_ENABLED, 0),
		Some(PropValue::Int(1))
	));
	let cpu = Instance::sequence_in_args(range, false);
	assert!(cpu.get(crate::host::PROP_GL_ENABLED, 0).is_none());
}

// ---- instanceChanged / interact --------------------------------------

static CHANGED_REASONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

unsafe extern "C" fn entry_record_reason(
	action: *const c_char,
	_: *const c_void,
	in_args: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, crate::host::ACTION_INSTANCE_CHANGED) {
		let props = unsafe { &*(in_args as *const PropertySet) };
		if let Some(PropValue::String(s)) = props.get(crate::host::PROP_CHANGE_REASON, 0) {
			CHANGED_REASONS
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.push(s.to_string_lossy().into_owned());
		}
	}
	status::OK
}

#[test]
fn instance_changed_reasons_and_failure() {
	CHANGED_REASONS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
	let inst = make_instance(entry_record_reason, vec![]);
	inst.instance_changed("gain", ChangeReason::UserEdited, 0.0, scale())
		.unwrap();
	inst.instance_changed("gain", ChangeReason::PluginEdited, 0.0, scale())
		.unwrap();
	inst.instance_changed("gain", ChangeReason::TimeChanged, 0.0, scale())
		.unwrap();
	assert_eq!(
		*CHANGED_REASONS.lock().unwrap_or_else(|e| e.into_inner()),
		vec![
			"OfxChangeUserEdited".to_string(),
			"OfxChangePluginEdited".to_string(),
			"OfxChangeTime".to_string(),
		]
	);

	let inst = make_instance(entry_err, vec![]);
	assert!(inst
		.instance_changed("gain", ChangeReason::UserEdited, 0.0, scale())
		.is_err());
}

unsafe extern "C" fn overlay_default(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::REPLY_DEFAULT
}

#[test]
fn new_interact_and_describe_paths() {
	// Not created → interact None / describe BadHandle.
	let inst = make_instance(entry_ok, vec![]);
	assert!(inst.interact().is_none());
	assert_eq!(inst.describe_interact(), status::ERR_BAD_HANDLE);

	// ReplyDefault with no overlay declared → no interact.
	let inst = make_instance(entry_default, vec![]);
	assert!(inst.new_interact().is_none());

	// The main entry accepts NewInteract → creates; describe goes through the
	// main entry.
	let inst = make_instance(entry_ok, vec![]);
	let interact = inst.new_interact().expect("new_interact");
	assert_eq!(inst.describe_interact(), status::OK);
	assert!(inst.interact().is_some());
	assert_eq!(Arc::strong_count(&interact), 2);

	// Overlay entry declared + ReplyDefault → still creates (official
	// describe/create sequence).
	let inst = make_instance(overlay_default, vec![]);
	inst.plugin.descriptor.props.set_one(
		crate::host::PROP_OVERLAY_INTERACT_V2,
		PropValue::Pointer(overlay_default as *const () as *mut c_void),
	);
	assert!(inst.new_interact().is_some());
	assert_eq!(inst.describe_interact(), status::REPLY_DEFAULT);
}

// ---- read_rect --------------------------------------------------------

#[test]
fn read_rect_rejects_wrong_types_and_missing() {
	let props = PropertySet::new();
	assert!(read_rect(&props, "missing").is_none());
	props.define(
		"r",
		vec![
			PropValue::Int(1),
			PropValue::Double(2.0),
			PropValue::Double(3.0),
			PropValue::Double(4.0),
		],
	);
	assert!(read_rect(&props, "r").is_none());
	props.define(
		"r",
		vec![
			PropValue::Double(1.0),
			PropValue::Int(2),
			PropValue::Double(3.0),
			PropValue::Double(4.0),
		],
	);
	assert!(read_rect(&props, "r").is_none());
	props.define(
		"r",
		vec![
			PropValue::Double(1.0),
			PropValue::Double(2.0),
			PropValue::Int(3),
			PropValue::Double(4.0),
		],
	);
	assert!(read_rect(&props, "r").is_none());
	props.define(
		"r",
		vec![
			PropValue::Double(1.0),
			PropValue::Double(2.0),
			PropValue::Double(3.0),
			PropValue::Int(4),
		],
	);
	assert!(read_rect(&props, "r").is_none());
	props.define(
		"r",
		vec![
			PropValue::Double(1.0),
			PropValue::Double(2.0),
			PropValue::Double(3.0),
			PropValue::Double(4.0),
		],
	);
	assert_eq!(read_rect(&props, "r").map(|r| r.x2), Some(3.0));
}

// ---- trace probes -----------------------------------------------------

/// Trace-probe branches enabled by QUERCUS_OFX_TRACE (logging for clip
/// prefs/RoD/RoI/isIdentity and the isIdentity failure path).
///
/// The probe's only effect besides the normal action result is a trace
/// line on stderr (no test-installable sink), so each call is pinned by
/// its return value; the env override is restored before the assertions
/// run so a failing expectation cannot leak `QUERCUS_OFX_TRACE` into the
/// next serialized test.
#[test]
fn trace_probe_branches() {
	static TRACE_LOCK: Mutex<()> = Mutex::new(());
	let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let prev = std::env::var_os("QUERCUS_OFX_TRACE");
	std::env::set_var("QUERCUS_OFX_TRACE", "1");

	let inst = make_instance(entry_ok, vec![clip("Source"), clip("Output")]);
	let prefs = inst.get_clip_preferences();
	let rod = inst.get_region_of_definition(0.0, scale());
	let region = OfxRectD {
		x1: 1.0,
		y1: 2.0,
		x2: 3.0,
		y2: 4.0,
	};
	let rois = inst.get_regions_of_interest(0.0, scale(), region);
	let identity = inst.is_identity(0.0);
	let bad = make_instance(entry_err, vec![]);
	let bad_identity = bad.is_identity(0.0);

	match prev {
		Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
		None => std::env::remove_var("QUERCUS_OFX_TRACE"),
	}

	// entry_ok writes nothing back: every negotiated value stays at the
	// host pre-set default, which is what the probe branch observes.
	let prefs = prefs.expect("entry_ok answers OK");
	assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
	assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
	assert_eq!(prefs.pixel_aspect_ratio, 1.0);
	assert_eq!(prefs.frame_rate, 1.0);
	assert_eq!(prefs.field, "");

	let rod = rod.expect("getRoD answers OK");
	assert_eq!(
		(rod.x1, rod.y1, rod.x2, rod.y2),
		(0.0, 0.0, 0.0, 0.0),
		"an untouched pre-defined RoD stays at the zero default"
	);

	// The plugin leaves the pre-defined per-clip ROI properties alone:
	// the result is those zero defaults, not the input region.
	let rois = rois.expect("getRoI answers OK");
	assert_eq!(rois.len(), 2, "one RoI per clip");
	assert!(
		rois.iter()
			.all(|r| (r.x1, r.y1, r.x2, r.y2) == (0.0, 0.0, 0.0, 0.0)),
		"untouched RoIs stay at the pre-defined zeros: {rois:?}"
	);

	// entry_ok leaves kOfxImageEffectPropIsIdentity empty: an OK action
	// without an identity declaration means "not an identity".
	assert!(
		identity.expect("isIdentity answers OK").is_none(),
		"an empty identity property means render"
	);

	// entry_err fails the action: the failure is logged and answered
	// None (never propagated as an error).
	assert!(
		bad_identity
			.expect("failed isIdentity is tolerated")
			.is_none(),
		"a failed isIdentity falls back to render"
	);
}
