use super::*;
use crate::node::NodeBehavior;
use crate::value::{NodeValueTable, ValueType};
use quercus_core::Rational;

#[test]
fn input_names() {
	let n = TextGeneratorV2;
	assert_eq!(n.input_name(TEXT_INPUT), "Text");
	assert_eq!(n.input_name(HTML_INPUT), "Enable HTML");
	assert_eq!(n.input_name(V_ALIGN_INPUT), "Vertical Align");
	assert_eq!(n.input_name(FONT_INPUT), "Font");
	assert_eq!(n.input_name(FONT_SIZE_INPUT), "Font Size");
	assert_eq!(
		n.input_name(crate::nodes::generatorwithmerge::BASE_INPUT),
		"Base"
	);
	assert_eq!(
		n.input_name(crate::nodes::shapenodebase::POSITION_INPUT),
		"Position"
	);
	assert_eq!(
		n.input_name(crate::nodes::shapenodebase::SIZE_INPUT),
		"Size"
	);
	assert_eq!(
		n.input_name(crate::nodes::shapenodebase::COLOR_INPUT),
		"Color"
	);
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inherited_and_own_inputs() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text2");
	assert_eq!(
		core.get_input(TEXT_INPUT).unwrap().value_type,
		ValueType::Text
	);
	assert_eq!(
		core.get_input(V_ALIGN_INPUT).unwrap().default,
		NodeValue::Combo(0)
	);
	assert_eq!(
		core.effect_input,
		crate::nodes::generatorwithmerge::BASE_INPUT
	);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
	// Inherited standard-value overrides.
	assert_eq!(
		core.standard_value(crate::nodes::shapenodebase::COLOR_INPUT, -1),
		NodeValue::Color([1.0, 1.0, 1.0, 1.0])
	);
	assert_eq!(
		core.standard_value(crate::nodes::shapenodebase::SIZE_INPUT, -1),
		NodeValue::Vec2([400.0, 300.0])
	);
}

#[test]
fn layout_request_uses_shape_size_and_72dpi() {
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
	row.insert(FONT_SIZE_INPUT.to_string(), NodeValue::Float(36.0));
	row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(false));
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([400.0, 300.0]),
	);
	let req = TextGeneratorV2::layout_request(&row);
	assert_eq!(req.text, "Hi");
	assert_eq!(req.font_size_pt, 36.0);
	assert_eq!(req.dots_per_meter, 2835);
	assert_eq!(req.wrap_width, 400.0);
	assert!(!req.center_horizontally);
}

#[test]
fn layout_request_html_translates_newlines() {
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text("a\nb".to_string()));
	row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(true));
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([100.0, 100.0]),
	);
	let req = TextGeneratorV2::layout_request(&row);
	assert_eq!(req.text, "a<br>b");
	assert_eq!(req.mode, TextLayoutMode::Html);
}

#[test]
fn base_and_draw_offsets() {
	let size = [400.0, 300.0];
	let pos = [0.0, 0.0];
	let base = TextGeneratorV2::base_offset(pos, size, 1920, 1080);
	assert_eq!(base, (0.0 - 200.0 + 960.0, 0.0 - 150.0 + 540.0));
	// Top: no vertical delta.
	assert_eq!(TextGeneratorV2::draw_offset(0, base, size, 100), base);
	// Center: size.y/2 - doc_height/2 (integer halving of doc height).
	assert_eq!(
		TextGeneratorV2::draw_offset(1, base, size, 100),
		(base.0, base.1 + 150.0 - 50.0)
	);
	// Bottom: size.y - doc_height.
	assert_eq!(
		TextGeneratorV2::draw_offset(2, base, size, 100),
		(base.0, base.1 + 300.0 - 100.0)
	);
}

#[test]
fn render_transform_clips_to_shape_rect() {
	let size = [400.0, 300.0];
	let base = (100.0, 200.0);
	let t = TextGeneratorV2::render_transform(0.5, base, base, size);
	assert_eq!(t.scale, 0.5);
	assert_eq!(t.draw_offset_x, 100.0);
	assert_eq!(t.draw_offset_y, 200.0);
	assert!(t.clip_enabled);
	assert_eq!(t.clip_offset_x, 100.0);
	assert_eq!(t.clip_offset_y, 200.0);
	assert_eq!(t.clip_width, 400.0);
	assert_eq!(t.clip_height, 300.0);
}

#[test]
fn measure_without_backend_returns_zero_size() {
	crate::nodes::textbackend::set_text_backends(None, None);
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
	row.insert(
		crate::nodes::shapenodebase::POSITION_INPUT.to_string(),
		NodeValue::Vec2([0.0, 0.0]),
	);
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([400.0, 300.0]),
	);
	row.insert(V_ALIGN_INPUT.to_string(), NodeValue::Combo(1));
	let (_req, doc, base, draw) = TextGeneratorV2::measure_and_layout(&row, 1920, 1080);
	assert_eq!(doc.width, 0.0);
	assert_eq!(doc.height, 0.0);
	assert_eq!(base, (760.0, 390.0));
	assert_eq!(draw, (760.0, 390.0 + 150.0));
}

#[test]
fn value_pushes_job_when_text_nonempty() {
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(matches!(
		table.get(ValueType::Texture),
		Some(NodeValue::Texture(h)) if h.is_null()
	));
}

#[test]
fn value_pushes_nothing_when_text_empty() {
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(table.is_empty());
}

#[test]
fn generate_frame_is_documented_noop() {
	let (core, behavior) = create();
	let mut frame = crate::handle::CHandle::null();
	behavior.generate_frame(&core, &mut frame, Rational::new(0, 1));
	assert!(frame.is_null());
}

#[test]
fn duplicate_copies_node() {
	let (_core, behavior) = create();
	let copy = behavior.duplicate(&_core).unwrap();
	assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.text2");
	assert_eq!(copy.name(), "Text (Legacy)");
}
