use super::*;

/// Records what the mock plugin entry saw, so the push-button test can
/// assert the kOfxActionInstanceChanged routing and its inArgs.
static PUSH_ENTRY_CALLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// PUSH_ENTRY_CALLS is a process-level static: tests that touch it must be serialised.
static PUSH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Mock plugin entry: instanceChanged records the action and change reason;
/// other actions/an empty action silently succeed. Call it directly to cover
/// the missing/mismatched-reason branches (push_button_clicked only writes a
/// String reason).
unsafe extern "C" fn recording_entry(
	action: *const c_char,
	_: *const c_void,
	in_args: *mut c_void,
	_: *mut c_void,
) -> i32 {
	if !action.is_null() {
		let action = unsafe {
			std::ffi::CStr::from_ptr(action)
				.to_string_lossy()
				.into_owned()
		};
		if action == crate::host::ACTION_INSTANCE_CHANGED {
			let mut calls = PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner());
			calls.push(action);
			// The instanceChanged inArgs contract (ofxCore.h:405-435):
			// kOfxPropChangeReason = kOfxChangeUserEdited.
			let props = unsafe { &*(in_args as *const crate::property::PropertySet) };
			let reason = props.get(crate::host::PROP_CHANGE_REASON, 0);
			let reason = match reason {
				Some(crate::property::Value::String(s)) => s.to_string_lossy().into_owned(),
				_ => String::new(),
			};
			calls.push(reason);
		}
	}
	0
}

#[test]
fn input_type_table_covers_all_ofx_kinds() {
	assert_eq!(input_type_for(ofx::TYPE_INTEGER), Some(ValueType::Int));
	assert_eq!(input_type_for(ofx::TYPE_DOUBLE), Some(ValueType::Float));
	assert_eq!(input_type_for(ofx::TYPE_BOOLEAN), Some(ValueType::Boolean));
	assert_eq!(input_type_for(ofx::TYPE_STRING), Some(ValueType::Text));
	assert_eq!(input_type_for(ofx::TYPE_RGB), Some(ValueType::Color));
	assert_eq!(input_type_for(ofx::TYPE_RGBA), Some(ValueType::Color));
	assert_eq!(input_type_for(ofx::TYPE_CHOICE), Some(ValueType::Combo));
	assert_eq!(input_type_for(ofx::TYPE_DOUBLE2D), Some(ValueType::Vec2));
	assert_eq!(input_type_for(ofx::TYPE_INTEGER2D), Some(ValueType::Vec2));
	assert_eq!(input_type_for(ofx::TYPE_DOUBLE3D), Some(ValueType::Vec3));
	assert_eq!(input_type_for(ofx::TYPE_INTEGER3D), Some(ValueType::Vec3));
	assert_eq!(
		input_type_for(ofx::TYPE_STRCHOICE),
		Some(ValueType::StrCombo)
	);
	assert_eq!(input_type_for(ofx::TYPE_BYTES), Some(ValueType::Binary));
	assert_eq!(input_type_for(ofx::TYPE_CUSTOM), Some(ValueType::Binary));
	assert_eq!(
		input_type_for(ofx::TYPE_PUSHBUTTON),
		Some(ValueType::PushButton)
	);
	// 容器与未知类型：跳过。
	assert_eq!(input_type_for(ofx::TYPE_GROUP), None);
	assert_eq!(input_type_for(ofx::TYPE_PAGE), None);
	assert_eq!(input_type_for("OfxParamTypeBogus"), None);
	// parametric → Parametric（值 = 曲线 JSON 文本）。
	assert_eq!(
		input_type_for(ofx::TYPE_PARAMETRIC),
		Some(ValueType::Parametric)
	);
}

#[test]
fn color_semantic_rules() {
	fn def_with(label: &str, ofx_type: &str) -> ParamDef {
		let def = ParamDef {
			props: PropertySet::new(),
			name: "p".into(),
			ofx_type: ofx_type.into(),
			default: ParamValue::Container,
		};
		def.props.set_one(
			ofx::PROP_LABEL,
			PropValue::String(CString::new(label).unwrap()),
		);
		def
	}
	let groups = HashMap::new();
	// 非颜色类型恒 "color"。
	assert_eq!(
		deduce_color_semantic(&def_with("Anything", ofx::TYPE_DOUBLE), &groups),
		"color"
	);
	// 规则 1：颜色关键词。
	assert_eq!(
		deduce_color_semantic(&def_with("Tint Color", ofx::TYPE_RGB), &groups),
		"color"
	);
	// 规则 2：标量关键词。
	assert_eq!(
		deduce_color_semantic(&def_with("Gamma Adjust", ofx::TYPE_RGBA), &groups),
		"scalar"
	);
}

#[test]
fn color_semantic_display_range_rule() {
	let def = ParamDef {
		props: PropertySet::new(),
		name: "rgb".into(),
		ofx_type: ofx::TYPE_RGB.into(),
		default: ParamValue::Container,
	};
	// 默认 (0,0,0) 全相等前先被规则 3 拦截：display max 越界。
	def.props
		.set_one(ofx::P_DISPLAY_MAX, PropValue::Double(2.0));
	let groups = HashMap::new();
	assert_eq!(deduce_color_semantic(&def, &groups), "scalar");
}

#[test]
fn clip_labels_special_case_names() {
	assert_eq!(clip_label_for_name("Source", None), "Source");
	assert_eq!(clip_label_for_name("SourceFrom", None), "From");
	assert_eq!(clip_label_for_name("SourceTo", None), "To");
	assert_eq!(clip_label_for_name("Overlay", None), "Overlay");
	let props = PropertySet::new();
	props.set_one(
		ofx::PROP_LABEL,
		PropValue::String(CString::new("Matte").unwrap()),
	);
	assert_eq!(clip_label_for_name("Overlay", Some(&props)), "Matte");
	// 有 props 但 label 为空（或类型不符）→ 回退 clip 名。
	let no_label = PropertySet::new();
	assert_eq!(clip_label_for_name("Overlay", Some(&no_label)), "Overlay");
	no_label.set_one(ofx::PROP_LABEL, PropValue::Int(1));
	assert_eq!(clip_label_for_name("Overlay", Some(&no_label)), "Overlay");
}

#[test]
fn instance_registry_roundtrip() {
	// 注册表对不存在的键返回 None；摘除未登记键 no-op。
	assert!(instance_from_id(u64::MAX).is_none());
	unregister_instance(u64::MAX);
}

#[test]
fn shared_instance_unknown_identifier_yields_none() {
	// 缓存里查无此标识 → None（montage 路径据此按"无求值器"
	// 告警并直通）。
	assert!(shared_plugin_instance("com.example.definitely-missing").is_none());
}

/// The simplest real plugin (AddOFX: adds a constant to every pixel) renders
/// successfully through the executor path — distinguishes "systemic host
/// problem" from "single-plugin problem" (skipped when the machine does not
/// have the plugin installed).
#[test]
fn add_plugin_renders_through_the_executor() {
	render_real_plugin_smoke("net.sf.openfx.AddPlugin");
}

/// The real GL-only plugin (CImg's ChromaKeyerOFX) renders successfully through
/// the executor path — the GL-kind flag must route it onto a usable GL path
/// instead of a MissingHostFeature purple frame (skipped when the machine does
/// not have the plugin installed).
#[test]
fn cimg_chromakeyer_renders_through_the_executor() {
	render_real_plugin_smoke("net.sf.openfx.ChromaKeyerPlugin");
}

fn render_real_plugin_smoke(identifier: &str) {
	let host = crate::host::Host::global();
	let _ = host.cache.scan();
	if host.cache.find(identifier).is_none() {
		eprintln!("{identifier} not installed; skipping");
		return;
	}
	install_render_executor();
	let instance = shared_plugin_instance(identifier)
		.expect("the shared instance factory resolves the plugin");
	if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
		let inst = instance_from_id(instance).expect("registered");
		let props = &inst.value.plugin.descriptor.props;
		props.with_locked(|all| {
			for p in all
				.iter()
				.filter(|p| p.name.contains("GL") || p.name.contains("OpenGL"))
			{
				eprintln!("[probe] desc prop {} = {:?}", p.name, p.values);
			}
		});
	}

	// 8x8 不透明绿帧（典型 keyer 输入）。
	let mut frame = quercus_render::eval::generate_frame(
		quercus_core::Rational::new(0, 1),
		(8, 8),
		quercus_core::PixelFormat::F32,
	)
	.unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.1f32, 0.9, 0.1, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	let spec = quercus_render::eval::JobSpec::Plugin {
		instance,
		type_id: "org.quercus.test-plugin".to_string(),
		time: 0.0,
		effect_input_id: Some("Source".to_string()),
		inputs: Vec::new(),
		values: Vec::new(),
	};
	let out = execute_plugin_job(&quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::wrap_frame(frame),
	})
	.expect("the GL-capable render succeeds (no MissingHostFeature purple)");
	let quercus_core::texture::Texture::Cpu(out_frame) = &out else {
		panic!("a CPU frame comes back");
	};
	let mut px = [0f32; 4];
	for i in 0..4 {
		px[i] = f32::from_le_bytes(out_frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	eprintln!("[smoke] {identifier} first pixel out = {px:?}");
	assert_ne!(
		px,
		[1.0, 0.0, 1.0, 1.0],
		"must not be the purple failure frame"
	);
	assert_ne!(px, [0.0, 0.0, 0.0, 0.0], "must not be a black empty frame");
}

/// Builds a minimal instance containing only a push-button parameter (registered directly in the registry).
fn instance_with_push_button() -> u64 {
	use crate::descriptor::EffectDescriptor;
	use crate::handle::RefBox;
	use crate::host::Plugin;
	use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
	use std::sync::atomic::AtomicU32;

	let plugin = Arc::new(Plugin {
		identifier: "test.plugin".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::new(),
		contexts: vec![],
		descriptor: EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: recording_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	});
	let mut params = ParamSetInstance { params: Vec::new() };
	params
		.params
		.push(Box::new(ParamInstance::from_def(ParamDef::new(
			"button",
			ofx::TYPE_PUSHBUTTON,
		))));
	params
		.params
		.push(Box::new(ParamInstance::from_def(ParamDef::new(
			"gain",
			ofx::TYPE_DOUBLE,
		))));
	let inst = crate::instance::Instance {
		props: crate::property::PropertySet::new(),
		plugin,
		context: "OfxImageEffectContextFilter".into(),
		params,
		clips: Vec::new(),
		node_identity: std::sync::atomic::AtomicUsize::new(0),
		destroyed: std::sync::atomic::AtomicBool::new(false),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: std::sync::atomic::AtomicBool::new(false),
		edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	};
	register_instance(Arc::new(RefBox {
		refs: AtomicU32::new(1),
		value: inst,
	}))
}

/// push_button_clicked: unknown instance/parameter and type mismatch → false;
/// a hit → true, sets the PushButton value and routes the press as
/// kOfxActionInstanceChanged (UserEdited, with inArgs carrying change reason /
/// name / type / time / render scale).
#[test]
fn push_button_clicked_routes_instance_changed() {
	let _g = PUSH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
	let id = instance_with_push_button();
	assert!(push_button_clicked(id, "button"));
	// 类型不匹配 / 查无参数 / 查无实例：不触发 entry。
	assert!(!push_button_clicked(id, "gain"));
	assert!(!push_button_clicked(id, "nope"));
	assert!(!push_button_clicked(u64::MAX, "button"));
	// 只 "button" 那次按下进了插件 entry，且 reason 是 UserEdited。
	let calls = PUSH_ENTRY_CALLS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone();
	assert_eq!(
		calls,
		vec!["OfxActionInstanceChanged", "OfxChangeUserEdited"]
	);
	unregister_instance(id);
}

/// Builds an instance with a parametric parameter (dimension 2, custom range,
/// two-dimensional UI color configured) and a String parameter (registered
/// directly in the registry).
fn instance_with_parametric() -> u64 {
	use crate::descriptor::EffectDescriptor;
	use crate::handle::RefBox;
	use crate::host::Plugin;
	use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
	use std::ffi::{c_char, c_void};
	use std::sync::atomic::AtomicU32;

	unsafe extern "C" fn dummy_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> i32 {
		0
	}
	let def = ParamDef::new("curve", ofx::TYPE_PARAMETRIC);
	def.props.set_one(
		ofx::PROP_LABEL,
		PropValue::String(CString::new("Curve").unwrap()),
	);
	def.props
		.set_one(ofx::P_PARAMETRIC_DIMENSION, PropValue::Int(2));
	def.props.define(
		ofx::P_PARAMETRIC_RANGE,
		vec![PropValue::Double(0.0), PropValue::Double(1.0)],
	);
	def.props.define(
		ofx::P_PARAMETRIC_UI_COLOUR,
		vec![
			PropValue::Double(1.0),
			PropValue::Double(0.0),
			PropValue::Double(0.0),
			PropValue::Double(0.0),
			PropValue::Double(1.0),
			PropValue::Double(0.0),
		],
	);
	let mut params = ParamSetInstance { params: Vec::new() };
	params.params.push(Box::new(ParamInstance::from_def(def)));
	params
		.params
		.push(Box::new(ParamInstance::from_def(ParamDef::new(
			"title",
			ofx::TYPE_STRING,
		))));
	let plugin = Arc::new(Plugin {
		identifier: "test.parametric".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::new(),
		contexts: vec![],
		descriptor: EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: dummy_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	});
	let inst = crate::instance::Instance {
		props: crate::property::PropertySet::new(),
		plugin,
		context: "OfxImageEffectContextFilter".into(),
		params,
		clips: Vec::new(),
		node_identity: std::sync::atomic::AtomicUsize::new(0),
		destroyed: std::sync::atomic::AtomicBool::new(false),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: std::sync::atomic::AtomicBool::new(false),
		edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	};
	register_instance(Arc::new(RefBox {
		refs: AtomicU32::new(1),
		value: inst,
	}))
}

/// The translation pass produces a Parametric input: value_type, default value
/// (= the default curve's JSON), display_name (= label), not keyframable plus
/// its own properties (dimension / range / one UI color per dimension).
#[test]
fn build_core_produces_parametric_input() {
	let id = instance_with_parametric();
	let inst = instance_from_id(id).expect("已登记");
	let core = build_core(&inst.value);

	let input = core.get_input("curve").expect("parametric 输入应存在");
	assert_eq!(input.value_type, ValueType::Parametric);
	assert_eq!(input.display_name, "Curve");
	assert_ne!(input.flags & input_flags::NOT_KEYFRAMABLE, 0);

	// 默认值 = def.default 曲线集的 JSON（type_default 产出 1 条
	// 恒等曲线；dimension 属性只做索引门控，未配置曲线按恒等读，
	// 不扩充默认值）。
	let expected =
		crate::param_curve::curves_to_json(&[crate::param_curve::Curve::identity(0.0, 1.0)]);
	assert_eq!(
		text_of(&input.default).as_deref(),
		Some(expected.as_str()),
		"默认值应为曲线 JSON"
	);
	// 非 Text 值走 None 臂（helper 自身两臂都有执行）。
	assert_eq!(text_of(&NodeValue::Int(1)), None);
	// 标准值 = 默认（工程序列化从这里走）。
	assert_eq!(core.standard_value("curve", -1), input.default);

	// 专属属性。
	let props = |name: &str| {
		input
			.properties
			.iter()
			.filter(|(k, _)| k == name)
			.map(|(_, v)| v.clone())
			.collect::<Vec<_>>()
	};
	assert_eq!(props("parametric_dimension"), vec![NodeValue::Int(2)]);
	assert_eq!(props("parametric_range"), vec![NodeValue::Vec2([0.0, 1.0])]);
	// 双维 → 两条颜色（重复键）。
	assert_eq!(
		props("parametric_ui_colour"),
		vec![
			NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
			NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
		]
	);
	unregister_instance(id);
}

/// Parametric input (Text JSON) → instance write-back: valid JSON is parsed and
/// `set_ofx(Parametric)`; bad JSON / type mismatch → silently ignored (current
/// value kept).
#[test]
fn parametric_input_writes_back_to_instance() {
	let id = instance_with_parametric();
	let inst = instance_from_id(id).expect("已登记");

	// 有效 JSON（双维，第二维单点）→ 曲线写回，求值即实例曲线。
	let json = r#"{"curves":[[{"key":0,"value":0,"slope":1},{"key":0.5,"value":0.7,"slope":1}],[{"key":0,"value":0,"slope":1}]]}"#;
	set_text_param(&inst.value, "curve", json);
	let curves =
		curves_of(inst.value.params.find("curve").unwrap().get()).expect("应写回 Parametric");
	assert_eq!(curves[0].evaluate(0.5), 0.7);
	assert_eq!(curves[1].len(), 1);
	// 非 Parametric 值走 None 臂（helper 自身两臂都有执行）。
	assert!(curves_of(ParamValue::PushButton).is_none());

	// 坏 JSON → 不改值（与字符串族类型不匹配静默一致）。
	let before = inst.value.params.find("curve").unwrap().get();
	set_text_param(&inst.value, "curve", "{broken");
	assert_eq!(inst.value.params.find("curve").unwrap().get(), before);

	// 非 parametric 参数（String）遇任意文本 → 字符串值路径。
	set_text_param(&inst.value, "title", "hello");
	assert!(matches!(
		inst.value.params.find("title").unwrap().get(),
		ParamValue::String(_)
	));
	unregister_instance(id);
}

// ---- default values / property helpers (coverage batch) --------------

fn def_of(ofx_type: &str) -> ParamDef {
	ParamDef {
		props: PropertySet::new(),
		name: "p".into(),
		ofx_type: ofx_type.into(),
		default: ParamValue::Container,
	}
}

#[test]
fn property_helpers_read_scalars_and_fall_back() {
	let props = PropertySet::new();
	props.define(
		ofx::P_DEFAULT,
		vec![PropValue::Double(2.5), PropValue::Int(7)],
	);
	assert_eq!(prop_double(&props, ofx::P_DEFAULT, 0), 2.5);
	assert_eq!(prop_double(&props, ofx::P_DEFAULT, 1), 7.0);
	assert_eq!(prop_int(&props, ofx::P_DEFAULT, 0), 2);
	assert_eq!(prop_int(&props, ofx::P_DEFAULT, 1), 7);
	props.set_one("s", PropValue::String(CString::new("hello").unwrap()));
	assert_eq!(prop_str(&props, "s", 0), "hello");

	// Missing / out-of-range / wrong-type values fall back quietly.
	assert_eq!(prop_str(&props, "missing", 0), "");
	assert_eq!(prop_double(&props, "missing", 0), 0.0);
	assert_eq!(prop_int(&props, "missing", 0), 0);
	assert_eq!(prop_double(&props, ofx::P_DEFAULT, 9), 0.0);
	assert_eq!(prop_str(&props, ofx::P_DEFAULT, 0), "");
}

#[test]
fn default_value_for_every_scalar_param_type() {
	let def = def_of(ofx::TYPE_INTEGER);
	def.props.set_one(ofx::P_DEFAULT, PropValue::Int(3));
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Int(3))
	));

	let def = def_of(ofx::TYPE_CHOICE);
	def.props.set_one(ofx::P_DEFAULT, PropValue::Int(1));
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Combo(1))
	));

	let def = def_of(ofx::TYPE_BOOLEAN);
	def.props.set_one(ofx::P_DEFAULT, PropValue::Int(1));
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Boolean(true))
	));
	def.props.set_one(ofx::P_DEFAULT, PropValue::Int(0));
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Boolean(false))
	));

	let def = def_of(ofx::TYPE_DOUBLE);
	def.props.set_one(ofx::P_DEFAULT, PropValue::Double(0.25));
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Float(v)) if (v - 0.25).abs() < 1e-9
	));

	for kind in [ofx::TYPE_STRING, ofx::TYPE_STRCHOICE] {
		let def = def_of(kind);
		def.props.set_one(
			ofx::P_DEFAULT,
			PropValue::String(CString::new("hi").unwrap()),
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Text(ref t)) if t == "hi"
		));
	}

	// CUSTOM keeps the raw bytes; BYTES is always empty.
	let def = def_of(ofx::TYPE_CUSTOM);
	def.props.set_one(
		ofx::P_DEFAULT,
		PropValue::String(CString::new("raw").unwrap()),
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Binary(ref b)) if b == b"raw"
	));
	let def = def_of(ofx::TYPE_BYTES);
	def.props.set_one(
		ofx::P_DEFAULT,
		PropValue::String(CString::new("ignored").unwrap()),
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Binary(ref b)) if b.is_empty()
	));

	// Unsupported kinds (and a bare parametric default) have no value.
	assert!(default_value_for_param(&def_of(ofx::TYPE_PUSHBUTTON)).is_none());
	assert!(default_value_for_param(&def_of(ofx::TYPE_GROUP)).is_none());
	assert!(default_value_for_param(&def_of("OfxParamTypeBogus")).is_none());
	assert!(default_value_for_param(&def_of(ofx::TYPE_PARAMETRIC)).is_none());
}

#[test]
fn default_value_for_color_and_vector_types() {
	let def = def_of(ofx::TYPE_RGB);
	def.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
		],
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Color(c))
			if c == [0.1, 0.2, 0.3, 1.0]
	));

	let def = def_of(ofx::TYPE_RGBA);
	def.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
			PropValue::Double(0.4),
		],
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Color(c))
			if c == [0.1, 0.2, 0.3, 0.4]
	));

	let def = def_of(ofx::TYPE_DOUBLE2D);
	def.props.define(
		ofx::P_DEFAULT,
		vec![PropValue::Double(1.5), PropValue::Double(2.5)],
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Vec2(v)) if v == [1.5, 2.5]
	));

	let def = def_of(ofx::TYPE_INTEGER2D);
	def.props
		.define(ofx::P_DEFAULT, vec![PropValue::Int(1), PropValue::Int(2)]);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Vec2(v)) if v == [1.0, 2.0]
	));

	let def = def_of(ofx::TYPE_INTEGER3D);
	def.props.define(
		ofx::P_DEFAULT,
		vec![PropValue::Int(1), PropValue::Int(2), PropValue::Int(3)],
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Vec3(v)) if v == [1.0, 2.0, 3.0]
	));
}

#[test]
fn project_extent_scales_normalised_defaults() {
	static EXTENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	let _guard = EXTENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

	set_project_extent(3840.0, 2160.0);
	assert_eq!(project_extent(), (3840.0, 2160.0));
	assert_eq!(to_canonical(0.5, 3840.0), 1920.0);
	// A zero extent passes the normalised value through untouched.
	assert_eq!(to_canonical(0.5, 0.0), 0.5);

	let def = def_of(ofx::TYPE_DOUBLE);
	def.props.set_one(ofx::P_DEFAULT, PropValue::Double(0.5));
	def.props.set_one(
		ofx::P_DEFAULT_COORD_SYS,
		PropValue::String(CString::new(ofx::V_COORD_NORMALISED).unwrap()),
	);
	assert!(matches!(
		default_value_for_param(&def),
		Some(NodeValue::Float(v)) if v == 1920.0
	));

	let vec3 = def_of(ofx::TYPE_DOUBLE3D);
	vec3.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.5),
			PropValue::Double(0.5),
			PropValue::Double(0.5),
		],
	);
	vec3.props.set_one(
		ofx::P_DEFAULT_COORD_SYS,
		PropValue::String(CString::new(ofx::V_COORD_NORMALISED).unwrap()),
	);
	assert!(matches!(
		default_value_for_param(&vec3),
		Some(NodeValue::Vec3(v)) if v == [1920.0, 1080.0, 1920.0]
	));

	set_project_extent(1920.0, 1080.0);
}

#[test]
fn registered_instance_count_is_observable() {
	let a = registered_instance_count();
	let b = registered_instance_count();
	assert_eq!(a, b);
}

// ---- branch coverage batch: translation tables / executor ------------

use crate::clip::ClipInstance;
use crate::descriptor::ClipDescriptor;
use std::ffi::{c_char, c_void};

/// Minimal plugin entry for host tests: every action succeeds (does nothing).
unsafe extern "C" fn ok_entry(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> i32 {
	crate::suites::status::OK
}

fn text(s: &str) -> PropValue {
	PropValue::String(CString::new(s).unwrap())
}

/// Text variant extraction (both arms are reached by callers; avoids an
/// unreachable panic branch in tests).
fn text_of(value: &NodeValue) -> Option<String> {
	match value {
		NodeValue::Text(s) => Some(s.clone()),
		_ => None,
	}
}

/// Parametric variant extraction (same as above).
fn curves_of(value: ParamValue) -> Option<Vec<crate::param_curve::Curve>> {
	match value {
		ParamValue::Parametric(c) => Some(c),
		_ => None,
	}
}

fn param_set(defs: Vec<ParamDef>) -> crate::param::ParamSetInstance {
	crate::param::ParamSetInstance {
		params: defs
			.into_iter()
			.map(|d| Box::new(crate::param::ParamInstance::from_def(d)))
			.collect(),
	}
}

fn clip(name: &str) -> ClipInstance {
	ClipInstance::from_descriptor(&ClipDescriptor::new(name))
}

fn fake_instance(
	identifier: &str,
	entry: crate::host::EntryPoint,
	params: crate::param::ParamSetInstance,
	clips: Vec<ClipInstance>,
) -> crate::instance::Instance {
	use crate::descriptor::EffectDescriptor;
	use crate::host::Plugin;
	use std::sync::atomic::{AtomicBool, AtomicUsize};

	let plugin = Arc::new(Plugin {
		identifier: identifier.into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::new(),
		contexts: vec![],
		descriptor: EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: AtomicBool::new(false),
	});
	crate::instance::Instance {
		props: PropertySet::new(),
		plugin,
		context: "OfxImageEffectContextFilter".into(),
		params,
		clips: clips.into_iter().map(Box::new).collect(),
		node_identity: AtomicUsize::new(0),
		destroyed: AtomicBool::new(false),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: AtomicBool::new(false),
		edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	}
}

fn register_fake(inst: crate::instance::Instance) -> u64 {
	use crate::handle::RefBox;
	use std::sync::atomic::AtomicU32;
	register_instance(Arc::new(RefBox {
		refs: AtomicU32::new(1),
		value: inst,
	}))
}

fn action_is(action: *const c_char, name: &str) -> bool {
	!action.is_null() && unsafe { std::ffi::CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
}

fn prop_values(input: &quercus_node::input::Input, key: &str) -> Vec<NodeValue> {
	input
		.properties
		.iter()
		.filter(|(k, _)| k == key)
		.map(|(_, v)| v.clone())
		.collect()
}

/// Rule 4 (all defaults equal) and rule 5 (parent/group keywords), plus the
/// dmin < -0.01 half of rule 3.
#[test]
fn color_semantic_rules_4_and_5_and_display_min() {
	let empty = HashMap::new();
	// 规则 4：默认值全相等 → scalar。
	let def = def_of(ofx::TYPE_RGB);
	def.props
		.define(ofx::P_DEFAULT, vec![PropValue::Double(0.5); 3]);
	assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

	// 规则 4 不等 + 无 parent → 兜底 color。
	let def = def_of(ofx::TYPE_RGB);
	def.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
		],
	);
	assert_eq!(deduce_color_semantic(&def, &empty), "color");

	// 规则 3 的 dmin 半边。
	let def = def_of(ofx::TYPE_RGB);
	def.props
		.set_one(ofx::P_DISPLAY_MIN, PropValue::Double(-0.5));
	assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

	// 规则 5：parent 名带标量关键词。
	let def = def_of(ofx::TYPE_RGB);
	def.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
		],
	);
	def.props.set_one(ofx::P_PARENT, text("gamma_group"));
	assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

	// 规则 5：parent 经 group 表解析出的 label 带关键词。
	let mut groups = HashMap::new();
	groups.insert("grp".to_string(), "Gamma".to_string());
	let def = def_of(ofx::TYPE_RGB);
	def.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
		],
	);
	def.props.set_one(ofx::P_PARENT, text("grp"));
	assert_eq!(deduce_color_semantic(&def, &groups), "scalar");
}

/// build_core's group/page first pass: empty label falls back to name, page
/// child mapping; skip row/column and empty-name/unknown-type parameters are
/// skipped.
#[test]
fn build_core_translates_groups_pages_and_skips_unknowns() {
	let group_labeled = ParamDef::new("grp_labeled", ofx::TYPE_GROUP);
	group_labeled
		.props
		.set_one(ofx::PROP_LABEL, text("Grouped"));
	let group_bare = ParamDef::new("grp_bare", ofx::TYPE_GROUP);
	group_bare.props.set_one(ofx::PROP_LABEL, text(""));
	let page = ParamDef::new("page_a", ofx::TYPE_PAGE);
	page.props.set_one(ofx::PROP_LABEL, text("Page A"));
	page.props.define(
		ofx::P_PAGE_CHILD,
		vec![
			text("p_double"),
			text("p_orphan"),
			text(ofx::PAGE_SKIP_ROW),
			text(ofx::PAGE_SKIP_COLUMN),
		],
	);
	let p_double = ParamDef::new("p_double", ofx::TYPE_DOUBLE);
	p_double.props.set_one(ofx::P_PARENT, text("grp_labeled"));
	let p_orphan = ParamDef::new("p_orphan", ofx::TYPE_DOUBLE);
	p_orphan.props.set_one(ofx::P_PARENT, text("missing_group"));
	let empty_name = ParamDef::new("", ofx::TYPE_DOUBLE);
	let bogus = ParamDef::new("bogus", "OfxParamTypeBogus");

	let inst = fake_instance(
		"test.groups.pages",
		ok_entry,
		param_set(vec![
			group_labeled,
			group_bare,
			page,
			p_double,
			p_orphan,
			empty_name,
			bogus,
		]),
		vec![],
	);
	let core = build_core(&inst);

	let double = core.get_input("p_double").expect("p_double 输入");
	assert_eq!(
		prop_values(double, "ui_group"),
		vec![NodeValue::Text("Grouped".into())]
	);
	assert_eq!(
		prop_values(double, "ui_page"),
		vec![NodeValue::Text("Page A".into())]
	);

	let orphan = core.get_input("p_orphan").expect("p_orphan 输入");
	// 未登记的 group → 回退 parent 名本身。
	assert_eq!(
		prop_values(orphan, "ui_group"),
		vec![NodeValue::Text("missing_group".into())]
	);
	assert_eq!(
		prop_values(orphan, "ui_page"),
		vec![NodeValue::Text("Page A".into())]
	);

	// group/page/未知/空名都不产出输入。
	assert!(core.get_input("grp_labeled").is_none());
	assert!(core.get_input("page_a").is_none());
	assert!(core.get_input("bogus").is_none());
	assert!(core.get_input("").is_none());
}

/// build_core's clip→texture input and effect_input fallback chain.
#[test]
fn build_core_selects_effect_input_from_clips() {
	// Source 存在 → effect_input = Source；Output 不入输入表。
	let inst = fake_instance(
		"test.clips.source",
		ok_entry,
		param_set(vec![]),
		vec![clip("Source"), clip("Output")],
	);
	let core = build_core(&inst);
	assert_eq!(core.effect_input, SOURCE_CLIP);
	assert!(core.get_input("Output").is_none());
	let source = core.get_input("Source").expect("Source 输入");
	assert_eq!(source.value_type, ValueType::Texture);
	assert_eq!(source.display_name, "Source");

	// 无 Source 但有 Texture → effect_input = Texture（不新建）。
	let inst = fake_instance(
		"test.clips.texture",
		ok_entry,
		param_set(vec![]),
		vec![clip("Texture"), clip("Output")],
	);
	let core = build_core(&inst);
	assert_eq!(core.effect_input, TEXTURE_INPUT);
	assert_eq!(core.get_input("Texture").unwrap().display_name, "Texture");

	// 只有非标准 clip → 合成 Texture 输入。
	let inst = fake_instance(
		"test.clips.mask",
		ok_entry,
		param_set(vec![]),
		vec![clip("Mask"), clip("Output")],
	);
	let core = build_core(&inst);
	assert_eq!(core.effect_input, TEXTURE_INPUT);
	let tex = core.get_input(TEXTURE_INPUT).expect("合成 Texture 输入");
	assert_eq!(tex.value_type, ValueType::Texture);
	assert_eq!(tex.display_name, "Texture");
	assert!(core.get_input("Mask").is_some());
}

/// Color input properties (semantic/min/max/tooltip) and the combo option
/// table (enum fallback, ChoiceOrder sorting, StrCombo value table).
#[test]
fn build_core_color_and_combo_property_tables() {
	let tint = ParamDef::new("tint", ofx::TYPE_RGB);
	tint.props.set_one(ofx::PROP_LABEL, text("Tint Color"));
	tint.props.set_one(ofx::P_HINT, text("Pick me"));
	tint.props
		.set_one(ofx::P_DISPLAY_MIN, PropValue::Double(0.0));
	tint.props
		.set_one(ofx::P_DISPLAY_MAX, PropValue::Double(1.0));

	let mode = ParamDef::new("mode", ofx::TYPE_CHOICE);
	mode.props
		.define(ofx::P_CHOICE_OPTION, vec![text("A"), text("B"), text("C")]);
	mode.props
		.define(ofx::P_CHOICE_ENUM, vec![text("a"), text("b"), text("c")]);
	mode.props.define(
		ofx::P_CHOICE_ORDER,
		vec![PropValue::Int(2), PropValue::Int(0), PropValue::Int(1)],
	);

	let labels_only = ParamDef::new("labels_only", ofx::TYPE_CHOICE);
	labels_only
		.props
		.define(ofx::P_CHOICE_OPTION, vec![text("OnlyA"), text("OnlyB")]);

	let sc = ParamDef::new("sc", ofx::TYPE_STRCHOICE);
	sc.props
		.define(ofx::P_CHOICE_ENUM, vec![text("v1"), text("v2")]);

	// 无 label + 默认值不等的 RGBA → 兜底 color，显示名回退参数 id。
	let plain = ParamDef::new("plain_rgb", ofx::TYPE_RGBA);
	plain.props.set_one(ofx::PROP_LABEL, text(""));
	plain.props.define(
		ofx::P_DEFAULT,
		vec![
			PropValue::Double(0.1),
			PropValue::Double(0.2),
			PropValue::Double(0.3),
			PropValue::Double(1.0),
		],
	);

	let inst = fake_instance(
		"test.color.combo",
		ok_entry,
		param_set(vec![tint, mode, labels_only, sc, plain]),
		vec![],
	);
	let core = build_core(&inst);

	let tint = core.get_input("tint").unwrap();
	assert_eq!(
		prop_values(tint, "color_semantic"),
		vec![NodeValue::Text("color".into())]
	);
	assert_eq!(prop_values(tint, "min"), vec![NodeValue::Float(0.0)]);
	assert_eq!(prop_values(tint, "max"), vec![NodeValue::Float(1.0)]);
	assert_eq!(
		prop_values(tint, "tooltip"),
		vec![NodeValue::Text("Pick me".into())]
	);

	let mode = core.get_input("mode").unwrap();
	assert_eq!(
		prop_values(mode, "combo_option"),
		vec![
			NodeValue::Text("B".into()),
			NodeValue::Text("C".into()),
			NodeValue::Text("A".into()),
		]
	);
	assert!(prop_values(mode, "combo_value").is_empty());

	// labels 有、enum 空 → values 回退 labels。
	let labels_only = core.get_input("labels_only").unwrap();
	assert_eq!(
		prop_values(labels_only, "combo_option"),
		vec![
			NodeValue::Text("OnlyA".into()),
			NodeValue::Text("OnlyB".into()),
		]
	);
	assert!(prop_values(labels_only, "combo_value").is_empty());

	// StrCombo：enum 回退 labels + combo_value 表。
	let sc = core.get_input("sc").unwrap();
	assert_eq!(
		prop_values(sc, "combo_option"),
		vec![NodeValue::Text("v1".into()), NodeValue::Text("v2".into()),]
	);
	assert_eq!(
		prop_values(sc, "combo_value"),
		vec![NodeValue::Text("v1".into()), NodeValue::Text("v2".into()),]
	);

	let plain = core.get_input("plain_rgb").unwrap();
	assert_eq!(plain.display_name, "plain_rgb");
	assert_eq!(
		prop_values(plain, "color_semantic"),
		vec![NodeValue::Text("color".into())]
	);
}

/// Breaks out of the loop when the parametric UI color table has less than one full dimension.
#[test]
fn build_core_parametric_partial_ui_colour() {
	let def = ParamDef::new("curve", ofx::TYPE_PARAMETRIC);
	def.props
		.set_one(ofx::P_PARAMETRIC_DIMENSION, PropValue::Int(2));
	def.props.define(
		ofx::P_PARAMETRIC_RANGE,
		vec![PropValue::Double(0.0), PropValue::Double(1.0)],
	);
	// 只够第 1 维（3 个 double）→ 第 2 维 break。
	def.props.define(
		ofx::P_PARAMETRIC_UI_COLOUR,
		vec![
			PropValue::Double(1.0),
			PropValue::Double(0.0),
			PropValue::Double(0.0),
		],
	);
	let inst = fake_instance(
		"test.parametric.partial",
		ok_entry,
		param_set(vec![def]),
		vec![],
	);
	let core = build_core(&inst);
	let input = core.get_input("curve").unwrap();
	assert_eq!(
		prop_values(input, "parametric_ui_colour"),
		vec![NodeValue::Color([1.0, 0.0, 0.0, 1.0])]
	);
	assert_eq!(
		prop_values(input, "parametric_dimension"),
		vec![NodeValue::Int(2)]
	);
	assert_eq!(
		prop_values(input, "parametric_range"),
		vec![NodeValue::Vec2([0.0, 1.0])]
	);
}

/// Display-name/description fallbacks and sub-category mapping.
#[test]
fn display_name_description_and_sub_category() {
	let inst = fake_instance("test.display", ok_entry, param_set(vec![]), vec![]);
	assert_eq!(plugin_display_name(&inst), "test.display");
	inst.plugin
		.descriptor
		.props
		.set_one(ofx::PROP_LABEL, text("Pretty"));
	assert_eq!(plugin_display_name(&inst), "Pretty");
	inst.plugin
		.descriptor
		.props
		.set_one(PROP_PLUGIN_DESCRIPTION, text("Desc"));
	assert_eq!(plugin_description(&inst), "Desc");
	// 未配置 → 空串。
	let bare = fake_instance("test.display.bare", ok_entry, param_set(vec![]), vec![]);
	assert_eq!(plugin_description(&bare), "");

	assert_eq!(sub_category_for("OfxImageEffectContextFilter"), "Filter");
	assert_eq!(
		sub_category_for("OfxImageEffectContextGenerator"),
		"Generator"
	);
	assert_eq!(
		sub_category_for("OfxImageEffectContextTransition"),
		"Transition"
	);
	assert_eq!(sub_category_for("OfxImageEffectContextGeneral"), "General");
	assert_eq!(sub_category_for("OfxImageEffectContextBogus"), "General");
}

/// set_text_param's type dispatch: String/StrChoice write-back, NUL rejection,
/// non-text types and missing parameters silently ignored.
#[test]
fn set_text_param_type_dispatch_and_nul_rejection() {
	let s = ParamDef::new("s", ofx::TYPE_STRING);
	let sc = ParamDef::new("sc", ofx::TYPE_STRCHOICE);
	let d = ParamDef::new("d", ofx::TYPE_DOUBLE);
	let inst = fake_instance(
		"test.text.dispatch",
		ok_entry,
		param_set(vec![s, sc, d]),
		vec![],
	);

	// 查无参数 → no-op。
	set_text_param(&inst, "missing", "x");

	// String：NUL → 拒绝（值保持），有效 → 写回。
	let before = inst.params.find("s").unwrap().get();
	set_text_param(&inst, "s", "with\0nul");
	assert_eq!(inst.params.find("s").unwrap().get(), before);
	set_text_param(&inst, "s", "hello");
	assert!(matches!(
		inst.params.find("s").unwrap().get(),
		ParamValue::String(_)
	));

	// StrChoice：NUL → 拒绝，有效 → 写回。
	let before = inst.params.find("sc").unwrap().get();
	set_text_param(&inst, "sc", "with\0nul");
	assert_eq!(inst.params.find("sc").unwrap().get(), before);
	set_text_param(&inst, "sc", "pick");
	assert!(matches!(
		inst.params.find("sc").unwrap().get(),
		ParamValue::StrChoice(_)
	));

	// 非文本类型 → 静默忽略。
	let before = inst.params.find("d").unwrap().get();
	set_text_param(&inst, "d", "5");
	assert_eq!(inst.params.find("d").unwrap().get(), before);
}

/// Edge branches of the recording entry (empty action, non-instanceChanged,
/// missing/mismatched change reason) — covered by calling the entry directly.
#[test]
fn push_button_recording_entry_edge_branches() {
	let _g = PUSH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	// 空 action → 不记录。
	let rc = unsafe {
		recording_entry(
			std::ptr::null(),
			std::ptr::null(),
			std::ptr::null_mut(),
			std::ptr::null_mut(),
		)
	};
	assert_eq!(rc, 0);
	// 其它 action → 不记录。
	let other = CString::new("OfxActionSomethingElse").unwrap();
	let rc = unsafe {
		recording_entry(
			other.as_ptr(),
			std::ptr::null(),
			std::ptr::null_mut(),
			std::ptr::null_mut(),
		)
	};
	assert_eq!(rc, 0);

	// instanceChanged 但 inArgs 缺 reason → 记录空串。
	*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
	let changed = CString::new(crate::host::ACTION_INSTANCE_CHANGED).unwrap();
	let props = PropertySet::new();
	unsafe {
		recording_entry(
			changed.as_ptr(),
			std::ptr::null(),
			&props as *const PropertySet as *mut c_void,
			std::ptr::null_mut(),
		)
	};
	assert_eq!(
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()),
		vec!["OfxActionInstanceChanged".to_string(), String::new()]
	);

	// reason 类型不符 → 空串。
	*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
	props.set_one(crate::host::PROP_CHANGE_REASON, PropValue::Int(3));
	unsafe {
		recording_entry(
			changed.as_ptr(),
			std::ptr::null(),
			&props as *const PropertySet as *mut c_void,
			std::ptr::null_mut(),
		)
	};
	assert_eq!(
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()),
		vec!["OfxActionInstanceChanged".to_string(), String::new()]
	);
}

/// An instanceChanged failure in push_button_clicked only logs and still returns true.
#[test]
fn push_button_clicked_instance_changed_failure_returns_true() {
	unsafe extern "C" fn fail_changed(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> i32 {
		if action_is(action, crate::host::ACTION_INSTANCE_CHANGED) {
			crate::suites::status::ERR_FATAL
		} else {
			crate::suites::status::OK
		}
	}
	let button = ParamDef::new("button", ofx::TYPE_PUSHBUTTON);
	button.props.set_one(ofx::PROP_LABEL, text("Button"));
	let inst = fake_instance(
		"test.push.fail",
		fail_changed,
		param_set(vec![button]),
		vec![],
	);
	let id = register_fake(inst);
	assert!(push_button_clicked(id, "button"));
	unregister_instance(id);
}

/// execute_plugin_job: non-Plugin spec, unregistered instance, dummy/zero-size
/// input, and the PushButton/None branches of parameter value dispatch.
#[test]
fn execute_plugin_job_spec_and_source_branches() {
	let inst = fake_instance("test.exec.branches", ok_entry, param_set(vec![]), vec![]);
	let id = register_fake(inst);

	fn plugin_spec(
		instance: u64,
		values: Vec<(String, NodeValue)>,
	) -> quercus_render::eval::JobSpec {
		quercus_render::eval::JobSpec::Plugin {
			instance,
			type_id: "org.quercus.test.exec".to_string(),
			time: 0.0,
			effect_input_id: Some(SOURCE_CLIP.to_string()),
			inputs: Vec::new(),
			values,
		}
	}

	// 非 Plugin spec → Invalid。
	let spec = quercus_render::eval::JobSpec::Generate;
	let req = quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::dummy(),
	};
	assert!(execute_plugin_job(&req).is_err());

	// 未登记实例 → Failed。
	let spec = plugin_spec(u64::MAX, Vec::new());
	let req = quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::dummy(),
	};
	assert!(execute_plugin_job(&req).is_err());

	// 登记实例 + dummy 源 → Failed。
	let spec = plugin_spec(id, Vec::new());
	let req = quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::dummy(),
	};
	assert!(execute_plugin_job(&req).is_err());

	// 登记实例 + 零尺寸（非 dummy）源 → generate_frame 失败。
	let pod = quercus_core::frame::VideoParamsPod {
		width: 1,
		height: 0,
		..Default::default()
	};
	let mut zero = quercus_core::texture::Frame::new();
	zero.set_video_params(pod);
	let spec = plugin_spec(id, Vec::new());
	let req = quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::wrap_frame(zero),
	};
	assert!(execute_plugin_job(&req).is_err());

	// 参数值分发：PushButton/None 跳过、数值走 POD、文本走 set_ofx；
	// 后续 render_frame 的结果不做断言（假插件没有真实渲染行为），
	// 这里只要求各分发分支被走到。
	let frame = quercus_render::eval::generate_frame(
		quercus_core::Rational::new(0, 1),
		(2, 2),
		quercus_core::PixelFormat::F32,
	)
	.expect("frame");
	let spec = plugin_spec(
		id,
		vec![
			("button".to_string(), NodeValue::PushButton),
			("none".to_string(), NodeValue::None),
			("gain".to_string(), NodeValue::Float(1.0)),
			("title".to_string(), NodeValue::Text("hello".to_string())),
		],
	);
	let req = quercus_render::eval::PluginJobRequest {
		spec: &spec,
		src: quercus_core::texture::Texture::wrap_frame(frame),
	};
	let _ = execute_plugin_job(&req);

	unregister_instance(id);
}

// ---- 真实测试插件（build.rs 产物）的宿主面覆盖 ----------------------

/// Serialises host global state (real plugin scanning/instantiation).
static HOST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture_bundle_dir() -> Option<std::path::PathBuf> {
	let out = std::path::PathBuf::from(env!("OUT_DIR"));
	let lib = out.join(if cfg!(target_os = "macos") {
		"quercus_test_plugin.dylib"
	} else {
		"quercus_test_plugin.so"
	});
	if !lib.is_file() {
		return None;
	}
	let platform = if cfg!(target_os = "macos") {
		"MacOS"
	} else if cfg!(target_os = "windows") {
		"Win64"
	} else {
		"Linux-x86-64"
	};
	let dir = std::env::temp_dir().join(format!(
		"quercus-ofx-plugin-unit-fixture-{}",
		std::process::id()
	));
	let bin_dir = dir
		.join("quercus-test-plugin.ofx.bundle/Contents")
		.join(platform);
	std::fs::create_dir_all(&bin_dir).ok()?;
	let target = bin_dir.join(if cfg!(target_os = "windows") {
		"plugin.dll"
	} else {
		"plugin"
	});
	if !target.exists() {
		std::fs::copy(&lib, &target).ok()?;
	}
	Some(dir)
}

fn ensure_fixture_loaded() -> bool {
	let Some(dir) = fixture_bundle_dir() else {
		return false;
	};
	if Host::global().cache.scan_path(&dir).is_err() {
		return false;
	}
	Host::global()
		.cache
		.find("org.quercus.test-plugin")
		.is_some()
}

fn with_trace<R>(f: impl FnOnce() -> R) -> R {
	static TRACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let prev = std::env::var_os("QUERCUS_OFX_TRACE");
	std::env::set_var("QUERCUS_OFX_TRACE", "1");
	let out = f();
	match prev {
		Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
		None => std::env::remove_var("QUERCUS_OFX_TRACE"),
	}
	out
}

/// duplicate_instance: registry gets a new instance (real test plugin) + unknown handle → None.
#[test]
fn duplicate_instance_recreates_and_unknown_handle_is_none() {
	let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	if !ensure_fixture_loaded() {
		eprintln!("SKIP: 测试插件未构建");
		return;
	}
	let inst = Host::global()
		.create_instance(
			"org.quercus.test-plugin",
			Some("OfxImageEffectContextFilter"),
		)
		.expect("测试插件实例");
	let handle = PluginInstanceHandle(register_instance(inst));
	let dup = duplicate_instance(handle).expect("duplicate 应成功");
	assert_ne!(dup.0, handle.0);
	let dup_inst = instance_from_id(dup.0).expect("duplicate 已登记");
	assert_eq!(dup_inst.value.plugin.identifier, "org.quercus.test-plugin");
	assert_eq!(dup_inst.value.context, "OfxImageEffectContextFilter");
	unregister_instance(dup.0);
	assert!(duplicate_instance(PluginInstanceHandle(u64::MAX)).is_none());
}

/// shared_plugin_instance: first access creates and caches an instance, the
/// next access hits the cache; unknown identifier → None.
#[test]
fn shared_plugin_instance_caches_and_misses() {
	let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	if !ensure_fixture_loaded() {
		eprintln!("SKIP: 测试插件未构建");
		return;
	}
	let first = shared_plugin_instance("org.quercus.test-plugin").expect("首访建实例");
	let second = shared_plugin_instance("org.quercus.test-plugin").expect("再访命中缓存");
	assert_eq!(first, second);
	assert!(shared_plugin_instance("com.example.no-such-plugin").is_none());
}

/// Runs the full render_real_plugin_smoke path with the test plugin produced by
/// build.rs (including the QUERCUS_OFX_TRACE probe branches), no longer
/// depending on a real plugin being installed on the machine.
#[test]
fn fixture_plugin_renders_through_the_executor() {
	let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	if !ensure_fixture_loaded() {
		eprintln!("SKIP: 测试插件未构建");
		return;
	}
	with_trace(|| render_real_plugin_smoke("org.quercus.test-plugin"));
}
