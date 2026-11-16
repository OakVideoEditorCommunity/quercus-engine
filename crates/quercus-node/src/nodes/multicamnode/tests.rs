use super::*;
use crate::id::NodeId;
use crate::node::NodeBehavior;
use crate::value::{NodeValueTable, ValueType};
use quercus_core::Rational;

/// A distinct, valid-looking node id for sequence connect tests.
fn fake_id(n: u32) -> NodeId {
	NodeId::from_identity(n as u64).unwrap()
}

#[test]
fn input_names() {
	let n = MultiCamNode { sequence: None };
	assert_eq!(n.input_name(CURRENT_INPUT), "Current");
	assert_eq!(n.input_name(SOURCES_INPUT), "Sources");
	assert_eq!(n.input_name(SEQUENCE_INPUT), "Sequence");
	assert_eq!(n.input_name(SEQUENCE_TYPE_INPUT), "Sequence Type");
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inputs() {
	let (core, behavior) = create();
	assert_eq!(
		NodeBehavior::type_id(&*behavior),
		"org.olivevideoeditor.Olive.multicam"
	);
	assert_eq!(
		core.get_input(CURRENT_INPUT).unwrap().value_type,
		ValueType::Combo
	);
	assert_eq!(
		core.get_input(CURRENT_INPUT).unwrap().default,
		NodeValue::Combo(0)
	);
	let sources = core.get_input(SOURCES_INPUT).unwrap();
	assert_ne!(sources.flags & crate::input::flags::ARRAY, 0);
	assert_ne!(sources.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	assert!(sources
		.properties
		.iter()
		.any(|(k, v)| k == "arraystart" && v == &NodeValue::Int(1)));
	assert_ne!(
		core.get_input(SEQUENCE_INPUT).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
		0
	);
	let seq_type = core.get_input(SEQUENCE_TYPE_INPUT).unwrap();
	assert_ne!(seq_type.flags & crate::input::flags::HIDDEN, 0);
	assert_ne!(seq_type.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	assert_ne!(seq_type.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
}

#[test]
fn current_source_reads_standard_value() {
	let (mut core, _) = create();
	let node = MultiCamNode { sequence: None };
	assert_eq!(node.current_source(&core), 0);
	core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(2));
	assert_eq!(node.current_source(&core), 2);
}

#[test]
fn source_count_is_array_size() {
	let (mut core, _) = create();
	// Grow the sources array to 3 elements.
	core.input_array_insert(SOURCES_INPUT, 0);
	core.input_array_insert(SOURCES_INPUT, 1);
	core.input_array_insert(SOURCES_INPUT, 2);
	let node = MultiCamNode { sequence: None };
	assert_eq!(node.source_count(&core), 3);
}

#[test]
fn rows_and_columns_square_growth() {
	assert_eq!(MultiCamNode::rows_and_columns(0), (1, 1));
	assert_eq!(MultiCamNode::rows_and_columns(1), (1, 1));
	assert_eq!(MultiCamNode::rows_and_columns(2), (1, 2));
	assert_eq!(MultiCamNode::rows_and_columns(3), (2, 2));
	assert_eq!(MultiCamNode::rows_and_columns(4), (2, 2));
	// The smaller dimension grows first: 1x1 -> 1x2 -> 2x2 -> 2x3.
	assert_eq!(MultiCamNode::rows_and_columns(5), (2, 3));
	assert_eq!(MultiCamNode::rows_and_columns(6), (2, 3));
	assert_eq!(MultiCamNode::rows_and_columns(9), (3, 3));
}

#[test]
fn index_row_cols_round_trip() {
	for (i, rows, cols) in [
		(0, 3, 3),
		(1, 3, 3),
		(2, 3, 3),
		(3, 3, 3),
		(8, 3, 3),
		(5, 2, 3),
	] {
		let (r, c) = MultiCamNode::index_to_row_cols(i, rows, cols);
		assert_eq!(r, i / cols);
		assert_eq!(c, i % cols);
		assert_eq!(MultiCamNode::rows_cols_to_index(r, c, rows, cols), i);
	}
}

#[test]
fn active_elements_selects_current_source() {
	let (mut core, _) = create();
	core.input_array_insert(SOURCES_INPUT, 0);
	core.input_array_insert(SOURCES_INPUT, 1);
	core.input_array_insert(SOURCES_INPUT, 2);
	let node = MultiCamNode { sequence: None };
	core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(1));
	assert_eq!(node.active_elements(&core), vec![1]);
	// Out of range -> no active elements.
	core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(9));
	assert!(node.active_elements(&core).is_empty());
}

#[test]
fn ignore_inputs_always_sequence() {
	let node = MultiCamNode { sequence: None };
	assert_eq!(
		node.ignore_inputs_for_rendering(),
		&[SEQUENCE_INPUT.to_string()]
	);
}

#[test]
fn value_pushes_connected_source_value() {
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(SOURCES_INPUT.to_string(), NodeValue::Float(7.0));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert_eq!(
		table.get(crate::value::ValueType::Float),
		Some(&NodeValue::Float(7.0))
	);
}

#[test]
fn value_pushes_nothing_when_sources_empty() {
	let (core, behavior) = create();
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert!(table.is_empty());
}

#[test]
fn sequence_connect_toggles_hidden_flag() {
	let (mut core, mut behavior) = create();
	assert_ne!(
		core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
		0
	);
	let seq = fake_id(7);
	behavior.input_connected(&mut core, SEQUENCE_INPUT, -1, seq);
	assert_eq!(
		core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
		0
	);
	behavior.input_disconnected(&mut core, SEQUENCE_INPUT, -1, seq);
	assert_ne!(
		core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
		0
	);
}

#[test]
fn duplicate_copies_sequence() {
	let seq = fake_id(7);
	let node = MultiCamNode {
		sequence: Some(seq),
	};
	let copy = node.duplicate(&NodeCore::new()).unwrap();
	assert_eq!(
		NodeBehavior::type_id(&*copy),
		"org.olivevideoeditor.Olive.multicam"
	);
	let down = copy
		.as_any()
		.unwrap()
		.downcast_ref::<MultiCamNode>()
		.unwrap();
	assert_eq!(down.sequence, Some(seq));
}
