use super::*;
use crate::util::{
	block_clip_create, block_in, block_length, block_out, same_block, track_append_block,
	track_block_at, track_block_count, NodeRef,
};
use quercus_core::TimeRange;
use quercus_node::block::ClipBlockBehavior;
use quercus_node::node::NodeCore;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::{TrackBehavior, TrackListBehavior};

/// Project fixture: a sequence owning one video track list with one
/// video track.
struct Fixture {
	project: Arc<Mutex<Project>>,
	seq: NodeRef,
	track: NodeRef,
}

fn fixture() -> Fixture {
	let project = Project::new();
	let (seq_id, list_id, track_id) = {
		let mut p = project.lock().unwrap();
		let (core, behavior) = SequenceBehavior::create();
		let seq_id = p.graph.add_node(core, behavior);
		let (core, behavior) = TrackListBehavior::create();
		let list_id = p.graph.add_node(core, behavior);
		let (core, behavior) = (
			NodeCore::new(),
			Box::new(TrackBehavior::new(TrackType::Video)),
		);
		let track_id = p.graph.add_node(core, behavior);
		{
			let seq = p.graph.get_mut(seq_id).unwrap();
			let s = seq
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<SequenceBehavior>()
				.unwrap();
			s.track_lists.push(list_id);
		}
		let list = p.graph.get_mut(list_id).unwrap();
		let l = list
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.unwrap();
		l.sequence = Some(seq_id);
		l.tracks.push(track_id);
		let track = p.graph.get_mut(track_id).unwrap();
		let t = track
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackBehavior>()
			.unwrap();
		t.kind = TrackType::Video;
		t.track_list = Some(list_id);
		(seq_id, list_id, track_id)
	};
	let _ = list_id;
	Fixture {
		project: project.clone(),
		seq: NodeRef::new(project.clone(), seq_id),
		track: NodeRef::new(project, track_id),
	}
}

/// Add a clip spanning `[in, out)` on the fixture's video track.
fn add_clip(fx: &Fixture, in_: Rational, out: Rational) -> NodeRef {
	let clip = block_clip_create(&fx.project);
	{
		let mut p = fx.project.lock().unwrap();
		let e = p.graph.get_mut(clip.id).unwrap();
		let c = e
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<ClipBlockBehavior>()
			.unwrap();
		c.core.range = TimeRange::new(in_, out);
	}
	track_append_block(&fx.track, &clip);
	clip
}

/// The pre-enable topology: the sequence feeds the clip's effect input.
fn connect_sequence_to_clip(fx: &Fixture, clip: &NodeRef) {
	let mut p = fx.project.lock().unwrap();
	p.graph
		.connect(fx.seq.id, clip.id, clip_input::TEXTURE_INPUT, -1)
		.unwrap();
}

/// The multicam node currently feeding `clip` (via `clip_find_multicam`).
#[allow(unused_variables)]
fn multicam_of(_fx: &Fixture, clip: &NodeRef) -> Option<NodeRef> {
	clip_find_multicam(clip)
}

/// The multicam node feeding `clip`, asserted present.
fn multicam_of_expect(fx: &Fixture, clip: &NodeRef) -> NodeRef {
	multicam_of(fx, clip).expect("clip has a multicam")
}

/// The `current_in` value of a multicam node.
fn current_of(fx: &Fixture, mc: &NodeRef) -> i32 {
	let p = fx.project.lock().unwrap();
	multicam_current_source(&p.graph, mc.id)
}

/// Set the `current_in` of a multicam node (test setup for non-default
/// initial source).
fn set_current(fx: &Fixture, mc: &NodeRef, source: i32) {
	let mut p = fx.project.lock().unwrap();
	multicam_set_current(&mut p.graph, mc.id, source);
}

/// The `sequence_type_in` value of a multicam node.
fn sequence_type_of(fx: &Fixture, mc: &NodeRef) -> i64 {
	let p = fx.project.lock().unwrap();
	let v = p
		.graph
		.get(mc.id)
		.unwrap()
		.core
		.standard_value(SEQUENCE_TYPE_INPUT, -1);
	match v {
		NodeValue::Combo(i) => i,
		other => panic!("sequence_type_in is {other:?}, expected Combo"),
	}
}

#[test]
fn labels_format_count() {
	assert_eq!(enable_label(3), "Multi-Cam Enabled On 3 Clip(s)");
	assert_eq!(disable_label(2), "Multi-Cam Disabled On 2 Clip(s)");
	assert_eq!(SWITCH_LABEL, "Switched Multi-Camera Source");
}

/// `clip_find_multicam` is None for a plain clip and Some after the
/// sequence is routed through a multicam (the C++
/// `ClipBlock::find_multicam` parity).
#[test]
fn find_multicam_positive_and_negative() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));

	// No multicam anywhere: nothing found.
	assert!(multicam_of(&fx, &clip).is_none());

	// A non-multicam source on the effect input is not reported either.
	connect_sequence_to_clip(&fx, &clip);
	assert!(multicam_of(&fx, &clip).is_none());

	// After the enable command, the multicam feeds the clip.
	let mut cmd = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	cmd.redo();
	let mc = multicam_of_expect(&fx, &clip);
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph.get(mc.id).unwrap().behavior.type_id(),
			"org.olivevideoeditor.Olive.multicam"
		);
	}

	// After undo the multicam is gone again.
	cmd.undo();
	assert!(multicam_of(&fx, &clip).is_none());
}

/// `multicam_enable` re-routes the sequence→clip edges through a fresh
/// multicam node and sets `sequence_type_in`; undo restores the exact
/// pre-enable edges.
#[test]
fn enable_reroutes_through_multicam_and_undo_restores() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));
	connect_sequence_to_clip(&fx, &clip);

	let mut cmd = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	cmd.redo();

	let mc = multicam_of_expect(&fx, &clip);
	{
		let p = fx.project.lock().unwrap();
		// The sequence no longer feeds the clip; the multicam does.
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(mc.id)
		);
		// The sequence feeds the multicam's sequence_in.
		assert_eq!(
			p.graph.connected_output(mc.id, SEQUENCE_INPUT, -1),
			Some(fx.seq.id)
		);
	}
	// The type selector mirrors the video track type.
	assert_eq!(sequence_type_of(&fx, &mc), 0);

	cmd.undo();
	// Back to the pre-enable edge; the multicam node is gone.
	assert!(multicam_of(&fx, &clip).is_none());
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(fx.seq.id)
		);
		assert!(!p.graph.is_valid(mc.id));
	}

	// Redo re-creates the multicam (fresh node id) and re-routes again.
	cmd.redo();
	let mc2 = multicam_of_expect(&fx, &clip);
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(mc2.id)
		);
	}
}

/// `multicam_disable` re-wires the multicam's outputs back to the
/// sequence and removes the node; undo restores the enabled state.
#[test]
fn disable_round_trip() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));
	connect_sequence_to_clip(&fx, &clip);

	let mut enable = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	enable.redo();
	let mc = multicam_of_expect(&fx, &clip);

	let mut disable = MultiCamDisableCommand::new(vec![clip.clone()]);
	disable.redo();
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(fx.seq.id)
		);
		assert!(!p.graph.is_valid(mc.id));
	}

	disable.undo();
	let restored_mc = multicam_of_expect(&fx, &clip);
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(restored_mc.id)
		);
		assert_eq!(
			p.graph.connected_output(restored_mc.id, SEQUENCE_INPUT, -1),
			Some(fx.seq.id)
		);
	}
}

/// `multicam_switch` without splitting sets the source on the clip and
/// its linked clips' multicams; a single undo restores every source.
#[test]
fn switch_no_split_updates_clip_and_links() {
	let fx = fixture();
	let clip_a = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));
	let clip_b = add_clip(&fx, Rational::new(50, 1), Rational::new(100, 1));
	connect_sequence_to_clip(&fx, &clip_a);
	connect_sequence_to_clip(&fx, &clip_b);
	{
		let mut p = fx.project.lock().unwrap();
		p.graph.link(clip_a.id, clip_b.id);
	}
	let mut enable =
		MultiCamEnableCommand::new(vec![clip_a.clone(), clip_b.clone()], fx.seq.clone());
	enable.redo();
	let mc_a = multicam_of_expect(&fx, &clip_a);
	let mc_b = multicam_of_expect(&fx, &clip_b);
	set_current(&fx, &mc_a, 1);
	set_current(&fx, &mc_b, 1);

	// Split disabled: the switch writes the source directly.
	let mut cmd = MultiCamSwitchCommand::new(clip_a.clone(), 2, false, Rational::new(30, 1));
	cmd.redo();
	assert_eq!(current_of(&fx, &mc_a), 2);
	assert_eq!(current_of(&fx, &mc_b), 2);
	// No split happened: still one block per track slot.
	assert_eq!(track_block_count(&fx.track), 2);

	cmd.undo();
	assert_eq!(current_of(&fx, &mc_a), 1);
	assert_eq!(current_of(&fx, &mc_b), 1);
	assert_eq!(track_block_count(&fx.track), 2);
}

/// `multicam_switch` with a playhead strictly inside the clip splits it
/// preserving links: the halves each own an independent multicam copy,
/// the post-playhead half switches source and its linked half follows,
/// the pre-playhead half keeps the old source. Undo restores the single
/// clip and its original source.
#[test]
fn switch_splits_and_copies_multicam() {
	let fx = fixture();
	let clip_a = add_clip(&fx, Rational::new(0, 1), Rational::new(100, 1));
	let clip_b = add_clip(&fx, Rational::new(0, 1), Rational::new(100, 1));
	// Same track layout: clip_b on a second track slot is created after;
	// link the two clips so the split is link-preserving.
	{
		let mut p = fx.project.lock().unwrap();
		p.graph.link(clip_a.id, clip_b.id);
	}
	connect_sequence_to_clip(&fx, &clip_a);
	connect_sequence_to_clip(&fx, &clip_b);
	let mut enable =
		MultiCamEnableCommand::new(vec![clip_a.clone(), clip_b.clone()], fx.seq.clone());
	enable.redo();
	let mc_a = multicam_of_expect(&fx, &clip_a);
	let mc_b = multicam_of_expect(&fx, &clip_b);
	set_current(&fx, &mc_a, 1);
	set_current(&fx, &mc_b, 1);

	// Switch at t=40 (strictly inside [0,100)) with split.
	let mut cmd = MultiCamSwitchCommand::new(clip_a.clone(), 3, true, Rational::new(40, 1));
	cmd.redo();

	// Two halves per clip (both linked clips split): 4 blocks total.
	assert_eq!(track_block_count(&fx.track), 4);
	let first = track_block_at(&fx.track, 0).unwrap();
	let second = track_block_at(&fx.track, 1).unwrap();
	assert!(
		same_block(&first, &clip_a),
		"original clip keeps the in half"
	);
	assert_eq!(block_in(&first), Rational::new(0, 1));
	assert_eq!(block_out(&first), Rational::new(40, 1));
	assert_eq!(block_in(&second), Rational::new(40, 1));
	assert_eq!(block_out(&second), Rational::new(100, 1));

	// The pre-playhead half keeps its own multicam and old source; the
	// post-playhead half has a distinct multicam copy with the new
	// source.
	let mc_first = multicam_of_expect(&fx, &first);
	let mc_second = multicam_of_expect(&fx, &second);
	assert_ne!(mc_first.id, mc_second.id, "independent multicam copies");
	assert_eq!(current_of(&fx, &mc_first), 1);
	assert_eq!(current_of(&fx, &mc_second), 3);

	// The linked clip's post-playhead half switched too.
	let linked_second = {
		let p = fx.project.lock().unwrap();
		// The second half of clip_a links to the second half of clip_b.
		let links = p.graph.links_of(second.id);
		links.iter().find(|id| **id != second.id).copied().unwrap()
	};
	assert_ne!(linked_second, clip_a.id);
	let mc_linked = multicam_of_expect(&fx, &NodeRef::new(fx.project.clone(), linked_second));
	assert_eq!(current_of(&fx, &mc_linked), 3);

	// A single undo restores the two original clips and the old sources.
	cmd.undo();
	assert_eq!(track_block_count(&fx.track), 2);
	assert_eq!(block_length(&clip_a), Rational::new(100, 1));
	assert_eq!(current_of(&fx, &multicam_of_expect(&fx, &clip_a)), 1);
	assert_eq!(current_of(&fx, &multicam_of_expect(&fx, &clip_b)), 1);

	// Redo re-splits (the copies re-attach identity-preserving) and
	// re-applies the new source.
	cmd.redo();
	assert_eq!(track_block_count(&fx.track), 4);
	let second_again = track_block_at(&fx.track, 1).unwrap();
	let mc_second_again = multicam_of_expect(&fx, &second_again);
	assert_eq!(current_of(&fx, &mc_second_again), 3);
	let mc_first_again = multicam_of_expect(&fx, &track_block_at(&fx.track, 0).unwrap());
	assert_eq!(current_of(&fx, &mc_first_again), 1);
}

/// The split's dependency-graph copy gives the two halves independent
/// multicam nodes: mutating one half's multicam does not affect the
/// other's.
#[test]
fn split_copies_dependency_graph_independently() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(100, 1));
	connect_sequence_to_clip(&fx, &clip);
	let mut enable = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	enable.redo();
	let mc_orig = multicam_of_expect(&fx, &clip);

	// Split manually through the split command.
	let mut split = crate::undosplit::BlockSplitCommand::new(clip.clone(), Rational::new(40, 1));
	split.prepare();
	split.redo();
	let second = split.new_block().unwrap();

	let mc_first = multicam_of_expect(&fx, &clip);
	let mc_second = multicam_of_expect(&fx, &second);
	assert_ne!(mc_first.id, mc_second.id);
	assert_ne!(mc_orig.id, mc_second.id);

	// Both copies kept the original source value.
	assert_eq!(current_of(&fx, &mc_first), 0);
	assert_eq!(current_of(&fx, &mc_second), 0);

	// Changing the second half's source leaves the first half's copy
	// untouched.
	let mut p = fx.project.lock().unwrap();
	multicam_set_current(&mut p.graph, mc_second.id, 5);
	drop(p);
	assert_eq!(current_of(&fx, &mc_first), 0);
	assert_eq!(current_of(&fx, &mc_second), 5);
}

/// A switch with `split_clip` but a playhead outside the clip does not
/// split; it writes the source directly (C++ only splits when the
/// playhead lies strictly inside).
#[test]
fn switch_outside_playhead_does_not_split() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));
	connect_sequence_to_clip(&fx, &clip);
	let mut enable = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	enable.redo();
	let mc = multicam_of_expect(&fx, &clip);

	let mut cmd = MultiCamSwitchCommand::new(
		clip.clone(),
		2,
		true,
		Rational::new(70, 1), // past the out point
	);
	cmd.redo();
	assert_eq!(track_block_count(&fx.track), 1);
	assert_eq!(current_of(&fx, &mc), 2);

	cmd.undo();
	assert_eq!(current_of(&fx, &mc), 0);
}

/// A switch on a clip without any multicam is a no-op (the C++
/// `if (!node_) return;` guard) — no split, no writes.
#[test]
fn switch_without_multicam_is_noop() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(100, 1));

	let mut cmd = MultiCamSwitchCommand::new(clip.clone(), 2, true, Rational::new(40, 1));
	cmd.redo();
	// No split happened.
	assert_eq!(track_block_count(&fx.track), 1);
	cmd.undo();
	assert_eq!(track_block_count(&fx.track), 1);
}

#[test]
fn disable_skips_clips_without_sequence_connection() {
	let fx = fixture();
	let clip = add_clip(&fx, Rational::new(0, 1), Rational::new(50, 1));
	connect_sequence_to_clip(&fx, &clip);
	let mut enable = MultiCamEnableCommand::new(vec![clip.clone()], fx.seq.clone());
	enable.redo();
	let mc = multicam_of_expect(&fx, &clip);

	// A second clip without any multicam is a no-op for the disable.
	let bare = add_clip(&fx, Rational::new(50, 1), Rational::new(100, 1));
	let mut disable = MultiCamDisableCommand::new(vec![clip.clone(), bare.clone()]);
	disable.redo();
	{
		let p = fx.project.lock().unwrap();
		assert_eq!(
			p.graph
				.connected_output(clip.id, clip_input::TEXTURE_INPUT, -1),
			Some(fx.seq.id)
		);
		assert!(!p.graph.is_valid(mc.id));
	}
	// The bare clip is untouched.
	assert!(multicam_of(&fx, &bare).is_none());
}
