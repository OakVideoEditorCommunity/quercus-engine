use super::*;
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn test_project() -> (ProjectRef, NodeId) {
	let project = Project::new();
	project.lock().unwrap().initialize().unwrap();
	let sequence = {
		let (core, behavior) = SequenceBehavior::create();
		project.lock().unwrap().graph.add_node(core, behavior)
	};
	super::super::tracks::add_track(&project, sequence, TrackType::Video).expect("video track");
	(project, sequence)
}

fn temp_srt(tag: &str, contents: &str) -> std::path::PathBuf {
	let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!(
		"quercus_subtitle_{tag}_{}_{n}.srt",
		std::process::id()
	));
	std::fs::write(&path, contents).unwrap();
	path
}

fn track_clips(project: &ProjectRef, sequence: NodeId) -> Vec<NodeId> {
	let guard = lock(project);
	let track = track_ids(&guard.graph, sequence, TrackType::Video)[0];
	super::super::sequence::clip_ids(&guard.graph, track)
}

fn text_source(project: &ProjectRef, clip: NodeId) -> Option<NodeId> {
	lock(project)
		.graph
		.connected_output(clip, clip_input::TEXTURE_INPUT, -1)
}

fn standard(project: &ProjectRef, node: NodeId, input: &str) -> NodeValue {
	lock(project)
		.graph
		.get(node)
		.expect("node")
		.core
		.standard_value(input, -1)
}

fn custom_style() -> SubtitleStyle {
	SubtitleStyle {
		font_family: "DejaVu Sans".to_string(),
		font_size: 48.0,
		color: [1.0, 0.25, 0.0, 1.0],
		outline_color: [0.0, 0.0, 1.0, 1.0],
		outline_width: 3.0,
		align: 2,
		..SubtitleStyle::default()
	}
}

#[test]
fn styled_import_applies_font_color_outline_and_alignment() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let (project, sequence) = test_project();
	let path = temp_srt("styled", "1\n00:00:01,000 --> 00:00:02,000\nStyled\n");
	let count =
		import_srt_styled(&project, sequence, 0, &path, &custom_style()).expect("styled import");
	assert_eq!(count, 1);
	let clips = track_clips(&project, sequence);
	assert_eq!(clips.len(), 1);
	let source = text_source(&project, clips[0]).expect("text source");
	{
		let guard = lock(&project);
		assert_eq!(node_type_id(&guard.graph, source), TEXT_FOOTAGE_TYPE_ID);
	}
	assert_eq!(
		standard(&project, source, FONT_FAMILY_INPUT),
		NodeValue::StrCombo("DejaVu Sans".to_string())
	);
	assert_eq!(
		standard(&project, source, FONT_SIZE_INPUT),
		NodeValue::Float(48.0)
	);
	assert_eq!(
		standard(&project, source, COLOR_INPUT),
		NodeValue::Color([1.0, 0.25, 0.0, 1.0])
	);
	assert_eq!(
		standard(&project, source, OUTLINE_ENABLED_INPUT),
		NodeValue::Boolean(true)
	);
	assert_eq!(
		standard(&project, source, OUTLINE_COLOR_INPUT),
		NodeValue::Color([0.0, 0.0, 1.0, 1.0])
	);
	assert_eq!(
		standard(&project, source, OUTLINE_WIDTH_INPUT),
		NodeValue::Float(3.0)
	);
	assert_eq!(
		standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
		NodeValue::Combo(2)
	);
	let _ = std::fs::remove_file(path);
}

#[test]
fn set_subtitle_style_is_one_undo_row_and_restores_previous_values() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let (project, sequence) = test_project();
	let path = temp_srt("undo", "1\n00:00:00,000 --> 00:00:01,000\nUndo\n");
	import_srt(&project, sequence, 0, &path).expect("import");
	let clip = track_clips(&project, sequence)[0];
	let source = text_source(&project, clip).expect("text source");
	let before = (
		standard(&project, source, FONT_FAMILY_INPUT),
		standard(&project, source, FONT_SIZE_INPUT),
		standard(&project, source, COLOR_INPUT),
		standard(&project, source, OUTLINE_ENABLED_INPUT),
		standard(&project, source, OUTLINE_COLOR_INPUT),
		standard(&project, source, OUTLINE_WIDTH_INPUT),
		standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
	);
	apply_subtitle_style(&project, clip, &custom_style()).expect("apply style");
	assert_eq!(
		standard(&project, source, FONT_SIZE_INPUT),
		NodeValue::Float(48.0)
	);
	assert_eq!(
		standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
		NodeValue::Combo(2)
	);
	let last_row = quercus_undo::global::count().unwrap() - 1;
	assert_eq!(
		quercus_undo::global::command_name(last_row).unwrap(),
		"Subtitle Style",
		"all style fields share one undo row"
	);
	quercus_undo::global::undo().expect("undo style");
	let after = (
		standard(&project, source, FONT_FAMILY_INPUT),
		standard(&project, source, FONT_SIZE_INPUT),
		standard(&project, source, COLOR_INPUT),
		standard(&project, source, OUTLINE_ENABLED_INPUT),
		standard(&project, source, OUTLINE_COLOR_INPUT),
		standard(&project, source, OUTLINE_WIDTH_INPUT),
		standard(&project, source, VERTICAL_ALIGNMENT_INPUT),
	);
	assert_eq!(after, before, "undo restores every style field");
	quercus_undo::global::redo().expect("redo style");
	assert_eq!(
		standard(&project, source, FONT_SIZE_INPUT),
		NodeValue::Float(48.0)
	);
	let _ = std::fs::remove_file(path);
}

#[test]
fn style_application_rejects_non_text_clip_without_panicking() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let (project, _sequence) = test_project();
	let clip = {
		let mut guard = lock(&project);
		let (core, behavior) = quercus_node::block::clip_create();
		guard.graph.add_node(core, behavior)
	};
	// A clip with no source is not a subtitle text clip.
	assert!(apply_subtitle_style(&project, clip, &SubtitleStyle::default()).is_err());
	// A clip fed by a non-text generator is rejected too.
	let solid = {
		let (core, behavior) = quercus_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.solidgenerator")
			.expect("solid generator registered");
		lock(&project).graph.add_node(core, behavior)
	};
	lock(&project)
		.graph
		.connect(solid, clip, clip_input::TEXTURE_INPUT, -1)
		.expect("connect solid");
	assert!(apply_subtitle_style(&project, clip, &SubtitleStyle::default()).is_err());
	// A non-clip node id is rejected, not dereferenced.
	assert!(apply_subtitle_style(&project, solid, &SubtitleStyle::default()).is_err());
}

#[test]
fn invalid_style_values_are_rejected_before_creating_clips() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let (project, sequence) = test_project();
	let path = temp_srt("invalid", "1\n00:00:00,000 --> 00:00:01,000\nNope\n");
	let invalid = [
		SubtitleStyle {
			font_size: 0.0,
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			font_size: -4.0,
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			font_size: f32::NAN,
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			outline_width: -1.0,
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			outline_width: f32::INFINITY,
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			color: [f64::NAN, 1.0, 1.0, 1.0],
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			outline_color: [0.0, f64::INFINITY, 0.0, 1.0],
			..SubtitleStyle::default()
		},
		SubtitleStyle {
			align: 3,
			..SubtitleStyle::default()
		},
	];
	for style in &invalid {
		assert!(
			style.validate().is_err(),
			"style should be invalid: {style:?}"
		);
		assert!(
			import_srt_styled(&project, sequence, 0, &path, style).is_err(),
			"invalid style must fail before import: {style:?}"
		);
	}
	assert!(
		track_clips(&project, sequence).is_empty(),
		"invalid styles must not create clips"
	);
	let _ = std::fs::remove_file(path);
}

#[test]
fn srt_export_roundtrips_text_and_time_after_styling() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let (project, sequence) = test_project();
	let path = temp_srt(
		"roundtrip",
		"1\n00:00:01,000 --> 00:00:02,500\n你好\n第二行\n\n2\n00:00:03,000 --> 00:00:04,000\nStyled export\n",
	);
	assert_eq!(
		import_srt_styled(&project, sequence, 0, &path, &custom_style()).expect("styled import"),
		2
	);
	let output = temp_srt("roundtrip_out", "");
	let exported = export_srt(&project, sequence, &output).expect("export");
	assert_eq!(exported, 2);
	let text = std::fs::read_to_string(&output).unwrap();
	assert!(text.contains("你好"));
	assert!(text.contains("Styled export"));
	assert!(
		!text.contains("DejaVu Sans"),
		"SRT carries text and timing, not style"
	);
	let cues = parse_srt(text.as_bytes()).expect("reparse export");
	assert_eq!(cues.len(), 2);
	assert!(cues[0].text.contains("第二行"));
	assert!((cues[0].in_seconds - 1.0).abs() < 0.05);
	assert!((cues[0].out_seconds - 2.5).abs() < 0.05);
	assert!((cues[1].in_seconds - 3.0).abs() < 0.05);
	assert!((cues[1].out_seconds - 4.0).abs() < 0.05);
	let _ = std::fs::remove_file(path);
	let _ = std::fs::remove_file(output);
}

#[test]
fn existing_unscoped_import_and_export_still_roundtrip() {
	let _g = crate::editor_commands::test_lock();
	quercus_undo::global::clear().unwrap();
	let project = Project::new();
	project.lock().unwrap().initialize().unwrap();
	let sequence = {
		let (core, behavior) = SequenceBehavior::create();
		project.lock().unwrap().graph.add_node(core, behavior)
	};
	super::super::tracks::add_track(&project, sequence, TrackType::Video).expect("video track");
	let path = temp_srt(
		"legacy",
		"1\n00:00:01,000 --> 00:00:02,500\n你好\n第二行\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond\n",
	);
	let count = import_srt(&project, sequence, 0, &path).expect("import");
	assert_eq!(count, 2);
	let output = temp_srt("legacy_out", "");
	let exported = export_srt(&project, sequence, &output).expect("export");
	assert_eq!(exported, 2);
	let text = std::fs::read_to_string(&output).unwrap();
	assert!(text.contains("你好"));
	assert!(text.contains("Second"));
	let exported_cues = parse_srt(text.as_bytes()).expect("reparse export");
	assert_eq!(exported_cues.len(), 2);
	assert!((exported_cues[0].in_seconds - 1.0).abs() < 0.05);
	assert!((exported_cues[0].out_seconds - 2.5).abs() < 0.05);
	let _ = std::fs::remove_file(path);
	let _ = std::fs::remove_file(output);
}
