use super::*;

use quercus_core::ocioutils::PixelFormat as QuercusPixelFormat;
use quercus_core::PixelFormat;
use quercus_node::block::ClipBlockBehavior;
use quercus_node::folder;
use quercus_node::id::NodeId;
use quercus_node::node::NodeCore;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use quercus_node::value::NodeValue;
use quercus_render::ticket::AudioSamples;

/// A fresh project (the same `Arc<Mutex<Project>>` shape the integration
/// fixtures build, but without any media I/O — these tests only touch
/// the pure graph-to-params marshalling).
fn project() -> ProjectRef {
	quercus_node::project::Project::new()
}

fn add_footage(project: &ProjectRef, filename: &str) -> NodeId {
	let (core, behavior) = quercus_node::footage::FootageBehavior::create();
	let id = project.lock().unwrap().graph.add_node(core, behavior);
	{
		let mut guard = project.lock().unwrap();
		if let Some(f) = guard
			.graph
			.get_mut(id)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<quercus_node::footage::FootageBehavior>())
		{
			f.filename = filename.to_string();
		}
	}
	id
}

fn add_folder(project: &ProjectRef) -> NodeId {
	let (core, behavior) = folder::create("");
	project.lock().unwrap().graph.add_node(core, behavior)
}

fn add_sequence(project: &ProjectRef) -> NodeId {
	let (core, behavior) = SequenceBehavior::create();
	project.lock().unwrap().graph.add_node(core, behavior)
}

fn add_clip(project: &ProjectRef, footage: Option<NodeId>, range: TimeRange) -> NodeId {
	let (core, behavior) = quercus_node::block::clip_create();
	let clip = project.lock().unwrap().graph.add_node(core, behavior);
	let mut guard = project.lock().unwrap();
	if let Some(c) = guard
		.graph
		.get_mut(clip)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
	{
		c.core.range = range;
		c.footage = footage;
	}
	clip
}

// ---- pure accessors -------------------------------------------------

#[test]
fn force_size_and_format_cover_off_and_on_paths() {
	let project = project();
	let make = |force: ForceParams| {
		RenderTask::new(
			Task::new("t", None),
			None,
			(project.clone(), NodeId::INVALID),
			force,
			None,
		)
	};

	// All-off defaults: no forced size, no forced format.
	let task = make(ForceParams::default());
	assert_eq!(task.force_size(), None);
	assert_eq!(task.force_format(), None);

	// A single positive dimension is not enough for a forced size.
	let task = make(ForceParams {
		force_width: 32,
		..ForceParams::default()
	});
	assert_eq!(task.force_size(), None);
	let task = make(ForceParams {
		force_width: 0,
		force_height: 32,
		..ForceParams::default()
	});
	assert_eq!(task.force_size(), None);

	// Both positive: forced.
	let task = make(ForceParams {
		force_width: 32,
		force_height: 16,
		..ForceParams::default()
	});
	assert_eq!(task.force_size(), Some((32, 16)));

	// Format codes map through `pixel_format_from_code`: -1 is off,
	// known codes map, unknown codes become Invalid.
	let task = make(ForceParams {
		force_format: PixelFormat::F32 as i32,
		..ForceParams::default()
	});
	assert_eq!(task.force_format(), Some(PixelFormat::F32));
	let task = make(ForceParams {
		force_format: 0,
		..ForceParams::default()
	});
	assert_eq!(task.force_format(), Some(PixelFormat::U8));
	let task = make(ForceParams {
		force_format: 99,
		..ForceParams::default()
	});
	assert_eq!(task.force_format(), Some(PixelFormat::Invalid));
}

#[test]
fn timebase_falls_back_for_missing_or_null_params() {
	let project = project();
	let make = |params: Option<VideoParams>| {
		RenderTask::new(
			Task::new("t", None),
			params,
			(project.clone(), NodeId::INVALID),
			ForceParams::default(),
			None,
		)
	};

	// No params at all.
	assert_eq!(make(None).timebase(), Rational::new(1, 1));

	// Null frame rate (the default `VideoParams`).
	assert_eq!(
		make(Some(VideoParams::new())).timebase(),
		Rational::new(1, 1)
	);

	// A valid frame rate flips into the frame duration.
	let mut params =
		VideoParams::new_basic(64, 64, QuercusPixelFormat::from_code(0), 4, 1, 1, 0, 1);
	params.set_frame_rate(25, 1);
	assert_eq!(make(Some(params)).timebase(), Rational::new(1, 25));
}

/// The placeholder render state is a valid, empty task.
#[test]
fn placeholder_is_closed_and_empty() {
	let task = RenderTask::placeholder();
	assert_eq!(task.total_frames(), 0);
	assert_eq!(task.viewer.1, NodeId::INVALID);
	assert!(task.viewer.0.lock().is_ok());
}

#[test]
fn classify_ticket_maps_submitter_keys_only() {
	let task = RenderTask::placeholder();
	let mut keys = HashMap::new();
	keys.insert(TicketId(7), (TICKET_VIDEO, 0, 1));
	keys.insert(TicketId(9), (TICKET_AUDIO, 0, 1));
	let mut slots = HashMap::new();
	slots.insert((TICKET_VIDEO, 0, 1), 3);

	assert_eq!(task.classify_ticket(&keys, TicketId(7), &slots), Some(3));
	// Audio key without a slot.
	assert_eq!(task.classify_ticket(&keys, TicketId(9), &slots), None);
	// Unknown ticket id.
	assert_eq!(task.classify_ticket(&keys, TicketId(99), &slots), None);
	// Same key with a different time.
	assert_eq!(
		task.classify_ticket(&keys, TicketId(7), &HashMap::new()),
		None
	);
}

// ---- graph -> ticket params -----------------------------------------

/// Build a track node holding `blocks` (mutated directly: the graph is
/// only inspected by the montage builders, never rendered here).
fn track_with(project: &ProjectRef, kind: TrackType, blocks: Vec<NodeId>) -> NodeId {
	let mut guard = project.lock().unwrap();
	let track = guard
		.graph
		.add_node(NodeCore::new(), Box::new(TrackBehavior::new(kind)));
	if let Some(t) = guard
		.graph
		.get_mut(track)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<TrackBehavior>())
	{
		t.blocks = blocks;
	}
	track
}

fn track_list_with(project: &ProjectRef, kind: TrackType, tracks: Vec<NodeId>) -> NodeId {
	let mut guard = project.lock().unwrap();
	let list = guard
		.graph
		.add_node(NodeCore::new(), Box::new(TrackListBehavior::new(kind)));
	if let Some(l) = guard
		.graph
		.get_mut(list)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<TrackListBehavior>())
	{
		l.tracks = tracks;
	}
	list
}

fn push_track_list(project: &ProjectRef, sequence: NodeId, list: NodeId) {
	let mut guard = project.lock().unwrap();
	if let Some(seq) = guard
		.graph
		.get_mut(sequence)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		seq.track_lists.push(list);
	}
}

#[test]
fn build_video_ticket_rejects_missing_and_non_viewer_nodes() {
	let project = project();
	for viewer in [NodeId::INVALID, add_folder(&project)] {
		let task = RenderTask::new(
			Task::new("t", None),
			None,
			(project.clone(), viewer),
			ForceParams::default(),
			None,
		);
		let error = task.build_video_ticket(Rational::new(0, 1)).unwrap_err();
		assert!(
			error.to_string().contains("viewer output"),
			"viewer {viewer:?}: {error}"
		);
	}
}

#[test]
fn build_video_ticket_marshals_footage_force_params() {
	let project = project();
	let footage = add_footage(&project, "/tmp/quercustask-inline.mp4");
	let mut task = RenderTask::new(
		Task::new("t", None),
		Some(VideoParams::new()),
		(project.clone(), footage),
		ForceParams {
			force_width: 32,
			force_height: 16,
			force_format: PixelFormat::F32 as i32,
			..ForceParams::default()
		},
		None,
	);
	task.set_render_inputs(0, false, TimeRange::default());

	let params = task.build_video_ticket(Rational::new(1, 2)).unwrap();
	assert_eq!(params.viewer, footage.identity());
	assert_eq!(
		params.footage,
		Some(("/tmp/quercustask-inline.mp4".to_string(), 0))
	);
	assert!(params.montage.is_empty());
	assert_eq!(params.force_size, Some((32, 16)));
	assert_eq!(params.force_format, Some(PixelFormat::F32));
	assert_eq!(params.cache, Some(footage.identity()));
	assert_eq!(params.time, Rational::new(1, 2));
	assert_eq!(params.project, task.viewer.0.lock().unwrap().uuid);
}

#[test]
fn video_montage_skips_degenerate_entries_and_keeps_valid_clips() {
	let project = project();
	let sequence = add_sequence(&project);
	let footage = add_footage(&project, "valid.mp4");
	let folder = add_folder(&project);

	let ok = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	let out_of_range = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(10, 1), Rational::new(11, 1)),
	);
	let no_footage = add_clip(
		&project,
		None,
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	let wrong_footage = add_clip(
		&project,
		None,
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	crate::nodeops::clip_set_footage(&project, wrong_footage, folder);

	// Track blocks: a non-block node, the valid clip, and every rejected
	// clip kind.
	let track = track_with(
		&project,
		TrackType::Video,
		vec![folder, ok, out_of_range, no_footage, wrong_footage],
	);
	// A track list with bad track ids mixed in.
	let list = track_list_with(
		&project,
		TrackType::Video,
		vec![NodeId::INVALID, folder, track],
	);
	let audio_list = track_list_with(&project, TrackType::Audio, vec![track]);
	// A valid id that is not a track list.
	let not_a_list = folder;
	// A valid id whose node was removed from the graph (the
	// `graph.get` miss branch).
	let removed = add_folder(&project);
	{
		let mut guard = project.lock().unwrap();
		let _ = guard.graph.remove_node(removed);
		assert!(removed.valid() && guard.graph.get(removed).is_none());
	}
	{
		let mut guard = project.lock().unwrap();
		if let Some(seq) = guard
			.graph
			.get_mut(sequence)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			seq.track_lists = vec![NodeId::INVALID, removed, not_a_list, audio_list, list];
		}
	}

	let montage = RenderTask::video_montage(&project, sequence, Rational::new(0, 1));
	assert_eq!(montage.len(), 1, "only the valid clip is kept");
	assert_eq!(montage[0].filename, "valid.mp4");
	assert_eq!(montage[0].in_time, Rational::new(0, 1));
	assert_eq!(montage[0].out_time, Rational::new(1, 1));
	assert_eq!(montage[0].media_in, Rational::new(0, 1));
	assert_eq!(montage[0].gain, 1.0);
	assert_eq!(montage[0].stream_index, 0);

	// A time outside the clip's range yields an empty montage.
	assert!(RenderTask::video_montage(&project, sequence, Rational::new(5, 1)).is_empty());

	// Missing sequence and non-sequence viewers are empty, not panics.
	assert!(RenderTask::video_montage(&project, NodeId::INVALID, Rational::new(0, 1)).is_empty());
	assert!(RenderTask::video_montage(&project, folder, Rational::new(0, 1)).is_empty());
}

#[test]
fn audio_montage_filters_kinds_and_reads_clip_audio_fields() {
	let project = project();
	let sequence = add_sequence(&project);
	let footage = add_footage(&project, "audio.mp4");
	let folder = add_folder(&project);

	let ok = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
	);
	let out_of_range = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(9, 1), Rational::new(10, 1)),
	);
	let no_footage = add_clip(
		&project,
		None,
		TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
	);
	let wrong_footage = add_clip(
		&project,
		None,
		TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
	);
	crate::nodeops::clip_set_footage(&project, wrong_footage, folder);

	let track = track_with(
		&project,
		TrackType::Audio,
		vec![
			NodeId::INVALID,
			folder,
			ok,
			out_of_range,
			no_footage,
			wrong_footage,
		],
	);
	let audio_list = track_list_with(
		&project,
		TrackType::Audio,
		vec![NodeId::INVALID, folder, track],
	);
	let video_list = track_list_with(&project, TrackType::Video, vec![track]);
	let removed = add_folder(&project);
	{
		let mut guard = project.lock().unwrap();
		let _ = guard.graph.remove_node(removed);
		assert!(removed.valid() && guard.graph.get(removed).is_none());
	}
	{
		let mut guard = project.lock().unwrap();
		if let Some(seq) = guard
			.graph
			.get_mut(sequence)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			seq.track_lists = vec![video_list, NodeId::INVALID, removed, audio_list];
		}
	}

	// Per-clip gain/pan/fade/audio-effect fields flow into the montage
	// (the export builder and the preview builder share MontageAudio).
	{
		let mut guard = project.lock().unwrap();
		if let Some(entry) = guard.graph.get_mut(ok) {
			if let Some(clip) = entry
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<quercus_node::block::ClipBlockBehavior>())
			{
				clip.core.audio_gain = 0.5;
				clip.core.audio_pan = -0.25;
				clip.core.audio_fade_in = Rational::new(1, 2);
				clip.core.audio_fade_out = Rational::new(1, 4);
				let mut volume = quercus_node::block::ClipAudioEffect::new(
					quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID,
				);
				volume.set_param(
					quercus_node::block::AUDIO_VOLUME_INPUT,
					quercus_node::value::NodeValue::Float(0.25),
				);
				clip.core.audio_effects = vec![volume];
			}
		}
	}
	let montage = RenderTask::audio_montage(&project, sequence, Rational::new(1, 1));
	assert_eq!(montage.len(), 1);
	assert_eq!(montage[0].filename, "audio.mp4");
	assert!((montage[0].gain - 0.5).abs() < 1e-6);
	assert!((montage[0].pan + 0.25).abs() < 1e-6);
	assert_eq!(montage[0].fade_in, Rational::new(1, 2));
	assert_eq!(montage[0].fade_out, Rational::new(1, 4));
	assert!(
		montage[0].effects.is_empty(),
		"audio carries no video effects"
	);
	assert_eq!(montage[0].audio_effects.len(), 1);
	assert_eq!(
		montage[0].audio_effects[0].type_id,
		quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID
	);
	assert_eq!(
		montage[0].audio_effects[0].param(quercus_node::block::AUDIO_VOLUME_INPUT),
		Some(&quercus_node::value::NodeValue::Float(0.25))
	);

	assert!(RenderTask::audio_montage(&project, sequence, Rational::new(5, 1)).is_empty());
	assert!(RenderTask::audio_montage(&project, NodeId::INVALID, Rational::new(0, 1)).is_empty());
	assert!(RenderTask::audio_montage(&project, folder, Rational::new(0, 1)).is_empty());
}

/// Export renders a whole range in one audio ticket, so every clip that
/// overlaps the range must be mixed. Regression: the montage used to keep
/// only the clips playing at the range start, which silenced narration and
/// music placed later on the timeline.
#[test]
fn audio_montage_in_includes_clips_overlapping_the_range() {
	let project = project();
	let sequence = add_sequence(&project);
	let footage = add_footage(&project, "audio.mp4");
	let at_start = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
	);
	let later = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(5, 1), Rational::new(7, 1)),
	);
	let at_range_end = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(10, 1), Rational::new(12, 1)),
	);
	let track = track_with(
		&project,
		TrackType::Audio,
		vec![at_start, later, at_range_end],
	);
	let audio_list = track_list_with(&project, TrackType::Audio, vec![track]);
	{
		let mut guard = project.lock().unwrap();
		if let Some(seq) = guard
			.graph
			.get_mut(sequence)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			seq.track_lists = vec![audio_list];
		}
	}

	let montage = RenderTask::audio_montage_in(
		&project,
		sequence,
		Rational::new(0, 1),
		Rational::new(10, 1),
	);
	let starts: Vec<Rational> = montage.iter().map(|clip| clip.in_time).collect();
	// The half-open range keeps the clip starting at 5 s and drops the one
	// starting exactly at the range end.
	assert_eq!(starts, vec![Rational::new(0, 1), Rational::new(5, 1)]);
	// The point query keeps its semantics.
	assert_eq!(
		RenderTask::audio_montage(&project, sequence, Rational::new(0, 1)).len(),
		1
	);
	assert_eq!(
		RenderTask::audio_montage(&project, sequence, Rational::new(6, 1)).len(),
		1
	);
	assert!(RenderTask::audio_montage(&project, sequence, Rational::new(3, 1)).is_empty());
}
#[test]
fn build_audio_ticket_reads_sequence_params_and_montage() {
	let project = project();
	let sequence = add_sequence(&project);
	let footage = add_footage(&project, "audio.mp4");
	let clip = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	let track = track_with(&project, TrackType::Audio, vec![clip]);
	let list = track_list_with(&project, TrackType::Audio, vec![track]);
	push_track_list(&project, sequence, list);

	let task = RenderTask::new(
		Task::new("t", None),
		None,
		(project.clone(), sequence),
		ForceParams::default(),
		None,
	);
	let params = task
		.build_audio_ticket(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)))
		.unwrap();
	assert_eq!(params.viewer, sequence.identity());
	assert_eq!(params.sample_rate, 48000);
	assert_eq!(params.channel_layout, 0x3);
	assert_eq!(params.montage.len(), 1);
	assert_eq!(params.montage[0].filename, "audio.mp4");
	assert_eq!(params.range.in_(), Rational::new(0, 1));

	// A footage viewer still yields a ticket (with an empty montage).
	let footage_task = RenderTask::new(
		Task::new("t", None),
		None,
		(project.clone(), footage),
		ForceParams::default(),
		None,
	);
	let params = footage_task
		.build_audio_ticket(TimeRange::new(Rational::new(0, 1), Rational::new(1, 2)))
		.unwrap();
	assert!(params.montage.is_empty());
	assert_eq!(params.sample_rate, 48000);

	// A stale viewer falls back to the defaults too (no panic).
	let stale = RenderTask::new(
		Task::new("t", None),
		None,
		(project, NodeId::INVALID),
		ForceParams::default(),
		None,
	);
	let params = stale
		.build_audio_ticket(TimeRange::new(Rational::new(0, 1), Rational::new(1, 2)))
		.unwrap();
	assert!(params.montage.is_empty());
}

#[test]
fn clip_effects_walks_chain_params_and_enable_state() {
	let project = project();
	let footage = add_footage(&project, "fx.mp4");
	let folder = add_folder(&project);

	// A node without an effect input ends the walk immediately.
	{
		let guard = project.lock().unwrap();
		assert!(RenderTask::clip_effects(&guard.graph, footage, Rational::new(0, 1)).is_empty());
		assert!(RenderTask::clip_effects(&guard.graph, folder, Rational::new(0, 1)).is_empty());
		assert!(
			RenderTask::clip_effects(&guard.graph, NodeId::INVALID, Rational::new(0, 1)).is_empty()
		);
	}

	// A clip whose effect input is set but not connected: the walk's
	// no-upstream break.
	let bare = add_clip(
		&project,
		None,
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	{
		let guard = project.lock().unwrap();
		assert!(RenderTask::clip_effects(&guard.graph, bare, Rational::new(0, 1)).is_empty());
	}

	// Direct footage -> clip connection: the source node is dropped
	// (the montage decodes the footage itself).
	let direct = add_clip(
		&project,
		Some(footage),
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	{
		let mut guard = project.lock().unwrap();
		guard
			.graph
			.connect(
				footage,
				direct,
				quercus_node::block::clip_input::TEXTURE_INPUT,
				-1,
			)
			.expect("connect footage");
	}
	{
		let guard = project.lock().unwrap();
		assert!(RenderTask::clip_effects(&guard.graph, direct, Rational::new(0, 1)).is_empty());
	}

	for enabled in [true, false] {
		let clip = add_clip(
			&project,
			None,
			TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		);
		let (ecore, ebehavior) = quercus_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.invert")
			.expect("invert registered");
		let mut guard = project.lock().unwrap();
		let effect = guard.graph.add_node(ecore, ebehavior);
		guard
			.graph
			.get_mut(effect)
			.unwrap()
			.core
			.set_standard_value(
				quercus_node::node::ENABLED_INPUT,
				-1,
				NodeValue::Boolean(enabled),
			);
		guard
			.graph
			.connect(footage, effect, "tex_in", -1)
			.expect("footage -> effect");
		guard
			.graph
			.connect(
				effect,
				clip,
				quercus_node::block::clip_input::TEXTURE_INPUT,
				-1,
			)
			.expect("effect -> clip");

		let effects = RenderTask::clip_effects(&guard.graph, clip, Rational::new(0, 1));
		assert_eq!(effects.len(), 1);
		assert_eq!(effects[0].type_id, "org.olivevideoeditor.Olive.invert");
		assert_eq!(effects[0].enabled, enabled);
		assert_eq!(effects[0].effect_input_id.as_deref(), Some("tex_in"));
		assert!(
			!effects[0].params.is_empty(),
			"the four channel toggles are collected"
		);
	}

	// A transform effect contributes scalars but skips its texture and
	// matrix inputs; a blur effect additionally skips its hidden
	// method-specific inputs; a volume effect skips its samples input.
	for (type_name, connect_input, skipped) in [
		(
			"org.olivevideoeditor.Olive.transform",
			"tex_in",
			"parent_in",
		),
		(
			"org.olivevideoeditor.Olive.blur",
			"tex_in",
			"directional_degrees_in",
		),
		(
			"org.olivevideoeditor.Olive.volume",
			"samples_in",
			"samples_in",
		),
	] {
		let clip = add_clip(
			&project,
			None,
			TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		);
		let (ecore, ebehavior) = quercus_node::factory::Factory::global()
			.create_any(type_name)
			.expect("effect registered");
		let mut guard = project.lock().unwrap();
		let effect = guard.graph.add_node(ecore, ebehavior);
		guard
			.graph
			.connect(footage, effect, connect_input, -1)
			.expect("footage -> effect");
		guard
			.graph
			.connect(
				effect,
				clip,
				quercus_node::block::clip_input::TEXTURE_INPUT,
				-1,
			)
			.expect("effect -> clip");

		let effects = RenderTask::clip_effects(&guard.graph, clip, Rational::new(0, 1));
		assert_eq!(effects.len(), 1, "{type_name}");
		assert_eq!(effects[0].type_id, type_name);
		assert!(
			effects[0].params.iter().all(|(id, _)| id != skipped),
			"{type_name}: {skipped} must be skipped"
		);
		assert!(
			effects[0].params.iter().all(|(id, _)| id != "tex_in"),
			"{type_name}: texture inputs must be skipped"
		);
	}
}

// ---- dispatch channel ------------------------------------------------

#[test]
fn dispatch_queue_pops_in_order_and_counts_running() {
	let dispatch = Box::into_raw(Box::new(RenderDispatch::new()));
	let audio = || {
		TicketPayload::Audio(AudioSamples {
			samples: vec![0.25, -0.25],
			sample_rate: 48000,
			channel_layout: 0x3,
			channel_count: 2,
		})
	};

	// Empty queue -> None.
	assert!(unsafe { (*dispatch).pop_finished() }.is_none());

	unsafe {
		(*dispatch).running.store(2, Ordering::SeqCst);
	}
	push_finished(TicketId(1), Ok(audio()), DispatchPtr(dispatch));
	push_finished(TicketId(2), Ok(audio()), DispatchPtr(dispatch));
	assert_eq!(unsafe { (*dispatch).running.load(Ordering::SeqCst) }, 0);

	let first = unsafe { (*dispatch).pop_finished() };
	assert!(matches!(
		first,
		Some((TicketId(1), Ok(TicketPayload::Audio(_))))
	));
	let second = unsafe { (*dispatch).pop_finished() };
	assert!(matches!(
		second,
		Some((TicketId(2), Ok(TicketPayload::Audio(_))))
	));
	assert!(unsafe { (*dispatch).pop_finished() }.is_none());

	unsafe {
		drop(Box::from_raw(dispatch));
	}
}

#[test]
fn wait_idle_returns_when_callbacks_finish() {
	let dispatch = RenderDispatch::new();
	dispatch.running.store(1, Ordering::SeqCst);
	let calls = AtomicUsize::new(0);
	let pump = || {
		// The first pump observes an in-flight ticket (so `wait_idle`
		// takes its wait branch); the second one completes it. No
		// ticket thread is involved, so the test stays deterministic.
		if calls.fetch_add(1, Ordering::SeqCst) >= 1 {
			dispatch.running.store(0, Ordering::SeqCst);
			dispatch.cv.notify_all();
		}
	};
	dispatch.wait_idle(&pump);
	assert!(calls.load(Ordering::SeqCst) >= 2, "the wait branch ran");
}

#[test]
fn wait_idle_breaks_immediately_when_idle() {
	let dispatch = RenderDispatch::new();
	let calls = AtomicUsize::new(0);
	dispatch.wait_idle(&|| {
		calls.fetch_add(1, Ordering::SeqCst);
	});
	assert_eq!(calls.load(Ordering::SeqCst), 1, "pump once, then break");
}
