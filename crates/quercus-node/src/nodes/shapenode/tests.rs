use super::*;
use crate::node::NodeBehavior;
use crate::value::{NodeValue, NodeValueTable, ValueType};
use quercus_core::Rational;

#[test]
fn input_names() {
	let n = ShapeNode;
	assert_eq!(n.input_name(TYPE_INPUT), "Type");
	assert_eq!(n.input_name(RADIUS_INPUT), "Radius");
	assert_eq!(
		n.input_name(super::super::generatorwithmerge::BASE_INPUT),
		"Base"
	);
	assert_eq!(
		n.input_name(super::super::shapenodebase::POSITION_INPUT),
		"Position"
	);
	assert_eq!(
		n.input_name(super::super::shapenodebase::SIZE_INPUT),
		"Size"
	);
	assert_eq!(
		n.input_name(super::super::shapenodebase::COLOR_INPUT),
		"Color"
	);
}

#[test]
fn create_wires_inputs_and_flags() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.shape");
	// `type_in` is prepended right after `enabled_in`.
	assert_eq!(core.inputs[1].id, TYPE_INPUT);
	assert_eq!(core.inputs[1].default, NodeValue::Combo(0));
	assert_eq!(
		core.get_input(RADIUS_INPUT).unwrap().default,
		NodeValue::Float(20.0)
	);
	assert_eq!(
		core.get_input(super::super::shapenodebase::SIZE_INPUT)
			.unwrap()
			.default,
		NodeValue::Vec2([100.0, 100.0])
	);
	assert_eq!(
		core.get_input(super::super::shapenodebase::COLOR_INPUT)
			.unwrap()
			.default,
		NodeValue::Color([1.0, 0.0, 0.0, 1.0])
	);
	assert_eq!(
		core.effect_input,
		super::super::generatorwithmerge::BASE_INPUT
	);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

#[test]
fn value_pushes_shape_job_payload() {
	let (core, behavior) = create();
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	match table.get(ValueType::Texture) {
		Some(NodeValue::Texture(h)) => {
			let payload = unsafe { crate::jobs::shader_job(h) }.expect("shader job payload boxed");
			assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.shape");
			assert_eq!(payload.shader_id, "shape");
			assert_eq!(payload.iterations, 1);
			assert_eq!(
				payload.effect_input,
				super::super::generatorwithmerge::BASE_INPUT
			);
			assert_eq!(payload.iterative_input, "");
		}
		_ => panic!("texture expected"),
	}
}

#[test]
fn value_with_base_merges_nested_shape_job() {
	let (core, behavior) = create();
	let inputs = crate::value::NodeValueRow::from([(
		super::super::generatorwithmerge::BASE_INPUT.to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	)]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	match table.get(ValueType::Texture) {
		Some(NodeValue::Texture(h)) => {
			let merge = unsafe { crate::jobs::shader_job(h) }.expect("merge job payload boxed");
			assert_eq!(merge.shader_id, "mrg");
			assert_eq!(merge.iterations, 1);
			assert!(merge
				.params
				.contains_key(super::super::generatorwithmerge::BASE_INPUT));
			// The shape job is nested as the merge's blend texture.
			match merge.params.get(crate::nodes::merge::BLEND_INPUT) {
				Some(NodeValue::Texture(blend)) => {
					let shape = unsafe { crate::jobs::shader_job(blend) }
						.expect("nested shape job payload boxed");
					assert_eq!(shape.shader_id, "shape");
				}
				_ => panic!("nested blend job expected"),
			}
		}
		_ => panic!("texture expected"),
	}
}

#[test]
fn shader_code_dispatches() {
	let n = ShapeNode;
	let shape = n.shader_code("shape").unwrap();
	assert!(shape.contains("const int SHAPE_RECTANGLE = 0;"));
	let mrg = n.shader_code("mrg").unwrap();
	assert!(mrg.contains("base_col *= 1.0 - blend_col.a;"));
	assert!(n.shader_code("other").is_none());
}

#[test]
fn input_value_changed_toggles_radius_hidden() {
	let (mut core, behavior) = create();
	let mut b = behavior;
	// Default type is rectangle (0): radius_in becomes hidden.
	b.input_value_changed(&mut core, TYPE_INPUT, -1);
	assert_ne!(
		core.get_input(RADIUS_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
		0
	);

	// Rounded rectangle (2): radius_in is shown.
	core.set_standard_value(TYPE_INPUT, -1, NodeValue::Combo(2));
	b.input_value_changed(&mut core, TYPE_INPUT, -1);
	assert_eq!(
		core.get_input(RADIUS_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
		0
	);
}

#[test]
fn duplicate_clones() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Shape");
}
