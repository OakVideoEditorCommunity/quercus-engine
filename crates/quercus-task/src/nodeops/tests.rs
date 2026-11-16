use super::*;
use quercus_node::track::TrackType;
use quercus_node::value::AudioParams;

// ---- fixtures -------------------------------------------------------

fn project() -> ProjectRef {
	Project::new()
}

fn sequence_with_list(project: &ProjectRef, kind: TrackType) -> (NodeId, NodeId) {
	let seq = sequence_create(project).expect("sequence");
	let list = sequence_track_list(project, seq, kind).expect("track list");
	(seq, list)
}

/// Append a track through the public command (the only path that wires
/// the `index`/`track_list` fields), returning its id.
fn append_track(project: &ProjectRef, list: NodeId) -> NodeId {
	let mut cmd = add_track_command(project.clone(), list);
	cmd.redo_now();
	let guard = lock_project(project);
	guard
		.graph
		.get(list)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TrackListBehavior>())
		.and_then(|l| l.tracks.last().copied())
		.expect("track appended")
}

fn add_block(project: &ProjectRef, kind: BlockKind) -> NodeId {
	block_create(project, kind).expect("block created")
}

fn add_adjustment(project: &ProjectRef) -> NodeId {
	let (core, behavior) = quercus_node::block::adjustment_create();
	lock_project(project).graph.add_node(core, behavior)
}

/// Give a block an exact timeline range (the public setters keep the
/// out-point anchored and shift the in-point, which is awkward for
/// length assertions).
fn set_block_range(project: &ProjectRef, block: NodeId, in_: Rational, out: Rational) {
	let mut guard = lock_project(project);
	let ok = set_block_core(&mut guard.graph, block, |core| {
		core.range = quercus_core::TimeRange::new(in_, out);
	});
	assert!(ok, "block {block:?} range set");
}

fn with_graph<R>(project: &ProjectRef, f: impl FnOnce(&Graph) -> R) -> R {
	let guard = lock_project(project);
	f(&guard.graph)
}

fn folder_children(project: &ProjectRef, folder: NodeId) -> Vec<NodeId> {
	with_graph(project, |g| {
		g.get(folder)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<FolderBehavior>())
			.map(|f| f.children.clone())
			.unwrap_or_default()
	})
}

fn bin_folder_of(project: &ProjectRef, node: NodeId) -> Option<NodeId> {
	with_graph(project, |g| g.get(node).and_then(|e| e.core.bin_folder))
}

fn write_clip(tag: &str) -> std::path::PathBuf {
	let path = std::env::temp_dir().join(format!(
		"quercustask_nodeops_{tag}_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip");
	path
}

// ---- shared block core ---------------------------------------------

#[test]
fn block_core_of_covers_every_block_kind_and_rejects_non_blocks() {
	let p = project();
	let clip = add_block(&p, BlockKind::Clip);
	let gap = add_block(&p, BlockKind::Gap);
	let transition = add_block(&p, BlockKind::Transition);
	let adjustment = add_adjustment(&p);
	let folder = folder_create(&p).expect("folder");
	let sequence = sequence_create(&p).expect("sequence");

	with_graph(&p, |g| {
		for id in [clip, gap, transition, adjustment] {
			assert!(
				block_core_of(g, id).is_some(),
				"block {id:?} must have a core"
			);
		}
		assert!(block_core_of(g, folder).is_none(), "folder is not a block");
		assert!(
			block_core_of(g, sequence).is_none(),
			"sequence is not a block"
		);
		assert!(block_core_of(g, NodeId::INVALID).is_none(), "stale id");
	});
}

#[test]
fn block_create_rejects_the_unknown_kind() {
	let p = project();
	assert!(block_create(&p, BlockKind::Other).is_none());
	assert!(block_create(&p, BlockKind::Clip).is_some());
	assert!(block_create(&p, BlockKind::Gap).is_some());
	assert!(block_create(&p, BlockKind::Transition).is_some());
}

// ---- node metadata --------------------------------------------------

#[test]
fn labels_round_trip_and_missing_nodes_stay_empty() {
	let p = project();
	let seq = sequence_create(&p).expect("sequence");
	assert_eq!(node_label(&p, seq), "");
	set_node_label(&p, seq, "Timeline 1");
	assert_eq!(node_label(&p, seq), "Timeline 1");

	// Missing / invalid nodes are safe no-ops.
	set_node_label(&p, NodeId::INVALID, "nope");
	assert_eq!(node_label(&p, NodeId::INVALID), "");
}

#[test]
fn node_type_ids_and_predicates_match_the_node_kind() {
	let p = project();
	let footage = footage_create(&p, None).expect("footage");
	let sequence = sequence_create(&p).expect("sequence");
	let folder = folder_create(&p).expect("folder");

	assert_eq!(node_type_id(&p, sequence), SEQUENCE_TYPE_ID);
	assert_eq!(node_type_id(&p, NodeId::INVALID), "");

	with_graph(&p, |g| {
		assert!(node_is_footage(g, footage));
		assert!(!node_is_footage(g, sequence));
		assert!(!node_is_footage(g, folder));
		assert!(node_is_sequence(g, sequence));
		assert!(!node_is_sequence(g, footage));
		assert!(!node_is_sequence(g, NodeId::INVALID));
	});
}

#[test]
fn context_position_is_a_safe_no_op_for_missing_nodes() {
	let p = project();
	let seq = sequence_create(&p).expect("sequence");
	node_set_context_position(&p, seq, NodeId::INVALID, 1.0, 2.0, true);
	node_set_context_position(&p, NodeId::INVALID, seq, 1.0, 2.0, false);
}

// ---- sequence / track list -----------------------------------------

#[test]
fn sequence_track_list_creates_once_per_kind_and_rejects_non_sequences() {
	let p = project();
	let seq = sequence_create(&p).expect("sequence");
	let video = sequence_track_list(&p, seq, TrackType::Video).expect("video list");
	let audio = sequence_track_list(&p, seq, TrackType::Audio).expect("audio list");
	let subtitle = sequence_track_list(&p, seq, TrackType::Subtitle).expect("subtitle list");
	assert_ne!(video, audio);
	assert_ne!(video, subtitle);
	// Created once: the sequence caches each list.
	assert_eq!(sequence_track_list(&p, seq, TrackType::Video), Some(video));
	assert_eq!(sequence_track_list(&p, seq, TrackType::Audio), Some(audio));
	assert_eq!(
		sequence_track_list(&p, seq, TrackType::Subtitle),
		Some(subtitle)
	);

	// Non-sequences and stale ids have no track lists.
	let folder = folder_create(&p).expect("folder");
	assert_eq!(sequence_track_list(&p, folder, TrackType::Video), None);
	assert_eq!(
		sequence_track_list(&p, NodeId::INVALID, TrackType::Video),
		None
	);
}

#[test]
fn sequence_params_default_and_read_back() {
	let p = project();
	let seq = sequence_create(&p).expect("sequence");
	// The domain constructor applies the default parameters.
	let default = sequence_video_params(&p, seq, 0).expect("default video params");
	assert_eq!((default.width(), default.height()), (1920, 1080));
	assert_eq!(default.channel_count(), 4);
	assert_eq!(sequence_audio_params(&p, seq), (48000, 0x3));
	assert!(sequence_video_params(&p, seq, 1).is_none(), "out of range");
	assert_eq!(sequence_audio_params(&p, NodeId::INVALID), (48000, 0x3));
	assert!(sequence_video_params(&p, NodeId::INVALID, 0).is_none());
	assert!(sequence_video_params(&p, folder_create(&p).expect("folder"), 0).is_none());

	// Replace the defaults and read them back through the quercus_core type.
	{
		let mut guard = lock_project(&p);
		let entry = guard.graph.get_mut(seq).expect("sequence");
		let s = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
			.expect("sequence behavior");
		s.video_params = vec![quercus_node::value::VideoParams {
			width: 1280,
			height: 720,
			frame_rate: Rational::new(24, 1),
			pixel_format: quercus_core::ocioutils::PixelFormat::F32.code(),
			channels: 4,
			interlaced: true,
		}];
		s.audio_params = vec![AudioParams {
			sample_rate: 44100,
			channel_layout: 0x3,
			format: 0,
		}];
	}
	let v = sequence_video_params(&p, seq, 0).expect("video params");
	assert_eq!((v.width(), v.height()), (1280, 720));
	assert_eq!(v.frame_rate(), (24, 1));
	assert_eq!(v.channel_count(), 4);
	assert_eq!(sequence_audio_params(&p, seq), (44100, 0x3));
}

// ---- footage --------------------------------------------------------

#[test]
fn footage_accessors_reject_wrong_node_types() {
	let p = project();
	let folder = folder_create(&p).expect("folder");
	assert!(!footage_set_filename(&p, folder, "x.mp4"));
	assert!(!footage_is_valid(&p, folder));
	assert_eq!(footage_filename(&p, folder), "");
	assert_eq!(footage_total_stream_count(&p, folder), 0);
	assert!(footage_video_params(&p, folder, 0).is_none());
	assert_eq!(footage_audio_sample_rate(&p, folder), None);
	assert_eq!(footage_filename(&p, NodeId::INVALID), "");
	assert!(!footage_is_valid(&p, NodeId::INVALID));
	// Cancellation on a wrong node is a safe no-op.
	footage_set_cancelled(&p, folder, true);
}

#[test]
fn footage_probe_round_trip_with_real_media() {
	let path = write_clip("probe");
	let p = project();
	let footage = footage_create(&p, None).expect("footage");
	assert!(!footage_is_valid(&p, footage));
	assert_eq!(footage_total_stream_count(&p, footage), 0);

	// A missing file fails the probe without panicking.
	assert!(!footage_set_filename(
		&p,
		footage,
		"/definitely/not/here.mp4"
	));
	assert!(!footage_is_valid(&p, footage));

	// A real clip probes: valid, with stream parameters.
	assert!(footage_set_filename(&p, footage, &path.to_string_lossy()));
	assert!(footage_is_valid(&p, footage));
	assert_eq!(footage_filename(&p, footage), path.to_string_lossy());
	assert!(footage_total_stream_count(&p, footage) >= 1);
	let video = footage_video_params(&p, footage, 0).expect("video params");
	assert_eq!((video.width(), video.height()), (64, 64));
	assert!(footage_audio_sample_rate(&p, footage).is_some());
	assert!(node_length(&p, footage) > Rational::new(0, 1));

	// Out-of-range stream index.
	assert!(footage_video_params(&p, footage, 99).is_none());

	// Overwriting a stream parameter round-trips through the quercus_core type.
	let mut params = CommonVideoParams::new_basic(
		32,
		32,
		quercus_core::ocioutils::PixelFormat::F32,
		4,
		1,
		1,
		0,
		1,
	);
	params.set_frame_rate(30, 1);
	footage_set_video_params(&p, footage, 0, &params);
	let back = footage_video_params(&p, footage, 0).expect("overwritten params");
	assert_eq!((back.width(), back.height()), (32, 32));
	assert_eq!(back.frame_rate(), (30, 1));
	// A wrong node type is a no-op.
	footage_set_video_params(&p, NodeId::INVALID, 0, &params);

	let _ = std::fs::remove_file(&path);
}

#[test]
fn node_length_covers_footage_sequence_and_other_types() {
	let path = write_clip("length");
	let p = project();
	let footage = footage_create(&p, None).expect("footage");
	assert!(footage_set_filename(&p, footage, &path.to_string_lossy()));
	assert!(node_length(&p, footage) > Rational::new(0, 1));

	let (seq, list) = sequence_with_list(&p, TrackType::Video);
	assert_eq!(node_length(&p, seq), Rational::new(0, 1), "empty sequence");
	let track = append_track(&p, list);
	let a = add_block(&p, BlockKind::Clip);
	let b = add_block(&p, BlockKind::Clip);
	assert!(track_append_block(&p, track, a));
	assert!(track_append_block(&p, track, b));
	set_block_range(&p, a, Rational::new(0, 1), Rational::new(2, 1));
	set_block_range(&p, b, Rational::new(2, 1), Rational::new(5, 1));
	assert_eq!(
		node_length(&p, seq),
		Rational::new(5, 1),
		"longest track out"
	);

	let folder = folder_create(&p).expect("folder");
	assert_eq!(node_length(&p, folder), Rational::new(0, 1));
	assert_eq!(node_length(&p, NodeId::INVALID), Rational::new(0, 1));

	let _ = std::fs::remove_file(&path);
}

// ---- tracks and blocks ---------------------------------------------

#[test]
fn tracklist_accessors_and_out_of_range() {
	let p = project();
	let (_, list) = sequence_with_list(&p, TrackType::Audio);
	assert_eq!(tracklist_track_count(&p, list), 0);
	assert!(tracklist_track_at(&p, list, 0).is_none());
	let track = append_track(&p, list);
	assert_eq!(tracklist_track_count(&p, list), 1);
	assert_eq!(tracklist_track_at(&p, list, 0), Some(track));
	assert!(tracklist_track_at(&p, list, 1).is_none());
	assert_eq!(track_type(&p, track), Some(TrackType::Audio));
	assert_eq!(track_type(&p, list), None);
	assert_eq!(track_type(&p, NodeId::INVALID), None);
	// A non-list node has no tracks.
	assert_eq!(tracklist_track_count(&p, track), 0);
}

#[test]
fn track_blocks_append_guard_and_length() {
	let p = project();
	let (_, list) = sequence_with_list(&p, TrackType::Video);
	let track = append_track(&p, list);
	let clip = add_block(&p, BlockKind::Clip);
	assert!(
		!track_append_block(&p, NodeId::INVALID, clip),
		"stale track"
	);
	assert!(track_append_block(&p, track, clip));
	assert_eq!(track_block_count(&p, track), 1);
	assert_eq!(track_block_at(&p, track, 0), Some(clip));
	assert!(track_block_at(&p, track, 1).is_none());
	assert_eq!(track_block_count(&p, NodeId::INVALID), 0);
	set_block_range(&p, clip, Rational::new(0, 1), Rational::new(0, 1));
	assert_eq!(
		track_length(&p, track),
		Rational::new(0, 1),
		"zero-length block"
	);
	set_block_range(&p, clip, Rational::new(1, 1), Rational::new(4, 1));
	assert_eq!(track_length(&p, track), Rational::new(4, 1), "max out");
	assert_eq!(track_length(&p, NodeId::INVALID), Rational::new(0, 1));
}

#[test]
fn block_getters_default_for_missing_nodes_and_setters_round_trip() {
	let p = project();
	let clip = add_block(&p, BlockKind::Clip);
	assert_eq!(block_in(&p, NodeId::INVALID), Rational::new(0, 1));
	assert_eq!(block_length(&p, NodeId::INVALID), Rational::new(0, 1));
	assert_eq!(block_media_in(&p, NodeId::INVALID), Rational::new(0, 1));

	clip_set_media_in(&p, clip, 2, 1);
	assert_eq!(block_media_in(&p, clip), Rational::new(2, 1));
	block_set_length_and_media_out(&p, clip, 5, 1);
	assert_eq!(block_length(&p, clip), Rational::new(5, 1));
	// Setters on a non-block/missing node are no-ops.
	block_set_length_and_media_out(&p, NodeId::INVALID, 1, 1);
	clip_set_media_in(&p, NodeId::INVALID, 9, 1);
}

#[test]
fn clip_footage_round_trip_and_non_clip_nodes() {
	let p = project();
	let clip = add_block(&p, BlockKind::Clip);
	let gap = add_block(&p, BlockKind::Gap);
	let footage = footage_create(&p, None).expect("footage");
	assert_eq!(clip_footage(&p, clip), None);
	clip_set_footage(&p, clip, footage);
	assert_eq!(clip_footage(&p, clip), Some(footage));
	assert_eq!(clip_footage(&p, gap), None);
	assert_eq!(clip_footage(&p, NodeId::INVALID), None);
	clip_set_footage(&p, gap, footage); // no-op on a gap
	assert_eq!(clip_footage(&p, gap), None);
}

#[test]
fn transition_offsets_default_and_set_length() {
	let p = project();
	let transition = add_block(&p, BlockKind::Transition);
	assert_eq!(transition_in_offset(&p, transition), Rational::new(0, 1));
	assert_eq!(transition_out_offset(&p, transition), Rational::new(0, 1));
	assert_eq!(
		transition_in_offset(&p, NodeId::INVALID),
		Rational::new(0, 1)
	);

	transition_set_offsets_and_length(&p, transition, 1, 2, 3, 4);
	assert_eq!(transition_in_offset(&p, transition), Rational::new(1, 2));
	assert_eq!(transition_out_offset(&p, transition), Rational::new(3, 4));
	assert_eq!(
		block_length(&p, transition),
		Rational::new(5, 4),
		"length = in + out"
	);
	// Wrong node types are no-ops.
	transition_set_offsets_and_length(&p, NodeId::INVALID, 1, 1, 1, 1);
}

#[test]
fn block_kind_and_adjustment_predicates() {
	let p = project();
	let adjustment = add_adjustment(&p);
	assert!(block_is_adjustment(&p, adjustment));
	assert!(!block_is_adjustment(&p, NodeId::INVALID));
	for (kind, expected) in [
		(BlockKind::Clip, BlockKind::Clip),
		(BlockKind::Gap, BlockKind::Gap),
		(BlockKind::Transition, BlockKind::Transition),
	] {
		let block = add_block(&p, kind);
		assert_eq!(block_kind(&p, block), expected);
	}
	assert_eq!(block_kind(&p, adjustment), BlockKind::Other);
	assert_eq!(block_kind(&p, NodeId::INVALID), BlockKind::Other);
	// An adjustment block is not one of the C-ABI block kinds.
	let folder = folder_create(&p).expect("folder");
	assert_eq!(block_kind(&p, folder), BlockKind::Other);
}

// ---- graph adapters and connections --------------------------------

#[test]
fn graph_range_adapters_default_for_missing_nodes() {
	let p = project();
	let (_, list) = sequence_with_list(&p, TrackType::Video);
	let track = append_track(&p, list);
	let clip = add_block(&p, BlockKind::Clip);
	assert!(track_append_block(&p, track, clip));
	set_block_range(&p, clip, Rational::new(2, 1), Rational::new(6, 1));

	with_graph(&p, |g| {
		let blocks = GraphBlockRange { graph: g };
		assert_eq!(blocks.in_(NodeId::INVALID), Rational::new(0, 1));
		assert_eq!(blocks.out(NodeId::INVALID), Rational::new(0, 1));
		assert_eq!(blocks.in_(clip), Rational::new(2, 1));
		assert_eq!(blocks.out(clip), Rational::new(6, 1));

		let tracks = GraphTrackRange { graph: g };
		assert_eq!(tracks.length(NodeId::INVALID), Rational::new(0, 1));
		assert_eq!(tracks.length(clip), Rational::new(0, 1), "not a track");
		assert_eq!(tracks.length(track), Rational::new(6, 1));
	});
}

#[test]
fn node_connect_validates_endpoints_and_inputs() {
	let p = project();
	let footage = footage_create(&p, None).expect("footage");
	let clip = add_block(&p, BlockKind::Clip);
	assert!(node_connect(&p, footage, clip, CLIP_TEXTURE_INPUT));
	assert!(
		!node_connect(&p, clip, footage, CLIP_TEXTURE_INPUT),
		"unknown input"
	);
	assert!(!node_connect(&p, NodeId::INVALID, clip, CLIP_TEXTURE_INPUT));
	assert!(!node_connect(
		&p,
		footage,
		NodeId::INVALID,
		CLIP_TEXTURE_INPUT
	));
}

#[test]
fn find_input_footage_direct_and_through_the_graph() {
	let p = project();
	let footage = footage_create(&p, None).expect("footage");
	let clip = add_block(&p, BlockKind::Clip);
	let folder = folder_create(&p).expect("folder");

	// A footage node answers with itself.
	with_graph(&p, |g| {
		assert_eq!(find_input_footage(g, footage), Some(footage))
	});
	// A clip answers with its recorded footage...
	clip_set_footage(&p, clip, footage);
	with_graph(&p, |g| {
		assert_eq!(find_input_footage(g, clip), Some(footage))
	});
	// ...but a stale recorded footage is ignored (falls through to the
	// graph walk, which finds nothing here).
	clip_set_footage(&p, clip, NodeId::INVALID);
	with_graph(&p, |g| assert_eq!(find_input_footage(g, clip), None));
	assert_eq!(node_find_input_footage(&p, clip), None);
	// A graph edge to a footage node is found by the walk.
	assert!(node_connect(&p, footage, clip, CLIP_TEXTURE_INPUT));
	assert_eq!(node_find_input_footage(&p, clip), Some(footage));
	// Non-footage, unconnected nodes have no footage.
	assert_eq!(node_find_input_footage(&p, folder), None);
	assert_eq!(node_find_input_footage(&p, NodeId::INVALID), None);
}

#[test]
fn pixel_format_codes_map_known_and_unknown() {
	use quercus_core::PixelFormat as P;
	assert_eq!(pixel_format_from_code(-1), P::Invalid);
	assert_eq!(pixel_format_from_code(0), P::U8);
	assert_eq!(pixel_format_from_code(1), P::U10);
	assert_eq!(pixel_format_from_code(2), P::U16);
	assert_eq!(pixel_format_from_code(3), P::F16);
	assert_eq!(pixel_format_from_code(4), P::F32);
	assert_eq!(pixel_format_from_code(99), P::Invalid);
}

// ---- undo commands --------------------------------------------------

#[test]
fn folder_add_child_command_redo_undo() {
	let p = project();
	let folder = folder_create(&p).expect("folder");
	let child = footage_create(&p, None).expect("footage");
	let mut cmd = folder_add_child_command((p.clone(), folder), (p.clone(), child));

	assert!(folder_children(&p, folder).is_empty());
	cmd.redo_now();
	assert_eq!(folder_children(&p, folder), vec![child]);
	assert_eq!(bin_folder_of(&p, child), Some(folder));

	cmd.undo_now();
	assert!(folder_children(&p, folder).is_empty());
	assert_eq!(bin_folder_of(&p, child), None);
}

#[test]
fn remove_node_command_restores_the_entry_and_folder_slot() {
	let p = project();
	let folder = folder_create(&p).expect("folder");
	let first = footage_create(&p, None).expect("first");
	let second = footage_create(&p, None).expect("second");
	folder_add_child_command((p.clone(), folder), (p.clone(), first)).redo_now();
	folder_add_child_command((p.clone(), folder), (p.clone(), second)).redo_now();
	assert_eq!(folder_children(&p, folder), vec![first, second]);

	let mut cmd = remove_node_command(p.clone(), first);
	cmd.redo_now();
	assert!(
		with_graph(&p, |g| g.get(first).is_none()),
		"removed from graph"
	);
	assert_eq!(folder_children(&p, folder), vec![second]);

	cmd.undo_now();
	assert!(with_graph(&p, |g| g.get(first).is_some()), "restored");
	assert_eq!(
		folder_children(&p, folder),
		vec![first, second],
		"slot restored"
	);
	assert_eq!(bin_folder_of(&p, first), Some(folder));
}

#[test]
fn remove_node_command_handles_nodes_without_a_folder() {
	let p = project();
	let node = folder_create(&p).expect("folder");
	let mut cmd = remove_node_command(p.clone(), node);
	cmd.redo_now();
	assert!(with_graph(&p, |g| g.get(node).is_none()));
	cmd.undo_now();
	assert!(with_graph(&p, |g| g.get(node).is_some()));
	assert_eq!(bin_folder_of(&p, node), None);
}

#[test]
fn add_track_command_appends_and_removes() {
	let p = project();
	let (_, list) = sequence_with_list(&p, TrackType::Subtitle);
	assert_eq!(tracklist_track_count(&p, list), 0);
	let mut cmd = add_track_command(p.clone(), list);
	cmd.redo_now();
	assert_eq!(tracklist_track_count(&p, list), 1);
	let track = tracklist_track_at(&p, list, 0).expect("track");
	assert_eq!(track_type(&p, track), Some(TrackType::Subtitle));
	cmd.undo_now();
	assert_eq!(tracklist_track_count(&p, list), 0);
	assert!(with_graph(&p, |g| g.get(track).is_none()), "track removed");

	// A stale list makes redo a no-op instead of a panic.
	let mut missing = add_track_command(p.clone(), NodeId::INVALID);
	missing.redo_now();
	missing.undo_now();
}
