use super::*;
use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
use quercus_core::Rational;

fn tex() -> NodeValue {
	NodeValue::Texture(crate::handle::CHandle::null())
}

fn run(core: &NodeCore, inputs: &NodeValueRow) -> ShaderJobPayload {
	let behavior = TransitionNode;
	let mut table = NodeValueTable::default();
	behavior.value(core, inputs, Rational::new(0, 1), &mut table);
	let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
		panic!("shader job expected");
	};
	unsafe { crate::jobs::shader_job(h) }
		.expect("shader job payload boxed")
		.clone()
}

fn both() -> NodeValueRow {
	NodeValueRow::from([
		(TEXTURE_INPUT.to_string(), tex()),
		(BLEND_INPUT.to_string(), tex()),
	])
}

#[test]
fn input_names() {
	let n = TransitionNode;
	assert_eq!(n.input_name(TEXTURE_INPUT), "From");
	assert_eq!(n.input_name(BLEND_INPUT), "To");
	assert_eq!(n.input_name(PROGRESS_INPUT), "Progress");
	assert_eq!(n.input_name(TYPE_INPUT), "Type");
	assert_eq!(n.input_name("other"), "other");
}

#[test]
fn create_wires_inputs_and_flags() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.transition");
	assert_eq!(behavior.name(), "Transition");
	assert_eq!(behavior.categories(), &[Category::Timeline]);
	assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	assert_eq!(core.effect_input, "");

	for id in [TEXTURE_INPUT, BLEND_INPUT] {
		let input = core.get_input(id).expect("texture input");
		assert_ne!(input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	}

	let progress = core.get_input(PROGRESS_INPUT).expect("progress input");
	assert_eq!(progress.default, NodeValue::Float(0.0));
	assert!(progress
		.properties
		.iter()
		.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
	assert!(progress
		.properties
		.iter()
		.any(|(k, v)| k == "max" && *v == NodeValue::Float(1.0)));

	let ty = core.get_input(TYPE_INPUT).expect("type input");
	assert_eq!(ty.default, NodeValue::Combo(0));
	assert_ne!(ty.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	assert_eq!(
		ty.properties.first(),
		Some(&(
			"combobox_strings".to_string(),
			NodeValue::Binary("Cross Dissolve,Fade,Wipe,Slide".as_bytes().to_vec())
		))
	);
}

#[test]
fn shader_ids_cover_every_type_name() {
	assert_eq!(TYPE_NAMES.len(), SHADER_IDS.len());
	assert_eq!(shader_id_for(0), "crossdissolve");
	assert_eq!(shader_id_for(3), "slide");
	assert_eq!(shader_id_for(-1), "crossdissolve");
	assert_eq!(shader_id_for(7), "slide");
}

#[test]
fn value_no_texture_pushes_nothing() {
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
fn value_tex_only_pushes_tex() {
	let (core, behavior) = create();
	let input = tex();
	let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), input.clone())]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.get(ValueType::Texture), Some(&input));
}

#[test]
fn value_blend_only_pushes_blend() {
	let (core, behavior) = create();
	let blend = tex();
	let inputs = NodeValueRow::from([(BLEND_INPUT.to_string(), blend.clone())]);
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.get(ValueType::Texture), Some(&blend));
}

#[test]
fn value_both_pushes_job_payload_with_both_textures() {
	let (core, _) = create();
	let payload = run(&core, &both());
	assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.transition");
	assert_eq!(
		payload.shader_id, "crossdissolve",
		"combo default is index 0"
	);
	assert_eq!(payload.iterations, 1);
	assert_eq!(payload.iterative_input, "");
	assert!(payload.params.contains_key(TEXTURE_INPUT));
	assert!(payload.params.contains_key(BLEND_INPUT));
	assert_eq!(
		payload.params.get(PROGRESS_INPUT),
		Some(&NodeValue::Float(0.0)),
		"the progress factor rides in the job params"
	);
}

#[test]
fn value_takes_progress_and_type_from_row() {
	let (core, _) = create();
	let mut inputs = both();
	inputs.insert(PROGRESS_INPUT.to_string(), NodeValue::Float(0.75));
	inputs.insert(TYPE_INPUT.to_string(), NodeValue::Combo(2));
	let payload = run(&core, &inputs);
	assert_eq!(
		payload.params.get(PROGRESS_INPUT),
		Some(&NodeValue::Float(0.75))
	);
	assert_eq!(payload.shader_id, "wipe");
}

#[test]
fn value_takes_type_from_core() {
	let (mut core, _) = create();
	core.set_standard_value(TYPE_INPUT, -1, NodeValue::Combo(3));
	let payload = run(&core, &both());
	assert_eq!(payload.shader_id, "slide");
}

#[test]
fn shader_code_selects_variant_without_switch() {
	let n = TransitionNode;
	let sources: Vec<String> = SHADER_IDS
		.iter()
		.map(|id| n.shader_code(id).expect("shader source"))
		.collect();
	for (i, glsl) in sources.iter().enumerate() {
		assert!(
			glsl.contains("uniform sampler2D tex_in;"),
			"{i} binds tex_in"
		);
		assert!(
			glsl.contains("uniform sampler2D blend_in;"),
			"{i} binds blend_in"
		);
		assert!(glsl.contains("uniform bool tex_in_enabled;"));
		assert!(glsl.contains("uniform bool blend_in_enabled;"));
		assert!(glsl.contains("uniform float progress_in;"));
		assert!(!glsl.contains("switch"));
	}
	for i in 0..sources.len() {
		for j in (i + 1)..sources.len() {
			assert_ne!(sources[i], sources[j], "types {i} and {j} differ");
		}
	}
	assert!(sources[0].contains("mix(tex_col, blend_col, progress_in)"));
	assert!(sources[3].contains("1.0 - progress_in"));
}

#[test]
fn shader_code_unknown_id_falls_back_to_cross_dissolve() {
	let n = TransitionNode;
	assert_eq!(
		n.shader_code("nonsense"),
		n.shader_code(SHADER_IDS[0]),
		"an unknown style id renders as cross dissolve"
	);
}

#[test]
fn duplicate_clones_behavior() {
	let (core, behavior) = create();
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Transition");
}
