use super::*;
use crate::node::NodeBehavior;
use crate::value::{NodeValueTable, ValueType};
use quercus_core::{Rational, TimeRange};

/// A plugin node with a non-null instance and cached metadata.
fn node() -> PluginNode {
	PluginNode {
		instance: PluginInstanceHandle(1),
		sub_category: "Filter".to_string(),
		name: "Test Plugin".to_string(),
		type_id: "org.example.testplugin".to_string(),
		description: "A test plugin".to_string(),
	}
}

#[test]
fn metadata_served_from_cache() {
	let n = node();
	assert_eq!(n.name(), "Test Plugin");
	assert_eq!(n.type_id(), "org.example.testplugin");
	assert_eq!(n.description(), "A test plugin");
	assert_eq!(n.sub_category(), "Filter");
	assert_eq!(n.categories(), &[Category::OpenFx]);
}

#[test]
fn instance_handle_null_semantics() {
	assert!(PluginInstanceHandle::null().is_null());
	assert!(!PluginInstanceHandle(1).is_null());
}

#[test]
fn value_tags_non_texture_inputs() {
	let n = node();
	let core = NodeCore::new();
	let mut row = NodeValueRow::default();
	row.insert("opacity".to_string(), NodeValue::Float(0.5));
	row.insert("mode".to_string(), NodeValue::Combo(2));
	let mut table = NodeValueTable::default();
	n.value(&core, &row, Rational::new(0, 1), &mut table);
	// Both values pushed, tagged with their input ids, plus the
	// plugin job texture (no texture input in the row -> no job).
	let tagged: Vec<(&str, &NodeValue)> = table
		.rows()
		.iter()
		.filter(|(_, _, t)| t.is_some())
		.map(|(_, v, t)| (t.as_deref().unwrap(), v))
		.collect();
	assert_eq!(tagged.len(), 2);
	assert!(tagged
		.iter()
		.any(|(id, v)| *id == "opacity" && *v == &NodeValue::Float(0.5)));
	assert!(tagged
		.iter()
		.any(|(id, v)| *id == "mode" && *v == &NodeValue::Combo(2)));
	// No texture pushed (no texture input in the row).
	assert!(table.get(ValueType::Texture).is_none());
}

#[test]
fn value_pushes_job_payload_for_texture_input() {
	let n = node();
	let mut core = NodeCore::new();
	core.effect_input = TEXTURE_INPUT.to_string();
	let mut row = NodeValueRow::default();
	row.insert(
		"tex_in".to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	);
	row.insert("gain".to_string(), NodeValue::Float(0.25));
	row.insert("none_in".to_string(), NodeValue::None);
	let mut table = NodeValueTable::default();
	let time = Rational::new(3, 2);
	n.value(&core, &row, time, &mut table);
	// The texture is consumed as the job source: one tagged param
	// value + the boxed plugin job payload.
	assert_eq!(table.count(), 2);
	let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
		panic!("expected the plugin job texture");
	};
	// SAFETY: the handle was created by value() boxing a
	// PluginJobPayload.
	let payload =
		unsafe { crate::jobs::plugin_job(h) }.expect("texture handle must box a PluginJobPayload");
	assert_eq!(payload.instance, PluginInstanceHandle(1));
	assert_eq!(payload.time, time);
	assert_eq!(payload.effect_input_id, TEXTURE_INPUT);
	assert_eq!(payload.values.len(), 3);
	assert_eq!(payload.values.get("gain"), Some(&NodeValue::Float(0.25)));
}

#[test]
fn value_pushes_job_for_source_clip_only_input() {
	// Gate: a plugin with only the simple-source clip connected still
	// emits its job (the C++ resolution tries `Source` before
	// `tex_in`). With no texture at all, nothing is pushed.
	let n = node();
	let core = NodeCore::new();

	let mut row = NodeValueRow::default();
	row.insert(
		SOURCE_CLIP.to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	);
	let mut table = NodeValueTable::default();
	n.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(
		matches!(table.get(ValueType::Texture), Some(NodeValue::Texture(h)) if !h.is_null()),
		"a source-clip-only row must still produce the plugin job"
	);

	let mut table = NodeValueTable::default();
	n.value(
		&core,
		&NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert!(
		table.get(ValueType::Texture).is_none(),
		"no texture anywhere -> no job"
	);
}

#[test]
fn value_pushes_nothing_without_instance() {
	let n = PluginNode {
		instance: PluginInstanceHandle::null(),
		..node()
	};
	let core = NodeCore::new();
	let mut row = NodeValueRow::default();
	row.insert(
		TEXTURE_INPUT.to_string(),
		NodeValue::Texture(crate::handle::CHandle::null()),
	);
	let mut table = NodeValueTable::default();
	n.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(table.is_empty());
}

#[test]
fn process_samples_copies_verbatim() {
	let n = node();
	let core = NodeCore::new();
	let input = crate::value::SampleBuffer {
		format: quercus_core::SampleFormat::F32Planar,
		channels: 2,
		sample_count: 3,
		// Planar layout: channel 0 plane [1, 2, 3], channel 1 plane
		// [4, 5, 6].
		data: [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]
			.iter()
			.flat_map(|f| f.to_le_bytes())
			.collect(),
	};
	let mut row = NodeValueRow::default();
	row.insert("samples_in".to_string(), NodeValue::Samples(input));
	let mut output = crate::value::SampleBuffer::default();
	n.process_samples(
		&core,
		&row,
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		&mut output,
	);
	assert!(output.is_allocated());
	assert_eq!(output.channels, 2);
	assert_eq!(output.sample_count, 3);
	assert_eq!(output.format, quercus_core::SampleFormat::F32Planar);
	for c in 0..2 {
		for i in 0..3 {
			let expected = 1.0 + (c * 3 + i) as f64;
			assert_eq!(output.sample_value(c, i), expected);
		}
	}
}

#[test]
fn process_samples_silences_output_on_missing_input() {
	let n = node();
	let core = NodeCore::new();
	let mut output = crate::value::SampleBuffer {
		format: quercus_core::SampleFormat::F32,
		channels: 1,
		sample_count: 2,
		data: [1.0f32, 2.0].iter().flat_map(|f| f.to_le_bytes()).collect(),
	};
	n.process_samples(
		&core,
		&NodeValueRow::default(),
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		&mut output,
	);
	assert_eq!(output.sample_value(0, 0), 0.0);
	assert_eq!(output.sample_value(0, 1), 0.0);
}

#[test]
fn generate_frame_is_documented_noop() {
	let n = node();
	let core = NodeCore::new();
	let mut frame = crate::handle::CHandle::null();
	n.generate_frame(&core, &mut frame, Rational::new(0, 1));
	assert!(frame.is_null());
}

#[test]
fn duplicate_uses_installed_duplicator() {
	// No duplicator installed -> None.
	set_plugin_duplicator(None);
	let n = node();
	assert!(n.duplicate(&NodeCore::new()).is_none());

	// Installed duplicator: fresh handle, metadata copied.
	set_plugin_duplicator(Some(Arc::new(|h: PluginInstanceHandle| {
		Some(PluginInstanceHandle(h.0 + 100))
	})));
	let dup = n.duplicate(&NodeCore::new()).expect("duplicator installed");
	let dup = dup.as_any().unwrap().downcast_ref::<PluginNode>().unwrap();
	assert_eq!(dup.instance_handle(), PluginInstanceHandle(101));
	assert_eq!(dup.name(), "Test Plugin");
	assert_eq!(dup.sub_category(), "Filter");

	// Duplicator failure -> None.
	set_plugin_duplicator(Some(Arc::new(|_: PluginInstanceHandle| None)));
	assert!(n.duplicate(&NodeCore::new()).is_none());
	set_plugin_duplicator(None);
}

#[test]
fn push_button_clicked_is_noop() {
	let mut n = node();
	let mut core = NodeCore::new();
	n.push_button_clicked(&mut core, "button".to_string());
}

#[test]
fn create_returns_placeholder_node() {
	let (core, behavior) = create();
	let n = behavior
		.as_any()
		.unwrap()
		.downcast_ref::<PluginNode>()
		.unwrap();
	assert!(n.instance.is_null());
	assert_eq!(n.sub_category(), "General");
	assert_eq!(behavior.name(), "");
	assert_eq!(behavior.type_id(), "");
	// No inputs beyond the standard enabled input.
	assert_eq!(core.inputs.len(), 1);
}
