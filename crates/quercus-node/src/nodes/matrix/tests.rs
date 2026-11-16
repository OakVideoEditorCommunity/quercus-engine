use super::*;
use crate::node::NodeBehavior;
use crate::value::{NodeValue, NodeValueTable, ValueType};
use quercus_core::Rational;

#[test]
fn input_names() {
	let n = MatrixGenerator;
	assert_eq!(n.input_name(POSITION_INPUT), "Position");
	assert_eq!(n.input_name(ROTATION_INPUT), "Rotation");
	assert_eq!(n.input_name(SCALE_INPUT), "Scale");
	assert_eq!(n.input_name(UNIFORM_SCALE_INPUT), "Uniform Scale");
	assert_eq!(n.input_name(ANCHOR_INPUT), "Anchor Point");
}

#[test]
fn create_wires_inputs() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ortho");
	assert_eq!(
		core.get_input(POSITION_INPUT).unwrap().default,
		NodeValue::Vec2([0.0, 0.0])
	);
	assert_eq!(
		core.get_input(SCALE_INPUT).unwrap().default,
		NodeValue::Vec2([1.0, 1.0])
	);
	assert_eq!(
		core.get_input(UNIFORM_SCALE_INPUT).unwrap().default,
		NodeValue::Boolean(true)
	);
}

#[test]
fn value_defaults_produce_identity() {
	let (core, behavior) = create();
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(
		table.get(ValueType::Matrix),
		Some(&NodeValue::Matrix(super::super::mathbase::identity_matrix()))
	);
}

#[test]
fn value_translate_position() {
	let (mut core, behavior) = create();
	core.set_standard_value(POSITION_INPUT, -1, NodeValue::Vec2([10.0, 20.0]));
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	// translate(10, 20) is stored at m[0][3] / m[1][3].
	let m = match table.get(ValueType::Matrix).unwrap() {
		NodeValue::Matrix(m) => *m,
		_ => panic!("matrix expected"),
	};
	assert_eq!(m[3], 10.0);
	assert_eq!(m[7], 20.0);
}

#[test]
fn value_non_uniform_scale() {
	let (mut core, behavior) = create();
	core.set_standard_value(UNIFORM_SCALE_INPUT, -1, NodeValue::Boolean(false));
	core.set_standard_value(SCALE_INPUT, -1, NodeValue::Vec2([2.0, 3.0]));
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	let m = match table.get(ValueType::Matrix).unwrap() {
		NodeValue::Matrix(m) => *m,
		_ => panic!("matrix expected"),
	};
	assert_eq!(m[0], 2.0);
	assert_eq!(m[5], 3.0);
}

#[test]
fn value_uniform_scale_replicates_x() {
	let (mut core, behavior) = create();
	// uniform_scale defaults to true.
	core.set_standard_value(SCALE_INPUT, -1, NodeValue::Vec2([2.0, 9.0]));
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	let m = match table.get(ValueType::Matrix).unwrap() {
		NodeValue::Matrix(m) => *m,
		_ => panic!("matrix expected"),
	};
	assert_eq!(m[0], 2.0);
	assert_eq!(m[5], 2.0, "uniform scale replicates X");
}

#[test]
fn compose_matrix_rotation_translate_order() {
	// rotate(90) post-multiplied by translate(-10, 0): a pure
	// translation (10, 0) under a 90-degree rotation lands in the
	// negative Y translation slot.
	let m = MatrixGenerator::compose_matrix(
		[0.0, 0.0],
		90.0,
		[1.0, 1.0],
		false,
		[-10.0, 0.0],
		super::super::mathbase::identity_matrix(),
	);
	assert!((m[0] - 0.0).abs() < 1e-9);
	assert!((m[5] - 0.0).abs() < 1e-9);
	assert!((m[7] - 10.0).abs() < 1e-9, "m[1][3] = +10 after rotate(90)");
	assert!((m[3] - 0.0).abs() < 1e-9);
}

#[test]
fn compose_matrix_identity_inputs() {
	let m = MatrixGenerator::compose_matrix(
		[0.0, 0.0],
		0.0,
		[1.0, 1.0],
		false,
		[0.0, 0.0],
		super::super::mathbase::identity_matrix(),
	);
	assert!(super::super::mathbase::matrix_is_identity(m));
}

#[test]
fn matrix_helpers_are_row_major() {
	let mut a = super::super::mathbase::identity_matrix();
	a[3] = 100.0; // translate x
	let mut b = super::super::mathbase::identity_matrix();
	b[5] = 2.0; // scale y
	let prod = matrix_mul(a, b);
	assert_eq!(prod[5], 2.0);
	assert_eq!(prod[3], 100.0, "post-multiplied translate survives");
}

#[test]
fn input_value_changed_toggles_disable1() {
	let (mut core, behavior) = create();
	let mut b = behavior;
	b.input_value_changed(&mut core, UNIFORM_SCALE_INPUT, -1);
	let scale = core.get_input(SCALE_INPUT).unwrap();
	assert!(scale
		.properties
		.iter()
		.any(|(k, v)| { k == "disable1" && *v == NodeValue::Boolean(true) }));

	core.set_standard_value(UNIFORM_SCALE_INPUT, -1, NodeValue::Boolean(false));
	b.input_value_changed(&mut core, UNIFORM_SCALE_INPUT, -1);
	let scale = core.get_input(SCALE_INPUT).unwrap();
	assert!(scale
		.properties
		.iter()
		.any(|(k, v)| { k == "disable1" && *v == NodeValue::Boolean(false) }));
}

#[test]
fn duplicate_clones() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Orthographic Matrix");
}
