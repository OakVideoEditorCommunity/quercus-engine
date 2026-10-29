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

//! quercusnode project/serializer helpers (direct Rust calls into the
//! quercusnode crate, single-lib unification; the former `bridge/node.rs`).
//!
//! The ove-xml/otio backends need quercusnode's project + serializer
//! families. Graph (de)serialization stays quercusnode's own serializer
//! (`load`/`save`); these helpers box the resulting
//! `Arc<Mutex<Project>>` into the canonical handle form only where a
//! project crosses the facade boundary (backends' `load` results and
//! `save` inputs) — inside the crate, projects travel as the plain
//! [`ProjectArc`] alias.

use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::handle::CHandle;

/// The project payload boxed by project handles.
pub type ProjectArc = Arc<Mutex<quercus_node::project::Project>>;

/// Box an existing project as an owned handle (refcount 1).
///
/// This is the facade-boundary conversion: backends hand loaded projects
/// to the caller (through [`crate::backend::LoadResult`] / the database
/// backend's `load_at`) as an quercusnode project handle; everything inside
/// the crate works with the boxed [`ProjectArc`] instead.
pub fn make_project_owned(project: ProjectArc) -> CHandle {
	quercus_node::handle::make_owned(project)
}

/// Read the boxed project of a project handle.
///
/// # Safety
/// The handle must box `Arc<Mutex<Project>>` (created by this module or
/// quercusnode's project helpers).
pub unsafe fn project_arc(h: &CHandle) -> Result<ProjectArc> {
	unsafe { quercus_node::handle::get::<ProjectArc>(h) }
		.cloned()
		.ok_or(crate::error::Error::Invalid)
}

/// Release a project handle created by [`make_project_owned`] (its box's
/// release callback drops one reference). NULL is a no-op.
pub fn release_project(h: CHandle) {
	if h.is_null() {
		return;
	}
	if let Some(release) = h.release {
		// SAFETY: `h` was produced by `make_project_owned`; the callback
		// owns the box and nulls nothing else.
		unsafe { release(h.ctx) };
	}
}

/// Load a project from XML text via the quercusnode serializer.
pub fn serializer_load(xml: &str) -> Result<ProjectArc> {
	quercus_node::serializer::load(xml).map_err(|e| crate::error::Error::Format(e.to_string()))
}

/// Serialize a project to XML text via the quercusnode serializer.
pub fn serializer_save(project: &quercus_node::project::Project) -> Result<String> {
	quercus_node::serializer::save(project).map_err(|e| crate::error::Error::Format(e.to_string()))
}

/// Create a sequence node with its default track lists (video, audio,
/// subtitle) in `graph`. Returns `(sequence_id, [video, audio,
/// subtitle] list ids)`.
///
/// Mirrors the private `append_default_track_lists` wiring of the former
/// `quercusnode_sequence_create` export so the otio backend can import
/// timelines into real projects.
pub fn create_sequence(
	graph: &mut quercus_node::graph::Graph,
) -> (quercus_node::id::NodeId, Vec<quercus_node::id::NodeId>) {
	use quercus_node::input::Input;
	use quercus_node::node::NodeCore;
	use quercus_node::sequence::SequenceBehavior;
	use quercus_node::track::{TrackListBehavior, TrackType};
	use quercus_node::value::{NodeValue, ValueType};

	let mut seq = SequenceBehavior::new();
	seq.set_default_parameters();
	let mut core = NodeCore::new();
	core.add_input(Input::new(
		quercus_node::sequence::TEXTURE_INPUT,
		ValueType::Texture,
		NodeValue::None,
	));
	core.add_input(Input::new(
		quercus_node::sequence::SAMPLES_INPUT,
		ValueType::Samples,
		NodeValue::None,
	));
	// One array input per track list (`track_in_%1`; the video list owns
	// track_in_0, audio track_in_1, subtitle track_in_2 — the C++
	// `Sequence::k_track_input_format` convention).
	for base in 0..3 {
		let mut track_input = Input::new(
			&quercus_node::sequence::TRACK_INPUT_FORMAT.replace("%1", &base.to_string()),
			ValueType::None,
			NodeValue::None,
		);
		track_input.flags |= quercus_node::input::flags::ARRAY;
		core.add_input(track_input);
	}
	let seq_id = graph.add_node(core, Box::new(seq));

	let mut lists = Vec::new();
	let mut base = 0i32;
	for kind in [TrackType::Video, TrackType::Audio, TrackType::Subtitle] {
		let mut behavior = TrackListBehavior::new(kind);
		behavior.sequence = Some(seq_id);
		behavior.array_base = base;
		let id = graph.add_node(NodeCore::new(), Box::new(behavior));
		lists.push(id);
		base += 1;
	}
	// Record the lists on the sequence behavior.
	if let Some(entry) = graph.get_mut(seq_id) {
		if let Some(s) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			s.track_lists = lists.clone();
		}
	}
	(seq_id, lists)
}
