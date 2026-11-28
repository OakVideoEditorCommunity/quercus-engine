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

//! OpenFX plugin node (C++ `engine/node/plugins/plugin.{h,cpp}`,
//! `olive::plugin::PluginNode`).
//!
//! Thin wrapper over an OFX plugin instance that lives in the
//! `quercusplugin` crate; no OFX types (`OFX::Host::ImageEffect::Instance`,
//! `kOfxParam*`, ...) are declared here because quercusnode sits below
//! quercusplugin in the dependency graph. The instance is represented as
//! the opaque [`PluginInstanceHandle`] — an identity key into the
//! quercusplugin crate's instance registry.
//!
//! All per-plugin data (inputs, defaults, properties, labels) is
//! discovered at runtime from the plugin descriptor by the quercusplugin
//! discovery pass (`quercus_ofx_plugin::node_factory`), which builds the
//! [`NodeCore`] inputs, constructs [`PluginNode`]s and registers one
//! factory entry per discovered plugin (the C++
//! `factory.cpp::register_plugin_nodes` + `PluginNode::PluginNode`
//! split across the dependency seam).

use crate::handle;
use std::any;
use std::sync::{Arc, Mutex, OnceLock};

use crate::factory::NodeMeta;
use crate::jobs::Job;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use quercus_core::{Rational, TimeRange};

/// Texture input id (C++ `plugin::k_texture_input`). Type: texture;
/// no default. Only synthesized when the plugin declares clip inputs
/// but none of them is the simple-source clip (see the discovery
/// pass); then it becomes the node's effect input with display name
/// "Texture".
pub const TEXTURE_INPUT: &str = "tex_in";

/// OFX simple-source clip name (C++ `kOfxImageEffectSimpleSourceClipName`):
/// the canonical first texture clip of a filter plugin.
pub const SOURCE_CLIP: &str = "Source";

/// Opaque handle to an OFX plugin instance owned by the `quercusplugin`
/// crate (identity key into its instance registry; C++
/// `OFX::Host::ImageEffect::Instance *`, member `plugin_instance_`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PluginInstanceHandle(pub u64);

impl PluginInstanceHandle {
	/// The null/absent instance handle (no plugin instance behind the
	/// node).
	pub fn null() -> Self {
		PluginInstanceHandle(0)
	}

	/// True when no instance is behind this handle.
	pub fn is_null(&self) -> bool {
		self.0 == 0
	}
}

/// Plugin job payload (C++ `plugin::PluginJob`): everything the render
/// seam needs to run the instance against the input texture. Boxed
/// into a texture-typed [`NodeValue`] by [`PluginNode::value`] and
/// resolved by the quercusrender evaluation seam (C++
/// `RenderProcessor::process_plugin_job`), which executes it through
/// the quercusplugin render driver.
#[derive(Clone, Debug)]
pub struct PluginJobPayload {
	/// The instance identity (quercusplugin registry key).
	pub instance: PluginInstanceHandle,
	/// The OFX plugin identifier (cross-process stable; the single OFX
	/// host process resolves its own instance from it).
	pub type_id: String,
	/// The request time (C++ `globals.time().in()`).
	pub time: Rational,
	/// The effect input id the main source texture arrives on (C++
	/// `node->get_effect_input_id()`).
	pub effect_input_id: String,
	/// Snapshot of the full input row (C++ `PluginJob` holds the
	/// `NodeValueRow`: the tagged non-texture values become param
	/// overrides, the texture values feed the clip inputs).
	pub values: NodeValueRow,
	/// Non-empty keyframe tracks for animatable node inputs (`element = -1`).
	pub animations: Vec<(String, crate::keyframe::KeyframeTrack)>,
}

/// OFX plugin node. Wraps one plugin instance; inputs mirror the
/// plugin's declared params and clips.
///
/// The C++ class has a single own member besides the instance
/// pointer, `sub_category_`. Because `name()`/`id()`/`description()`
/// read the plugin descriptor at call time in C++ and the Rust trait
/// returns `&str`, this port caches those strings on the struct
/// (populated from the descriptor at construction).
pub struct PluginNode {
	/// The wrapped plugin instance (C++ `plugin_instance_`; an
	/// quercusplugin registry identity here).
	instance: PluginInstanceHandle,
	/// Sub-category derived from the OFX context (C++
	/// `sub_category_`): "Filter", "Generator", "Transition", or
	/// "General".
	sub_category: String,
	/// Cached display name (C++ `name()` reads the descriptor's
	/// `kOfxPropLabel` on every call).
	name: String,
	/// Cached type id (C++ `id()` = the plugin identifier).
	type_id: String,
	/// Cached description (C++ `description()` reads the
	/// descriptor's `kOfxPropPluginDescription`).
	description: String,
}

/// The plugin-node duplicator (installed by the quercusplugin crate): old
/// instance identity -> fresh instance identity (the C++
/// `PluginNode::copy()` creates a fresh instance — filter context
/// preferred, else the plugin's first declared context). `None` when
/// instance creation fails.
type PluginDuplicator = dyn Fn(PluginInstanceHandle) -> Option<PluginInstanceHandle> + Send + Sync;

static DUPLICATOR: OnceLock<Mutex<Option<Arc<PluginDuplicator>>>> = OnceLock::new();

fn duplicator_slot() -> &'static Mutex<Option<Arc<PluginDuplicator>>> {
	DUPLICATOR.get_or_init(|| Mutex::new(None))
}

/// Install the plugin-node duplicator (quercusplugin discovery path;
/// `None` clears it). Without it [`PluginNode::duplicate`] returns
/// `None`.
pub fn set_plugin_duplicator(dup: Option<Arc<PluginDuplicator>>) {
	*duplicator_slot().lock().unwrap_or_else(|e| e.into_inner()) = dup;
}

impl PluginNode {
	/// Constructor for the quercusplugin discovery path (the C++
	/// `PluginNode::PluginNode(instance)`; the input walk happens in
	/// the quercusplugin translation pass, which owns the OFX types).
	pub fn new(
		instance: PluginInstanceHandle,
		name: String,
		type_id: String,
		description: String,
		sub_category: String,
	) -> Self {
		PluginNode {
			instance,
			sub_category,
			name,
			type_id,
			description,
		}
	}

	/// The wrapped instance identity (quercusplugin registry key).
	pub fn instance_handle(&self) -> PluginInstanceHandle {
		self.instance
	}

	/// Forwards a push-button parameter activation to the wrapped plugin.
	///
	/// `core` is the node's data (a real implementation needs it to read the
	/// parameter's current value and trigger re-evaluation) and `name` is the input id
	/// of the push button that was pressed. The current implementation is a
	/// deliberate no-op — the upstream `push_button_clicked()` is also a no-op — kept
	/// so UI call sites can be wired now. It has no effect and does not invalidate the
	/// node.
	pub fn push_button_clicked(&mut self, core: &mut NodeCore, name: String) {
		let _ = (core, name);
	}
}

impl NodeBehavior for PluginNode {
	/// Human-readable name (C++ `name()` = the plugin descriptor's
	/// `kOfxPropLabel`; served from the cached copy).
	fn name(&self) -> &str {
		&self.name
	}

	/// Stable type id (C++ `id()` = the plugin identifier; served
	/// from the cached copy).
	fn type_id(&self) -> &str {
		&self.type_id
	}

	/// Categories (C++ `category()`; always OpenFx).
	fn categories(&self) -> &[Category] {
		&[Category::OpenFx]
	}

	/// Sub-category (C++ `sub_category()` = the context string
	/// captured at construction).
	fn sub_category(&self) -> &str {
		&self.sub_category
	}

	/// Description (C++ `description()` = the descriptor's
	/// `kOfxPropPluginDescription`; served from the cached copy).
	fn description(&self) -> &str {
		&self.description
	}

	/// Evaluate outputs (C++ `value()`): re-pushes every non-texture,
	/// non-none input value tagged with its input id (the tag routes
	/// the value to the matching OFX param); then resolves the input
	/// texture — simple-source clip first, then `tex_in`, then the
	/// first texture-typed input — and, when both texture and plugin
	/// instance exist, pushes a [`PluginJobPayload`] boxed into a
	/// texture-typed value (the C++ `table->push(k_texture,
	/// tex->to_job(job), this)`; the render seam executes the job).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		// Re-push every non-texture, non-none input value, tagged with
		// its input id.
		for (id, v) in inputs.iter() {
			if matches!(v, NodeValue::Texture(_) | NodeValue::None) {
				continue;
			}
			table.push(v.value_type(), v.clone(), Some(id.clone()));
		}

		// Resolve the input texture: simple-source clip, then tex_in,
		// then the first texture-typed input.
		let tex = inputs
			.get(SOURCE_CLIP)
			.filter(|v| matches!(v, NodeValue::Texture(_)))
			.or_else(|| {
				inputs
					.get(TEXTURE_INPUT)
					.filter(|v| matches!(v, NodeValue::Texture(_)))
			})
			.or_else(|| inputs.values().find(|v| matches!(v, NodeValue::Texture(_))));

		if tex.is_some() && !self.instance.is_null() {
			let animations = core
				.keyframes
				.iter()
				.filter(|(_, element, track)| *element == -1 && !track.keys().is_empty())
				.map(|(id, _, track)| (id.clone(), track.clone()))
				.collect();
			let payload = Job::PluginJob(PluginJobPayload {
				instance: self.instance,
				type_id: self.type_id.clone(),
				time,
				effect_input_id: core.effect_input.clone(),
				values: inputs.clone(),
				animations,
			});
			table.push(
				crate::value::ValueType::Texture,
				NodeValue::Texture(handle::make_owned(payload)),
				None,
			);
		}
	}

	/// Process audio samples (C++ `process_samples()`): passthrough —
	/// silences the output when the input is empty/unallocated,
	/// otherwise reallocates the output to the input's params when
	/// mismatched and copies every channel verbatim.
	///
	/// The C++ receives the input buffer from the audio traversal; the
	/// Rust trait hands only the value row, so the input buffer is
	/// located as the first samples value in the row.
	fn process_samples(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		range: TimeRange,
		output: &mut crate::value::SampleBuffer,
	) {
		let _ = (core, range);

		let input = match inputs.values().find(|v| matches!(v, NodeValue::Samples(_))) {
			Some(NodeValue::Samples(b)) => b,
			_ => {
				// Empty/unallocated input: silence the output (C++
				// `output.silence()`).
				if output.is_allocated() {
					output.data.fill(0);
				}
				return;
			}
		};

		if !input.is_allocated() || input.channels == 0 || input.sample_count == 0 {
			if output.is_allocated() {
				output.data.fill(0);
			}
			return;
		}

		// Reallocate the output to the input's params when mismatched
		// (C++ `set_audio_params` + `set_sample_count` + `allocate`).
		if !output.is_allocated()
			|| output.channels != input.channels
			|| output.sample_count != input.sample_count
		{
			output.format = input.format;
			output.channels = input.channels;
			output.sample_count = input.sample_count;
			let bps = input.format.bytes_per_sample();
			output.data = vec![0u8; input.channels * input.sample_count * bps];
		}

		// Copy every channel verbatim (C++ `output.fast_set(input, c)`).
		for c in 0..input.channels {
			for i in 0..input.sample_count {
				let v = input.sample_value(c, i);
				output.set_sample_value(c, i, v);
			}
		}
	}

	/// Direct frame generation (C++ `generate_frame()`): allocates
	/// the destination frame if needed and zero-fills it (plugins do
	/// their real image work in the plugin job, not here).
	///
	/// The Rust frame is an opaque [`crate::handle::CHandle`]
	/// whose pixels cannot be read or written from this crate, so the
	/// body is a documented no-op (`// CPP-PARITY: plugin.cpp`
	/// `generate_frame`).
	fn generate_frame(&self, core: &NodeCore, frame: &mut handle::CHandle, time: Rational) {
		let _ = (core, frame, time);
	}

	/// Deep copy (C++ `copy()`): asks the quercusplugin side (through the
	/// installed [`set_plugin_duplicator`] hook) for a fresh plugin
	/// instance — filter context when supported, else the plugin's
	/// first declared context — and wraps it in a new behavior; the
	/// caller clones the core (inputs included), matching the C++
	/// `Node::copy` split. `None` when there is no instance or no
	/// duplicator installed.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		if self.instance.is_null() {
			return None;
		}
		let dup = duplicator_slot()
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()?;
		let new_handle = dup(self.instance)?;
		Some(Box::new(PluginNode {
			instance: new_handle,
			sub_category: self.sub_category.clone(),
			name: self.name.clone(),
			type_id: self.type_id.clone(),
			description: self.description.clone(),
		}))
	}

	/// Downcast to [`Self`] (instance/metadata access).
	fn as_any(&self) -> Option<&dyn any::Any> {
		Some(self)
	}

	/// Mutable downcast (see [`NodeBehavior::as_any`]).
	fn as_any_mut(&mut self) -> Option<&mut dyn any::Any> {
		Some(self)
	}
}

/// Constructor (C++ `PluginNode::PluginNode(instance)`): the real
/// construction (instance creation + the OFX param/clip -> input
/// translation) belongs to the quercusplugin discovery pass
/// (`quercus_ofx_plugin::node_factory`), which registers one factory entry per
/// discovered plugin. This placeholder (null instance, empty cached
/// metadata) stays for the static-registration surface and tests.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let core = NodeCore::new();
	let node = PluginNode {
		instance: PluginInstanceHandle::null(),
		sub_category: "General".to_string(),
		name: String::new(),
		type_id: String::new(),
		description: String::new(),
	};
	(core, Box::new(node))
}

/// Register this node type. NOTE: unlike built-in nodes, plugin nodes
/// have no static type id — the quercusplugin discovery pass appends one
/// factory entry per discovered OFX plugin at runtime (C++
/// `factory.cpp::register_plugin_nodes`) through
/// [`crate::factory::Factory::register_dynamic`], keyed by plugin
/// identifier, so there is no fixed `NodeMeta` literal to push. This
/// function is a no-op placeholder kept for the static table.
pub fn register(meta: &mut Vec<NodeMeta>) {
	let _ = meta;
}

#[cfg(test)]
mod tests;
