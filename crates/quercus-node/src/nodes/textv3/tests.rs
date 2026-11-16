use super::*;
use crate::handle::CHandle;
use crate::node::NodeBehavior;
use crate::nodes::textbackend::TextRenderTarget;
use crate::value::{NodeValueTable, ValueType};
use quercus_core::Rational;

#[test]
fn input_names() {
	let n = TextGeneratorV3 {
		dont_emit_valign: false,
	};
	assert_eq!(n.input_name(TEXT_INPUT), "Text");
	// REDESIGN: the plain-text input shares the "Text" display name.
	assert_eq!(n.input_name(PLAIN_TEXT_INPUT), "Text");
	assert_eq!(n.input_name(FONT_FAMILY_INPUT), "Font Family");
	assert_eq!(n.input_name(FONT_SIZE_INPUT), "Font Size");
	assert_eq!(n.input_name(OUTLINE_ENABLED_INPUT), "Outline");
	assert_eq!(n.input_name(OUTLINE_COLOR_INPUT), "Outline Color");
	assert_eq!(n.input_name(OUTLINE_WIDTH_INPUT), "Outline Width");
	assert_eq!(n.input_name(GLOW_ENABLED_INPUT), "Glow");
	assert_eq!(n.input_name(GLOW_COLOR_INPUT), "Glow Color");
	assert_eq!(n.input_name(GLOW_RADIUS_INPUT), "Glow Radius");
	assert_eq!(n.input_name(VERTICAL_ALIGNMENT_INPUT), "Vertical Alignment");
	assert_eq!(n.input_name(ARGS_INPUT), "Arguments");
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
	// The hidden use_args_in input has no display name override.
	assert_eq!(n.input_name(USE_ARGS_INPUT), USE_ARGS_INPUT);
}

#[test]
fn create_wires_inherited_and_own_inputs() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text3");
	assert_eq!(
		core.get_input(TEXT_INPUT).unwrap().value_type,
		ValueType::Text
	);
	assert!(core
		.get_input(TEXT_INPUT)
		.unwrap()
		.properties
		.iter()
		.any(|(k, v)| k == "vieweronly" && v == &NodeValue::Boolean(true)));
	let valign = core.get_input(VERTICAL_ALIGNMENT_INPUT).unwrap();
	assert_ne!(valign.flags & crate::input::flags::HIDDEN, 0);
	assert_ne!(valign.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	assert_ne!(valign.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	let use_args = core.get_input(USE_ARGS_INPUT).unwrap();
	assert_eq!(use_args.default, NodeValue::Boolean(true));
	let args = core.get_input(ARGS_INPUT).unwrap();
	assert_ne!(args.flags & crate::input::flags::ARRAY, 0);
	assert!(args
		.properties
		.iter()
		.any(|(k, v)| k == "arraystart" && v == &NodeValue::Int(1)));
	// The base has no color input of its own (ShapeNodeBase(false));
	// the font color input (REDESIGN wave 3) takes that exact slot.
	assert_eq!(
		core.get_input(crate::nodes::shapenodebase::COLOR_INPUT)
			.unwrap()
			.default,
		NodeValue::Color([1.0, 1.0, 1.0, 1.0])
	);
	assert_eq!(
		core.standard_value(crate::nodes::shapenodebase::SIZE_INPUT, -1),
		NodeValue::Vec2([400.0, 300.0])
	);
	assert_eq!(
		core.effect_input,
		crate::nodes::generatorwithmerge::BASE_INPUT
	);
	// REDESIGN: v3 left the create menu (text is a footage/clip now), so
	// this test flipped from the pre-redesign expectation (`== 0`).
	assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
	// It stays a video effect: legacy chains must keep evaluating.
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

/// The W6 contract: hidden from the add menus, still a working effect
/// node for the chains (and the footage path) that create it directly.
#[test]
fn hidden_from_create_menu_but_still_a_video_effect() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text3");
	assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
}

#[test]
fn alignment_round_trip() {
	for v in [
		VerticalAlignment::Top,
		VerticalAlignment::Middle,
		VerticalAlignment::Bottom,
	] {
		let gizmo = TextGeneratorV3::get_gizmo_alignment_from_ours(v);
		assert_eq!(TextGeneratorV3::get_our_alignment_from_gizmos(gizmo), v);
	}
	assert_eq!(
		TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Top),
		0
	);
	assert_eq!(
		TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Middle),
		2
	);
	assert_eq!(
		TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Bottom),
		1
	);
	// Unknown gizmo values map to Top.
	assert_eq!(
		TextGeneratorV3::get_our_alignment_from_gizmos(99),
		VerticalAlignment::Top
	);
}

#[test]
fn format_string_expands_args() {
	let args = vec!["foo".to_string(), "bar".to_string()];
	assert_eq!(
		TextGeneratorV3::format_string("hello %1", &args),
		"hello foo"
	);
	assert_eq!(TextGeneratorV3::format_string("%2 %1", &args), "bar foo");
	// Out of range expands to nothing.
	assert_eq!(TextGeneratorV3::format_string("[%3]", &args), "[]");
	assert_eq!(TextGeneratorV3::format_string("[%0]", &args), "[]");
}

#[test]
fn format_string_percent_escapes() {
	let args = vec!["foo".to_string()];
	assert_eq!(TextGeneratorV3::format_string("100%%", &args), "100%");
	assert_eq!(TextGeneratorV3::format_string("%%1", &args), "%1");
	// Lone % before non-digit/non-% is copied verbatim.
	assert_eq!(TextGeneratorV3::format_string("%x %", &args), "%x %");
	// Trailing % is copied verbatim.
	assert_eq!(TextGeneratorV3::format_string("end%", &args), "end%");
}

#[test]
fn format_string_out_of_int_range_fails_to_zero() {
	let args = vec!["foo".to_string()];
	assert_eq!(
		TextGeneratorV3::format_string("%99999999999999999999", &args),
		""
	);
	assert_eq!(TextGeneratorV3::format_string("%2147483648", &args), "");
	assert_eq!(TextGeneratorV3::format_string("%2147483647", &args), "");
}

#[test]
fn format_string_multidigit_and_reuse() {
	let args = vec!["a".to_string(), "b".to_string(), "c".to_string()];
	// %10 parses as index 10 (out of range with 3 args) -> empty.
	assert_eq!(TextGeneratorV3::format_string("%10", &args), "");
	assert_eq!(TextGeneratorV3::format_string("%2%2%2", &args), "bbb");
}

#[test]
fn layout_request_uses_olive_html_and_96dpi() {
	let mut row = NodeValueRow::default();
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("<p>Hi</p>".to_string()),
	);
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([400.0, 300.0]),
	);
	// The legacy path is driven explicitly: the backend state is a
	// process-global that other tests install hooks into.
	let req = TextGeneratorV3::layout_request_path(false, &row);
	assert_eq!(req.text, "<p>Hi</p>");
	assert_eq!(req.mode, TextLayoutMode::OliveHtml);
	assert_eq!(req.dots_per_meter, 3780);
	assert_eq!(req.wrap_width, 400.0);
}

#[test]
fn layout_request_plain_text_path_uses_structured_inputs() {
	let mut row = NodeValueRow::default();
	row.insert(
		PLAIN_TEXT_INPUT.to_string(),
		NodeValue::Text("hello".to_string()),
	);
	row.insert(
		FONT_FAMILY_INPUT.to_string(),
		NodeValue::StrCombo("Noto Sans".to_string()),
	);
	row.insert(FONT_SIZE_INPUT.to_string(), NodeValue::Float(48.0));
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([400.0, 300.0]),
	);
	// The row also carries legacy HTML: the plain path must ignore it.
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("<p>legacy</p>".to_string()),
	);
	let req = TextGeneratorV3::layout_request_path(true, &row);
	assert_eq!(req.text, "hello");
	assert_eq!(req.mode, TextLayoutMode::PlainText);
	assert_eq!(req.font_family, "Noto Sans");
	assert_eq!(req.font_size_pt, 48.0);
	assert_eq!(req.dots_per_meter, 3780);
	assert_eq!(req.wrap_width, 400.0);
}

#[test]
fn job_text_prefers_the_row_value() {
	let (core, _behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(
		PLAIN_TEXT_INPUT.to_string(),
		NodeValue::Text("row plain".to_string()),
	);
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("row html".to_string()),
	);
	assert_eq!(
		TextGeneratorV3::job_text(true, &core, &row, Rational::new(0, 1)),
		"row plain"
	);
	assert_eq!(
		TextGeneratorV3::job_text(false, &core, &row, Rational::new(0, 1)),
		"row html"
	);
}

#[test]
fn job_text_falls_back_to_the_core_value() {
	let (mut core, _behavior) = create();
	core.set_standard_value(
		PLAIN_TEXT_INPUT,
		-1,
		NodeValue::Text("core plain".to_string()),
	);
	core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("core html".to_string()));
	let row = NodeValueRow::default();
	assert_eq!(
		TextGeneratorV3::job_text(true, &core, &row, Rational::new(0, 1)),
		"core plain"
	);
	assert_eq!(
		TextGeneratorV3::job_text(false, &core, &row, Rational::new(0, 1)),
		"core html"
	);
}

#[test]
fn base_and_draw_offsets() {
	let size = [400.0, 300.0];
	let base = TextGeneratorV3::base_offset([0.0, 0.0], size, 1920, 1080);
	assert_eq!(base, (760.0, 390.0));
	// Top: no delta; middle/bottom use double math on doc.height.
	assert_eq!(
		TextGeneratorV3::draw_offset(VerticalAlignment::Top, base, size, 100.0),
		base
	);
	assert_eq!(
		TextGeneratorV3::draw_offset(VerticalAlignment::Middle, base, size, 100.0),
		(base.0, base.1 + 150.0 - 50.0)
	);
	assert_eq!(
		TextGeneratorV3::draw_offset(VerticalAlignment::Bottom, base, size, 100.0),
		(base.0, base.1 + 300.0 - 100.0)
	);
}

/// Serializes the tests that install a process-global text backend;
/// shared with the `textbackend` module's own tests, so no test
/// observes another module's install.
use crate::nodes::textbackend::TEST_BACKEND_LOCK as BACKEND_LOCK;

/// Measure hook for tests that only need "a backend is installed".
fn noop_measure(_req: &TextLayoutRequest) -> TextLayoutSize {
	TextLayoutSize::default()
}

#[test]
fn measure_without_backend_returns_zero_size() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(None, None);
	let mut row = NodeValueRow::default();
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("<p>Hi</p>".to_string()),
	);
	let (_req, doc) = TextGeneratorV3::measure_and_layout(&row);
	assert_eq!(doc.width, 0.0);
	assert_eq!(doc.height, 0.0);
}

#[test]
fn value_uses_plain_text_when_a_backend_is_installed() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), None);
	assert!(TextGeneratorV3::plain_text_path());

	// The row carries an empty legacy HTML only: the non-empty plain
	// text default is what makes the job push, so this fails on the
	// legacy path (empty text, no base -> empty table).
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(matches!(
		table.get(ValueType::Texture),
		Some(NodeValue::Texture(h)) if h.is_null()
	));

	// The public layout request follows the installed backend.
	let mut req_row = NodeValueRow::default();
	req_row.insert(
		PLAIN_TEXT_INPUT.to_string(),
		NodeValue::Text("hi".to_string()),
	);
	assert_eq!(
		TextGeneratorV3::layout_request(&req_row).mode,
		TextLayoutMode::PlainText
	);

	crate::nodes::textbackend::set_text_backends(None, None);
	assert!(!TextGeneratorV3::plain_text_path());
}

#[test]
fn value_pushes_job_when_text_nonempty() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(None, None);
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("<p>Hi</p>".to_string()),
	);
	row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(false));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(matches!(
		table.get(ValueType::Texture),
		Some(NodeValue::Texture(h)) if h.is_null()
	));
}

#[test]
fn value_expands_args_from_row() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(None, None);
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(
		TEXT_INPUT.to_string(),
		NodeValue::Text("Hello %1".to_string()),
	);
	row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(true));
	row.insert(ARGS_INPUT.to_string(), NodeValue::Text("World".to_string()));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(matches!(
		table.get(ValueType::Texture),
		Some(NodeValue::Texture(h)) if h.is_null()
	));
	// The expanded text is carried by the (deferred) job, which has no
	// payload here; the expansion math itself is covered by
	// format_string tests.
}

#[test]
fn value_passes_base_through_when_text_empty() {
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
	row.insert(
		crate::nodes::generatorwithmerge::BASE_INPUT.to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(matches!(
		table.get(ValueType::Texture),
		Some(NodeValue::Texture(_))
	));
}

#[test]
fn value_pushes_nothing_when_text_empty_and_no_base() {
	let (core, behavior) = create();
	let mut row = NodeValueRow::default();
	// Both text inputs are emptied explicitly: which one `value` reads
	// depends on the process-global backend state, which the installer
	// tests change and restore concurrently.
	row.insert(PLAIN_TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
	row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(table.is_empty());
}

#[test]
fn gizmo_activation_toggles_use_args() {
	let (mut core, mut behavior) = create();
	let node = behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<TextGeneratorV3>()
		.unwrap();
	node.gizmo_activated(&mut core);
	assert_eq!(
		core.standard_value(USE_ARGS_INPUT, -1),
		NodeValue::Boolean(false)
	);
	assert!(node.dont_emit_valign);
	node.gizmo_deactivated(&mut core);
	assert_eq!(
		core.standard_value(USE_ARGS_INPUT, -1),
		NodeValue::Boolean(true)
	);
	assert!(node.dont_emit_valign);
}

#[test]
fn set_vertical_alignment_undoable_maps_through_gizmo_alignment() {
	let (mut core, mut behavior) = create();
	let node = behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<TextGeneratorV3>()
		.unwrap();
	// Gizmo vcenter (2) maps back to Middle (1).
	node.set_vertical_alignment_undoable(&mut core, 2);
	assert_eq!(
		core.standard_value(VERTICAL_ALIGNMENT_INPUT, -1),
		NodeValue::Combo(1)
	);
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
	assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.text3");
	assert_eq!(copy.name(), "Text");
}

#[test]
fn redesign_inputs_have_defaults_and_flags() {
	let (core, _behavior) = create();

	let plain = core.get_input(PLAIN_TEXT_INPUT).unwrap();
	assert_eq!(plain.value_type, ValueType::Text);
	assert_eq!(
		plain.default,
		NodeValue::Text(DEFAULT_PLAIN_TEXT.to_string())
	);

	let family = core.get_input(FONT_FAMILY_INPUT).unwrap();
	assert_eq!(family.value_type, ValueType::StrCombo);
	assert_eq!(family.default, NodeValue::StrCombo(String::new()));

	let size = core.get_input(FONT_SIZE_INPUT).unwrap();
	assert_eq!(size.value_type, ValueType::Float);
	assert_eq!(size.default, NodeValue::Float(72.0));
	assert!(size
		.properties
		.iter()
		.any(|(k, v)| k == "min" && v == &NodeValue::Float(1.0)));

	let outline = core.get_input(OUTLINE_ENABLED_INPUT).unwrap();
	assert_eq!(outline.value_type, ValueType::Boolean);
	assert_eq!(outline.default, NodeValue::Boolean(false));

	let outline_color = core.get_input(OUTLINE_COLOR_INPUT).unwrap();
	assert_eq!(outline_color.value_type, ValueType::Color);
	assert_eq!(
		outline_color.default,
		NodeValue::Color([0.0, 0.0, 0.0, 1.0])
	);

	let outline_width = core.get_input(OUTLINE_WIDTH_INPUT).unwrap();
	assert_eq!(outline_width.value_type, ValueType::Float);
	assert_eq!(outline_width.default, NodeValue::Float(2.0));
	assert!(outline_width
		.properties
		.iter()
		.any(|(k, v)| k == "min" && v == &NodeValue::Float(0.0)));

	let glow = core.get_input(GLOW_ENABLED_INPUT).unwrap();
	assert_eq!(glow.value_type, ValueType::Boolean);
	assert_eq!(glow.default, NodeValue::Boolean(false));

	let glow_color = core.get_input(GLOW_COLOR_INPUT).unwrap();
	assert_eq!(glow_color.value_type, ValueType::Color);
	assert_eq!(glow_color.default, NodeValue::Color([1.0, 1.0, 0.0, 1.0]));

	let glow_radius = core.get_input(GLOW_RADIUS_INPUT).unwrap();
	assert_eq!(glow_radius.value_type, ValueType::Float);
	assert_eq!(glow_radius.default, NodeValue::Float(8.0));
	assert!(glow_radius
		.properties
		.iter()
		.any(|(k, v)| k == "min" && v == &NodeValue::Float(0.0)));

	// Every redesign input is user-facing (none hidden).
	for id in [
		PLAIN_TEXT_INPUT,
		FONT_FAMILY_INPUT,
		FONT_SIZE_INPUT,
		OUTLINE_ENABLED_INPUT,
		OUTLINE_COLOR_INPUT,
		OUTLINE_WIDTH_INPUT,
		GLOW_ENABLED_INPUT,
		GLOW_COLOR_INPUT,
		GLOW_RADIUS_INPUT,
	] {
		assert_eq!(
			core.get_input(id).unwrap().flags & crate::input::flags::HIDDEN,
			0
		);
	}

	// The legacy input keeps its default and vieweronly property, and
	// is now hidden.
	let legacy = core.get_input(TEXT_INPUT).unwrap();
	assert_eq!(
		legacy.default,
		NodeValue::Text(LEGACY_DEFAULT_TEXT_HTML.to_string())
	);
	assert_ne!(legacy.flags & crate::input::flags::HIDDEN, 0);
	assert!(legacy
		.properties
		.iter()
		.any(|(k, v)| k == "vieweronly" && v == &NodeValue::Boolean(true)));

	// The shape base still has no color input of its own
	// (ShapeNodeBase(false)); the REDESIGN wave-3 font color input
	// takes that exact slot (white default).
	assert_eq!(
		core.get_input(crate::nodes::shapenodebase::COLOR_INPUT)
			.unwrap()
			.default,
		NodeValue::Color([1.0, 1.0, 1.0, 1.0])
	);
}

#[test]
fn strip_html_to_plain_drops_tags() {
	assert_eq!(strip_html_to_plain("<p>a</p><p>b</p>"), "ab");
	assert_eq!(strip_html_to_plain("<br>"), "");
	assert_eq!(strip_html_to_plain("<p></p>"), "");
	assert_eq!(strip_html_to_plain("a<br>b"), "ab");
	// Whitespace is neither collapsed nor trimmed.
	assert_eq!(strip_html_to_plain("<p>a b</p>"), "a b");
	// A complete tag drops only itself; the text around it stays.
	assert_eq!(strip_html_to_plain("<p>abc"), "abc");
	// An unterminated tag swallows the remainder.
	assert_eq!(strip_html_to_plain("a<b"), "a");
	assert_eq!(strip_html_to_plain("<p"), "");
}

#[test]
fn strip_html_to_plain_decodes_entities() {
	assert_eq!(
		strip_html_to_plain("a &amp;&lt;&gt;&quot;&#39;&nbsp;b"),
		"a &<>\"' b"
	);
	// A bare '&' and unknown entities are kept verbatim, one pass only.
	assert_eq!(strip_html_to_plain("a & b &fake; c"), "a & b &fake; c");
	assert_eq!(strip_html_to_plain("&amp;amp;"), "&amp;");
	// Entities are decoded after tags are dropped: an encoded tag stays
	// literal text.
	assert_eq!(strip_html_to_plain("&lt;p&gt;"), "<p>");
}

#[test]
fn migrate_legacy_html_moves_stripped_text_once() {
	let (mut core, _behavior) = create();
	core.set_standard_value(
		TEXT_INPUT,
		-1,
		NodeValue::Text("<p>Hello <b>World</b></p>".to_string()),
	);
	assert!(migrate_legacy_html(&mut core));
	assert_eq!(
		core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text("Hello World".to_string())
	);
	// The legacy value is left untouched and further calls are no-ops.
	assert_eq!(
		core.standard_value(TEXT_INPUT, -1),
		NodeValue::Text("<p>Hello <b>World</b></p>".to_string())
	);
	assert!(!migrate_legacy_html(&mut core));
	assert_eq!(
		core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text("Hello World".to_string())
	);
}

#[test]
fn migrate_legacy_html_leaves_defaults_and_edits_alone() {
	let (mut core, _behavior) = create();
	// A node at its defaults: the legacy default HTML is not migrated
	// (a new node must keep the redesign default across a save/load).
	assert!(!migrate_legacy_html(&mut core));
	assert_eq!(
		core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text(DEFAULT_PLAIN_TEXT.to_string())
	);

	// An edited plain text is never overwritten.
	core.set_standard_value(PLAIN_TEXT_INPUT, -1, NodeValue::Text("mine".to_string()));
	core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("<p>legacy</p>".to_string()));
	assert!(!migrate_legacy_html(&mut core));
	assert_eq!(
		core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text("mine".to_string())
	);

	// A legacy HTML with no text content migrates nothing.
	core.set_standard_value(PLAIN_TEXT_INPUT, -1, NodeValue::Text(String::new()));
	core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("<p></p>".to_string()));
	assert!(!migrate_legacy_html(&mut core));
	assert_eq!(
		core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text(String::new())
	);
}

#[test]
fn migrate_legacy_html_noop_without_plain_text_input() {
	// A pre-redesign core (no plain_text_in at all) must not panic.
	let mut core = NodeCore::new();
	core.add_input(crate::input::Input::new(
		TEXT_INPUT,
		ValueType::Text,
		NodeValue::Text("<p>x</p>".to_string()),
	));
	assert!(!migrate_legacy_html(&mut core));
}

/// Render hook for the post-process tests: paints the middle half of
/// the target (`x`, `y` in `[dim / 4, 3 * dim / 4)`) solid white — a
/// coverage block whose dilation and box blur are exactly computable.
fn solid_render(
	_req: &TextLayoutRequest,
	_transform: &TextRenderTransform,
	target: TextRenderTarget,
) {
	if target.channel_count != 4 {
		return;
	}
	let stride = target.linesize_bytes as usize;
	let (w, h) = (target.width as usize, target.height as usize);
	for y in h / 4..3 * h / 4 {
		for x in w / 4..3 * w / 4 {
			let at = y * stride + x * 4;
			target.data[at..at + 4].copy_from_slice(&[255; 4]);
		}
	}
}

/// The evaluation row of the post-process tests: a 16x16 raster with a
/// 2-pixel black outline and/or a 4-pixel yellow glow.
fn post_row(outline: bool, glow: bool) -> NodeValueRow {
	let mut row = NodeValueRow::new();
	row.insert(
		PLAIN_TEXT_INPUT.to_string(),
		NodeValue::Text("X".to_string()),
	);
	row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(false));
	row.insert(
		crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
		NodeValue::Vec2([16.0, 16.0]),
	);
	row.insert(
		OUTLINE_ENABLED_INPUT.to_string(),
		NodeValue::Boolean(outline),
	);
	row.insert(
		OUTLINE_COLOR_INPUT.to_string(),
		NodeValue::Color([0.0, 0.0, 0.0, 1.0]),
	);
	row.insert(OUTLINE_WIDTH_INPUT.to_string(), NodeValue::Float(2.0));
	row.insert(GLOW_ENABLED_INPUT.to_string(), NodeValue::Boolean(glow));
	row.insert(
		GLOW_COLOR_INPUT.to_string(),
		NodeValue::Color([1.0, 1.0, 0.0, 1.0]),
	);
	row.insert(GLOW_RADIUS_INPUT.to_string(), NodeValue::Float(4.0));
	row
}

/// Evaluate [`TextGeneratorV3::value`] and return the texture handle it
/// pushed.
fn push_value(core: &NodeCore, behavior: &dyn NodeBehavior, row: &NodeValueRow) -> CHandle {
	let mut table = NodeValueTable::default();
	behavior.value(core, row, Rational::new(0, 1), &mut table);
	match table.get(ValueType::Texture) {
		Some(NodeValue::Texture(handle)) => *handle,
		other => panic!("expected a texture row, got {other:?}"),
	}
}

/// The job payload boxed by a deferred texture handle.
fn job_of(handle: &CHandle) -> &ShaderJobPayload {
	unsafe { crate::jobs::shader_job(handle) }.expect("handle carries a ShaderJobPayload")
}

/// The shader id of the pass a deferred texture handle runs.
fn shader_id_of(handle: &CHandle) -> &str {
	&job_of(handle).shader_id
}

/// A texture-typed job param (the effect input or a merge layer).
fn param_texture<'a>(handle: &'a CHandle, input: &str) -> &'a CHandle {
	match job_of(handle).params.get(input) {
		Some(NodeValue::Texture(tex)) => tex,
		other => panic!("param {input:?} is not a texture: {other:?}"),
	}
}

/// A float job param.
fn param_float(handle: &CHandle, input: &str) -> f64 {
	match job_of(handle).params.get(input) {
		Some(NodeValue::Float(f)) => *f,
		other => panic!("param {input:?} is not a float: {other:?}"),
	}
}

/// A color job param.
fn param_color(handle: &CHandle, input: &str) -> [f64; 4] {
	match job_of(handle).params.get(input) {
		Some(NodeValue::Color(c)) => *c,
		other => panic!("param {input:?} is not a color: {other:?}"),
	}
}

/// A vec2 job param.
fn param_vec2(handle: &CHandle, input: &str) -> [f64; 2] {
	match job_of(handle).params.get(input) {
		Some(NodeValue::Vec2(v)) => *v,
		other => panic!("param {input:?} is not a vec2: {other:?}"),
	}
}

/// Identity of the refcounted box behind a handle: every clone of a
/// job param addrefs the same box, so equal pointers mean "the same
/// texture was fed to both passes".
fn job_ptr(handle: &CHandle) -> usize {
	handle.ctx as usize
}

/// Assert `handle` boxes the 16x16 CPU coverage frame the rasterizer
/// staging-allocates (not a shader job).
fn assert_cpu_texture(handle: &CHandle) {
	match unsafe { crate::handle::get_checked::<Texture>(handle) } {
		Some(Texture::Cpu(frame)) => assert_eq!((frame.width, frame.height), (16, 16)),
		other => panic!("expected a CPU coverage frame, got {other:?}"),
	}
}

/// One pixel of a CPU coverage frame's F32 RGBA data.
fn pixel_of(handle: &CHandle, x: usize, y: usize) -> [f32; 4] {
	let Some(Texture::Cpu(frame)) = (unsafe { crate::handle::get_checked::<Texture>(handle) })
	else {
		panic!("expected a CPU coverage frame");
	};
	let stride = frame.linesize_bytes();
	let at = y * stride + x * 16;
	let mut out = [0f32; 4];
	for (c, v) in out.iter_mut().enumerate() {
		*v = f32::from_le_bytes(frame.data[at + c * 4..at + c * 4 + 4].try_into().unwrap());
	}
	out
}

#[test]
fn post_job_off_still_rasterizes_the_plain_text() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
	let (core, behavior) = create();
	let row = post_row(false, false);
	let handle = push_value(&core, behavior.as_ref(), &row);
	crate::nodes::textbackend::set_text_backends(None, None);
	// Both passes off with a backend installed: the plain (tinted)
	// raster, not the pre-backend deferred null job.
	assert_cpu_texture(&handle);
	assert_eq!(pixel_of(&handle, 8, 8), [1.0, 1.0, 1.0, 1.0]);
	assert_eq!(pixel_of(&handle, 0, 0), [0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn font_color_tints_the_raster_premultiplied() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
	let (core, behavior) = create();
	let mut row = post_row(false, false);
	row.insert(
		COLOR_INPUT.to_string(),
		NodeValue::Color([1.0, 0.0, 0.0, 0.5]),
	);
	let handle = push_value(&core, behavior.as_ref(), &row);
	crate::nodes::textbackend::set_text_backends(None, None);
	assert_cpu_texture(&handle);
	// The white coverage scales per channel, alpha included.
	assert_eq!(pixel_of(&handle, 8, 8), [1.0, 0.0, 0.0, 0.5]);
	assert_eq!(pixel_of(&handle, 0, 0), [0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn outline_chain_dilates_then_colorizes_over_the_text() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
	let (core, behavior) = create();
	let row = post_row(true, false);
	let handle = push_value(&core, behavior.as_ref(), &row);
	crate::nodes::textbackend::set_text_backends(None, None);

	// The pushed chain is the stroke merged with the text on top: the
	// text is the merge's top (`blend_in`) layer, its CPU coverage
	// raster the head of that branch.
	assert_eq!(shader_id_of(&handle), "mrg");
	let stroke = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
	let text = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
	assert_cpu_texture(text);

	// The stroke is the colorized dilation of the raster.
	assert_eq!(shader_id_of(stroke), OUTLINE_COLORIZE_SHADER_ID);
	assert_eq!(
		param_color(stroke, OUTLINE_COLOR_INPUT),
		[0.0, 0.0, 0.0, 1.0]
	);
	assert_eq!(param_vec2(stroke, RESOLUTION_INPUT), [16.0, 16.0]);
	let dilated = param_texture(stroke, POST_TEXTURE_INPUT);
	assert_eq!(shader_id_of(dilated), OUTLINE_DILATE_SHADER_ID);
	assert_eq!(job_of(dilated).iterations, 1);
	assert_eq!(param_float(dilated, OUTLINE_WIDTH_INPUT), 2.0);
	assert_eq!(param_vec2(dilated, RESOLUTION_INPUT), [16.0, 16.0]);
	assert_eq!(
		job_ptr(param_texture(dilated, POST_TEXTURE_INPUT)),
		job_ptr(text)
	);
}

#[test]
fn glow_chain_blurs_once_per_axis_then_colorizes() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
	let (core, behavior) = create();
	let row = post_row(false, true);
	let handle = push_value(&core, behavior.as_ref(), &row);
	crate::nodes::textbackend::set_text_backends(None, None);

	// Glow only: the glow is merged beneath the bare text.
	assert_eq!(shader_id_of(&handle), "mrg");
	let glow = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
	let text = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
	assert_cpu_texture(text);

	assert_eq!(shader_id_of(glow), GLOW_COLORIZE_SHADER_ID);
	assert_eq!(param_color(glow, GLOW_COLOR_INPUT), [1.0, 1.0, 0.0, 1.0]);
	assert_eq!(param_vec2(glow, RESOLUTION_INPUT), [16.0, 16.0]);
	let blurred = param_texture(glow, POST_TEXTURE_INPUT);
	assert_eq!(shader_id_of(blurred), GLOW_BLUR_SHADER_ID);
	// Two iterations: one per axis (horizontal, then vertical).
	assert_eq!(job_of(blurred).iterations, 2);
	assert_eq!(param_float(blurred, GLOW_RADIUS_INPUT), 4.0);
	assert_eq!(param_vec2(blurred, RESOLUTION_INPUT), [16.0, 16.0]);
	assert_eq!(
		job_ptr(param_texture(blurred, POST_TEXTURE_INPUT)),
		job_ptr(text)
	);
}

#[test]
fn outline_and_glow_glow_the_stroke() {
	let _guard = BACKEND_LOCK.lock().unwrap();
	crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
	let (core, behavior) = create();
	let row = post_row(true, true);
	let handle = push_value(&core, behavior.as_ref(), &row);
	crate::nodes::textbackend::set_text_backends(None, None);

	// Both on: the glow is drawn over the stroke (which is drawn over
	// the text), and it samples the stroke itself — the blur's input
	// is the stroke's merge job, not the bare coverage raster.
	assert_eq!(shader_id_of(&handle), "mrg");
	let glow = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
	let stroke = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
	assert_eq!(shader_id_of(stroke), "mrg");

	let stroke_colorized = param_texture(stroke, crate::nodes::merge::BASE_INPUT);
	assert_eq!(shader_id_of(stroke_colorized), OUTLINE_COLORIZE_SHADER_ID);
	assert_cpu_texture(param_texture(stroke, crate::nodes::merge::BLEND_INPUT));

	let blurred = param_texture(glow, POST_TEXTURE_INPUT);
	assert_eq!(shader_id_of(blurred), GLOW_BLUR_SHADER_ID);
	assert_eq!(
		job_ptr(param_texture(blurred, POST_TEXTURE_INPUT)),
		job_ptr(stroke)
	);
}
