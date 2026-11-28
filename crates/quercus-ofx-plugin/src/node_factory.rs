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

//! OFX plugin → node factory wiring (stage 6a).
//!
//! Corresponds to the C++ `NodeFactory::register_plugin_nodes`
//! (factory.cpp:148-186) plus the parameter-translation constructor of
//! `PluginNode::PluginNode(Instance*)` (engine/node/plugins/plugin.cpp:274-514).
//! OFX types exist only in this crate, so the translation lives here; the
//! quercusnode side carries only the behavior
//! ([`quercus_node::nodes::plugin::PluginNode`]) — this split is forced by
//! the dependency direction (quercusplugin → quercusrender → quercusnode).
//!
//! Responsibilities:
//! - **Instance registry**: the node holds a
//!   [`quercus_node::nodes::plugin::PluginInstanceHandle`] (u64 key), while
//!   this crate holds an `Arc<RefBox<Instance>>` (C++ factory instances live
//!   for the lifetime of the library entry; the Rust equivalent keeps them
//!   alive for the process lifetime via the registry).
//! - **Parameter translation**: 15 classes of OFX parameters → quercusnode
//!   inputs (the type table mirrors plugin.cpp:340-378 verbatim), default
//!   value caching, color-semantics heuristics, combo ordering,
//!   secret→hidden, ui_group/ui_page, clip→texture input, effect_input
//!   selection (plugin.cpp:494-514).
//! - **Render executor**: [`install_render_executor`] installs the
//!   render_driver into quercusrender's executor slot (dependency inversion;
//!   quercusrender cannot see this crate) and into quercusnode's duplicator
//!   slot.
//!
//! For other app injection points see [`crate::progress::set_reporter_factory`]
//! (progress UI) and [`crate::suites::timeline::set_active_viewer_provider`]
//! (the timeline suite's fallback time source).

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use quercus_node::factory::{DynNodeConstructor, DynamicNodeMeta};
use quercus_node::input::{flags as input_flags, Input};
use quercus_node::node::{Category, NodeBehavior, NodeCore};
use quercus_node::nodes::plugin::{PluginInstanceHandle, PluginNode, SOURCE_CLIP, TEXTURE_INPUT};
use quercus_node::value::{NodeValue, ValueType};

use crate::handle::RefBox;
use crate::host::Host;
use crate::instance::Instance;
use crate::param::{self as ofx, ParamDef, ParamValue};
use crate::property::{PropertySet, Value as PropValue};

/// kOfxPropPluginDescription (descriptor root property).
const PROP_PLUGIN_DESCRIPTION: &str = "OfxPropPluginDescription";

// ---------------------------------------------------------------------------
// Instance registry
// ---------------------------------------------------------------------------

static INSTANCES: OnceLock<Mutex<HashMap<u64, Arc<RefBox<Instance>>>>> = OnceLock::new();
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn instances() -> &'static Mutex<HashMap<u64, Arc<RefBox<Instance>>>> {
	INSTANCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers an instance and returns a non-zero handle key (the identity of
/// the C++ `Instance*` pointer).
/// The instance lives for the process lifetime (matching the C++ factory's
/// ownership; `Drop` sends destroyInstance).
pub fn register_instance(inst: Arc<RefBox<Instance>>) -> u64 {
	let id = NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(id, inst);
	id
}

/// Looks up the instance registry by handle key and returns the shared
/// instance reference.
///
/// Returns `None` when the lookup fails (not registered, already
/// [`unregister_instance`]d, or key 0). The returned `Arc` keeps the instance
/// alive for as long as the caller holds it and can be used across `Host`
/// calls. Corresponds to the C++ factory's `Instance*` pointer identity.
///
/// # Arguments
///
/// * `id` - Non-zero handle key allocated by [`register_instance`].
pub fn instance_from_id(id: u64) -> Option<Arc<RefBox<Instance>>> {
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&id)
		.cloned()
}

/// Removes `id` from the instance registry (node-destruction path).
///
/// Afterwards [`instance_from_id`] can no longer find the key; existing `Arc`
/// references stay valid and the `Instance`'s `Drop` (which sends
/// destroyInstance) runs only when the last reference is released. An
/// unregistered key is a no-op and does not report an error.
///
/// # Arguments
///
/// * `id` - Handle key to remove.
pub fn unregister_instance(id: u64) {
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&id);
}

/// Current registration count (tests/diagnostics).
pub fn registered_instance_count() -> usize {
	instances().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Triggers a push-button parameter (the Rust counterpart of the C++
/// `quercusengine_plugin_node_push_button_clicked`; called by the
/// inspector's button widget).
///
/// OFX push buttons carry no value: the host's press signal is a single
/// set on the parameter, then the press is routed to the plugin as
/// `kOfxActionInstanceChanged` (UserEdited) so the plugin reacts in its
/// own instanceChanged action (the OFX contract, ofxCore.h:405-435;
/// real plugins such as CImg depend on that action). Locates the
/// instance and parameter, type-checks, marks the value, then dispatches
/// the action. Returns false when the instance/parameter is unknown or
/// the parameter is not a push button; an instanceChanged failure is
/// logged and still reported as a successful press (the value was set).
pub fn push_button_clicked(instance: u64, param_name: &str) -> bool {
	let Some(inst) = instance_from_id(instance) else {
		return false;
	};
	let Some(p) = inst.value.params.find(param_name) else {
		return false;
	};
	if p.def.ofx_type != ofx::TYPE_PUSHBUTTON {
		return false;
	}
	p.set_ofx(ParamValue::PushButton);
	// The current render context supplies the time / render scale when the
	// press happens during a render; outside one, time 0 and scale 1:1.
	let (time, scale) = crate::suites::render_ctx()
		.map(|ctx| (ctx.time, ctx.scale))
		.unwrap_or((0.0, crate::instance::RenderScale { x: 1.0, y: 1.0 }));
	if let Err(e) = inst.value.instance_changed(
		param_name,
		crate::param::ChangeReason::UserEdited,
		time,
		scale,
	) {
		eprintln!("push_button_clicked: instanceChanged failed for \"{param_name}\": {e}");
	}
	true
}

// ---------------------------------------------------------------------------
// Project extent (conversion base for normalised-coordinate defaults → canonical)
// ---------------------------------------------------------------------------
//
// C++ get_project_extent reads Current::current_video_params
// (plugin.cpp:45-50); this crate has no project context, so the app injects it
// via set_project_extent; if not injected, the HD default is used.

static PROJECT_EXTENT: OnceLock<Mutex<(f64, f64)>> = OnceLock::new();

/// Injects the project extent, used to convert default values of normalised
/// coordinates into canonical values.
///
/// When an OFX parameter declares
/// `kOfxParamPropDefaultCoordinateSystem = normalised`, its default value is
/// converted using the extent (see [`default_value_for_param`] and
/// [`to_canonical`]). When no extent is injected the HD default 1920×1080 is
/// used; this crate does not hold project context, so the app calls this when
/// opening a project or switching sequences.
///
/// # Arguments
///
/// * `width` / `height` - Project canvas size (pixels, i.e. canonical units).
pub fn set_project_extent(width: f64, height: f64) {
	*project_extent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner()) = (width, height);
}

fn project_extent_slot() -> &'static Mutex<(f64, f64)> {
	PROJECT_EXTENT.get_or_init(|| Mutex::new((1920.0, 1080.0)))
}

fn project_extent() -> (f64, f64) {
	*project_extent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// Converts normalised coordinates (0..1 values relative to the extent) into
/// canonical pixel values.
///
/// When `extent > 0` returns `normalised * extent`; otherwise (extent not
/// injected or 0) returns the input unchanged, avoiding division by zero and
/// NaN. Corresponds to the C++ function of the same name (plugin.cpp:52-55).
///
/// # Arguments
///
/// * `normalised` - Normalised coordinate, usually 0..1 but out-of-range
///   values are allowed.
/// * `extent` - Extent size of that axis (pixels).
fn to_canonical(normalised: f64, extent: f64) -> f64 {
	if extent > 0.0 {
		normalised * extent
	} else {
		normalised
	}
}

// ---------------------------------------------------------------------------
// Property read helpers
// ---------------------------------------------------------------------------

/// Reads the string value of the `index`-th same-named property in a property
/// set.
///
/// Returns an empty `String` when the value type is not String or the
/// property is missing (the common fallback for absent OFX properties); uses
/// a lossy conversion, so non-UTF-8 bytes are replaced.
///
/// # Arguments
///
/// * `props` - Property set of the parameter/descriptor.
/// * `name` - OFX property name (e.g. `OfxPropLabel`).
/// * `index` - Index of the same-named multi-value property, starting at 0.
fn prop_str(props: &PropertySet, name: &str, index: usize) -> String {
	match props.get(name, index) {
		Some(PropValue::String(s)) => s.to_string_lossy().into_owned(),
		_ => String::new(),
	}
}

/// Reads the floating-point value of the `index`-th same-named property in a
/// property set.
///
/// Returns a Double directly and widens an Int to f64; returns 0.0 when the
/// property is missing or has another type. Used for numeric properties such
/// as default/display-min/display-max.
///
/// # Arguments
///
/// * `props` - Property set.
/// * `name` - OFX property name.
/// * `index` - Index of the same-named multi-value property, starting at 0.
fn prop_double(props: &PropertySet, name: &str, index: usize) -> f64 {
	match props.get(name, index) {
		Some(PropValue::Double(v)) => v,
		Some(PropValue::Int(v)) => v as f64,
		_ => 0.0,
	}
}

/// Reads the integer value of the `index`-th same-named property in a property
/// set.
///
/// Returns an Int directly and truncates a Double to i32; returns 0 when the
/// property is missing or has another type.
///
/// # Arguments
///
/// * `props` - Property set of the parameter/descriptor.
/// * `name` - OFX property name.
/// * `index` - Index of the same-named multi-value property, starting at 0.
fn prop_int(props: &PropertySet, name: &str, index: usize) -> i32 {
	match props.get(name, index) {
		Some(PropValue::Int(v)) => v,
		Some(PropValue::Double(v)) => v as i32,
		_ => 0,
	}
}

fn is_normalised_coord_system(def: &ParamDef) -> bool {
	prop_str(&def.props, ofx::P_DEFAULT_COORD_SYS, 0) == ofx::V_COORD_NORMALISED
}

// ---------------------------------------------------------------------------
// Default values (plugin.cpp default_value_for_param, :57-131)
// ---------------------------------------------------------------------------

/// Builds the node default value for a parameter according to its OFX type.
///
/// The numeric family reads `kOfxParamPropDefault`: Integer→Int,
/// Choice→Combo, Boolean→`!=0`, Double→Float (normalised coordinates are
/// converted to canonical using the project extent); String/StrChoice→Text,
/// Custom→Binary (raw bytes preserved); RGB/RGBA read 3/4 components (alpha
/// defaults to 1.0); Double2D/3D and Integer2D/3D become Vec2/Vec3;
/// Bytes→empty Binary; Parametric→deterministic JSON text of the
/// `def.default` curve (node inputs have no curve type). PushButton/Group/Page
/// and unknown types have no default value and return `None`.
///
/// # Arguments
///
/// * `def` - Parameter description (type, property set, describe-time default
///   value).
fn default_value_for_param(def: &ParamDef) -> Option<NodeValue> {
	let props = &def.props;
	match def.ofx_type.as_str() {
		ofx::TYPE_INTEGER => Some(NodeValue::Int(prop_int(props, ofx::P_DEFAULT, 0) as i64)),
		ofx::TYPE_CHOICE => Some(NodeValue::Combo(prop_int(props, ofx::P_DEFAULT, 0) as i64)),
		ofx::TYPE_BOOLEAN => Some(NodeValue::Boolean(prop_int(props, ofx::P_DEFAULT, 0) != 0)),
		ofx::TYPE_DOUBLE => {
			let mut val = prop_double(props, ofx::P_DEFAULT, 0);
			if is_normalised_coord_system(def) {
				let (x_size, _) = project_extent();
				val = to_canonical(val, x_size);
			}
			Some(NodeValue::Float(val))
		}
		ofx::TYPE_STRING | ofx::TYPE_STRCHOICE => {
			Some(NodeValue::Text(prop_str(props, ofx::P_DEFAULT, 0)))
		}
		ofx::TYPE_CUSTOM => {
			// C++ also reads the default as a string (plugin.cpp:83-87); node
			// inputs are binary, preserved byte-for-byte.
			Some(NodeValue::Binary(
				prop_str(props, ofx::P_DEFAULT, 0).into_bytes(),
			))
		}
		ofx::TYPE_RGB | ofx::TYPE_RGBA => {
			let count = if def.ofx_type == ofx::TYPE_RGBA { 4 } else { 3 };
			let mut values = [0.0, 0.0, 0.0, 1.0];
			for i in 0..count {
				values[i] = prop_double(props, ofx::P_DEFAULT, i);
			}
			let alpha = if count == 4 { values[3] } else { 1.0 };
			Some(NodeValue::Color([values[0], values[1], values[2], alpha]))
		}
		ofx::TYPE_DOUBLE2D | ofx::TYPE_DOUBLE3D | ofx::TYPE_INTEGER2D | ofx::TYPE_INTEGER3D => {
			let is_double = matches!(
				def.ofx_type.as_str(),
				ofx::TYPE_DOUBLE2D | ofx::TYPE_DOUBLE3D
			);
			let count = if matches!(
				def.ofx_type.as_str(),
				ofx::TYPE_DOUBLE2D | ofx::TYPE_INTEGER2D
			) {
				2
			} else {
				3
			};
			let mut values = [0.0f64; 3];
			if is_double {
				for i in 0..count {
					values[i] = prop_double(props, ofx::P_DEFAULT, i);
				}
				if is_normalised_coord_system(def) {
					let (x_size, y_size) = project_extent();
					values[0] = to_canonical(values[0], x_size);
					values[1] = to_canonical(values[1], y_size);
					if count == 3 {
						values[2] = to_canonical(values[2], x_size);
					}
				}
			} else {
				for i in 0..count {
					values[i] = prop_int(props, ofx::P_DEFAULT, i) as f64;
				}
			}
			if count == 2 {
				Some(NodeValue::Vec2([values[0], values[1]]))
			} else {
				Some(NodeValue::Vec3([values[0], values[1], values[2]]))
			}
		}
		ofx::TYPE_BYTES => Some(NodeValue::Binary(Vec::new())),
		// parametric: value = deterministic JSON of the default curve (node
		// inputs have no curve model, carried as Text; undo/project
		// serialization comes for free with standard values). The default curve
		// comes from def.default (the default the plugin modified via the
		// parametric suite at describe time), not the P_DEFAULT property
		// (parametric has no scalar default).
		ofx::TYPE_PARAMETRIC => match &def.default {
			ParamValue::Parametric(curves) => {
				Some(NodeValue::Text(crate::param_curve::curves_to_json(curves)))
			}
			// Defensive: type_default always yields Parametric; this is only a fallback.
			_ => None,
		},
		// PushButton/Group/Page/unknown → invalid (C++ ends with
		// return QVariant()).
		_ => None,
	}
}

/// Per-plugin default-value cache (C++ g_plugin_param_defaults, plugin.cpp:37).
static DEFAULTS: OnceLock<Mutex<HashMap<String, HashMap<String, NodeValue>>>> = OnceLock::new();

fn defaults_slot() -> &'static Mutex<HashMap<String, HashMap<String, NodeValue>>> {
	DEFAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Returns the parameter default-value table for a plugin (process-level
/// cache), built on first access.
///
/// The cache key is the plugin identifier; when building, Group/Page/PushButton
/// and empty-name parameters are skipped and all others are written through
/// [`default_value_for_param`] as `parameter name → NodeValue`. Returns a clone,
/// which the caller may modify freely. Concurrent first access is serialised by
/// a mutex (`OnceLock<Mutex<…>>`).
///
/// # Arguments
///
/// * `plugin_id` - Plugin identifier (cache key).
/// * `params` - That instance's parameter set; used only when building on first
///   access.
fn cached_defaults(
	plugin_id: &str,
	params: &crate::param::ParamSetInstance,
) -> HashMap<String, NodeValue> {
	{
		let cache = defaults_slot().lock().unwrap_or_else(|e| e.into_inner());
		if let Some(hit) = cache.get(plugin_id) {
			return hit.clone();
		}
	}
	let mut defaults = HashMap::new();
	for p in &params.params {
		let ofx_type = p.def.ofx_type.as_str();
		if ofx_type == ofx::TYPE_GROUP
			|| ofx_type == ofx::TYPE_PAGE
			|| ofx_type == ofx::TYPE_PUSHBUTTON
		{
			continue;
		}
		if p.def.name.is_empty() {
			continue;
		}
		if let Some(v) = default_value_for_param(&p.def) {
			defaults.insert(p.def.name.clone(), v);
		}
	}
	defaults_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(plugin_id.to_string(), defaults.clone());
	defaults
}

// ---------------------------------------------------------------------------
// Color-semantics heuristics (plugin.cpp deduce_color_semantic, :133-222)
// ---------------------------------------------------------------------------

/// Decides whether an RGB/RGBA parameter is a color picker (`"color"`) or a
/// per-channel scalar group (`"scalar"`).
///
/// Applies the heuristics in order: label/hint/name containing color keywords
/// (color/colour/fill/tint/key) → color; containing scalar keywords
/// (gamma/contrast/gain/offset/…) → scalar; display min/max clearly outside
/// [0,1] → scalar; all channel defaults equal → scalar; parent group name or
/// group label containing scalar keywords → scalar; otherwise color. For
/// non-RGB/RGBA types returns "color" directly (callers only use this on Color
/// inputs).
///
/// # Arguments
///
/// * `def` - Parameter description (type + property set).
/// * `group_labels` - Group parameter name → display label (collected in the
///   first pass), used for the parent-group check in rule 5.
fn deduce_color_semantic(def: &ParamDef, group_labels: &HashMap<String, String>) -> &'static str {
	const COLOR_KEYWORDS: &[&str] = &["color", "colour", "fill", "tint", "key"];
	const SCALAR_KEYWORDS: &[&str] = &[
		"gamma",
		"contrast",
		"gain",
		"offset",
		"saturation",
		"exposure",
		"brightness",
		"lift",
		"multiply",
		"scale",
		"pivot",
	];

	if def.ofx_type != ofx::TYPE_RGB && def.ofx_type != ofx::TYPE_RGBA {
		return "color";
	}

	let label = prop_str(&def.props, ofx::PROP_LABEL, 0).to_lowercase();
	let hint = prop_str(&def.props, ofx::P_HINT, 0).to_lowercase();
	let name = def.name.to_lowercase();

	// Rule 1: explicit color keyword → color.
	for kw in COLOR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "color";
		}
	}

	// Rule 2: explicit scalar/grading keyword → scalar.
	for kw in SCALAR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "scalar";
		}
	}

	// Rule 3: display range clearly outside [0,1] → scalar.
	let dim = if def.ofx_type == ofx::TYPE_RGBA { 4 } else { 3 };
	for i in 0..dim {
		let dmin = prop_double(&def.props, ofx::P_DISPLAY_MIN, i);
		let dmax = prop_double(&def.props, ofx::P_DISPLAY_MAX, i);
		if dmin < -0.01 || dmax > 1.01 {
			return "scalar";
		}
	}

	// Rule 4: all default values equal → scalar (lean).
	let mut defs = [0.0f64; 4];
	for i in 0..dim {
		defs[i] = prop_double(&def.props, ofx::P_DEFAULT, i);
	}
	let all_equal = (1..dim).all(|i| defs[i] == defs[0]);
	if all_equal {
		return "scalar";
	}

	// Rule 5: parent group name contains a scalar keyword → scalar.
	let parent = prop_str(&def.props, ofx::P_PARENT, 0).to_lowercase();
	if !parent.is_empty() {
		let group_label = group_labels
			.get(&prop_str(&def.props, ofx::P_PARENT, 0))
			.map(|s| s.to_lowercase())
			.unwrap_or_default();
		for kw in SCALAR_KEYWORDS {
			if parent.contains(kw) || group_label.contains(kw) {
				return "scalar";
			}
		}
	}

	// Fallback.
	"color"
}

// ---------------------------------------------------------------------------
// clip display name (plugin.cpp clip_label_for_name, :248-270)
// ---------------------------------------------------------------------------

/// Computes the display name of a clip (texture input) on a node.
///
/// Transition-specific clip names are mapped first: `Source`→"Source",
/// `SourceFrom`→"From", `SourceTo`→"To"; otherwise the clip property
/// `OfxPropLabel` is preferred, falling back to the clip name itself when that
/// is empty. Corresponds to C++ `clip_label_for_name` (plugin.cpp:248-270).
///
/// # Arguments
///
/// * `name` - OFX clip name.
/// * `clip_props` - The clip's property set; pass `None` when it has no
///   properties.
fn clip_label_for_name(name: &str, clip_props: Option<&PropertySet>) -> String {
	// Transition-context clip names (ofxImageEffect.h:1435-1441).
	if name == SOURCE_CLIP {
		return "Source".to_string();
	}
	if name == "SourceFrom" {
		return "From".to_string();
	}
	if name == "SourceTo" {
		return "To".to_string();
	}
	if let Some(props) = clip_props {
		let label = prop_str(props, ofx::PROP_LABEL, 0);
		if !label.is_empty() {
			return label;
		}
	}
	name.to_string()
}

// ---------------------------------------------------------------------------
// Parameter translation: OFX parameters → node inputs (plugin.cpp:331-490)
// ---------------------------------------------------------------------------

/// Maps an OFX type → node input value type (the type table in
/// plugin.cpp:340-378; Group/Page and unknown types (k_none) have no input →
/// None and are skipped).
fn input_type_for(ofx_type: &str) -> Option<ValueType> {
	Some(match ofx_type {
		ofx::TYPE_INTEGER => ValueType::Int,
		ofx::TYPE_DOUBLE => ValueType::Float,
		ofx::TYPE_BOOLEAN => ValueType::Boolean,
		ofx::TYPE_STRING => ValueType::Text,
		ofx::TYPE_RGB | ofx::TYPE_RGBA => ValueType::Color,
		ofx::TYPE_CHOICE => ValueType::Combo,
		ofx::TYPE_DOUBLE2D | ofx::TYPE_INTEGER2D => ValueType::Vec2,
		ofx::TYPE_DOUBLE3D | ofx::TYPE_INTEGER3D => ValueType::Vec3,
		ofx::TYPE_STRCHOICE => ValueType::StrCombo,
		ofx::TYPE_BYTES | ofx::TYPE_CUSTOM => ValueType::Binary,
		ofx::TYPE_PUSHBUTTON => ValueType::PushButton,
		// parametric: value = JSON text of the default curve (see
		// [`crate::param_curve::curves_to_json`]).
		ofx::TYPE_PARAMETRIC => ValueType::Parametric,
		_ => return None,
	})
}

/// Builds the plugin's node input table (`NodeCore`) from a descriptor
/// instance.
///
/// Scans the parameters in two passes: the first collects group labels and the
/// page→parameter mapping; the second translates every value parameter into an
/// input (defaults from [`cached_defaults`], secret→HIDDEN, label→display_name,
/// the `ui_group`/`ui_page` properties, the parametric dimensions/domain/UI
/// color properties, Color's `color_semantic`+min/max+tooltip, and
/// Combo/StrCombo's `combo_option`/`combo_value` with a stable sort by
/// ChoiceOrder). Then turns every clip (except "Output") into a Texture input,
/// and finally decides effect_input: prefer the `Source` clip, else an existing
/// `Texture` input, otherwise add a `Texture` input when other clips exist and
/// select it.
///
/// # Arguments
///
/// * `inst` - Plugin instance that has been created and described.
///
/// The returned core is taken over by `PluginNode::new`; this function does not
/// register an instance.
fn build_core(inst: &Instance) -> NodeCore {
	let mut core = NodeCore::new();

	let defaults = cached_defaults(&inst.plugin.identifier, &inst.params);

	// Pass 1: group/page labels and param→page mapping (plugin.cpp:300-330).
	let mut group_labels: HashMap<String, String> = HashMap::new();
	let mut page_for_param: HashMap<String, String> = HashMap::new();
	for p in &inst.params.params {
		let def = &p.def;
		match def.ofx_type.as_str() {
			ofx::TYPE_GROUP => {
				let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
				group_labels.insert(
					def.name.clone(),
					if label.is_empty() {
						def.name.clone()
					} else {
						label
					},
				);
			}
			ofx::TYPE_PAGE => {
				let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
				let page_label = if label.is_empty() {
					def.name.clone()
				} else {
					label
				};
				let count = def.props.dimension(ofx::P_PAGE_CHILD);
				for i in 0..count {
					let child = prop_str(&def.props, ofx::P_PAGE_CHILD, i);
					if child == ofx::PAGE_SKIP_ROW || child == ofx::PAGE_SKIP_COLUMN {
						continue;
					}
					page_for_param.insert(child, page_label.clone());
				}
			}
			_ => {}
		}
	}

	// Pass 2: value parameters → inputs (plugin.cpp:331-490).
	for p in &inst.params.params {
		let def = &p.def;
		let Some(value_type) = input_type_for(&def.ofx_type) else {
			continue;
		};
		let input_id = def.name.clone();
		if input_id.is_empty() {
			continue;
		}

		let is_secret = prop_int(&def.props, ofx::P_SECRET, 0) != 0;

		// Default value (cache table; C++ defaults.value(input_id)).
		let mut input = match defaults.get(&input_id) {
			Some(default_value) => {
				let input = Input::new(&input_id, value_type, default_value.clone());
				if !matches!(value_type, ValueType::PushButton) {
					core.set_standard_value(&input_id, 0, default_value.clone());
				}
				input
			}
			None => Input::new(&input_id, value_type, NodeValue::None),
		};

		if is_secret {
			input.flags |= input_flags::HIDDEN;
		}
		let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
		input.display_name = if label.is_empty() {
			input_id.clone()
		} else {
			label
		};

		let parent = prop_str(&def.props, ofx::P_PARENT, 0);
		if !parent.is_empty() {
			let group = group_labels.get(&parent).cloned().unwrap_or(parent.clone());
			input
				.properties
				.push(("ui_group".to_string(), NodeValue::Text(group)));
		}
		if let Some(page) = page_for_param.get(&input_id) {
			input
				.properties
				.push(("ui_page".to_string(), NodeValue::Text(page.clone())));
		}

		// Parametric-specific properties (consumer = inspector curve editor;
		// values moved over from the OfxParametricParameterSuite property
		// table):
		// - "parametric_dimension": int, dimension count;
		// - "parametric_range": Vec2, domain (lo, hi);
		// - "parametric_ui_colour": one per dimension (repeated key, matching
		//   the combo_option convention), Color([r,g,b,1]), taken from the
		//   double×3N property; not carried when unconfigured.
		// No parameter animation (phase 1) → not keyframable.
		if matches!(value_type, ValueType::Parametric) {
			input.flags |= input_flags::NOT_KEYFRAMABLE;
			let dim = prop_int(&def.props, ofx::P_PARAMETRIC_DIMENSION, 0).max(1);
			input.properties.push((
				"parametric_dimension".to_string(),
				NodeValue::Int(dim as i64),
			));
			input.properties.push((
				"parametric_range".to_string(),
				NodeValue::Vec2([
					prop_double(&def.props, ofx::P_PARAMETRIC_RANGE, 0),
					prop_double(&def.props, ofx::P_PARAMETRIC_RANGE, 1),
				]),
			));
			let colour_dim = def.props.dimension(ofx::P_PARAMETRIC_UI_COLOUR);
			for i in 0..dim as usize {
				if i * 3 + 2 >= colour_dim {
					break;
				}
				input.properties.push((
					"parametric_ui_colour".to_string(),
					NodeValue::Color([
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3),
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3 + 1),
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3 + 2),
						1.0,
					]),
				));
			}
		}

		if matches!(value_type, ValueType::Color) {
			let semantic = deduce_color_semantic(def, &group_labels);
			input.properties.push((
				"color_semantic".to_string(),
				NodeValue::Text(semantic.to_string()),
			));
			// display min/max taken from dimension 0 (plugin.cpp:418-424).
			input.properties.push((
				"min".to_string(),
				NodeValue::Float(prop_double(&def.props, ofx::P_DISPLAY_MIN, 0)),
			));
			input.properties.push((
				"max".to_string(),
				NodeValue::Float(prop_double(&def.props, ofx::P_DISPLAY_MAX, 0)),
			));
			let hint = prop_str(&def.props, ofx::P_HINT, 0);
			if !hint.is_empty() {
				input
					.properties
					.push(("tooltip".to_string(), NodeValue::Text(hint)));
			}
		}

		if matches!(value_type, ValueType::Combo | ValueType::StrCombo) {
			let mut option_labels = Vec::new();
			let mut option_values = Vec::new();
			let label_count = def.props.dimension(ofx::P_CHOICE_OPTION);
			let value_count = def.props.dimension(ofx::P_CHOICE_ENUM);
			for i in 0..label_count {
				option_labels.push(prop_str(&def.props, ofx::P_CHOICE_OPTION, i));
			}
			for i in 0..value_count {
				option_values.push(prop_str(&def.props, ofx::P_CHOICE_ENUM, i));
			}
			if option_labels.is_empty() && !option_values.is_empty() {
				option_labels = option_values.clone();
			}
			if option_values.is_empty() && !option_labels.is_empty() {
				option_values = option_labels.clone();
			}

			// Stable sort by ChoiceOrder (plugin.cpp:449-472).
			let order_count = def.props.dimension(ofx::P_CHOICE_ORDER);
			if order_count == option_labels.len() && option_labels.len() == option_values.len() {
				let mut indices: Vec<usize> = (0..option_labels.len()).collect();
				indices.sort_by_key(|&i| prop_int(&def.props, ofx::P_CHOICE_ORDER, i));
				option_labels = indices.iter().map(|&i| option_labels[i].clone()).collect();
				option_values = indices.iter().map(|&i| option_values[i].clone()).collect();
			}

			// combo options are carried via repeated-key properties (NodeValue
			// has no string-table variant; consumers collect all
			// ("combo_option", _), and str_combo's value table is
			// ("combo_value", _)—the equivalent of C++ set_combo_box_strings /
			// "combo_value_str").
			for label in &option_labels {
				input
					.properties
					.push(("combo_option".to_string(), NodeValue::Text(label.clone())));
			}
			if matches!(value_type, ValueType::StrCombo) {
				for value in &option_values {
					input
						.properties
						.push(("combo_value".to_string(), NodeValue::Text(value.clone())));
				}
			}
		}

		core.add_input(input);
	}

	// clip → texture input (plugin.cpp:492-501).
	let mut has_texture_input = false;
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		let mut input = Input::new(&clip.name, ValueType::Texture, NodeValue::None);
		input.display_name = clip_label_for_name(&clip.name, Some(&clip.props));
		core.add_input(input);
		has_texture_input = true;
	}

	// effect_input selection (plugin.cpp:503-514).
	if core.has_input(SOURCE_CLIP) {
		core.effect_input = SOURCE_CLIP.to_string();
	} else if core.has_input(TEXTURE_INPUT) {
		core.effect_input = TEXTURE_INPUT.to_string();
	} else if has_texture_input {
		let mut input = Input::new(TEXTURE_INPUT, ValueType::Texture, NodeValue::None);
		input.display_name = "Texture".to_string();
		core.add_input(input);
		core.effect_input = TEXTURE_INPUT.to_string();
	}

	core
}

/// Maps an OFX image-effect context to the factory's display sub-category.
///
/// `OfxImageEffectContextFilter/Generator/Transition` map to
/// Filter/Generator/Transition respectively, and everything else (General and
/// unknown contexts) maps to "General". The result is written to
/// `DynamicNodeMeta::sub_category` and determines the grouping in the create
/// menu.
///
/// # Arguments
///
/// * `context` - OFX context string.
fn sub_category_for(context: &str) -> &'static str {
	match context {
		"OfxImageEffectContextFilter" => "Filter",
		"OfxImageEffectContextGenerator" => "Generator",
		"OfxImageEffectContextTransition" => "Transition",
		_ => "General",
	}
}

/// The plugin's display name in the create menu.
///
/// Takes the descriptor's `OfxPropLabel`; when that is empty it falls back to
/// the plugin identifier so the menu entry is never empty. Corresponds to C++
/// `PluginNode::name` (plugin.cpp:517-524).
///
/// # Arguments
///
/// * `inst` - Plugin instance (reads `inst.plugin.descriptor.props`).
fn plugin_display_name(inst: &Instance) -> String {
	let label = prop_str(&inst.plugin.descriptor.props, ofx::PROP_LABEL, 0);
	if label.is_empty() {
		inst.plugin.identifier.clone()
	} else {
		label
	}
}

/// The description text of the plugin descriptor.
///
/// Reads the `OfxPropPluginDescription` property; returns an empty string when
/// the plugin does not provide it (not `None`; callers use it directly as an
/// empty description). Corresponds to C++ `PluginNode::description`
/// (plugin.cpp:531-538).
///
/// # Arguments
///
/// * `inst` - Plugin instance (reads the descriptor property set).
fn plugin_description(inst: &Instance) -> String {
	prop_str(&inst.plugin.descriptor.props, PROP_PLUGIN_DESCRIPTION, 0)
}

// ---------------------------------------------------------------------------
// Node construction (a fresh instance per graph build—the C++ library entry
// sharing one instance is the Qt parent/child ownership model; on the Rust side
// each node owning an exclusive instance is required for duplicate and
// concurrent rendering)
// ---------------------------------------------------------------------------

/// Creates a new plugin instance for `(identifier, context)` and wraps it as a
/// node.
///
/// Creates the instance through `Host::global().create_instance`, lets
/// [`build_core`] produce the input table, reads the display name/description/
/// sub-category, registers the instance in the registry, then wraps it with
/// `PluginNode::new` and returns `(core, behavior)`. Returns `None` when
/// instance creation fails (e.g. the plugin does not support that context), and
/// the caller falls back to an empty plugin node.
///
/// # Arguments
///
/// * `identifier` - Plugin identifier (also used as the factory type id).
/// * `context` - OFX image-effect context string; determines the display
///   sub-category.
fn create_plugin_node(
	identifier: &str,
	context: &str,
) -> Option<(NodeCore, Box<dyn NodeBehavior>)> {
	let inst = Host::global()
		.create_instance(identifier, Some(context))
		.ok()?;
	let core = build_core(&inst.value);
	let name = plugin_display_name(&inst.value);
	let description = plugin_description(&inst.value);
	let sub_category = sub_category_for(context).to_string();
	let id = register_instance(inst);
	let node = PluginNode::new(
		PluginInstanceHandle(id),
		name,
		identifier.to_string(),
		description,
		sub_category,
	);
	Some((core, Box::new(node)))
}

/// Scans the host's plugin cache and registers dynamic entries with the node
/// factory (C++ `NodeFactory::register_plugin_nodes`, factory.cpp:148-186).
/// Returns the list of newly registered type ids; existing ids are skipped
/// (register_dynamic de-duplicates, matching the C++ existing_ids check).
pub fn register_plugin_nodes() -> Vec<String> {
	install_render_executor();

	let host = Host::global();
	let mut registered = Vec::new();
	for i in 0..host.cache.count() {
		let Some(plugin) = host.cache.at(i) else {
			continue;
		};

		// Metadata instance (name/description; discarded after creation).
		// Context selection: filter first, falling back in order when
		// instantiation fails (createInstance of plugins like TrackerPM only
		// runs under tracker/general/paint). A plugin where every context fails
		// cannot be instantiated (common for vendor-suite-only plugins such as
		// Vegas stereo)—log the reason, do not skip silently.
		let (context, inst) = match host.create_instance_preferred(&plugin.identifier) {
			Ok(pair) => pair,
			Err(e) => {
				eprintln!("[ofx] {}: instance creation failed: {e}", plugin.identifier);
				continue;
			}
		};
		let name = plugin_display_name(&inst.value);
		let description = plugin_description(&inst.value);
		let sub_category = sub_category_for(&context).to_string();
		drop(inst);

		let identifier = plugin.identifier.clone();
		let create: DynNodeConstructor = Arc::new(move || {
			create_plugin_node(&identifier, &context)
				.unwrap_or_else(quercus_node::nodes::plugin::create)
		});

		let meta = DynamicNodeMeta {
			type_id: plugin.identifier.clone(),
			name,
			categories: vec![Category::OpenFx],
			sub_category,
			description,
			create,
		};
		if quercus_node::factory::Factory::global().register_dynamic(meta) {
			registered.push(plugin.identifier.clone());
		}
	}
	registered
}

// ---------------------------------------------------------------------------
// Render executor + duplicator (the quercusplugin-side half of dependency
// inversion)
// ---------------------------------------------------------------------------

/// Writes text-family parameters back to the plugin instance (POD value
/// overrides cannot express strings, so `set_ofx` is used directly).
///
/// Dispatches on the parameter's OFX type: String → `ParamValue::String`,
/// StrChoice → `ParamValue::StrChoice` (text containing NUL is rejected; a
/// failing `CString::new` is ignored), Parametric → parses the JSON into a
/// curve set and writes it back as `ParamValue::Parametric` (format described
/// in [`crate::param_curve::curves_from_json`]). A missing parameter,
/// mismatched type, or invalid JSON is silently ignored.
///
/// # Arguments
///
/// * `inst` - Target plugin instance.
/// * `key` - Parameter name (OFX parameter identifier).
/// * `text` - Text form of the new value; curve JSON for Parametric
///   parameters.
fn set_text_param(inst: &Instance, key: &str, text: &str) {
	let Some(p) = inst.params.find(key) else {
		return;
	};
	match p.def.ofx_type.as_str() {
		ofx::TYPE_STRING => {
			let Ok(cs) = CString::new(text) else {
				return;
			};
			p.set_ofx(ParamValue::String(cs));
		}
		ofx::TYPE_STRCHOICE => {
			let Ok(cs) = CString::new(text) else {
				return;
			};
			p.set_ofx(ParamValue::StrChoice(cs));
		}
		ofx::TYPE_PARAMETRIC => {
			if let Some(curves) = crate::param_curve::curves_from_json(text) {
				p.set_ofx(ParamValue::Parametric(curves));
			}
		}
		_ => {}
	}
}

/// Render-executor slot implementation: hands a `PluginJobRequest` to the
/// plugin render path.
///
/// Handles only `JobSpec::Plugin`: looks up the registry by `instance`; writes
/// the text/StrChoice/curves in `values` back to the instance
/// ([`set_text_param`]) and collects POD values as numeric overrides for
/// `render_driver`; creates the output texture at the input texture size (for
/// GPU inputs with `QUERCUS_OFX_GL` + GL zero-copy, uses a GPU texture from the
/// same `GpuContext`, otherwise CPU F32; creation failure also falls back to
/// CPU); finally calls `render_driver::render_frame` and returns the output
/// texture.
///
/// # Arguments
///
/// * `req` - Render request: `spec` (plugin instance id, time, effect input,
///   input table, parameter overrides) and `src` (the evaluated input texture,
///   which must not be a dummy).
///
/// # Errors
///
/// * `Error::Invalid`: `spec` is not `JobSpec::Plugin`.
/// * `Error::Failed`: the instance is not registered (already released), the
///   input is a dummy, output frame allocation fails, or the plugin render
///   fails.
fn execute_plugin_job(
	req: &quercus_render::eval::PluginJobRequest<'_>,
) -> quercus_render::error::Result<quercus_core::texture::Texture> {
	use quercus_render::error::Error;

	let quercus_render::eval::JobSpec::Plugin {
		instance,
		type_id: _,
		time,
		effect_input_id,
		inputs,
		values,
		animations,
	} = req.spec
	else {
		return Err(Error::Invalid);
	};

	let inst = instance_from_id(*instance).ok_or_else(|| {
		Error::Failed(format!(
			"plugin instance {instance} is not registered (instance already released?)"
		))
	})?;

	if req.src.is_dummy() {
		return Err(Error::Failed(
			"plugin job has no available input texture".into(),
		));
	}

	// Parameter injection: the numeric family goes through render_driver's POD
	// overrides; the string/curve family has no POD representation, so
	// set_ofx is called directly here (matching pluginrenderer.cpp's
	// StringInstance::set branch + parametric JSON write-back).
	let mut pod_values = Vec::new();
	for (key, nv) in values {
		match nv {
			NodeValue::Text(s) | NodeValue::StrCombo(s) => set_text_param(&inst.value, key, s),
			NodeValue::PushButton | NodeValue::None => {}
			other => {
				if let Some(v) = crate::node::Value::from_node_value(other) {
					pod_values.push((key.clone(), v));
				}
			}
		}
	}

	// Output texture: same size as the input. For GPU inputs with backend
	// OpenFX GL zero-copy support, the output must also be a GPU texture on the
	// same GpuContext; otherwise keep CPU F32 (the old path). Creation failure
	// falls back to CPU without blocking the plugin.
	let (w, h) = req.src.size();
	let dst = match &req.src {
		quercus_core::texture::Texture::Gpu { ctx, .. } => {
			let gpu = ctx
				.as_any()
				.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>());
			match gpu {
				// CPU-only OFX renders must not target a GPU texture: the
				// CPU image would be uploaded and then downloaded again,
				// which has been losing the Vulkan device on the graph path.
				Some(gpu)
					if gpu.supports_gl_zerocopy()
						&& std::env::var_os("QUERCUS_OFX_GL").is_some() =>
				{
					match gpu.create_texture(w, h) {
						Ok(token) => quercus_core::texture::Texture::gpu(
							ctx.clone(),
							token,
							w,
							h,
							quercus_core::PixelFormat::F32,
						),
						Err(err) => {
							eprintln!("[ofx] GPU output texture unavailable, CPU fallback: {err}");
							quercus_core::texture::Texture::wrap_frame(
								quercus_render::eval::generate_frame(
									quercus_core::Rational::from_double(*time),
									(w, h),
									quercus_core::PixelFormat::F32,
								)?,
							)
						}
					}
				}
				_ => quercus_core::texture::Texture::wrap_frame(
					quercus_render::eval::generate_frame(
						quercus_core::Rational::from_double(*time),
						(w, h),
						quercus_core::PixelFormat::F32,
					)?,
				),
			}
		}
		_ => quercus_core::texture::Texture::wrap_frame(quercus_render::eval::generate_frame(
			quercus_core::Rational::from_double(*time),
			(w, h),
			quercus_core::PixelFormat::F32,
		)?),
	};
	let job = crate::render_driver::RenderJob {
		time: *time,
		dst,
		src: Some(req.src.clone()),
		effect_input_id: effect_input_id.clone(),
		inputs: inputs.clone(),
		values: pod_values,
		animations: animations.clone(),
		// GL render path: OpenGL-only plugins (CImg suite) return
		// kOfxStatErrMissingHostFeature directly in isIdentity on the CPU path
		// (the purple-frame root cause). The marker context only serves
		// use_opengl kind determination; real GL rendering goes through
		// gl_bridge's offscreen context (also available in the headless
		// quercus-worker process).
		renderer: Some(std::sync::Arc::new(crate::render::GlKindMarker)),
		clear_destination: false,
		interactive: false,
	};
	let (out, _rois) = crate::render_driver::render_frame(&inst.value, &job)
		.map_err(|e| Error::Failed(format!("plugin render failed: {e:?}")))?;
	Ok(out)
}

/// Duplicator slot implementation: creates a **new** plugin instance for a
/// duplicated node.
///
/// Looks up the old handle to get the identifier and context, calls
/// createInstance again through `Host`, then registers the new instance in the
/// registry and returns the new handle. Returns `None` when the old instance is
/// not found or the new one cannot be created; `PluginNode::duplicate` then
/// also returns `None` (duplication failed). Each node owns an exclusive
/// instance, so duplicated nodes do not share parameter state with the node
/// they were copied from.
///
/// # Arguments
///
/// * `old` - Instance handle of the node being duplicated.
fn duplicate_instance(old: PluginInstanceHandle) -> Option<PluginInstanceHandle> {
	let inst = instance_from_id(old.0)?;
	let identifier = inst.value.plugin.identifier.clone();
	let context = inst.value.context.clone();
	let new_inst = Host::global()
		.create_instance(&identifier, Some(&context))
		.ok()?;
	Some(PluginInstanceHandle(register_instance(new_inst)))
}

static EXECUTOR_INSTALLED: OnceLock<()> = OnceLock::new();

/// Installs the render executor into quercusrender's executor slot and the
/// duplicator into quercusnode (idempotent). [`register_plugin_nodes`] already
/// includes this; the app may also call it directly when initialising the
/// render pipeline on its own.
pub fn install_render_executor() {
	EXECUTOR_INSTALLED.get_or_init(|| {
		quercus_render::eval::set_plugin_executor(Some(Arc::new(execute_plugin_job)));
		quercus_render::eval::set_plugin_instance_factory(Some(Arc::new(shared_plugin_instance)));
		quercus_render::eval::set_plugin_gpu_query(Some(Arc::new(|identifier: &str| {
			if std::env::var_os("QUERCUS_OFX_GL").is_none() {
				return false;
			}
			let Some(id) = shared_plugin_instance(identifier) else {
				return false;
			};
			instance_from_id(id)
				.map(|inst| crate::render_driver::plugin_supports_opengl(&inst.value))
				.unwrap_or(false)
		})));
		quercus_node::nodes::plugin::set_plugin_duplicator(Some(Arc::new(duplicate_instance)));
	});
}

// ---------------------------------------------------------------------------
// Shared instances for the montage render path (the render process lazily
// creates per plugin identifier + process-level cache)
// ---------------------------------------------------------------------------

/// A montage effect stack carries a plugin identifier rather than an instance
/// id (the montage is resolved from the timeline in the main process, while
/// instances live in the render process). The render process creates instances
/// on demand through this factory and caches them: parameters are injected by
/// the montage before every render (the values overrides of
/// [`execute_plugin_job`]), and the worker renders sequentially on a single
/// thread, so shared instances have no cross-frame state conflicts.
fn shared_plugin_instance(identifier: &str) -> Option<u64> {
	static SHARED: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
	let cache = SHARED.get_or_init(|| Mutex::new(HashMap::new()));
	if let Some(&id) = cache
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(identifier)
	{
		return Some(id);
	}
	let host = Host::global();
	host.cache.find(identifier)?;
	// Context selection: filter first, falling back in order when
	// instantiation fails (same strategy as [`register_plugin_nodes`], see
	// [`Host::create_instance_preferred`]).
	let (_context, inst) = host.create_instance_preferred(identifier).ok()?;
	let id = register_instance(inst);
	cache
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(identifier.to_string(), id);
	Some(id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
