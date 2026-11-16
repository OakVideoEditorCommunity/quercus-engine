use super::*;
use crate::value::{NodeValue, NodeValueTable, ValueType};
use quercus_core::Rational;

#[test]
fn input_names() {
	let n = ChromaKeyNode;
	assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
	assert_eq!(n.input_name(GARBAGE_MATTE_INPUT), "Garbage Matte");
	assert_eq!(n.input_name(CORE_MATTE_INPUT), "Core Matte");
	assert_eq!(n.input_name(COLOR_INPUT), "Key Color");
	assert_eq!(n.input_name(SHADOWS_INPUT), "Shadows");
	assert_eq!(n.input_name(HIGHLIGHTS_INPUT), "Highlights");
	assert_eq!(n.input_name(UPPER_TOLERANCE_INPUT), "Upper Tolerance");
	assert_eq!(n.input_name(LOWER_TOLERANCE_INPUT), "Lower Tolerance");
	assert_eq!(n.input_name(INVERT_INPUT), "Invert Mask");
	assert_eq!(n.input_name(MASK_ONLY_INPUT), "Show Mask Only");
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inputs_flags_and_properties() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.chromakey");
	assert_ne!(
		core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
			.unwrap()
			.flags & crate::input::flags::NOT_KEYFRAMABLE,
		0
	);
	assert_eq!(
		core.get_input(COLOR_INPUT).unwrap().default,
		NodeValue::Color([0.0, 1.0, 0.0, 1.0])
	);
	assert_eq!(
		core.get_input(LOWER_TOLERANCE_INPUT).unwrap().default,
		NodeValue::Float(5.0)
	);
	assert_eq!(
		core.get_input(UPPER_TOLERANCE_INPUT).unwrap().default,
		NodeValue::Float(25.0)
	);
	assert_eq!(
		core.get_input(HIGHLIGHTS_INPUT).unwrap().default,
		NodeValue::Float(100.0)
	);
	assert_eq!(
		core.get_input(SHADOWS_INPUT).unwrap().default,
		NodeValue::Float(100.0)
	);
	assert_eq!(
		core.get_input(INVERT_INPUT).unwrap().default,
		NodeValue::Boolean(false)
	);
	assert_eq!(
		core.get_input(MASK_ONLY_INPUT).unwrap().default,
		NodeValue::Boolean(false)
	);
	for id in [GARBAGE_MATTE_INPUT, CORE_MATTE_INPUT] {
		assert_ne!(
			core.get_input(id).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
	}
	assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

#[test]
fn shader_code_replaces_stub_marker() {
	let n = ChromaKeyNode;
	let stub = "float SceneLinearToCIEXYZ_d65(vec4 c){ return 0.0; }";
	let code = n.shader_code(stub).unwrap();
	assert!(!code.contains("%1"));
	assert!(code.contains(stub));
}

#[test]
fn legacy_input_ids_remap_misspellings() {
	let n = ChromaKeyNode;
	assert_eq!(
		n.map_legacy_input_id("upper_tolerence_in"),
		UPPER_TOLERANCE_INPUT
	);
	assert_eq!(
		n.map_legacy_input_id("lower_tolerence_in"),
		LOWER_TOLERANCE_INPUT
	);
	assert_eq!(
		n.map_legacy_input_id("anything_else_in"),
		"anything_else_in"
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
fn value_texture_pushes_shader_job_payload() {
	let (core, behavior) = create();
	// The payload carries the whole input row (the renderer packs the
	// uniforms from it), with the full node identity + effect input.
	let inputs = crate::value::NodeValueRow::from([
		(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		),
		(
			COLOR_INPUT.to_string(),
			NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
		),
	]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
		panic!("expected a texture-typed value");
	};
	let payload =
		unsafe { crate::jobs::shader_job(handle) }.expect("payload boxed behind the handle");
	assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.chromakey");
	assert_eq!(payload.shader_id, "");
	assert_eq!(payload.iterations, 1);
	assert_eq!(payload.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
	// The whole input row rides along, so the renderer can pack the
	// Key Color + tolerances uniforms from it.
	assert_eq!(
		payload.params.get(COLOR_INPUT),
		Some(&NodeValue::Color([1.0, 0.0, 0.0, 1.0]))
	);
	assert!(payload
		.params
		.contains_key(crate::nodes::ociobase::TEXTURE_INPUT));
}

#[test]
fn duplicate_clones() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Chroma Key");
}
