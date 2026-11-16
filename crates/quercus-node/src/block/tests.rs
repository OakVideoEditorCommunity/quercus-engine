use super::*;
use crate::project::Project;
use crate::serializer::{XmlRead, XmlWrite};

/// The clip's `tex_in` is declared as a connectable, non-keyframable
/// texture input and is the node's effect input (C++ ClipBlock
/// prepends the texture input and sets it as the effect input).
#[test]
fn clip_effect_input_and_texture_input() {
	let (core, _) = clip_create();
	assert_eq!(core.effect_input, clip_input::TEXTURE_INPUT);

	let tex = core
		.get_input(clip_input::TEXTURE_INPUT)
		.expect("clip declares a texture input");
	assert_eq!(tex.value_type, ValueType::Texture);
	assert_eq!(tex.default, NodeValue::None);
	assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	assert!(
		tex.is_connectable(),
		"effects attach through the texture input"
	);

	// The texture input sits right after the inherited `enabled_in`,
	// ahead of the static clip inputs (C++ prepend convention).
	let ids: Vec<&str> = core.inputs.iter().map(|i| i.id.as_str()).collect();
	assert_eq!(
		ids,
		vec![
			crate::node::ENABLED_INPUT,
			clip_input::TEXTURE_INPUT,
			clip_input::MEDIA_IN,
			clip_input::SPEED,
			clip_input::REVERSE,
			clip_input::MAINTAIN_AUDIO_PITCH,
			clip_input::LOOP_MODE,
		]
	);
}

fn clip_with(speed: f64, reversed: bool) -> ClipBlockBehavior {
	ClipBlockBehavior {
		core: BlockCore {
			range: TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
			media_in: Rational::new(5, 1),
			speed,
			reversed,
			..BlockCore::default()
		},
		footage: None,
	}
}

/// Timeline -> media time mapping on `tex_in` (C++
/// `ClipBlock::InputTimeAdjustment`): speed first, then reverse, then
/// the media in-point offset. Other inputs pass through unchanged.
#[test]
fn clip_input_time_adjustment_maps_timeline_to_media() {
	let time = TimeRange::new(Rational::new(12, 1), Rational::new(13, 1));
	let map = |c: &ClipBlockBehavior| {
		c.input_time_adjustment(&NodeCore::new(), clip_input::TEXTURE_INPUT, -1, time, false)
	};

	// Speed 1: media = (12 - 10) + 5 = 7.
	assert_eq!(
		map(&clip_with(1.0, false)),
		TimeRange::new(Rational::new(7, 1), Rational::new(8, 1))
	);
	// Speed 2: media = (12 - 10) * 2 + 5 = 9.
	assert_eq!(
		map(&clip_with(2.0, false)),
		TimeRange::new(Rational::new(9, 1), Rational::new(10, 1))
	);
	// Speed 0 clamps to the media in-point.
	assert_eq!(
		map(&clip_with(0.0, false)),
		TimeRange::new(Rational::new(5, 1), Rational::new(6, 1))
	);
	// Reversed flips inside the block span before the media offset:
	// (10 - (12 - 10)) + 5 = 13.
	assert_eq!(
		map(&clip_with(1.0, true)),
		TimeRange::new(Rational::new(13, 1), Rational::new(14, 1))
	);
	// Reversed + speed 2: (10 - (12 - 10) * 2) + 5 = 11.
	assert_eq!(
		map(&clip_with(2.0, true)),
		TimeRange::new(Rational::new(11, 1), Rational::new(12, 1))
	);
	// Non-`tex_in` inputs pass through untouched.
	assert_eq!(
		clip_with(2.0, true).input_time_adjustment(&NodeCore::new(), "other_in", -1, time, false),
		time
	);
}

/// The clip copies the connected `tex_in` texture to its output;
/// disabled clips, unconnected clips and non-clip blocks emit nothing
/// (a bare clip is inert, matching C++ `ClipBlock::ProcessFrame`).
#[test]
fn clip_value_passes_connected_texture_only() {
	let mut inputs = NodeValueRow::new();
	let handle = crate::handle::make_owned(42i32);
	// The value owns the `make_owned` reference; the inserted row is a
	// proper `NodeValue` clone (addref'd), never a bare handle copy.
	let tex_value = NodeValue::Texture(handle);
	inputs.insert(clip_input::TEXTURE_INPUT.to_string(), tex_value.clone());

	let mut table = NodeValueTable::default();
	clip_with(1.0, false).value(&NodeCore::empty(), &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.count(), 1);
	let NodeValue::Texture(out) = table.get(ValueType::Texture).unwrap() else {
		unreachable!()
	};
	assert_eq!(out.ctx, handle.ctx, "the same texture box passes through");

	let mut disabled = clip_with(1.0, false);
	disabled.core.enabled = false;
	let mut table = NodeValueTable::default();
	disabled.value(&NodeCore::empty(), &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.count(), 0, "disabled clip emits nothing");

	let mut table = NodeValueTable::default();
	clip_with(1.0, false).value(
		&NodeCore::empty(),
		&NodeValueRow::new(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(table.count(), 0, "unconnected clip emits nothing");

	for behavior in [
		Box::new(GapBlockBehavior::new()) as Box<dyn NodeBehavior>,
		Box::new(TransitionBlockBehavior::new()) as Box<dyn NodeBehavior>,
	] {
		let mut table = NodeValueTable::default();
		behavior.value(
			&NodeCore::empty(),
			&NodeValueRow::new(),
			Rational::new(0, 1),
			&mut table,
		);
		assert_eq!(table.count(), 0, "gap/transition emit nothing");
	}
}

/// An effect node can be chained onto the clip through `tex_in`: the
/// connection succeeds and resolves back to the effect.
#[test]
fn clip_texture_input_accepts_effects() {
	let project = Project::new();
	let (clip_id, effect_id) = {
		let mut p = project.lock().unwrap();
		let (ccore, cbehavior) = clip_create();
		let clip = p.graph.add_node(ccore, cbehavior);
		let (ecore, ebehavior) = (crate::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.opacity")
			.unwrap()
			.create)();
		let effect = p.graph.add_node(ecore, ebehavior);
		p.graph
			.connect(effect, clip, clip_input::TEXTURE_INPUT, -1)
			.unwrap();
		(clip, effect)
	};
	let p = project.lock().unwrap();
	assert_eq!(
		p.graph
			.connected_output(clip_id, clip_input::TEXTURE_INPUT, -1),
		Some(effect_id)
	);
}

/// The adjustment layer declares one connectable, non-keyframable
/// `tex_in` texture input and uses it as the effect input (the clip's
/// convention, without the media/speed inputs).
#[test]
fn adjustment_effect_input_and_texture_input() {
	let (core, behavior) = adjustment_create();
	assert_eq!(behavior.name(), "Adjustment Layer");
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.adjustment");
	assert_eq!(core.effect_input, adjustment_input::TEXTURE_INPUT);

	let tex = core
		.get_input(adjustment_input::TEXTURE_INPUT)
		.expect("adjustment declares a texture input");
	assert_eq!(tex.value_type, ValueType::Texture);
	assert_eq!(tex.default, NodeValue::None);
	assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	assert!(
		tex.is_connectable(),
		"effects attach through the texture input"
	);

	// `tex_in` sits right after the inherited `enabled_in`, and the
	// adjustment declares no further inputs.
	let ids: Vec<&str> = core.inputs.iter().map(|i| i.id.as_str()).collect();
	assert_eq!(
		ids,
		vec![crate::node::ENABLED_INPUT, adjustment_input::TEXTURE_INPUT,]
	);
}

/// The adjustment layer copies the connected `tex_in` texture to its
/// output; disabled and unconnected adjustment layers emit nothing
/// (the renderer feeds the chain's head when it drives the block).
#[test]
fn adjustment_value_passes_connected_texture_only() {
	let mut inputs = NodeValueRow::new();
	let handle = crate::handle::make_owned(43i32);
	inputs.insert(
		adjustment_input::TEXTURE_INPUT.to_string(),
		NodeValue::Texture(handle),
	);

	let behavior = AdjustmentBlockBehavior::new();
	let mut table = NodeValueTable::default();
	behavior.value(&NodeCore::empty(), &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.count(), 1);
	let NodeValue::Texture(out) = table.get(ValueType::Texture).unwrap() else {
		unreachable!()
	};
	assert_eq!(out.ctx, handle.ctx, "the same texture box passes through");

	let mut disabled = AdjustmentBlockBehavior::new();
	disabled.core.enabled = false;
	let mut table = NodeValueTable::default();
	disabled.value(&NodeCore::empty(), &inputs, Rational::new(0, 1), &mut table);
	assert_eq!(table.count(), 0, "disabled adjustment emits nothing");

	let mut table = NodeValueTable::default();
	behavior.value(
		&NodeCore::empty(),
		&NodeValueRow::new(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(table.count(), 0, "unconnected adjustment emits nothing");
}

/// The clip's audio fields (gain/pan/fade/effect chain) ride the
/// `<custom>` block segment and load back losslessly.
#[test]
fn block_audio_fields_roundtrip() {
	let mut core = BlockCore::default();
	core.audio_gain = 0.5;
	core.audio_pan = -0.25;
	core.audio_fade_in = Rational::new(1, 2);
	core.audio_fade_out = Rational::new(3, 4);
	let mut volume = ClipAudioEffect::new(AUDIO_EFFECT_VOLUME_TYPE_ID);
	volume.set_param(AUDIO_VOLUME_INPUT, NodeValue::Float(0.5));
	let mut pan = ClipAudioEffect::new(AUDIO_EFFECT_PAN_TYPE_ID);
	pan.enabled = false;
	pan.set_param(AUDIO_PAN_INPUT, NodeValue::Float(0.25));
	core.audio_effects = vec![volume, pan];

	let mut writer = crate::serializer::XmlWriterBridge::new().unwrap();
	writer.start_element("custom");
	save_block_core(&mut writer, &core);
	writer.end_element();
	let xml = writer.output();
	assert!(xml.contains("<audio_pan>-0.25</audio_pan>"));
	assert!(xml.contains("<audio_fade_in>1/2</audio_fade_in>"));
	assert!(xml.contains("type_id=\"org.olivevideoeditor.Olive.pan\""));

	let mut reader = crate::serializer::XmlReaderBridge::new(&xml).unwrap();
	assert!(reader.next_start_element());
	let mut loaded = BlockCore::default();
	load_block_core(&mut reader, &mut loaded, &mut |_, _| false);
	assert_eq!(loaded.audio_gain, core.audio_gain);
	assert_eq!(loaded.audio_pan, core.audio_pan);
	assert_eq!(loaded.audio_fade_in, core.audio_fade_in);
	assert_eq!(loaded.audio_fade_out, core.audio_fade_out);
	assert_eq!(loaded.audio_effects, core.audio_effects);
}

/// A pre-existing clip segment without the new audio elements keeps
/// the values already on the core (old projects open with the
/// defaults; the load is an overlay, not a reset).
#[test]
fn block_audio_fields_keep_defaults_for_old_files() {
	let xml = "<custom><range in=\"0/1\" out=\"1/1\"/><media_in>0/1</media_in>\
			<speed>1</speed><reversed>0</reversed><enabled>1</enabled>\
			<maintain_audio_pitch>0</maintain_audio_pitch><loop_mode>0</loop_mode>\
			<audio_gain>1</audio_gain></custom>";
	let mut reader = crate::serializer::XmlReaderBridge::new(xml).unwrap();
	assert!(reader.next_start_element());
	let mut loaded = BlockCore::default();
	loaded.audio_pan = 0.75;
	loaded.audio_fade_in = Rational::new(3, 1);
	loaded.audio_effects.push(ClipAudioEffect::new("stale"));
	load_block_core(&mut reader, &mut loaded, &mut |_, _| false);
	// Elements absent from the file leave the incoming defaults alone
	// (a fresh BlockCore has pan 0, fades 0, empty chain).
	assert_eq!(loaded.audio_gain, 1.0);
	assert_eq!(loaded.audio_pan, 0.75);
	assert_eq!(loaded.audio_fade_in, Rational::new(3, 1));
	assert_eq!(loaded.audio_effects.len(), 1);
}
