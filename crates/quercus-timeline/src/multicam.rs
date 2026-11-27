// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Multi-camera editing commands (C++
//! `app/widget/multicam/multicamwidget.cpp::Switch`,
//! `app/widget/timelinewidget/timelinewidget.cpp::multicam_enabled_triggered`).
//!
//! A multi-cam clip is a clip whose effect input (`tex_in`, C++
//! `buffer_in`) is fed by a [`MultiCamNode`] (`quercus_node::nodes::multicamnode`).
//! The node's `sequence_in` connects to the sequence whose track list
//! supplies the angles; `sources_in` carries no real edges — the render
//! resolves element `i` to track `i` of the selected track list
//! (`sequence_type_in`).
//!
//! The three operations:
//!
//! * [`multicam_enable`] inserts a fresh `MultiCamNode` between the
//!   sequence and each clip: every input the sequence fed along the clip's
//!   dependency chain is re-routed through the node, `sequence_in` is
//!   connected, and `sequence_type_in` mirrors the clip's track type.
//! * [`multicam_disable`] is the reverse: the node's outputs are re-wired
//!   straight to the sequence and the node is removed.
//! * [`multicam_switch`] changes `current_in`. With `split_clip` and the
//!   playhead strictly inside the clip, the clip (and its linked blocks)
//!   is first split preserving links — each new half owns an independent
//!   copy of the clip's dependency graph (incl. its own `MultiCamNode`),
//!   so the halves after the playhead switch source while the halves
//!   before keep theirs.
//!
//! Every operation is exposed as a single undo command ([`UndoCommand`]);
//! the caller pushes it with the labels
//! [`ENABLE_LABEL`]/[`DISABLE_LABEL`]/[`SWITCH_LABEL`] (or the
//! `enable_label`/`disable_label` helpers), matching the C++ undo names.

use crate::undocommon;
use std::collections::HashSet;
use std::iter;
use std::sync::{Arc, Mutex};

use quercus_core::Rational;
use quercus_node::block::clip_input;
use quercus_node::graph::{Graph, NodeEntry};
use quercus_node::id::NodeId;
use quercus_node::nodes::multicamnode::{
	MultiCamNode, CURRENT_INPUT, SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT,
};
use quercus_node::project::Project;
use quercus_node::track::TrackType;
use quercus_node::value::NodeValue;
use quercus_undo::undocommand::UndoCommand;

use crate::undosplit::BlockSplitPreservingLinksCommand;
use crate::util::{block_in, block_kind, block_out, block_track, BlockKind, NodeRef};

/// Undo label for [`multicam_enable`] (C++
/// `tr("Multi-Cam Enabled On %1 Clip(s)")`).
pub const ENABLE_LABEL: &str = "Multi-Cam Enabled On %1 Clip(s)";
/// Undo label for [`multicam_disable`] (C++
/// `tr("Multi-Cam Disabled On %1 Clip(s)")`).
pub const DISABLE_LABEL: &str = "Multi-Cam Disabled On %1 Clip(s)";
/// Undo label for [`multicam_switch`] (C++
/// `tr("Switched Multi-Camera Source")`).
pub const SWITCH_LABEL: &str = "Switched Multi-Camera Source";

/// Fills [`ENABLE_LABEL`] with `clip_count`, yielding e.g. "Multi-Cam Enabled On
/// 3 Clip(s)".
///
/// Pass the number of clips the enable command covers; only the `%1` placeholder
/// is substituted. Counterpart of [`disable_label`].
pub fn enable_label(clip_count: usize) -> String {
	ENABLE_LABEL.replace("%1", &clip_count.to_string())
}

/// Fills [`DISABLE_LABEL`] with `clip_count`, yielding e.g. "Multi-Cam Disabled
/// On 3 Clip(s)".
///
/// Pass the number of clips the disable command covers; only the `%1` placeholder
/// is substituted. Counterpart of [`enable_label`].
pub fn disable_label(clip_count: usize) -> String {
	DISABLE_LABEL.replace("%1", &clip_count.to_string())
}

// ---------------------------------------------------------------------------
// Lookup helpers
// ---------------------------------------------------------------------------

/// `ClipBlock::find_multicam()` — the `MultiCamNode` feeding the clip's
/// effect input (`tex_in`, C++ `buffer_in`), searched at depth 1 and then
/// along the dependency chain exactly like the C++
/// `find_input_nodes_connected_to_input<MultiCamNode>(input, 1)`
/// (`// CPP-PARITY: clip.cpp:657-665`).
pub fn clip_find_multicam(clip: &NodeRef) -> Option<NodeRef> {
	let project = clip.project.clone();
	{
		let p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		let mut list = Vec::new();
		find_input_nodes_connected_to_input_internal(
			&p.graph,
			clip.id,
			clip_input::TEXTURE_INPUT,
			-1,
			1,
			&mut list,
		);
		list.first().copied()
	}
	.map(|id| NodeRef::new(project, id))
}

/// One step of the C++ `find_input_nodes_connected_to_input`: the node
/// feeding `node.input[element]`, then a recursive walk of its inputs,
/// stopping after `maximum` matches (`maximum == 0` = unlimited).
fn find_input_nodes_connected_to_input_internal(
	graph: &Graph,
	node: NodeId,
	input: &str,
	element: i32,
	maximum: usize,
	list: &mut Vec<NodeId>,
) {
	let Some(source) = graph.connected_output(node, input, element) else {
		return;
	};
	if is_multicam(graph, source) {
		list.push(source);
		if maximum != 0 && list.len() == maximum {
			return;
		}
	}
	find_input_node_internal(graph, source, maximum, list);
}

/// C++ `find_input_node_internal` — walk `node`'s input connections,
/// checking each source and recursing.
fn find_input_node_internal(graph: &Graph, node: NodeId, maximum: usize, list: &mut Vec<NodeId>) {
	for (from, _input, _element) in graph.input_connections(node) {
		if is_multicam(graph, from) {
			list.push(from);
			if maximum != 0 && list.len() == maximum {
				return;
			}
		}
		find_input_node_internal(graph, from, maximum, list);
		if maximum != 0 && list.len() == maximum {
			return;
		}
	}
}

/// Whether `id` resolves to a `MultiCamNode` in `graph`.
///
/// Used while walking input connections to find the nodes feeding a clip, so a
/// missing or non-multicam id simply answers `false`; this never panics.
fn is_multicam(graph: &Graph, id: NodeId) -> bool {
	graph
		.get(id)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<MultiCamNode>())
		.is_some()
}

/// Set/clear the `MultiCamNode` sequence state after a `sequence_in` edge
/// edit (the graph arena fires no behavior events, so the command keeps the
/// behavior in sync — C++ `InputConnectedEvent`/`InputDisconnectedEvent`).
fn sync_multicam_sequence(graph: &mut Graph, mc: NodeId, sequence: Option<NodeId>) {
	let Some(entry) = graph.get_mut(mc) else {
		return;
	};
	if let Some(mc_node) = entry
		.behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<MultiCamNode>())
	{
		mc_node.set_sequence(&mut entry.core, sequence);
	}
}

/// C++ `Node::find_ways_node_arrives_here()` — every input slot along
/// `node`'s dependency chain that is fed (directly or transitively) by
/// `output`. Returns `(target, input_id, element)` where the target's
/// input is directly fed by `output`.
fn find_ways_node_arrives_here(
	graph: &Graph,
	output: NodeId,
	node: NodeId,
	v: &mut Vec<(NodeId, String, i32)>,
) {
	for (from, input, element) in graph.input_connections(node) {
		if from == output {
			v.push((node, input, element));
		} else {
			find_ways_node_arrives_here(graph, output, from, v);
		}
	}
}

/// Reads the selected angle of a multicam node.
///
/// Returns the node's `current_in` standard value as an `i32` — a zero-based
/// index into the sequence's track list — or `-1` when `mc` is missing from
/// `graph` (stale id) or the value is unavailable. Pairs with
/// `multicam_set_current`, whose undo path stores the value returned here.
/// Consumers treat `-1` as "no angle selected".
fn multicam_current_source(graph: &Graph, mc: NodeId) -> i32 {
	graph
		.get(mc)
		.map(|e| e.core.standard_value(CURRENT_INPUT, -1).to_double() as i32)
		.unwrap_or(-1)
}

/// Selects the angle of a multicam node by writing its `current_in` standard
/// value.
///
/// `graph` is the project graph, `mc` the multicam node, and `source` the
/// zero-based angle index into the node's selected track list; `-1` means "no
/// selection", which is also what a stale node reads as (see
/// `multicam_current_source`). Does nothing when `mc` is not in the graph.
/// `current_in` is a single-value input, so the element index is `-1`.
fn multicam_set_current(graph: &mut Graph, mc: NodeId, source: i32) {
	if let Some(entry) = graph.get_mut(mc) {
		entry
			.core
			.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(source as i64));
	}
}

/// The clip's track media type (C++ `track_type_of(block_track(c))`);
/// defaults to video when the clip is trackless.
fn clip_track_type(clip: &NodeRef) -> TrackType {
	block_track(clip)
		.and_then(|track| {
			let p = track
				.project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			p.graph
				.get(track.id)
				.and_then(|e| e.behavior.as_any())
				.and_then(|a| a.downcast_ref::<quercus_node::track::TrackBehavior>())
				.map(|t| t.kind)
		})
		.unwrap_or(TrackType::Video)
}

/// Linked block ids of `block` in the project graph (C++
/// `Block::block_links()`).
fn block_links(clip: &NodeRef) -> Vec<NodeRef> {
	let project = clip.project.clone();
	let p = project
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner());
	p.graph
		.links_of(clip.id)
		.into_iter()
		.map(|id| NodeRef::new(project.clone(), id))
		.collect()
}

/// For every clip in `clips`, the `(clip, multicam, old_current)` tuple for
/// each multicam found on the clip or its clip links (C++ Switch: set the
/// source on the new clip's multicam and every linked clip's multicam).
fn collect_switch_targets(clip: &NodeRef) -> Vec<(NodeRef, NodeId, i32)> {
	let project = clip.project.clone();
	let mut targets = Vec::new();
	let mut seen: HashSet<NodeId> = HashSet::new();
	for c in iter::once(clip.clone()).chain(block_links(clip)) {
		if !seen.insert(c.id) {
			continue;
		}
		if block_kind(&c) != BlockKind::Clip {
			continue;
		}
		if let Some(mc) = clip_find_multicam(&c) {
			let p = project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			let old = multicam_current_source(&p.graph, mc.id);
			targets.push((c, mc.id, old));
		}
	}
	targets
}

// ---------------------------------------------------------------------------
// MultiCamEnableCommand
// ---------------------------------------------------------------------------

/// Per-clip state of [`MultiCamEnableCommand`].
struct EnableState {
	/// The multicam node created for the clip (valid after the first redo).
	multicam: Option<NodeId>,
	/// Detached arena entry while the node is out of the graph between
	/// `undo` and the next `redo`.
	entry: Option<NodeEntry>,
	/// `sequence_type_in` value (the clip's track type ordinal).
	sequence_type: i32,
	/// Inputs the sequence fed along the clip's chain before the enable
	/// (`(target, input_id, element)`), re-routed through the multicam.
	rerouted: Vec<(NodeId, String, i32)>,
}

/// `MultiCamEnableCommand` — wrap each clip's source through a fresh
/// `MultiCamNode` (C++ `multicam_enabled_triggered(true)`). One undo
/// command covering every clip; push with [`enable_label`].
pub struct MultiCamEnableCommand {
	/// Clips to enable multicam on.
	clips: Vec<NodeRef>,
	/// The sequence whose track list supplies the angles.
	sequence: NodeRef,
	/// Per-clip state, built on the first redo.
	state: Vec<EnableState>,
}

impl MultiCamEnableCommand {
	/// Creates the enable command for `clips`, capturing nothing yet.
	///
	/// `clips` are the clips to wrap in a `MultiCamNode` (already-enabled ones are
	/// skipped at redo time) and `sequence` is the sequence whose track list supplies
	/// the angles and which gets re-routed through each node. Nodes and edges are
	/// created lazily by `prepare` on the first `redo`.
	pub fn new(clips: Vec<NodeRef>, sequence: NodeRef) -> Self {
		Self {
			clips,
			sequence,
			state: Vec::new(),
		}
	}

	/// Plans each clip: creates its `MultiCamNode` and records the sequence-fed
	/// inputs to re-route.
	///
	/// For each clip without an existing multicam it adds the node to the project
	/// graph, stores the clip's track type (video/audio) as `sequence_type_in`, and
	/// collects every `(target, input, element)` slot the command's `sequence`
	/// currently reaches along the clip's chain. Runs once — later calls return early
	/// while state exists. Already-enabled clips are skipped, so `state` need not be
	/// index-aligned with `clips`.
	fn prepare(&mut self) {
		if !self.state.is_empty() {
			return;
		}
		let project = self.sequence.project.clone();
		for clip in &self.clips {
			// Defense in depth: an already-enabled clip keeps its multicam;
			// wrapping it again would nest a second node. Skipped clips push
			// no state, and redo/undo only walk `state`, so the two lists
			// need not be index-aligned.
			if clip_find_multicam(clip).is_some() {
				continue;
			}
			let mc = {
				let (core, behavior) = quercus_node::nodes::multicamnode::create();
				let mut p = project
					.lock()
					.unwrap_or_else(|poisoned| poisoned.into_inner());
				p.graph.add_node(core, behavior)
			};
			let sequence_type = clip_track_type(clip).to_c();
			let rerouted = {
				let p = project
					.lock()
					.unwrap_or_else(|poisoned| poisoned.into_inner());
				let mut v = Vec::new();
				find_ways_node_arrives_here(&p.graph, self.sequence.id, clip.id, &mut v);
				v
			};
			self.state.push(EnableState {
				multicam: Some(mc),
				entry: None,
				sequence_type,
				rerouted,
			});
		}
	}

	/// Inserts the planned multicam nodes and re-routes the sequence edges through
	/// them.
	///
	/// Calls `prepare` first, then for each planned clip: re-attaches the node if a
	/// previous `undo` detached it, sets `sequence_type_in`, moves the sequence-fed
	/// inputs over to the node, connects `sequence_in`, and syncs the node's cached
	/// sequence state. Safe to call after both `undo` and the first execution; the
	/// return values of the individual graph connects are ignored.
	pub fn redo(&mut self) {
		self.prepare();
		let project = self.sequence.project.clone();
		for st in self.state.iter_mut() {
			let Some(mc) = st.multicam else {
				continue;
			};
			let mut p = project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			// Re-attach the node if a previous undo detached it.
			if let Some(entry) = st.entry.take() {
				p.graph.add_entry(entry, mc);
			}
			// Set the sequence type selector (the clip's track type).
			if let Some(entry) = p.graph.get_mut(mc) {
				entry.core.set_standard_value(
					SEQUENCE_TYPE_INPUT,
					-1,
					NodeValue::Combo(st.sequence_type as i64),
				);
			}
			// Disconnect the sequence from each input it fed and connect
			// the multicam in its place.
			for (target, input, element) in &st.rerouted {
				p.graph
					.disconnect(self.sequence.id, *target, input, *element);
				p.graph.connect(mc, *target, input, *element).ok();
			}
			// Connect the sequence to the multicam's sequence_in.
			p.graph.disconnect(self.sequence.id, mc, SEQUENCE_INPUT, -1);
			p.graph
				.connect(self.sequence.id, mc, SEQUENCE_INPUT, -1)
				.ok();
			// Keep the behavior's cached sequence state in sync.
			sync_multicam_sequence(&mut p.graph, mc, Some(self.sequence.id));
		}
	}

	/// `undo`: disconnect the multicam, re-connect the sequence straight to
	/// each original input, and detach the multicam node.
	pub fn undo(&mut self) {
		let project = self.sequence.project.clone();
		for st in self.state.iter_mut() {
			let Some(mc) = st.multicam else {
				continue;
			};
			let mut p = project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			// The multicam fed every re-routed input; wire the sequence
			// back directly.
			for (target, input, element) in &st.rerouted {
				p.graph.disconnect(mc, *target, input, *element);
				p.graph
					.connect(self.sequence.id, *target, input, *element)
					.ok();
			}
			// Drop the sequence_in edge.
			p.graph.disconnect(self.sequence.id, mc, SEQUENCE_INPUT, -1);
			// Detach the multicam node (identity preserved for the next
			// redo).
			if st.entry.is_none() {
				st.entry = p.graph.take_node(mc);
			}
		}
	}

	/// Wrap as an quercusundo command value.
	pub fn to_command(self) -> UndoCommand {
		undocommon::box_command(self)
	}
}

impl undocommon::Command for MultiCamEnableCommand {
	fn redo(&mut self) {
		self.redo();
	}

	fn undo(&mut self) {
		self.undo();
	}
}

/// Builds the undo command that turns multi-cam on for `clips`
/// (C++ `multicam_enabled_triggered(true)`).
///
/// One fresh `MultiCamNode` is inserted per clip: every input the `sequence` fed
/// along the clip's dependency chain is re-routed through the node,
/// `sequence_in` is connected to `sequence`, and `sequence_type_in` mirrors the
/// clip's track type. Clips that already have a multicam are skipped, so the
/// command never nests a second node. Push with [`enable_label`]; pairs with
/// [`multicam_disable`].
pub fn multicam_enable(clips: Vec<NodeRef>, sequence: NodeRef) -> UndoCommand {
	MultiCamEnableCommand::new(clips, sequence).to_command()
}

// ---------------------------------------------------------------------------
// MultiCamDisableCommand
// ---------------------------------------------------------------------------

/// Per-clip state of [`MultiCamDisableCommand`].
struct DisableState {
	/// The multicam node currently feeding the clip.
	multicam: NodeId,
	/// The sequence the multicam pulled angles from (from its
	/// `sequence_in` edge).
	sequence: NodeId,
	/// The multicam's output edges `(target, input_id, element)` that
	/// must be re-wired straight to the sequence.
	outputs: Vec<(NodeId, String, i32)>,
	/// Detached arena entry while the node is out of the graph between
	/// `undo` and the next `redo`.
	entry: Option<NodeEntry>,
}

/// `MultiCamDisableCommand` — bypass a clip's `MultiCamNode` and remove it
/// (C++ `multicam_enabled_triggered(false)`). One undo command covering
/// every clip; push with [`disable_label`].
pub struct MultiCamDisableCommand {
	/// Clips to disable multicam on.
	clips: Vec<NodeRef>,
	/// Per-clip state, built on the first redo.
	state: Vec<DisableState>,
}

impl MultiCamDisableCommand {
	/// Creates the disable command for `clips`, capturing nothing yet.
	///
	/// The multicam nodes, their sequences, and the output edges are discovered
	/// lazily by `prepare` on the first `redo`, so the clips must still be live then.
	pub fn new(clips: Vec<NodeRef>) -> Self {
		Self {
			clips,
			state: Vec::new(),
		}
	}

	/// Plans each clip: records its multicam node, the sequence that node reads from
	/// (`sequence_in`), and the node's output edges to re-wire.
	///
	/// Runs once — later calls return early while state exists, which makes `redo`
	/// idempotent across undo/redo cycles. Clips without a multicam, or whose
	/// `sequence_in` is unconnected, contribute no state and are left untouched.
	/// Called by `redo`.
	fn prepare(&mut self) {
		if !self.state.is_empty() {
			return;
		}
		let clips = self.clips.clone();
		for clip in clips {
			let Some(mc) = clip_find_multicam(&clip) else {
				continue;
			};
			let (sequence, outputs) = {
				let p = clip
					.project
					.lock()
					.unwrap_or_else(|poisoned| poisoned.into_inner());
				let sequence = p
					.graph
					.connected_output(mc.id, SEQUENCE_INPUT, -1)
					.unwrap_or(NodeId::INVALID);
				let outputs = p.graph.output_connections(mc.id);
				(sequence, outputs)
			};
			if sequence == NodeId::INVALID {
				// No connected sequence: nothing to bypass back to; leave
				// the node untouched.
				continue;
			}
			self.state.push(DisableState {
				multicam: mc.id,
				sequence,
				outputs,
				entry: None,
			});
		}
	}

	/// `redo`: re-wire the multicam's outputs straight to the sequence and
	/// detach the multicam node.
	pub fn redo(&mut self) {
		self.prepare();
		let mut project: Option<Arc<Mutex<Project>>> = None;
		for st in self.state.iter_mut() {
			if project.is_none() {
				if let Some(clip) = self.clips.first() {
					project = Some(clip.project.clone());
				} else {
					return;
				}
			}
			let project = project.as_ref().expect("set above");
			let mut p = project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			// Re-attach the node if a previous undo detached it.
			if let Some(entry) = st.entry.take() {
				p.graph.add_entry(entry, st.multicam);
			}
			// Re-wire outputs to the sequence.
			for (target, input, element) in &st.outputs {
				p.graph.disconnect(st.multicam, *target, input, *element);
				p.graph.connect(st.sequence, *target, input, *element).ok();
			}
			// Detach the multicam (the sequence_in edge goes with it).
			if st.entry.is_none() {
				st.entry = p.graph.take_node(st.multicam);
			}
		}
	}

	/// `undo`: re-insert the multicam node, re-route the outputs through it
	/// (disconnecting the sequence), and re-connect the sequence_in edge.
	pub fn undo(&mut self) {
		// `undo` only runs after `redo`, which returned early when `clips`
		// was empty, so a project is always available here.
		let project = self
			.clips
			.first()
			.map(|c| c.project.clone())
			.unwrap_or_else(Project::new);
		for st in self.state.iter_mut() {
			let mut p = project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			if let Some(entry) = st.entry.take() {
				p.graph.add_entry(entry, st.multicam);
			}
			// The sequence now feeds every output directly; re-route them
			// through the multicam and restore the sequence_in edge.
			for (target, input, element) in &st.outputs {
				p.graph.disconnect(st.sequence, *target, input, *element);
				p.graph.connect(st.multicam, *target, input, *element).ok();
			}
			p.graph
				.connect(st.sequence, st.multicam, SEQUENCE_INPUT, -1)
				.ok();
			sync_multicam_sequence(&mut p.graph, st.multicam, Some(st.sequence));
		}
	}

	/// Wrap as an quercusundo command value.
	pub fn to_command(self) -> UndoCommand {
		undocommon::box_command(self)
	}
}

impl undocommon::Command for MultiCamDisableCommand {
	fn redo(&mut self) {
		self.redo();
	}

	fn undo(&mut self) {
		self.undo();
	}
}

/// Builds the undo command that removes multi-cam from `clips`
/// (C++ `multicam_enabled_triggered(false)`).
///
/// For every clip the `MultiCamNode` feeding its effect input is bypassed: the
/// node's outputs are connected straight to the sequence it pulled angles from
/// and the node is detached from the graph, with `undo` wiring it back. Clips
/// without a multicam, or whose multicam has no `sequence_in` edge, are skipped.
/// Push with [`disable_label`]; pairs with [`multicam_enable`].
pub fn multicam_disable(clips: Vec<NodeRef>) -> UndoCommand {
	MultiCamDisableCommand::new(clips).to_command()
}

// ---------------------------------------------------------------------------
// MultiCamSwitchCommand
// ---------------------------------------------------------------------------

/// `MultiCamSwitchCommand` — change the multicam source, optionally
/// splitting the clip at the playhead first (C++ `MulticamWidget::Switch`).
/// One undo command for the whole operation; push with [`SWITCH_LABEL`].
pub struct MultiCamSwitchCommand {
	/// The clip whose multicam source changes.
	clip: NodeRef,
	/// The new source index.
	source: i32,
	/// Whether to split the clip at the playhead (source change applies
	/// from the playhead forward).
	split_clip: bool,
	/// The playhead time (split point).
	playhead: Rational,
	/// The split command, when `split_clip` and the playhead lies strictly
	/// inside the clip.
	split: Option<BlockSplitPreservingLinksCommand>,
	/// Whether the clip was actually split on the first redo.
	did_split: bool,
	/// `(clip, multicam, old_source)` targets captured on the first redo;
	/// the ids stay stable across undo/redo (the split re-attaches its
	/// copies identity-preserving).
	targets: Vec<(NodeRef, NodeId, i32)>,
	/// True when the clip has no multicam: the command is a no-op (the C++
	/// `if (!node_) return;` guard).
	noop: bool,
}

impl MultiCamSwitchCommand {
	/// Creates the switch command without touching the graph.
	///
	/// `clip` is the multi-cam clip to switch, `source` the new angle index
	/// (`current_in`), `split_clip` whether to cut at `playhead`, and `playhead` the
	/// split time in project time. Nothing is captured until the first
	/// `MultiCamSwitchCommand::redo`, so the clip must still be live at that point.
	pub fn new(clip: NodeRef, source: i32, split_clip: bool, playhead: Rational) -> Self {
		Self {
			clip,
			source,
			split_clip,
			playhead,
			split: None,
			did_split: false,
			targets: Vec::new(),
			noop: false,
		}
	}

	/// `redo`: split (if requested and the playhead is strictly inside),
	/// then write `current_in` on the affected multicam copies.
	pub fn redo(&mut self) {
		if self.split.is_none() && !self.noop {
			if clip_find_multicam(&self.clip).is_none() {
				// C++ `if (!node_) return;` — nothing to switch.
				self.noop = true;
				return;
			}
			if self.split_clip {
				let clip_in = block_in(&self.clip);
				let clip_out = block_out(&self.clip);
				if clip_in < self.playhead && self.playhead < clip_out {
					// Split the clip and every linked block, preserving
					// links. Each half keeps its own multicam copy.
					let mut blocks = vec![self.clip.clone()];
					blocks.extend(block_links(&self.clip));
					let times = vec![self.playhead];
					let mut split = BlockSplitPreservingLinksCommand::new(blocks, times);
					split.redo();
					let new_clip = split.get_split(&self.clip, 0);
					self.did_split = true;
					// Targets come from the new (post-playhead) halves.
					if let Some(new_clip) = new_clip {
						self.targets = collect_switch_targets(&new_clip);
					}
					self.split = Some(split);
				}
			}
			if !self.did_split {
				// No split (split disabled or playhead outside the clip):
				// the whole clip (and its links) switch source.
				self.targets = collect_switch_targets(&self.clip);
			}
		} else if self.did_split {
			// Redo after undo: re-run the split (the halves re-attach
			// identity-preserving) and re-apply the stored targets.
			if let Some(split) = self.split.as_mut() {
				split.redo();
			}
		}

		// Write the new source on every captured multicam.
		let project = self.clip.project.clone();
		let mut p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		for (_clip, mc, _old) in &self.targets {
			multicam_set_current(&mut p.graph, *mc, self.source);
		}
	}

	/// `undo`: restore the old sources and undo the split.
	pub fn undo(&mut self) {
		// Restore the previous sources first (the targets are still
		// attached); undoing the split afterwards discards the copies.
		let project = self.clip.project.clone();
		let mut p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		for (_clip, mc, old) in &self.targets {
			multicam_set_current(&mut p.graph, *mc, *old);
		}
		drop(p);
		if self.did_split {
			if let Some(split) = self.split.as_mut() {
				split.undo();
			}
		}
	}

	/// Wrap as an quercusundo command value.
	pub fn to_command(self) -> UndoCommand {
		undocommon::box_command(self)
	}
}

impl undocommon::Command for MultiCamSwitchCommand {
	fn redo(&mut self) {
		self.redo();
	}

	fn undo(&mut self) {
		self.undo();
	}
}

/// Builds the undo command that switches a multi-cam clip to another source
/// (C++ `MulticamWidget::Switch`).
///
/// With `split_clip` true and `playhead` strictly inside the clip
/// (`in < playhead < out`), the clip and its linked blocks are split first, so
/// only the post-playhead halves switch: each half owns its own copy of the
/// multicam node. Without a split the whole clip and its links switch.
/// `source` is the new `current_in` index into the sequence's track list; it is
/// written to every multicam found on the clip and its linked clips.
///
/// The command is a no-op when the clip has no multicam. Push it with
/// [`SWITCH_LABEL`]; `undo` restores the previous sources and reverts the split.
pub fn multicam_switch(
	clip: NodeRef,
	source: i32,
	split_clip: bool,
	playhead: Rational,
) -> UndoCommand {
	MultiCamSwitchCommand::new(clip, source, split_clip, playhead).to_command()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
mod branch_tests {
	use super::*;

	#[test]
	fn empty_enable_and_disable_commands_are_safe() {
		let project = Project::new();
		let (seq_id, _) = {
			let mut p = project.lock().unwrap();
			let (core, behavior) = quercus_node::sequence::SequenceBehavior::create();
			(core, behavior)
		};
		let _ = (seq_id, ());
		let fx = {
			// Build a minimal sequence handle without relying on the test
			// fixture module (kept private to the other test module).
			let mut p = project.lock().unwrap();
			let (core, behavior) = quercus_node::sequence::SequenceBehavior::create();
			let seq = p.graph.add_node(core, behavior);
			NodeRef::new(project.clone(), seq)
		};
		let mut enable = MultiCamEnableCommand::new(Vec::new(), fx.clone());
		enable.redo();
		enable.undo();
		let mut disable = MultiCamDisableCommand::new(Vec::new());
		disable.redo();
		disable.undo();
	}
}

#[cfg(test)]
mod guard_tests {
	use super::*;
	use crate::util::NodeRef;
	use quercus_node::node::NodeCore;
	use quercus_node::nodes::multicamnode::{MultiCamNode, SOURCES_INPUT};
	use quercus_node::project::Project;
	use quercus_node::track::{TrackBehavior, TrackType};

	#[test]
	fn sync_and_track_type_handle_missing_or_trackless_nodes() {
		let project = Project::new();
		let (core, behavior) = quercus_node::block::clip_create();
		let clip = project.lock().unwrap().graph.add_node(core, behavior);
		{
			let mut p = project.lock().unwrap();
			// Missing node: no panic.
			sync_multicam_sequence(&mut p.graph, NodeId::INVALID, None);
			// Non-multicam node: no panic.
			sync_multicam_sequence(&mut p.graph, clip, None);
		}
		let clip_ref = NodeRef::new(project.clone(), clip);
		assert_eq!(clip_track_type(&clip_ref), TrackType::Video);

		let (core, behavior) = quercus_node::nodes::multicamnode::create();
		let mc = project.lock().unwrap().graph.add_node(core, behavior);
		{
			let mut p = project.lock().unwrap();
			sync_multicam_sequence(&mut p.graph, mc, Some(clip));
		}
		let p = project.lock().unwrap();
		let entry = p.graph.get(mc).unwrap();
		let node = entry
			.behavior
			.as_any()
			.unwrap()
			.downcast_ref::<MultiCamNode>()
			.unwrap();
		assert_eq!(node.sequence, Some(clip));
	}

	#[test]
	fn find_input_node_walks_cycles_and_respects_maximum() {
		let project = Project::new();
		let (core, behavior) = quercus_node::nodes::multicamnode::create();
		let first = project.lock().unwrap().graph.add_node(core, behavior);
		let (core, behavior) = quercus_node::nodes::multicamnode::create();
		let second = project.lock().unwrap().graph.add_node(core, behavior);
		{
			let mut p = project.lock().unwrap();
			p.graph.connect(first, second, SOURCES_INPUT, -1).unwrap();
			let mut list = Vec::new();
			find_input_node_internal(&p.graph, second, 1, &mut list);
			assert_eq!(list, vec![first]);
		}
	}
}
