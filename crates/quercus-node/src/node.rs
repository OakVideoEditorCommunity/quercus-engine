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

//! Node core data and the behavior trait — the C++ `Node` class,
//! restructured for Rust. COVERAGE.md maps every C++ method to its
//! Rust home; this file carries the virtual surface ([`NodeBehavior`])
//! and the shared data ([`NodeCore`]).

use quercus_core::{Rational, TimeRange};

use crate::id::NodeId;
use crate::input::{Input, ValueHint};
use crate::keyframe::KeyframeTrack;
use crate::value::{NodeValueRow, NodeValueTable};

/// Node category (mirrors C++ `Node::CategoryID` order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
	/// Output nodes.
	Output,
	/// Effects.
	Effect,
	/// Generators.
	Generator,
	/// Inputs (footage).
	Input,
	/// Math/combine.
	Math,
	/// Color.
	Color,
	/// Distort.
	Distort,
	/// Filter.
	Filter,
	/// Keying.
	Keying,
	/// OpenFX plugins.
	OpenFx,
	/// Timeline structural (tracks/blocks).
	Timeline,
	/// Groups.
	Group,
}

/// A gizmo: viewport-interaction data object (C++ `NodeGizmo`). Data
/// only — drawing and mouse handling live in facade/app.
#[derive(Clone)]
pub struct Gizmo {
	/// Keyframed position inputs (track references).
	pub position_inputs: Vec<(String, i32, i32)>,
	/// Current drag position.
	pub drag_point: (f64, f64),
}

/// Shared per-node data (the C++ `Node` member fields). Behavior lives
/// in [`NodeBehavior`].
#[derive(Clone)]
pub struct NodeCore {
	/// Inputs by id (array inputs hold multiple elements).
	pub inputs: Vec<Input>,
	/// Keyframe tracks per (input, element).
	pub keyframes: Vec<(String, i32, KeyframeTrack)>,
	/// Caches as opaque quercusrender handles.
	pub caches: NodeCaches,
	/// Node flags bitmask (hidden, dont-show-in-param-view, ...).
	pub flags: u64,
	/// Editor position (serialization only).
	pub position: (f64, f64),
	/// User label (C++ label_).
	pub label: String,
	/// Color override index (-1 = none).
	pub override_color: i32,
	/// Effect input id (C++ effect_input_).
	pub effect_input: String,
	/// Value hints per (input, element).
	pub hints: Vec<((String, i32), ValueHint)>,
	/// Group-context positions (C++ context_positions_).
	pub context_positions: Vec<(NodeId, (f64, f64), bool)>,
	/// Linked nodes (C++ links_).
	pub links: Vec<NodeId>,
	/// Bin folder membership (None = not in the bin).
	pub bin_folder: Option<NodeId>,
	/// Caches master toggle (C++ caches_enabled_).
	pub caches_enabled: bool,
	/// Gizmos owned by this node.
	pub gizmos: Vec<Gizmo>,
	/// Currently dragged gizmo index.
	pub current_gizmo: Option<usize>,
	/// Standard (non-keyframed) values keyed by (input id, element);
	/// falls back to [`Input::default`] (the C++ `NodeInputImmediate`
	/// `standard_value_` map — `// CPP-PARITY: inputimmediate.h`).
	pub standard_values: std::collections::HashMap<(String, i32), crate::value::NodeValue>,
}

/// The always-present "enabled" input id (C++
/// `Node::k_enabled_input`, `"enabled_in"`).
pub const ENABLED_INPUT: &str = "enabled_in";

/// Node flag bits (C++ `Node::Flag` enum; values cross the C ABI and
/// project XML as ints — `// CPP-PARITY: node.h:109`).
pub mod flags {
	/// `k_dont_show_in_param_view`.
	pub const DONT_SHOW_IN_PARAM_VIEW: u64 = 0x1;
	/// `k_video_effect`.
	pub const VIDEO_EFFECT: u64 = 0x2;
	/// `k_audio_effect`.
	pub const AUDIO_EFFECT: u64 = 0x4;
	/// `k_dont_show_in_create_menu`.
	pub const DONT_SHOW_IN_CREATE_MENU: u64 = 0x8;
	/// `k_is_item` (C++ `node.h:119`): bin items (folders, footage,
	/// sequences) — dependency-graph copies share these instead of
	/// cloning them (`// CPP-PARITY: node.cpp:1159,1199`).
	pub const IS_ITEM: u64 = 0x10;
}

impl NodeCore {
	/// Bare core with no inputs (vacant arena slots).
	pub fn empty() -> NodeCore {
		NodeCore {
			inputs: Vec::new(),
			keyframes: Vec::new(),
			caches: NodeCaches::default(),
			flags: 0,
			position: (0.0, 0.0),
			label: String::new(),
			override_color: -1,
			effect_input: String::new(),
			hints: Vec::new(),
			context_positions: Vec::new(),
			links: Vec::new(),
			bin_folder: None,
			caches_enabled: true,
			gizmos: Vec::new(),
			current_gizmo: None,
			standard_values: std::collections::HashMap::new(),
		}
	}

	/// Fresh core for a node constructor: adds the standard `enabled_in`
	/// boolean input (default true) first, exactly like the C++ `Node`
	/// constructor (`// CPP-PARITY: node.cpp:91`).
	pub fn new() -> NodeCore {
		let mut core = NodeCore::empty();
		core.add_input(Input::new(
			ENABLED_INPUT,
			crate::value::ValueType::Boolean,
			crate::value::NodeValue::Boolean(true),
		));
		core
	}

	/// Append an input descriptor to the node's input list.
	///
	/// `input` is moved into the core and becomes the last input in declaration
	/// order; its id is how values, keyframes and graph connections address it. The
	/// constructor's `enabled_in` input is already present, so new inputs come
	/// after it. Duplicate ids are not rejected; lookup returns the first match.
	pub fn add_input(&mut self, input: Input) {
		self.inputs.push(input);
	}

	/// Look up an input descriptor by its stable id.
	///
	/// Returns the first input whose `id` matches, in declaration order, or `None`
	/// when the node does not declare it. The reference borrows the node; use
	/// [`NodeCore::get_input_mut`] for mutation.
	pub fn get_input(&self, id: &str) -> Option<&Input> {
		self.inputs.iter().find(|i| i.id == id)
	}

	/// Mutable lookup of an input descriptor by id.
	///
	/// Returns the first matching input in declaration order, or `None`. Pairs with
	/// [`NodeCore::get_input`]; only the descriptor is exposed here, while values
	/// are stored separately through `standard_value`/`set_standard_value`.
	pub fn get_input_mut(&mut self, id: &str) -> Option<&mut Input> {
		self.inputs.iter_mut().find(|i| i.id == id)
	}

	/// Position of an input in declaration order.
	///
	/// Returns the index into [`NodeCore::inputs`] for `id`, or `None` when the
	/// node does not declare it. This is the order used by the parameter view and
	/// by array-element bookkeeping; it is not a value slot.
	pub fn input_index(&self, id: &str) -> Option<usize> {
		self.inputs.iter().position(|i| i.id == id)
	}

	/// Whether the node declares an input with id `id`.
	///
	/// Equivalent to `input_index(id).is_some()`; returns `false` for unknown or
	/// empty ids and never panics.
	pub fn has_input(&self, id: &str) -> bool {
		self.inputs.iter().any(|i| i.id == id)
	}

	/// Remove the input `id`, if present (C++ `Node::remove_input`).
	pub fn remove_input(&mut self, id: &str) -> bool {
		let before = self.inputs.len();
		self.inputs.retain(|i| i.id != id);
		self.inputs.len() != before
	}

	/// Declared value type of an input.
	///
	/// Returns the `ValueType` recorded on the descriptor, or `None` when `id` is
	/// unknown. This is the type the node expects at render time, not the type of
	/// the currently stored value (see [`NodeCore::standard_value`]).
	pub fn input_data_type(&self, id: &str) -> Option<crate::value::ValueType> {
		self.get_input(id).map(|i| i.value_type)
	}

	/// Flag bits of an input descriptor.
	///
	/// Returns the descriptor's `flags` field, or `0` when `id` is unknown, so an
	/// unknown input looks like a plain input instead of failing. Bit meanings are
	/// defined in [`crate::input::flags`].
	pub fn input_flags(&self, id: &str) -> u32 {
		self.get_input(id).map(|i| i.flags).unwrap_or(0)
	}

	/// Display name of an input.
	///
	/// Returns the name stored on the descriptor (the non-virtual part of the C++
	/// `get_input_name`), falling back to `id` itself when the input is unknown.
	/// Unlike [`NodeBehavior::input_name`], this does not apply behavior-level
	/// overrides such as the localized "Enabled" label.
	pub fn input_display_name(&self, id: &str) -> String {
		self.get_input(id)
			.map(|i| i.display_name.clone())
			.unwrap_or_else(|| id.to_string())
	}

	/// Array size of `id` (0 for non-array inputs; C++ `input_array_size`).
	pub fn input_array_size(&self, id: &str) -> usize {
		self.get_input(id).map(|i| i.array_size).unwrap_or(0)
	}

	/// Grow/shrink an array input's element count, inserting/removing the
	/// given element index. Per-element standard values and keyframe
	/// tracks shift to keep their element mapping (C++
	/// `Node::input_array_insert`/`input_array_remove`, values half).
	pub fn input_array_insert(&mut self, id: &str, index: usize) {
		if let Some(input) = self.get_input_mut(id) {
			input.array_size += 1;
		}
		let size = self.input_array_size(id);
		// Shift standard values and keyframe tracks up one element.
		for e in (index + 1..size).rev() {
			self.move_element_value(id, e - 1, e);
			self.move_element_keyframes(id, e - 1, e);
		}
		// The freshly inserted slot carries no value or track.
		self.standard_values.remove(&(id.to_string(), index as i32));
		self.remove_element_keyframes(id, index);
	}

	/// Shrink an array input by one element, removing element `index`.
	///
	/// Out-of-range indexes (including every index of a non-array input) are a
	/// no-op. The removed element's standard value and keyframe track are dropped,
	/// later elements shift down by one, and the tail slot is cleared. Counterpart
	/// of [`NodeCore::input_array_insert`]; per-element value hints are not shifted
	/// by either operation. `index` must be below `input_array_size(id)`.
	pub fn input_array_remove(&mut self, id: &str, index: usize) {
		let size = self.input_array_size(id);
		if index >= size {
			return;
		}
		// Drop the removed element's own value and track first (the
		// shift below only overwrites targets whose source has data).
		self.standard_values.remove(&(id.to_string(), index as i32));
		self.remove_element_keyframes(id, index);
		// Shift values/keyframes down one element, then drop the tail.
		for e in index..size.saturating_sub(1) {
			self.move_element_value(id, e + 1, e);
			self.move_element_keyframes(id, e + 1, e);
		}
		self.standard_values
			.remove(&(id.to_string(), (size - 1) as i32));
		self.remove_element_keyframes(id, size - 1);
		if let Some(input) = self.get_input_mut(id) {
			input.array_size = input.array_size.saturating_sub(1);
		}
	}

	/// Move one standard value from element `from` to element `to`.
	///
	/// Used by [`NodeCore::input_array_insert`] and
	/// [`NodeCore::input_array_remove`] when shifting array elements. Only an
	/// existing value is moved; when `from` has no override, `to` is left alone,
	/// and a value already stored at `to` is overwritten.
	fn move_element_value(&mut self, id: &str, from: usize, to: usize) {
		let key = |e: usize| (id.to_string(), e as i32);
		if let Some(v) = self.standard_values.remove(&key(from)) {
			self.standard_values.insert(key(to), v);
		}
	}

	/// Move the keyframe track of `(id, from)` to element `to`.
	///
	/// Removes the source track and installs it at the target element, replacing a
	/// track already stored there or appending a new entry. No-op when `from` has
	/// no track. Used for the element shifting in [`NodeCore::input_array_insert`]
	/// and [`NodeCore::input_array_remove`].
	fn move_element_keyframes(&mut self, id: &str, from: usize, to: usize) {
		let track = match self
			.keyframes
			.iter()
			.position(|(i, e, _)| i == id && *e == from as i32)
		{
			Some(i) => self.keyframes.remove(i).2,
			None => return,
		};
		// Replace or insert at the target element.
		if let Some(slot) = self
			.keyframes
			.iter_mut()
			.find(|(i, e, _)| i == id && *e == to as i32)
		{
			slot.2 = track;
		} else {
			self.keyframes.push((id.to_string(), to as i32, track));
		}
	}

	/// Drop the keyframe track of `(id, element)`, if present.
	///
	/// Used when an array element is deleted or a fresh slot is inserted by
	/// [`NodeCore::input_array_insert`]. Silently does nothing when the pair has no
	/// track.
	fn remove_element_keyframes(&mut self, id: &str, element: usize) {
		self.keyframes
			.retain(|(i, e, _)| !(i == id && *e == element as i32));
	}

	/// The keyframe track of `(id, element)`, if any.
	///
	/// Returns `None` when that pair has never been keyframed. An existing track
	/// may still be empty (all keys removed); [`NodeCore::is_input_keyframing`]
	/// treats an empty track as not keyframing.
	pub fn keyframe_track(&self, id: &str, element: i32) -> Option<&KeyframeTrack> {
		self.keyframes
			.iter()
			.find(|(i, e, _)| i == id && *e == element)
			.map(|(_, _, t)| t)
	}

	/// Mutable keyframe-track access, creating the track on first use.
	///
	/// Returns the track for `(id, element)`, pushing an empty default track when
	/// the pair has none, so callers can insert keys without checking existence
	/// first. Consecutive calls for the same pair return the same track; an empty
	/// track is not removed automatically. `element` is `-1` for the primary value.
	pub fn keyframe_track_mut(&mut self, id: &str, element: i32) -> &mut KeyframeTrack {
		if let Some(i) = self
			.keyframes
			.iter()
			.position(|(i, e, _)| i == id && *e == element)
		{
			return &mut self.keyframes[i].2;
		}
		self.keyframes
			.push((id.to_string(), element, KeyframeTrack::default()));
		let last = self.keyframes.len() - 1;
		&mut self.keyframes[last].2
	}

	/// Standard (non-keyframed) value of (input, element): the per-element
	/// override or the input's default (C++ `get_standard_value`).
	pub fn standard_value(&self, id: &str, element: i32) -> crate::value::NodeValue {
		self.standard_values
			.get(&(id.to_string(), element))
			.cloned()
			.or_else(|| self.get_input(id).map(|i| i.default.clone()))
			.unwrap_or(crate::value::NodeValue::None)
	}

	/// Set the standard value of (input, element) (C++ `set_standard_value`).
	pub fn set_standard_value(&mut self, id: &str, element: i32, value: crate::value::NodeValue) {
		self.standard_values
			.insert((id.to_string(), element), value);
	}

	/// Value of `input` at `time`: keyframes when the (input, element)
	/// track is non-empty, else the standard value (C++
	/// `get_value_at_time`; `// CPP-PARITY: node.cpp:465`).
	pub fn value_at_time(
		&self,
		id: &str,
		element: i32,
		time: quercus_core::Rational,
	) -> crate::value::NodeValue {
		match self.keyframe_track(id, element) {
			Some(track) if !track.keys().is_empty() => track
				.value_at(time)
				.unwrap_or_else(|| self.standard_value(id, element)),
			_ => self.standard_value(id, element),
		}
	}

	/// Whether (input, element) is being keyframed (C++
	/// `Node::is_input_keyframing`): the keyframe track exists and is
	/// non-empty.
	pub fn is_input_keyframing(&self, id: &str, element: i32) -> bool {
		self.keyframe_track(id, element)
			.map(|t| !t.keys().is_empty())
			.unwrap_or(false)
	}

	/// Whether (input, element) is static at evaluation time (C++
	/// `Node::is_input_static`): neither connected nor keyframed.
	/// `inputs` is the render-time input row — a connected input appears
	/// in the row under its id.
	pub fn is_input_static(
		&self,
		inputs: &crate::value::NodeValueRow,
		id: &str,
		element: i32,
	) -> bool {
		!inputs.contains_key(id) && !self.is_input_keyframing(id, element)
	}

	/// Set the value hint for (input, element) (C++ `set_value_hint_for_input`).
	pub fn set_value_hint(&mut self, id: &str, element: i32, hint: ValueHint) {
		if let Some(slot) = self
			.hints
			.iter_mut()
			.find(|((i, e), _)| i == id && *e == element)
		{
			slot.1 = hint;
		} else {
			self.hints.push(((id.to_string(), element), hint));
		}
	}

	/// The value hint registered for `(input, element)`, if any.
	///
	/// A hint lists the types a connected input accepts (see [`ValueHint`]) and is
	/// installed with [`NodeCore::set_value_hint`]. Returns `None` when the pair
	/// has no hint, in which case consumers fall back to the input's declared value
	/// type.
	pub fn value_hint(&self, id: &str, element: i32) -> Option<&ValueHint> {
		self.hints
			.iter()
			.find(|((i, e), _)| i == id && *e == element)
			.map(|(_, h)| h)
	}

	/// True when `context` appears in this node's context-position map
	/// (C++ `context_contains_node`).
	pub fn context_contains(&self, context: NodeId) -> bool {
		self.context_positions.iter().any(|(c, _, _)| *c == context)
	}

	/// Set this node's position in `context` (C++
	/// `set_node_position_in_context`). Returns true when newly added.
	pub fn set_context_position(
		&mut self,
		context: NodeId,
		x: f64,
		y: f64,
		expanded: bool,
	) -> bool {
		let added = !self.context_contains(context);
		if let Some(slot) = self
			.context_positions
			.iter_mut()
			.find(|(c, _, _)| *c == context)
		{
			slot.1 = (x, y);
			slot.2 = expanded;
		} else {
			self.context_positions.push((context, (x, y), expanded));
		}
		added
	}

	/// Remove this node from `context`; false when absent (C++
	/// `remove_node_from_context`).
	pub fn remove_from_context(&mut self, context: NodeId) -> bool {
		let before = self.context_positions.len();
		self.context_positions.retain(|(c, _, _)| *c != context);
		self.context_positions.len() != before
	}
}

/// The node's quercusrender caches (frame/thumbnail/audio/waveform),
/// owned handles released with the node.
///
/// Cross-module payloads: the cache objects live behind opaque
/// quercusrender handles created lazily by the facade (quercusengine reads
/// `caches.video` directly through the C ABI), and quercusrender depends on
/// quercusnode, so no Rust type is nameable here — the handle is the
/// boundary representation.
#[derive(Clone)]
pub struct NodeCaches {
	/// Video frame hash cache.
	pub video: crate::handle::CHandle,
	/// Thumbnail cache.
	pub thumbnail: crate::handle::CHandle,
	/// Audio playback cache.
	pub audio: crate::handle::CHandle,
	/// Waveform cache.
	pub waveform: crate::handle::CHandle,
}

impl Default for NodeCaches {
	/// All empty handles (caches are created lazily when a node
	/// enters a project; `// CPP-PARITY: node.cpp:102`).
	fn default() -> Self {
		NodeCaches {
			video: crate::handle::CHandle::null(),
			thumbnail: crate::handle::CHandle::null(),
			audio: crate::handle::CHandle::null(),
			waveform: crate::handle::CHandle::null(),
		}
	}
}

/// The polymorphic surface of a node — every C++ virtual on `Node`
/// becomes a method here (see COVERAGE.md §1/§7/§8/§10).
pub trait NodeBehavior: Send {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str;

	/// Short menu name (C++ `short_name()`; defaults to [`Self::name`]).
	fn short_name(&self) -> &str {
		self.name()
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str;

	/// The categories this node type belongs to.
	///
	/// Returns the [`Category`] entries used to place the node in the node library
	/// and create menu. The default is an empty slice, meaning the node opts out of
	/// categorized menus; concrete nodes override it (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[]
	}

	/// Optional sub-category label within the node's primary category.
	///
	/// Used by the create menu to group entries under a category. The default is
	/// `""`, meaning the node has no sub-category (C++ `sub_category()`).
	fn sub_category(&self) -> &str {
		""
	}

	/// User-facing description of what this node does.
	///
	/// Shown as help/tooltip text in the UI. The default is `""` (no description);
	/// concrete nodes override it with a concise summary (C++ `description()`).
	fn description(&self) -> &str {
		""
	}

	/// Display name of an input, for the UI and menus.
	///
	/// `id` is an input id and the returned name borrows from it, so it lives as
	/// long as `id`. The default returns `id` unchanged except for
	/// [`ENABLED_INPUT`], which always displays as "Enabled"; nodes override this
	/// to localize their own input names (C++ virtual `get_input_name()`).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		// The standard enabled input displays as "Enabled" on every node
		// (`// CPP-PARITY: node.cpp:155` retranslate).
		if id == crate::node::ENABLED_INPUT {
			"Enabled"
		} else {
			id
		}
	}

	/// The option labels of a combo input (C++ `set_combo_box_strings`,
	/// called from each node's `retranslate`). Empty for non-combo inputs;
	/// the default is no options.
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		let _ = id;
		Vec::new()
	}

	/// Input ids the renderer must not traverse when evaluating this node.
	///
	/// Nodes list inputs that they consume but that are not value dependencies of
	/// the render, so pulling them into the graph would be wrong or wasteful. The
	/// default returns an empty slice, so every input is traversed (C++
	/// `ignore_inputs_for_rendering()`).
	fn ignore_inputs_for_rendering(&self) -> &[String] {
		&[]
	}

	/// Array element indices that are active at `time`.
	///
	/// `input` is the array input id and `time` is in rational seconds. Returns the
	/// elements whose values are currently meaningful for array inputs whose
	/// activity varies with time; the default returns an empty vector, meaning no
	/// element is active (C++ `get_active_elements_at_time()`).
	fn active_elements_at_time(&self, input: &str, time: Rational) -> Vec<i32> {
		let _ = (input, time);
		Vec::new()
	}

	/// Time range of the video frames currently held in this node's cache.
	///
	/// `core` is the node's data. Returns the cached span, or an empty
	/// [`TimeRange`] (the default, `[0, 0]`) when nothing is cached; the render
	/// cache uses it to decide which frames can be reused. Paired with
	/// [`NodeBehavior::audio_cache_range`]; corresponds to C++
	/// `get_video_cache_range()`.
	fn video_cache_range(&self, core: &NodeCore) -> TimeRange {
		let _ = core;
		TimeRange::default()
	}

	/// Time range of the audio currently held in this node's cache.
	///
	/// `core` is the node's data. Returns the cached span, or an empty
	/// [`TimeRange`] (the default, `[0, 0]`) when nothing is cached; the cache
	/// layer uses it to serve already-processed samples. Audio producers override
	/// it (C++ `get_audio_cache_range()`).
	fn audio_cache_range(&self, core: &NodeCore) -> TimeRange {
		let _ = core;
		TimeRange::default()
	}

	/// Value hint for an input (C++ `get_value_hint_for_input()` virtual).
	fn value_hint_for_input(&self, input: &str) -> Option<ValueHint> {
		let _ = input;
		None
	}

	/// Resolve which upstream node supplies a render-time connection.
	///
	/// `core` is the node's data, `input` the input id and `element` its array
	/// element. Returns the node that actually renders the value, or `None` (the
	/// default) to resolve the connection directly through the graph. Groups
	/// override this to redirect to the node inside the group (C++
	/// `get_connected_render_output()`).
	fn connected_render_output(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
	) -> Option<NodeId> {
		let _ = (core, input, element);
		None
	}

	/// Time adjustment through this node (C++
	/// `input_time_adjustment()`/`output_time_adjustment()`; clips
	/// override for speed/reverse, the time nodes for offset/remap).
	/// `core` is the node's own data — the C++ implementations read
	/// member inputs (e.g. the offset/remap value) at call time.
	fn input_time_adjustment(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		let _ = (core, input, element, traverse);
		time
	}

	/// Apply this node's output-side time mapping to a range.
	///
	/// `time` is the range to map, `input`/`element` select the input whose mapping
	/// applies, and `traverse` asks the implementation to follow time-adjusting
	/// nodes upstream. Returns the mapped range; the default is identity.
	/// Counterpart of [`NodeBehavior::input_time_adjustment`] (C++
	/// `output_time_adjustment()`).
	fn output_time_adjustment(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		let _ = (core, input, element, traverse);
		time
	}

	/// Evaluate the node for one frame and append its outputs to `table`.
	///
	/// `core` is the node's data, `inputs` the row of already-evaluated input
	/// values for the current frame, `time` the frame time in rational seconds, and
	/// `table` the value table that receives this node's outputs. The default does
	/// nothing (no outputs); concrete nodes override it (C++ `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		let _ = (core, inputs, time, table);
	}

	/// Process a span of audio samples and append the result to `output`.
	///
	/// `core` is the node's data, `inputs` the row of already-evaluated input
	/// values and `range` the sample span to process (rational seconds). `output`
	/// is extended in place with this node's samples, which must stay aligned with
	/// `range`. Default does nothing (C++ `process_samples()`).
	fn process_samples(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		range: TimeRange,
		output: &mut crate::value::SampleBuffer,
	) {
		let _ = (core, inputs, range, output);
	}

	/// Direct frame generation (C++ `generate_frame()`; CPU-render
	/// nodes).
	fn generate_frame(
		&self,
		core: &NodeCore,
		frame: &mut crate::handle::CHandle,
		time: Rational,
	) {
		let _ = (core, frame, time);
	}

	/// Return GLSL source for a shader this node needs, keyed by `request`.
	///
	/// `request` is the shader identifier/type string chosen by the renderer; the
	/// returned source is compiled once and cached by the caller. Returns `None`
	/// when the node provides no shader for that request, which is also the
	/// default. GPU-rendered nodes override it (C++ `get_shader_code()`).
	fn shader_code(&self, request: &str) -> Option<String> {
		let _ = request;
		None
	}

	/// Refresh this node's gizmo transforms and positions.
	///
	/// `row` holds the evaluated input values for the current frame and `core` the
	/// node data (read-only here). Gizmo-owning nodes override this to pull the
	/// gizmo's position values out of the row before the viewport draws it; the
	/// default does nothing (C++
	/// `gizmo_transformation()`/`update_gizmo_positions()`).
	fn gizmo_update(&self, core: &NodeCore, row: &NodeValueRow) {
		let _ = (core, row);
	}

	/// Handle a gizmo drag event coming from the viewport.
	///
	/// `start` is `true` for the drag-start event and `false` for subsequent moves;
	/// `x`/`y` are the pointer position supplied by the caller (in its viewport
	/// coordinate space) and `modifiers` is the keyboard-modifier bitmask. `core`
	/// is mutable so the handler can write the dragged input values back. Default
	/// does nothing (C++ `gizmo_drag_start`/`gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Notify the node that one of its input values changed.
	///
	/// Called after `input[element]` is set (including while a fresh node is being
	/// configured); `core` is mutable so the node can recompute dependent state,
	/// such as a type input switching the output type or toggling other parameters.
	/// `element` is `-1` for the primary value. Default does nothing (C++
	/// `InputValueChangedEvent`).
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		let _ = (core, input, element);
	}

	/// React to a new edge arriving at one of this node's inputs.
	///
	/// `core` is the node's mutable data. `input` is the id of the input that
	/// received the edge, `element` its array element (`-1` for the primary value),
	/// and `source` the upstream node now supplying the value. Counterpart of
	/// [`NodeBehavior::input_disconnected`]; the default does nothing (C++
	/// `InputConnectedEvent`).
	fn input_connected(&mut self, core: &mut NodeCore, input: &str, element: i32, source: NodeId) {
		let _ = (core, input, element, source);
	}

	/// Edge disconnected from an input (C++ `InputDisconnectedEvent`).
	fn input_disconnected(
		&mut self,
		core: &mut NodeCore,
		input: &str,
		element: i32,
		source: NodeId,
	) {
		let _ = (core, input, element, source);
	}

	/// React to a new edge from this node's output to a downstream input.
	///
	/// `core` is the node's mutable data. `target` is the downstream node, `input`
	/// the id of the input on `target` that received the edge, and `element` its
	/// array element (`-1` for the primary connection). The input-side counterpart
	/// is [`NodeBehavior::input_connected`]; the default does nothing (C++
	/// `OutputConnectedEvent`).
	fn output_connected(&mut self, core: &mut NodeCore, target: NodeId, input: &str, element: i32) {
		let _ = (core, target, input, element);
	}

	/// React to an edge being removed from this node's output.
	///
	/// `core` is the node's mutable data. `target` is the downstream node, `input`
	/// the id of the input on `target` that was wired, and `element` its array
	/// element (`-1` for the primary connection). Counterpart of
	/// [`NodeBehavior::output_connected`]; the default does nothing (C++
	/// `OutputDisconnectedEvent`).
	fn output_disconnected(
		&mut self,
		core: &mut NodeCore,
		target: NodeId,
		input: &str,
		element: i32,
	) {
		let _ = (core, target, input, element);
	}

	/// Attached to a preview/viewer (C++ `ConnectedToPreviewEvent`).
	fn connected_to_preview(&mut self, core: &mut NodeCore) {
		let _ = core;
	}

	/// Inserted into / removed from a project graph (C++
	/// `AddedToGraphEvent` / `RemovedFromGraphEvent`).
	fn added_to_graph(&mut self, core: &mut NodeCore) {
		let _ = core;
	}

	/// React to the node being removed from a project graph.
	///
	/// The counterpart of [`NodeBehavior::added_to_graph`]; `core` is the node's
	/// mutable data. The default does nothing; nodes override it to release
	/// graph-dependent state such as caches or node references (C++
	/// `RemovedFromGraphEvent`).
	fn removed_from_graph(&mut self, core: &mut NodeCore) {
		let _ = core;
	}

	/// React to a change in the node's internal link list.
	///
	/// Called after links are added to or removed from `core.links` (node-to-node
	/// link references, not graph connections); `core` is the node's mutable data.
	/// The default does nothing. Corresponds to the C++ `LinkChangeEvent`.
	fn link_changed(&mut self, core: &mut NodeCore) {
		let _ = core;
	}

	/// Deep copy (C++ `copy()`); None = not copiable.
	fn duplicate(&self, core: &NodeCore) -> Option<Box<dyn NodeBehavior>>;

	/// Load the node's custom state from the `<custom>` segment.
	///
	/// `core` is the node being deserialized and `reader` is positioned on the
	/// `<custom>` start element. The default consumes that element with
	/// `skip_current_element` and returns `true`, keeping the node-body parser at
	/// the correct depth; nodes with extra state override it to parse their own
	/// children.
	///
	/// # Returns
	///
	/// `true` when the segment was consumed; `false` when an implementor could not
	/// parse it (the default always returns `true`).
	fn load_custom(
		&mut self,
		core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		let _ = core;
		// The default has no custom state; consume the segment so the
		// node-body parser continues at the correct depth (the reader is
		// positioned on the `<custom>` start element).
		reader.skip_current_element();
		true
	}

	/// Write the node's custom state into the `<custom>` segment.
	///
	/// Counterpart of [`NodeBehavior::load_custom`]. `writer` is positioned inside
	/// the `<custom>` element that the node serializer has already opened, and
	/// `core` is the node's data. The default writes nothing; nodes with extra
	/// serializable state emit child elements here and parse them back in
	/// `load_custom`.
	fn save_custom(&self, core: &NodeCore, writer: &mut dyn crate::serializer::XmlWrite) {
		let _ = (core, writer);
	}

	/// Finish loading: run the fixups that need the fully parsed project.
	///
	/// Called once after the node's inputs, links and custom state have been read;
	/// `core` is the node's own data and may be mutated. The default is a no-op, so
	/// nodes override it only when they must recompute derived state or remap
	/// legacy serialized values. Corresponds to the C++
	/// `PostLoadEvent`/`LoadFinishedEvent`.
	fn post_load(&mut self, core: &mut NodeCore) {
		let _ = core;
	}

	/// Legacy input id mapping for old project versions (C++
	/// `get_input_id_for_legacy_id()`; default identity).
	fn map_legacy_input_id<'a>(&self, id: &'a str) -> &'a str {
		id
	}

	/// Downcast to the concrete behavior (used by the timeline families
	/// to reach `FolderBehavior`/`TrackBehavior`/`SequenceBehavior`
	/// state). Default `None`; concrete behaviors override.
	fn as_any(&self) -> Option<&dyn std::any::Any> {
		None
	}

	/// Mutable downcast (see [`NodeBehavior::as_any`]).
	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		None
	}
}
