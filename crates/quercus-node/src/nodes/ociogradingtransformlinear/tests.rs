use super::*;
use crate::keyframe::{Interpolation, Keyframe};
use crate::value::{NodeValue, NodeValueTable, ValueType};
use quercus_core::Rational;

fn node() -> OCIOGradingTransformLinearNode {
	OCIOGradingTransformLinearNode {
		base: OcioBase::new(),
	}
}

/// Property value lookup helper for tests.
fn property(core: &NodeCore, input: &str, key: &str) -> Option<NodeValue> {
	core.get_input(input).and_then(|i| {
		i.properties
			.iter()
			.find(|(k, _)| k == key)
			.map(|(_, v)| v.clone())
	})
}

#[test]
fn input_names() {
	let n = node();
	assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
	assert_eq!(n.input_name(CONTRAST_INPUT), "Contrast");
	assert_eq!(n.input_name(OFFSET_INPUT), "Offset");
	assert_eq!(n.input_name(EXPOSURE_INPUT), "Exposure");
	assert_eq!(n.input_name(SATURATION_INPUT), "Saturation");
	assert_eq!(n.input_name(PIVOT_INPUT), "Pivot");
	assert_eq!(n.input_name(CLAMP_BLACK_ENABLE_INPUT), "Enable Black Clamp");
	assert_eq!(n.input_name(CLAMP_BLACK_INPUT), "Black Clamp");
	assert_eq!(n.input_name(CLAMP_WHITE_ENABLE_INPUT), "Enable White Clamp");
	assert_eq!(n.input_name(CLAMP_WHITE_INPUT), "White Clamp");
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inputs_flags_and_properties() {
	let (core, behavior) = create();
	assert_eq!(
		behavior.type_id(),
		"org.olivevideoeditor.Olive.ociogradingtransformlinear"
	);
	assert_ne!(
		core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
			.unwrap()
			.flags & crate::input::flags::NOT_KEYFRAMABLE,
		0
	);
	assert_eq!(
		core.get_input(CONTRAST_INPUT).unwrap().default,
		NodeValue::Vec4([1.0, 1.0, 1.0, 1.0])
	);
	assert_eq!(
		core.get_input(OFFSET_INPUT).unwrap().default,
		NodeValue::Vec4([0.0; 4])
	);
	assert_eq!(
		core.get_input(EXPOSURE_INPUT).unwrap().default,
		NodeValue::Vec4([0.0; 4])
	);
	assert_eq!(
		core.get_input(SATURATION_INPUT).unwrap().default,
		NodeValue::Float(1.0)
	);
	assert_eq!(
		core.get_input(PIVOT_INPUT).unwrap().default,
		NodeValue::Float(0.18)
	);
	assert_eq!(
		core.get_input(CLAMP_BLACK_ENABLE_INPUT).unwrap().default,
		NodeValue::Boolean(false)
	);
	assert_eq!(
		core.get_input(CLAMP_BLACK_INPUT).unwrap().default,
		NodeValue::Float(0.0)
	);
	assert_eq!(
		core.get_input(CLAMP_WHITE_ENABLE_INPUT).unwrap().default,
		NodeValue::Boolean(false)
	);
	assert_eq!(
		core.get_input(CLAMP_WHITE_INPUT).unwrap().default,
		NodeValue::Float(1.0)
	);
	// Component colors on every vec4 grading input.
	for id in [CONTRAST_INPUT, OFFSET_INPUT, EXPOSURE_INPUT] {
		let input = core.get_input(id).unwrap();
		assert!(input
			.properties
			.iter()
			.any(|(k, v)| k == "color0" && *v == NodeValue::Text("#c0c0c0".into())));
		assert!(input
			.properties
			.iter()
			.any(|(k, v)| k == "color3" && *v == NodeValue::Text("#0000ff".into())));
	}
	// Initial white-clamp minimum constraint: black (0.0) + 0.000001.
	assert_eq!(
		property(&core, CLAMP_WHITE_INPUT, "min"),
		Some(NodeValue::Float(0.000001))
	);
	// Clamp enabled properties mirror the enable inputs (false at
	// construction).
	assert_eq!(
		property(&core, CLAMP_BLACK_INPUT, "enabled"),
		Some(NodeValue::Boolean(false))
	);
	assert_eq!(
		property(&core, CLAMP_WHITE_INPUT, "enabled"),
		Some(NodeValue::Boolean(false))
	);
	assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

#[test]
fn update_clamp_white_minimum_tracks_black_clamp() {
	let mut core = NodeCore::new();
	// Provide the clamp inputs so the property write lands.
	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.set_standard_value(CLAMP_BLACK_INPUT, -1, NodeValue::Float(0.5));
	let mut n = node();
	n.update_clamp_white_minimum(&mut core);
	assert_eq!(
		property(&core, CLAMP_WHITE_INPUT, "min"),
		Some(NodeValue::Float(0.500001))
	);
}

#[test]
fn update_clamp_white_minimum_skips_keyframed_black_clamp() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.keyframe_track_mut(CLAMP_BLACK_INPUT, -1)
		.set_key(Keyframe {
			time: Rational::new(0, 1),
			value: NodeValue::Float(0.5),
			interpolation: Interpolation::Hold,
			bezier_in: (0.0, 0.0),
			bezier_out: (0.0, 0.0),
		});
	let mut n = node();
	n.update_clamp_white_minimum(&mut core);
	// Keyframed: the static minimum is not updated.
	assert_eq!(property(&core, CLAMP_WHITE_INPUT, "min"), None);
}

#[test]
fn input_value_changed_mirrors_enable_toggles() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_ENABLE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_ENABLE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	));
	core.set_standard_value(CLAMP_BLACK_ENABLE_INPUT, -1, NodeValue::Boolean(true));
	let mut n = node();
	n.input_value_changed(&mut core, CLAMP_BLACK_ENABLE_INPUT, 0);
	assert_eq!(
		property(&core, CLAMP_BLACK_INPUT, "enabled"),
		Some(NodeValue::Boolean(true))
	);
	// White enable mirrors too.
	core.set_standard_value(CLAMP_WHITE_ENABLE_INPUT, -1, NodeValue::Boolean(true));
	n.input_value_changed(&mut core, CLAMP_WHITE_ENABLE_INPUT, 0);
	assert_eq!(
		property(&core, CLAMP_WHITE_INPUT, "enabled"),
		Some(NodeValue::Boolean(true))
	);
}

#[test]
fn input_value_changed_black_clamp_reconstrains_white_minimum() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	));
	core.set_standard_value(CLAMP_BLACK_INPUT, -1, NodeValue::Float(0.2));
	let mut n = node();
	n.input_value_changed(&mut core, CLAMP_BLACK_INPUT, 0);
	assert_eq!(
		property(&core, CLAMP_WHITE_INPUT, "min"),
		Some(NodeValue::Float(0.200001))
	);
}

#[test]
fn value_no_texture_pushes_nothing() {
	let (core, behavior) = create();
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert!(table.is_empty());
}

#[test]
fn value_texture_pushes_real_grading_job() {
	let (core, behavior) = create();
	let inputs = crate::value::NodeValueRow::from([
		(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		),
		(
			CONTRAST_INPUT.to_string(),
			NodeValue::Vec4([0.5, 0.25, 0.5, 0.75]),
		),
		(
			OFFSET_INPUT.to_string(),
			NodeValue::Vec4([0.25, -0.125, 0.0, 0.125]),
		),
		(
			EXPOSURE_INPUT.to_string(),
			NodeValue::Vec4([1.0, 0.0, 0.0, 0.0]),
		),
	]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let tex = table.get(ValueType::Texture).expect("texture pushed");
	let NodeValue::Texture(handle) = tex else {
		panic!("pushed value is not a texture handle");
	};
	let job = unsafe { crate::jobs::shader_job(handle) }.expect("real shader job");
	assert_eq!(job.shader_id, "rgb");
	assert_eq!(
		job.type_id,
		"org.olivevideoeditor.Olive.ociogradingtransformlinear"
	);
	assert_eq!(job.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
	// vec4 (RGBM: x = master) rewrites: contrast RGB = c*m, offset
	// RGB = c+m, exposure RGB = 2^(c+m).
	assert_eq!(
		job.params.get(CONTRAST_INPUT),
		Some(&NodeValue::Vec3([0.125, 0.25, 0.375]))
	);
	assert_eq!(
		job.params.get(OFFSET_INPUT),
		Some(&NodeValue::Vec3([0.125, 0.25, 0.375]))
	);
	assert_eq!(
		job.params.get(EXPOSURE_INPUT),
		Some(&NodeValue::Vec3([2.0, 2.0, 2.0]))
	);
	// Scalar defaults from the node's standard values.
	assert_eq!(
		job.params.get(SATURATION_INPUT),
		Some(&NodeValue::Float(1.0))
	);
	assert_eq!(job.params.get(PIVOT_INPUT), Some(&NodeValue::Float(0.18)));
	// Clamps disabled -> OCIO sentinels (NoClampBlack/NoClampWhite).
	assert_eq!(
		job.params.get(CLAMP_BLACK_INPUT),
		Some(&NodeValue::Float(-1.0))
	);
	assert_eq!(
		job.params.get(CLAMP_WHITE_INPUT),
		Some(&NodeValue::Float(2.0))
	);
	assert_eq!(
		job.params.get("ocio_grading_primary_localBypass"),
		Some(&NodeValue::Boolean(false))
	);
}

#[test]
fn value_enables_clamp_when_flagged_of_white_and_black() {
	let (core, behavior) = create();
	let inputs = crate::value::NodeValueRow::from([
		(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		),
		(
			CLAMP_BLACK_ENABLE_INPUT.to_string(),
			NodeValue::Boolean(true),
		),
		(
			CLAMP_WHITE_ENABLE_INPUT.to_string(),
			NodeValue::Boolean(true),
		),
		(CLAMP_BLACK_INPUT.to_string(), NodeValue::Float(0.5)),
		(CLAMP_WHITE_INPUT.to_string(), NodeValue::Float(0.4)),
	]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let tex = table.get(ValueType::Texture).expect("texture pushed");
	let NodeValue::Texture(handle) = tex else {
		panic!("pushed value is not a texture handle");
	};
	let job = unsafe { crate::jobs::shader_job(handle) }.expect("real shader job");
	// White <= black is raised to black + 0.000001.
	assert_eq!(
		job.params.get(CLAMP_BLACK_INPUT),
		Some(&NodeValue::Float(0.5))
	);
	assert_eq!(
		job.params.get(CLAMP_WHITE_INPUT),
		Some(&NodeValue::Float(0.500001))
	);
}

#[test]
fn shader_code_splices_stub_marker() {
	let n = node();
	let stub = "vec4 ove_grading_primary(vec4 c) { return c; }";
	let code = n.shader_code(stub).unwrap();
	assert!(!code.contains("%1"));
	assert!(code.contains("ove_grading_primary(col)"));
	assert!(code.contains(stub));
}

#[test]
fn duplicate_clones() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "OCIO Color Grading (Linear)");
}
