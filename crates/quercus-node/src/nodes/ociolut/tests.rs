use super::*;
use crate::value::{NodeValue, NodeValueTable, ValueType};
use quercus_core::Rational;

fn node() -> OCIOLutNode {
	OCIOLutNode {
		base: OcioBase::new(),
		state: Mutex::new(ProcessorState::default()),
	}
}

#[test]
fn input_names() {
	let n = node();
	assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
	assert_eq!(n.input_name(FILE_INPUT), "LUT File");
	assert_eq!(n.input_name(DIRECTION_INPUT), "Direction");
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inputs_flags_and_properties() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ociolut");
	assert_ne!(
		core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
			.unwrap()
			.flags & crate::input::flags::NOT_KEYFRAMABLE,
		0
	);
	let file = core.get_input(FILE_INPUT).unwrap();
	assert_eq!(file.default, NodeValue::Text(String::new()));
	assert_ne!(file.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	assert_ne!(file.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	// `*.*` filter fallback without the render bridge.
	assert!(file
		.properties
		.iter()
		.any(|(k, v)| k == "filter"
			&& *v == NodeValue::Text("LUT Files (*.*);;All Files (*)".into())));
	assert!(file
		.properties
		.iter()
		.any(|(k, v)| k == "placeholder" && *v == NodeValue::Text("Select a LUT file".into())));
	assert!(file
		.properties
		.iter()
		.any(|(k, v)| k == "lut_library" && *v == NodeValue::Boolean(true)));
	let dir = core.get_input(DIRECTION_INPUT).unwrap();
	assert_eq!(dir.default, NodeValue::Combo(0));
	assert_ne!(dir.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	assert_ne!(dir.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

#[test]
fn read_direction_input_parses_combo_and_legacy_strings() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		DIRECTION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));
	assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
	assert_eq!(OCIOLutNode::read_direction_input(&core), 1);
	// Legacy string storage (old serializers).
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("forward".into()));
	assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("Inverse".into()));
	assert_eq!(OCIOLutNode::read_direction_input(&core), 1);
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("0".into()));
	assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
}

#[test]
fn last_error_roundtrip_and_set_once() {
	let n = node();
	assert_eq!(n.last_error(), "");
	n.set_last_error("OCIO LUT: file does not exist: /nope.cube");
	assert_eq!(n.last_error(), "OCIO LUT: file does not exist: /nope.cube");
	// No-op when unchanged.
	n.set_last_error("OCIO LUT: file does not exist: /nope.cube");
	assert_eq!(n.last_error(), "OCIO LUT: file does not exist: /nope.cube");
	n.set_last_error("");
	assert_eq!(n.last_error(), "");
}

#[test]
fn create_processor_from_inputs_resets_markers_without_lut() {
	let n = node();
	let core = NodeCore::new();
	{
		let mut state = n.state.lock().unwrap();
		state.dirty = true;
		state.last_path = "/tmp/foo.cube".to_string();
		state.last_direction = 1;
		state.last_processor = Some(std::sync::Arc::new(
			quercus_core::color::ColorProcessor::pass_through(),
		));
		state.last_error = "stale error".to_string();
	}
	// No FILE_INPUT on the core -> empty path branch: markers reset
	// and the stale error clears (no LUT selected is not an error).
	let created = n.create_processor_from_inputs(&core);
	assert!(!created);
	let state = n.state.lock().unwrap();
	assert!(!state.dirty);
	assert_eq!(state.last_path, "");
	assert_eq!(state.last_direction, -1);
	assert!(state.last_processor.is_none());
	assert_eq!(state.last_error, "");
}

#[test]
fn ensure_processor_rebuilds_when_no_cached_processor() {
	let n = node();
	let core = NodeCore::new();
	// Fresh state is dirty -> rebuild (resets markers, returns false).
	n.ensure_processor(&core);
	let state = n.state.lock().unwrap();
	assert!(!state.dirty);
	assert!(state.last_processor.is_none());
}

#[test]
fn input_value_changed_marks_dirty() {
	let mut core = NodeCore::new();
	let mut n = node();
	n.state.lock().unwrap().dirty = false;
	n.input_value_changed(&mut core, FILE_INPUT, 0);
	assert!(n.state.lock().unwrap().dirty);
	n.state.lock().unwrap().dirty = false;
	n.input_value_changed(&mut core, DIRECTION_INPUT, 0);
	assert!(n.state.lock().unwrap().dirty);
	// Unrelated inputs are ignored.
	n.state.lock().unwrap().dirty = false;
	n.input_value_changed(&mut core, crate::nodes::ociobase::TEXTURE_INPUT, 0);
	assert!(!n.state.lock().unwrap().dirty);
}

#[test]
fn config_changed_marks_dirty() {
	let mut core = NodeCore::new();
	let mut n = node();
	n.state.lock().unwrap().dirty = false;
	n.config_changed(&mut core);
	assert!(n.state.lock().unwrap().dirty);
}

#[test]
fn file_path_reads_text_standard_value() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		FILE_INPUT,
		crate::value::ValueType::Text,
		crate::value::NodeValue::Text(String::new()),
	));
	core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
	assert_eq!(OCIOLutNode::file_path(&core), "/tmp/x.cube");
	// Non-text standard value (or missing input) yields empty.
	core.set_standard_value(FILE_INPUT, -1, NodeValue::Float(3.0));
	assert_eq!(OCIOLutNode::file_path(&core), "");
}

#[test]
fn ensure_processor_reuses_cached_when_unchanged() {
	let n = node();
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		FILE_INPUT,
		crate::value::ValueType::Text,
		crate::value::NodeValue::Text(String::new()),
	));
	core.add_input(crate::input::Input::new(
		DIRECTION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));
	core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
	{
		let mut state = n.state.lock().unwrap();
		state.dirty = false;
		state.last_path = "/tmp/x.cube".to_string();
		state.last_direction = 1;
		state.last_processor = Some(std::sync::Arc::new(
			quercus_core::color::ColorProcessor::pass_through(),
		));
	}
	// Unchanged path+direction with a live processor: early return,
	// markers are preserved (create_processor_from_inputs would have
	// reset them).
	n.ensure_processor(&core);
	let state = n.state.lock().unwrap();
	assert_eq!(state.last_path, "/tmp/x.cube");
	assert_eq!(state.last_direction, 1);
	assert!(state.last_processor.is_some());
}

#[test]
fn generate_processor_wraps_ensure_processor() {
	let mut n = node();
	let mut core = NodeCore::new();
	n.state.lock().unwrap().dirty = true;
	// Worker branch: generate_processor just ensures; markers reset.
	n.generate_processor(&mut core);
	let state = n.state.lock().unwrap();
	assert!(!state.dirty);
	assert_eq!(state.last_path, "");
	assert_eq!(state.last_direction, -1);
}

#[test]
fn is_main_process_always_false_without_bridge() {
	assert!(!OCIOLutNode::is_main_process());
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
fn value_passes_texture_through_without_processor() {
	// Without the render bridge no LUT processor can be created, so
	// the base value() passes the input texture through unchanged.
	let core = NodeCore::new();
	let n = node();
	let tex = NodeValue::Texture(crate::handle::CHandle::null());
	let inputs = crate::value::NodeValueRow::from([(
		crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
		tex.clone(),
	)]);
	let mut table = NodeValueTable::default();
	n.value(&core, &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.get(ValueType::Texture), Some(&tex));
}

#[test]
fn value_pushes_color_transform_job_with_processor() {
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		FILE_INPUT,
		crate::value::ValueType::Text,
		crate::value::NodeValue::Text(String::new()),
	));
	core.add_input(crate::input::Input::new(
		DIRECTION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));
	core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
	core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
	let n = node();
	let processor = std::sync::Arc::new(quercus_core::color::ColorProcessor::pass_through());
	{
		// Prime the cache so value()'s ensure_processor early-returns
		// (unchanged path/direction, live cached processor).
		let mut state = n.state.lock().unwrap();
		state.dirty = false;
		state.last_path = "/tmp/x.cube".to_string();
		state.last_direction = 1;
		state.last_processor = Some(processor.clone());
	}
	n.base.set_processor(Some(processor));
	let inputs = crate::value::NodeValueRow::from([(
		crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	)]);
	let mut table = NodeValueTable::default();
	n.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
		panic!("job row expected");
	};
	assert!(unsafe { crate::jobs::color_transform_job(handle) }.is_some());
}

#[test]
fn duplicate_clones() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "OCIO LUT");
}
