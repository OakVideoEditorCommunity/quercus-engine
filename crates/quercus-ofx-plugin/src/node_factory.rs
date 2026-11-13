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
// 实例注册表
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
// 项目幅面（normalised 坐标默认值 → canonical 的换算基准）
// ---------------------------------------------------------------------------
//
// C++ get_project_extent 读 Current::current_video_params
// （plugin.cpp:45-50）；crate 侧无项目上下文，app 经
// set_project_extent 注入，未注入用 HD 缺省。

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
// 属性读取助手
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
// 默认值（plugin.cpp default_value_for_param，:57-131）
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
			// C++ 亦按字符串读默认（plugin.cpp:83-87）；节点输入是
			// binary，按字节保留。
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
		// parametric：值 = 默认曲线的确定性 JSON（节点输入无曲线模型，
		// 经 Text 承载；撤销/工程序列化随标准值免费获得）。默认曲线
		// 来自 def.default（describe 期插件经 parametric suite 改过
		// 的默认），不是 P_DEFAULT 属性（parametric 无标量默认）。
		ofx::TYPE_PARAMETRIC => match &def.default {
			ParamValue::Parametric(curves) => {
				Some(NodeValue::Text(crate::param_curve::curves_to_json(curves)))
			}
			// 防御：type_default 恒产出 Parametric，此处只兜底。
			_ => None,
		},
		// PushButton/Group/Page/未知 → invalid（C++ 末尾
		// return QVariant()）。
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
// 颜色语义启发式（plugin.cpp deduce_color_semantic，:133-222）
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

	// 规则 1：显式颜色关键词 → color。
	for kw in COLOR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "color";
		}
	}

	// 规则 2：显式标量/调色关键词 → scalar。
	for kw in SCALAR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "scalar";
		}
	}

	// 规则 3：display 范围显著越出 [0,1] → scalar。
	let dim = if def.ofx_type == ofx::TYPE_RGBA { 4 } else { 3 };
	for i in 0..dim {
		let dmin = prop_double(&def.props, ofx::P_DISPLAY_MIN, i);
		let dmax = prop_double(&def.props, ofx::P_DISPLAY_MAX, i);
		if dmin < -0.01 || dmax > 1.01 {
			return "scalar";
		}
	}

	// 规则 4：默认值全相等 → scalar（lean）。
	let mut defs = [0.0f64; 4];
	for i in 0..dim {
		defs[i] = prop_double(&def.props, ofx::P_DEFAULT, i);
	}
	let all_equal = (1..dim).all(|i| defs[i] == defs[0]);
	if all_equal {
		return "scalar";
	}

	// 规则 5：父 group 名含标量关键词 → scalar。
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

	// 兜底。
	"color"
}

// ---------------------------------------------------------------------------
// clip 显示名（plugin.cpp clip_label_for_name，:248-270）
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
	// 过渡上下文 clip 名（ofxImageEffect.h:1435-1441）。
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
// 参数翻译：OFX 参数 → 节点输入（plugin.cpp:331-490）
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
		// parametric：值 = 默认曲线的 JSON 文本（见
		// [`crate::param_curve::curves_to_json`]）。
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

	// 第 1 遍：group/page 标签与 param→page 映射（plugin.cpp:300-330）。
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

	// 第 2 遍：值参数 → 输入（plugin.cpp:331-490）。
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

		// 默认值（缓存表；C++ defaults.value(input_id)）。
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

		// parametric 专属属性（消费方 = 检查器曲线编辑器；值从
		// OfxParametricParameterSuite 的属性表搬运）：
		// - "parametric_dimension"：int，维度数；
		// - "parametric_range"：Vec2，定义域 (lo, hi)；
		// - "parametric_ui_colour"：每维一条（重复键，对齐 combo_option
		//   惯例），Color([r,g,b,1])，取自 double×3N 属性，未配置不携带。
		// 无参数动画（第 1 期）→ 不可键帧。
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
			// display min/max 取第 0 维（plugin.cpp:418-424）。
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

			// ChoiceOrder 稳定排序（plugin.cpp:449-472）。
			let order_count = def.props.dimension(ofx::P_CHOICE_ORDER);
			if order_count == option_labels.len() && option_labels.len() == option_values.len() {
				let mut indices: Vec<usize> = (0..option_labels.len()).collect();
				indices.sort_by_key(|&i| prop_int(&def.props, ofx::P_CHOICE_ORDER, i));
				option_labels = indices.iter().map(|&i| option_labels[i].clone()).collect();
				option_values = indices.iter().map(|&i| option_values[i].clone()).collect();
			}

			// combo 选项经重复键属性携带（NodeValue 无字符串表变体；
			// 消费方按 ("combo_option", _) 全量收集，str_combo 的
			// 值表为 ("combo_value", _)——C++ set_combo_box_strings /
			// "combo_value_str" 的等价物）。
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

	// clip → 纹理输入（plugin.cpp:492-501）。
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

	// effect_input 选择（plugin.cpp:503-514）。
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
// 节点构造（每次建图都新建实例——C++ 库条目共享一个实例是 Qt 父子
// 所有权模型；Rust 侧每节点独占实例才能支持 duplicate 与并发渲染）
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

		// 元数据实例（name/description；建完即弃）。上下文选择：
		// filter 优先、实例化失败按序回退（TrackerPM 这类插件的
		// createInstance 只在 tracker/general/paint 下能跑）。所有
		// 上下文都失败的插件无法实例化（常见于只支持 Vegas 立体声
		// 等厂商套件的插件）——记录原因，不静默跳过。
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
// 渲染执行器 + duplicator（依赖反转的 quercusplugin 侧半环）
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
	} = req.spec
	else {
		return Err(Error::Invalid);
	};

	let inst = instance_from_id(*instance)
		.ok_or_else(|| Error::Failed(format!("插件实例 {instance} 未登记（实例已释放？）")))?;

	if req.src.is_dummy() {
		return Err(Error::Failed("plugin job 无可用输入纹理".into()));
	}

	// 参数注入：数值族走 render_driver 的 POD 覆盖；字符串/曲线族
	// POD 无表达，这里直接 set_ofx（对齐 pluginrenderer.cpp 的
	// StringInstance::set 分支 + parametric 的 JSON 写回）。
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

	// 输出纹理：与输入同尺寸。GPU 输入且后端支持 OpenFX GL 零拷贝时，
	// 输出也必须是同一 GpuContext 上的 GPU 纹理；否则保持 CPU F32
	// （旧路径）。创建失败回退 CPU，不阻断插件。
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
				},
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
		// GL 渲染路径：OpenGL-only 插件（CImg 套件）在 CPU 路径的
		// isIdentity 直接回 kOfxStatErrMissingHostFeature（紫帧根因）。
		// 标记上下文只服务于 use_opengl 的 kind 判定；真正的 GL 渲染走
		// gl_bridge 的离屏上下文（无头的 quercus-worker 进程同样可用）。
		renderer: Some(std::sync::Arc::new(crate::render::GlKindMarker)),
		clear_destination: false,
		interactive: false,
	};
	let (out, _rois) = crate::render_driver::render_frame(&inst.value, &job)
		.map_err(|e| Error::Failed(format!("插件渲染失败：{e:?}")))?;
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
// montage 渲染路径的共享实例（渲染进程按插件标识惰性创建 + 进程级缓存）
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
	// 上下文选择：filter 优先、实例化失败按序回退（与
	// [`register_plugin_nodes`] 同一策略，见
	// [`Host::create_instance_preferred`]）。
	let (_context, inst) = host.create_instance_preferred(identifier).ok()?;
	let id = register_instance(inst);
	cache
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(identifier.to_string(), id);
	Some(id)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	/// Records what the mock plugin entry saw, so the push-button test can
	/// assert the kOfxActionInstanceChanged routing and its inArgs.
	static PUSH_ENTRY_CALLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

	/// PUSH_ENTRY_CALLS is a process-level static: tests that touch it must be serialised.
	static PUSH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	/// Mock plugin entry: instanceChanged records the action and change reason;
	/// other actions/an empty action silently succeed. Call it directly to cover
	/// the missing/mismatched-reason branches (push_button_clicked only writes a
	/// String reason).
	unsafe extern "C" fn recording_entry(
		action: *const c_char,
		_: *const c_void,
		in_args: *mut c_void,
		_: *mut c_void,
	) -> i32 {
		if !action.is_null() {
			let action = unsafe {
				std::ffi::CStr::from_ptr(action)
					.to_string_lossy()
					.into_owned()
			};
			if action == crate::host::ACTION_INSTANCE_CHANGED {
				let mut calls = PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner());
				calls.push(action);
				// The instanceChanged inArgs contract (ofxCore.h:405-435):
				// kOfxPropChangeReason = kOfxChangeUserEdited.
				let props = unsafe { &*(in_args as *const crate::property::PropertySet) };
				let reason = props.get(crate::host::PROP_CHANGE_REASON, 0);
				let reason = match reason {
					Some(crate::property::Value::String(s)) => s.to_string_lossy().into_owned(),
					_ => String::new(),
				};
				calls.push(reason);
			}
		}
		0
	}

	#[test]
	fn input_type_table_covers_all_ofx_kinds() {
		assert_eq!(input_type_for(ofx::TYPE_INTEGER), Some(ValueType::Int));
		assert_eq!(input_type_for(ofx::TYPE_DOUBLE), Some(ValueType::Float));
		assert_eq!(input_type_for(ofx::TYPE_BOOLEAN), Some(ValueType::Boolean));
		assert_eq!(input_type_for(ofx::TYPE_STRING), Some(ValueType::Text));
		assert_eq!(input_type_for(ofx::TYPE_RGB), Some(ValueType::Color));
		assert_eq!(input_type_for(ofx::TYPE_RGBA), Some(ValueType::Color));
		assert_eq!(input_type_for(ofx::TYPE_CHOICE), Some(ValueType::Combo));
		assert_eq!(input_type_for(ofx::TYPE_DOUBLE2D), Some(ValueType::Vec2));
		assert_eq!(input_type_for(ofx::TYPE_INTEGER2D), Some(ValueType::Vec2));
		assert_eq!(input_type_for(ofx::TYPE_DOUBLE3D), Some(ValueType::Vec3));
		assert_eq!(input_type_for(ofx::TYPE_INTEGER3D), Some(ValueType::Vec3));
		assert_eq!(
			input_type_for(ofx::TYPE_STRCHOICE),
			Some(ValueType::StrCombo)
		);
		assert_eq!(input_type_for(ofx::TYPE_BYTES), Some(ValueType::Binary));
		assert_eq!(input_type_for(ofx::TYPE_CUSTOM), Some(ValueType::Binary));
		assert_eq!(
			input_type_for(ofx::TYPE_PUSHBUTTON),
			Some(ValueType::PushButton)
		);
		// 容器与未知类型：跳过。
		assert_eq!(input_type_for(ofx::TYPE_GROUP), None);
		assert_eq!(input_type_for(ofx::TYPE_PAGE), None);
		assert_eq!(input_type_for("OfxParamTypeBogus"), None);
		// parametric → Parametric（值 = 曲线 JSON 文本）。
		assert_eq!(
			input_type_for(ofx::TYPE_PARAMETRIC),
			Some(ValueType::Parametric)
		);
	}

	#[test]
	fn color_semantic_rules() {
		fn def_with(label: &str, ofx_type: &str) -> ParamDef {
			let def = ParamDef {
				props: PropertySet::new(),
				name: "p".into(),
				ofx_type: ofx_type.into(),
				default: ParamValue::Container,
			};
			def.props.set_one(
				ofx::PROP_LABEL,
				PropValue::String(CString::new(label).unwrap()),
			);
			def
		}
		let groups = HashMap::new();
		// 非颜色类型恒 "color"。
		assert_eq!(
			deduce_color_semantic(&def_with("Anything", ofx::TYPE_DOUBLE), &groups),
			"color"
		);
		// 规则 1：颜色关键词。
		assert_eq!(
			deduce_color_semantic(&def_with("Tint Color", ofx::TYPE_RGB), &groups),
			"color"
		);
		// 规则 2：标量关键词。
		assert_eq!(
			deduce_color_semantic(&def_with("Gamma Adjust", ofx::TYPE_RGBA), &groups),
			"scalar"
		);
	}

	#[test]
	fn color_semantic_display_range_rule() {
		let def = ParamDef {
			props: PropertySet::new(),
			name: "rgb".into(),
			ofx_type: ofx::TYPE_RGB.into(),
			default: ParamValue::Container,
		};
		// 默认 (0,0,0) 全相等前先被规则 3 拦截：display max 越界。
		def.props
			.set_one(ofx::P_DISPLAY_MAX, PropValue::Double(2.0));
		let groups = HashMap::new();
		assert_eq!(deduce_color_semantic(&def, &groups), "scalar");
	}

	#[test]
	fn clip_labels_special_case_names() {
		assert_eq!(clip_label_for_name("Source", None), "Source");
		assert_eq!(clip_label_for_name("SourceFrom", None), "From");
		assert_eq!(clip_label_for_name("SourceTo", None), "To");
		assert_eq!(clip_label_for_name("Overlay", None), "Overlay");
		let props = PropertySet::new();
		props.set_one(
			ofx::PROP_LABEL,
			PropValue::String(CString::new("Matte").unwrap()),
		);
		assert_eq!(clip_label_for_name("Overlay", Some(&props)), "Matte");
		// 有 props 但 label 为空（或类型不符）→ 回退 clip 名。
		let no_label = PropertySet::new();
		assert_eq!(clip_label_for_name("Overlay", Some(&no_label)), "Overlay");
		no_label.set_one(ofx::PROP_LABEL, PropValue::Int(1));
		assert_eq!(clip_label_for_name("Overlay", Some(&no_label)), "Overlay");
	}

	#[test]
	fn instance_registry_roundtrip() {
		// 注册表对不存在的键返回 None；摘除未登记键 no-op。
		assert!(instance_from_id(u64::MAX).is_none());
		unregister_instance(u64::MAX);
	}

	#[test]
	fn shared_instance_unknown_identifier_yields_none() {
		// 缓存里查无此标识 → None（montage 路径据此按"无求值器"
		// 告警并直通）。
		assert!(shared_plugin_instance("com.example.definitely-missing").is_none());
	}

	/// The simplest real plugin (AddOFX: adds a constant to every pixel) renders
	/// successfully through the executor path — distinguishes "systemic host
	/// problem" from "single-plugin problem" (skipped when the machine does not
	/// have the plugin installed).
	#[test]
	fn add_plugin_renders_through_the_executor() {
		render_real_plugin_smoke("net.sf.openfx.AddPlugin");
	}

	/// The real GL-only plugin (CImg's ChromaKeyerOFX) renders successfully through
	/// the executor path — the GL-kind flag must route it onto a usable GL path
	/// instead of a MissingHostFeature purple frame (skipped when the machine does
	/// not have the plugin installed).
	#[test]
	fn cimg_chromakeyer_renders_through_the_executor() {
		render_real_plugin_smoke("net.sf.openfx.ChromaKeyerPlugin");
	}

	fn render_real_plugin_smoke(identifier: &str) {
		let host = crate::host::Host::global();
		let _ = host.cache.scan();
		if host.cache.find(identifier).is_none() {
			eprintln!("{identifier} not installed; skipping");
			return;
		}
		install_render_executor();
		let instance = shared_plugin_instance(identifier)
			.expect("the shared instance factory resolves the plugin");
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			let inst = instance_from_id(instance).expect("registered");
			let props = &inst.value.plugin.descriptor.props;
			props.with_locked(|all| {
				for p in all
					.iter()
					.filter(|p| p.name.contains("GL") || p.name.contains("OpenGL"))
				{
					eprintln!("[probe] desc prop {} = {:?}", p.name, p.values);
				}
			});
		}

		// 8x8 不透明绿帧（典型 keyer 输入）。
		let mut frame = quercus_render::eval::generate_frame(
			quercus_core::Rational::new(0, 1),
			(8, 8),
			quercus_core::PixelFormat::F32,
		)
		.unwrap();
		for px in frame.data.chunks_exact_mut(16) {
			for (c, v) in px.chunks_exact_mut(4).zip([0.1f32, 0.9, 0.1, 1.0]) {
				c.copy_from_slice(&v.to_le_bytes());
			}
		}
		let spec = quercus_render::eval::JobSpec::Plugin {
			instance,
			type_id: "org.quercus.test-plugin".to_string(),
			time: 0.0,
			effect_input_id: Some("Source".to_string()),
			inputs: Vec::new(),
			values: Vec::new(),
		};
		let out = execute_plugin_job(&quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::wrap_frame(frame),
		})
		.expect("the GL-capable render succeeds (no MissingHostFeature purple)");
		let quercus_core::texture::Texture::Cpu(out_frame) = &out else {
			panic!("a CPU frame comes back");
		};
		let mut px = [0f32; 4];
		for i in 0..4 {
			px[i] = f32::from_le_bytes(out_frame.data[i * 4..i * 4 + 4].try_into().unwrap());
		}
		eprintln!("[smoke] {identifier} first pixel out = {px:?}");
		assert_ne!(
			px,
			[1.0, 0.0, 1.0, 1.0],
			"must not be the purple failure frame"
		);
		assert_ne!(px, [0.0, 0.0, 0.0, 0.0], "must not be a black empty frame");
	}

	/// Builds a minimal instance containing only a push-button parameter (registered directly in the registry).
	fn instance_with_push_button() -> u64 {
		use crate::descriptor::EffectDescriptor;
		use crate::handle::RefBox;
		use crate::host::Plugin;
		use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
		use std::sync::atomic::AtomicU32;

		let plugin = Arc::new(Plugin {
			identifier: "test.plugin".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: recording_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		});
		let mut params = ParamSetInstance { params: Vec::new() };
		params
			.params
			.push(Box::new(ParamInstance::from_def(ParamDef::new(
				"button",
				ofx::TYPE_PUSHBUTTON,
			))));
		params
			.params
			.push(Box::new(ParamInstance::from_def(ParamDef::new(
				"gain",
				ofx::TYPE_DOUBLE,
			))));
		let inst = crate::instance::Instance {
			props: crate::property::PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: Vec::new(),
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		register_instance(Arc::new(RefBox {
			refs: AtomicU32::new(1),
			value: inst,
		}))
	}

	/// push_button_clicked: unknown instance/parameter and type mismatch → false;
	/// a hit → true, sets the PushButton value and routes the press as
	/// kOfxActionInstanceChanged (UserEdited, with inArgs carrying change reason /
	/// name / type / time / render scale).
	#[test]
	fn push_button_clicked_routes_instance_changed() {
		let _g = PUSH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
		let id = instance_with_push_button();
		assert!(push_button_clicked(id, "button"));
		// 类型不匹配 / 查无参数 / 查无实例：不触发 entry。
		assert!(!push_button_clicked(id, "gain"));
		assert!(!push_button_clicked(id, "nope"));
		assert!(!push_button_clicked(u64::MAX, "button"));
		// 只 "button" 那次按下进了插件 entry，且 reason 是 UserEdited。
		let calls = PUSH_ENTRY_CALLS
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone();
		assert_eq!(
			calls,
			vec!["OfxActionInstanceChanged", "OfxChangeUserEdited"]
		);
		unregister_instance(id);
	}

	/// Builds an instance with a parametric parameter (dimension 2, custom range,
	/// two-dimensional UI color configured) and a String parameter (registered
	/// directly in the registry).
	fn instance_with_parametric() -> u64 {
		use crate::descriptor::EffectDescriptor;
		use crate::handle::RefBox;
		use crate::host::Plugin;
		use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
		use std::ffi::{c_char, c_void};
		use std::sync::atomic::AtomicU32;

		unsafe extern "C" fn dummy_entry(
			_: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> i32 {
			0
		}
		let def = ParamDef::new("curve", ofx::TYPE_PARAMETRIC);
		def.props.set_one(
			ofx::PROP_LABEL,
			PropValue::String(CString::new("Curve").unwrap()),
		);
		def.props
			.set_one(ofx::P_PARAMETRIC_DIMENSION, PropValue::Int(2));
		def.props.define(
			ofx::P_PARAMETRIC_RANGE,
			vec![PropValue::Double(0.0), PropValue::Double(1.0)],
		);
		def.props.define(
			ofx::P_PARAMETRIC_UI_COLOUR,
			vec![
				PropValue::Double(1.0),
				PropValue::Double(0.0),
				PropValue::Double(0.0),
				PropValue::Double(0.0),
				PropValue::Double(1.0),
				PropValue::Double(0.0),
			],
		);
		let mut params = ParamSetInstance { params: Vec::new() };
		params.params.push(Box::new(ParamInstance::from_def(def)));
		params
			.params
			.push(Box::new(ParamInstance::from_def(ParamDef::new(
				"title",
				ofx::TYPE_STRING,
			))));
		let plugin = Arc::new(Plugin {
			identifier: "test.parametric".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		});
		let inst = crate::instance::Instance {
			props: crate::property::PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: Vec::new(),
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		register_instance(Arc::new(RefBox {
			refs: AtomicU32::new(1),
			value: inst,
		}))
	}

	/// The translation pass produces a Parametric input: value_type, default value
	/// (= the default curve's JSON), display_name (= label), not keyframable plus
	/// its own properties (dimension / range / one UI color per dimension).
	#[test]
	fn build_core_produces_parametric_input() {
		let id = instance_with_parametric();
		let inst = instance_from_id(id).expect("已登记");
		let core = build_core(&inst.value);

		let input = core.get_input("curve").expect("parametric 输入应存在");
		assert_eq!(input.value_type, ValueType::Parametric);
		assert_eq!(input.display_name, "Curve");
		assert_ne!(input.flags & input_flags::NOT_KEYFRAMABLE, 0);

		// 默认值 = def.default 曲线集的 JSON（type_default 产出 1 条
		// 恒等曲线；dimension 属性只做索引门控，未配置曲线按恒等读，
		// 不扩充默认值）。
		let expected =
			crate::param_curve::curves_to_json(&[crate::param_curve::Curve::identity(0.0, 1.0)]);
		assert_eq!(
			text_of(&input.default).as_deref(),
			Some(expected.as_str()),
			"默认值应为曲线 JSON"
		);
		// 非 Text 值走 None 臂（helper 自身两臂都有执行）。
		assert_eq!(text_of(&NodeValue::Int(1)), None);
		// 标准值 = 默认（工程序列化从这里走）。
		assert_eq!(core.standard_value("curve", -1), input.default);

		// 专属属性。
		let props = |name: &str| {
			input
				.properties
				.iter()
				.filter(|(k, _)| k == name)
				.map(|(_, v)| v.clone())
				.collect::<Vec<_>>()
		};
		assert_eq!(props("parametric_dimension"), vec![NodeValue::Int(2)]);
		assert_eq!(props("parametric_range"), vec![NodeValue::Vec2([0.0, 1.0])]);
		// 双维 → 两条颜色（重复键）。
		assert_eq!(
			props("parametric_ui_colour"),
			vec![
				NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
				NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
			]
		);
		unregister_instance(id);
	}

	/// Parametric input (Text JSON) → instance write-back: valid JSON is parsed and
	/// `set_ofx(Parametric)`; bad JSON / type mismatch → silently ignored (current
	/// value kept).
	#[test]
	fn parametric_input_writes_back_to_instance() {
		let id = instance_with_parametric();
		let inst = instance_from_id(id).expect("已登记");

		// 有效 JSON（双维，第二维单点）→ 曲线写回，求值即实例曲线。
		let json = r#"{"curves":[[{"key":0,"value":0,"slope":1},{"key":0.5,"value":0.7,"slope":1}],[{"key":0,"value":0,"slope":1}]]}"#;
		set_text_param(&inst.value, "curve", json);
		let curves =
			curves_of(inst.value.params.find("curve").unwrap().get()).expect("应写回 Parametric");
		assert_eq!(curves[0].evaluate(0.5), 0.7);
		assert_eq!(curves[1].len(), 1);
		// 非 Parametric 值走 None 臂（helper 自身两臂都有执行）。
		assert!(curves_of(ParamValue::PushButton).is_none());

		// 坏 JSON → 不改值（与字符串族类型不匹配静默一致）。
		let before = inst.value.params.find("curve").unwrap().get();
		set_text_param(&inst.value, "curve", "{broken");
		assert_eq!(inst.value.params.find("curve").unwrap().get(), before);

		// 非 parametric 参数（String）遇任意文本 → 字符串值路径。
		set_text_param(&inst.value, "title", "hello");
		assert!(matches!(
			inst.value.params.find("title").unwrap().get(),
			ParamValue::String(_)
		));
		unregister_instance(id);
	}

	// ---- default values / property helpers (coverage batch) --------------

	fn def_of(ofx_type: &str) -> ParamDef {
		ParamDef {
			props: PropertySet::new(),
			name: "p".into(),
			ofx_type: ofx_type.into(),
			default: ParamValue::Container,
		}
	}

	#[test]
	fn property_helpers_read_scalars_and_fall_back() {
		let props = PropertySet::new();
		props.define(
			ofx::P_DEFAULT,
			vec![PropValue::Double(2.5), PropValue::Int(7)],
		);
		assert_eq!(prop_double(&props, ofx::P_DEFAULT, 0), 2.5);
		assert_eq!(prop_double(&props, ofx::P_DEFAULT, 1), 7.0);
		assert_eq!(prop_int(&props, ofx::P_DEFAULT, 0), 2);
		assert_eq!(prop_int(&props, ofx::P_DEFAULT, 1), 7);
		props.set_one("s", PropValue::String(CString::new("hello").unwrap()));
		assert_eq!(prop_str(&props, "s", 0), "hello");

		// Missing / out-of-range / wrong-type values fall back quietly.
		assert_eq!(prop_str(&props, "missing", 0), "");
		assert_eq!(prop_double(&props, "missing", 0), 0.0);
		assert_eq!(prop_int(&props, "missing", 0), 0);
		assert_eq!(prop_double(&props, ofx::P_DEFAULT, 9), 0.0);
		assert_eq!(prop_str(&props, ofx::P_DEFAULT, 0), "");
	}

	#[test]
	fn default_value_for_every_scalar_param_type() {
		let def = def_of(ofx::TYPE_INTEGER);
		def.props.set_one(ofx::P_DEFAULT, PropValue::Int(3));
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Int(3))
		));

		let def = def_of(ofx::TYPE_CHOICE);
		def.props.set_one(ofx::P_DEFAULT, PropValue::Int(1));
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Combo(1))
		));

		let def = def_of(ofx::TYPE_BOOLEAN);
		def.props.set_one(ofx::P_DEFAULT, PropValue::Int(1));
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Boolean(true))
		));
		def.props.set_one(ofx::P_DEFAULT, PropValue::Int(0));
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Boolean(false))
		));

		let def = def_of(ofx::TYPE_DOUBLE);
		def.props.set_one(ofx::P_DEFAULT, PropValue::Double(0.25));
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Float(v)) if (v - 0.25).abs() < 1e-9
		));

		for kind in [ofx::TYPE_STRING, ofx::TYPE_STRCHOICE] {
			let def = def_of(kind);
			def.props.set_one(
				ofx::P_DEFAULT,
				PropValue::String(CString::new("hi").unwrap()),
			);
			assert!(matches!(
				default_value_for_param(&def),
				Some(NodeValue::Text(ref t)) if t == "hi"
			));
		}

		// CUSTOM keeps the raw bytes; BYTES is always empty.
		let def = def_of(ofx::TYPE_CUSTOM);
		def.props.set_one(
			ofx::P_DEFAULT,
			PropValue::String(CString::new("raw").unwrap()),
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Binary(ref b)) if b == b"raw"
		));
		let def = def_of(ofx::TYPE_BYTES);
		def.props.set_one(
			ofx::P_DEFAULT,
			PropValue::String(CString::new("ignored").unwrap()),
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Binary(ref b)) if b.is_empty()
		));

		// Unsupported kinds (and a bare parametric default) have no value.
		assert!(default_value_for_param(&def_of(ofx::TYPE_PUSHBUTTON)).is_none());
		assert!(default_value_for_param(&def_of(ofx::TYPE_GROUP)).is_none());
		assert!(default_value_for_param(&def_of("OfxParamTypeBogus")).is_none());
		assert!(default_value_for_param(&def_of(ofx::TYPE_PARAMETRIC)).is_none());
	}

	#[test]
	fn default_value_for_color_and_vector_types() {
		let def = def_of(ofx::TYPE_RGB);
		def.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
			],
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Color(c))
				if c == [0.1, 0.2, 0.3, 1.0]
		));

		let def = def_of(ofx::TYPE_RGBA);
		def.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
				PropValue::Double(0.4),
			],
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Color(c))
				if c == [0.1, 0.2, 0.3, 0.4]
		));

		let def = def_of(ofx::TYPE_DOUBLE2D);
		def.props.define(
			ofx::P_DEFAULT,
			vec![PropValue::Double(1.5), PropValue::Double(2.5)],
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Vec2(v)) if v == [1.5, 2.5]
		));

		let def = def_of(ofx::TYPE_INTEGER2D);
		def.props
			.define(ofx::P_DEFAULT, vec![PropValue::Int(1), PropValue::Int(2)]);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Vec2(v)) if v == [1.0, 2.0]
		));

		let def = def_of(ofx::TYPE_INTEGER3D);
		def.props.define(
			ofx::P_DEFAULT,
			vec![PropValue::Int(1), PropValue::Int(2), PropValue::Int(3)],
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Vec3(v)) if v == [1.0, 2.0, 3.0]
		));
	}

	#[test]
	fn project_extent_scales_normalised_defaults() {
		static EXTENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		let _guard = EXTENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());

		set_project_extent(3840.0, 2160.0);
		assert_eq!(project_extent(), (3840.0, 2160.0));
		assert_eq!(to_canonical(0.5, 3840.0), 1920.0);
		// A zero extent passes the normalised value through untouched.
		assert_eq!(to_canonical(0.5, 0.0), 0.5);

		let def = def_of(ofx::TYPE_DOUBLE);
		def.props.set_one(ofx::P_DEFAULT, PropValue::Double(0.5));
		def.props.set_one(
			ofx::P_DEFAULT_COORD_SYS,
			PropValue::String(CString::new(ofx::V_COORD_NORMALISED).unwrap()),
		);
		assert!(matches!(
			default_value_for_param(&def),
			Some(NodeValue::Float(v)) if v == 1920.0
		));

		let vec3 = def_of(ofx::TYPE_DOUBLE3D);
		vec3.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.5),
				PropValue::Double(0.5),
				PropValue::Double(0.5),
			],
		);
		vec3.props.set_one(
			ofx::P_DEFAULT_COORD_SYS,
			PropValue::String(CString::new(ofx::V_COORD_NORMALISED).unwrap()),
		);
		assert!(matches!(
			default_value_for_param(&vec3),
			Some(NodeValue::Vec3(v)) if v == [1920.0, 1080.0, 1920.0]
		));

		set_project_extent(1920.0, 1080.0);
	}

	#[test]
	fn registered_instance_count_is_observable() {
		let a = registered_instance_count();
		let b = registered_instance_count();
		assert_eq!(a, b);
	}

	// ---- branch coverage batch: translation tables / executor ------------

	use crate::clip::ClipInstance;
	use crate::descriptor::ClipDescriptor;
	use std::ffi::{c_char, c_void};

	/// Minimal plugin entry for host tests: every action succeeds (does nothing).
	unsafe extern "C" fn ok_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> i32 {
		crate::suites::status::OK
	}

	fn text(s: &str) -> PropValue {
		PropValue::String(CString::new(s).unwrap())
	}

	/// Text variant extraction (both arms are reached by callers; avoids an
	/// unreachable panic branch in tests).
	fn text_of(value: &NodeValue) -> Option<String> {
		match value {
			NodeValue::Text(s) => Some(s.clone()),
			_ => None,
		}
	}

	/// Parametric variant extraction (same as above).
	fn curves_of(value: ParamValue) -> Option<Vec<crate::param_curve::Curve>> {
		match value {
			ParamValue::Parametric(c) => Some(c),
			_ => None,
		}
	}

	fn param_set(defs: Vec<ParamDef>) -> crate::param::ParamSetInstance {
		crate::param::ParamSetInstance {
			params: defs
				.into_iter()
				.map(|d| Box::new(crate::param::ParamInstance::from_def(d)))
				.collect(),
		}
	}

	fn clip(name: &str) -> ClipInstance {
		ClipInstance::from_descriptor(&ClipDescriptor::new(name))
	}

	fn fake_instance(
		identifier: &str,
		entry: crate::host::EntryPoint,
		params: crate::param::ParamSetInstance,
		clips: Vec<ClipInstance>,
	) -> crate::instance::Instance {
		use crate::descriptor::EffectDescriptor;
		use crate::host::Plugin;
		use std::sync::atomic::{AtomicBool, AtomicUsize};

		let plugin = Arc::new(Plugin {
			identifier: identifier.into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: AtomicBool::new(false),
		});
		crate::instance::Instance {
			props: PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: clips.into_iter().map(Box::new).collect(),
			node_identity: AtomicUsize::new(0),
			destroyed: AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		}
	}

	fn register_fake(inst: crate::instance::Instance) -> u64 {
		use crate::handle::RefBox;
		use std::sync::atomic::AtomicU32;
		register_instance(Arc::new(RefBox {
			refs: AtomicU32::new(1),
			value: inst,
		}))
	}

	fn action_is(action: *const c_char, name: &str) -> bool {
		!action.is_null()
			&& unsafe { std::ffi::CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
	}

	fn prop_values(input: &quercus_node::input::Input, key: &str) -> Vec<NodeValue> {
		input
			.properties
			.iter()
			.filter(|(k, _)| k == key)
			.map(|(_, v)| v.clone())
			.collect()
	}

	/// Rule 4 (all defaults equal) and rule 5 (parent/group keywords), plus the
	/// dmin < -0.01 half of rule 3.
	#[test]
	fn color_semantic_rules_4_and_5_and_display_min() {
		let empty = HashMap::new();
		// 规则 4：默认值全相等 → scalar。
		let def = def_of(ofx::TYPE_RGB);
		def.props
			.define(ofx::P_DEFAULT, vec![PropValue::Double(0.5); 3]);
		assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

		// 规则 4 不等 + 无 parent → 兜底 color。
		let def = def_of(ofx::TYPE_RGB);
		def.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
			],
		);
		assert_eq!(deduce_color_semantic(&def, &empty), "color");

		// 规则 3 的 dmin 半边。
		let def = def_of(ofx::TYPE_RGB);
		def.props
			.set_one(ofx::P_DISPLAY_MIN, PropValue::Double(-0.5));
		assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

		// 规则 5：parent 名带标量关键词。
		let def = def_of(ofx::TYPE_RGB);
		def.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
			],
		);
		def.props.set_one(ofx::P_PARENT, text("gamma_group"));
		assert_eq!(deduce_color_semantic(&def, &empty), "scalar");

		// 规则 5：parent 经 group 表解析出的 label 带关键词。
		let mut groups = HashMap::new();
		groups.insert("grp".to_string(), "Gamma".to_string());
		let def = def_of(ofx::TYPE_RGB);
		def.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
			],
		);
		def.props.set_one(ofx::P_PARENT, text("grp"));
		assert_eq!(deduce_color_semantic(&def, &groups), "scalar");
	}

	/// build_core's group/page first pass: empty label falls back to name, page
	/// child mapping; skip row/column and empty-name/unknown-type parameters are
	/// skipped.
	#[test]
	fn build_core_translates_groups_pages_and_skips_unknowns() {
		let group_labeled = ParamDef::new("grp_labeled", ofx::TYPE_GROUP);
		group_labeled
			.props
			.set_one(ofx::PROP_LABEL, text("Grouped"));
		let group_bare = ParamDef::new("grp_bare", ofx::TYPE_GROUP);
		group_bare.props.set_one(ofx::PROP_LABEL, text(""));
		let page = ParamDef::new("page_a", ofx::TYPE_PAGE);
		page.props.set_one(ofx::PROP_LABEL, text("Page A"));
		page.props.define(
			ofx::P_PAGE_CHILD,
			vec![
				text("p_double"),
				text("p_orphan"),
				text(ofx::PAGE_SKIP_ROW),
				text(ofx::PAGE_SKIP_COLUMN),
			],
		);
		let p_double = ParamDef::new("p_double", ofx::TYPE_DOUBLE);
		p_double.props.set_one(ofx::P_PARENT, text("grp_labeled"));
		let p_orphan = ParamDef::new("p_orphan", ofx::TYPE_DOUBLE);
		p_orphan.props.set_one(ofx::P_PARENT, text("missing_group"));
		let empty_name = ParamDef::new("", ofx::TYPE_DOUBLE);
		let bogus = ParamDef::new("bogus", "OfxParamTypeBogus");

		let inst = fake_instance(
			"test.groups.pages",
			ok_entry,
			param_set(vec![
				group_labeled,
				group_bare,
				page,
				p_double,
				p_orphan,
				empty_name,
				bogus,
			]),
			vec![],
		);
		let core = build_core(&inst);

		let double = core.get_input("p_double").expect("p_double 输入");
		assert_eq!(
			prop_values(double, "ui_group"),
			vec![NodeValue::Text("Grouped".into())]
		);
		assert_eq!(
			prop_values(double, "ui_page"),
			vec![NodeValue::Text("Page A".into())]
		);

		let orphan = core.get_input("p_orphan").expect("p_orphan 输入");
		// 未登记的 group → 回退 parent 名本身。
		assert_eq!(
			prop_values(orphan, "ui_group"),
			vec![NodeValue::Text("missing_group".into())]
		);
		assert_eq!(
			prop_values(orphan, "ui_page"),
			vec![NodeValue::Text("Page A".into())]
		);

		// group/page/未知/空名都不产出输入。
		assert!(core.get_input("grp_labeled").is_none());
		assert!(core.get_input("page_a").is_none());
		assert!(core.get_input("bogus").is_none());
		assert!(core.get_input("").is_none());
	}

	/// build_core's clip→texture input and effect_input fallback chain.
	#[test]
	fn build_core_selects_effect_input_from_clips() {
		// Source 存在 → effect_input = Source；Output 不入输入表。
		let inst = fake_instance(
			"test.clips.source",
			ok_entry,
			param_set(vec![]),
			vec![clip("Source"), clip("Output")],
		);
		let core = build_core(&inst);
		assert_eq!(core.effect_input, SOURCE_CLIP);
		assert!(core.get_input("Output").is_none());
		let source = core.get_input("Source").expect("Source 输入");
		assert_eq!(source.value_type, ValueType::Texture);
		assert_eq!(source.display_name, "Source");

		// 无 Source 但有 Texture → effect_input = Texture（不新建）。
		let inst = fake_instance(
			"test.clips.texture",
			ok_entry,
			param_set(vec![]),
			vec![clip("Texture"), clip("Output")],
		);
		let core = build_core(&inst);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_eq!(core.get_input("Texture").unwrap().display_name, "Texture");

		// 只有非标准 clip → 合成 Texture 输入。
		let inst = fake_instance(
			"test.clips.mask",
			ok_entry,
			param_set(vec![]),
			vec![clip("Mask"), clip("Output")],
		);
		let core = build_core(&inst);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		let tex = core.get_input(TEXTURE_INPUT).expect("合成 Texture 输入");
		assert_eq!(tex.value_type, ValueType::Texture);
		assert_eq!(tex.display_name, "Texture");
		assert!(core.get_input("Mask").is_some());
	}

	/// Color input properties (semantic/min/max/tooltip) and the combo option
	/// table (enum fallback, ChoiceOrder sorting, StrCombo value table).
	#[test]
	fn build_core_color_and_combo_property_tables() {
		let tint = ParamDef::new("tint", ofx::TYPE_RGB);
		tint.props.set_one(ofx::PROP_LABEL, text("Tint Color"));
		tint.props.set_one(ofx::P_HINT, text("Pick me"));
		tint.props
			.set_one(ofx::P_DISPLAY_MIN, PropValue::Double(0.0));
		tint.props
			.set_one(ofx::P_DISPLAY_MAX, PropValue::Double(1.0));

		let mode = ParamDef::new("mode", ofx::TYPE_CHOICE);
		mode.props
			.define(ofx::P_CHOICE_OPTION, vec![text("A"), text("B"), text("C")]);
		mode.props
			.define(ofx::P_CHOICE_ENUM, vec![text("a"), text("b"), text("c")]);
		mode.props.define(
			ofx::P_CHOICE_ORDER,
			vec![PropValue::Int(2), PropValue::Int(0), PropValue::Int(1)],
		);

		let labels_only = ParamDef::new("labels_only", ofx::TYPE_CHOICE);
		labels_only
			.props
			.define(ofx::P_CHOICE_OPTION, vec![text("OnlyA"), text("OnlyB")]);

		let sc = ParamDef::new("sc", ofx::TYPE_STRCHOICE);
		sc.props
			.define(ofx::P_CHOICE_ENUM, vec![text("v1"), text("v2")]);

		// 无 label + 默认值不等的 RGBA → 兜底 color，显示名回退参数 id。
		let plain = ParamDef::new("plain_rgb", ofx::TYPE_RGBA);
		plain.props.set_one(ofx::PROP_LABEL, text(""));
		plain.props.define(
			ofx::P_DEFAULT,
			vec![
				PropValue::Double(0.1),
				PropValue::Double(0.2),
				PropValue::Double(0.3),
				PropValue::Double(1.0),
			],
		);

		let inst = fake_instance(
			"test.color.combo",
			ok_entry,
			param_set(vec![tint, mode, labels_only, sc, plain]),
			vec![],
		);
		let core = build_core(&inst);

		let tint = core.get_input("tint").unwrap();
		assert_eq!(
			prop_values(tint, "color_semantic"),
			vec![NodeValue::Text("color".into())]
		);
		assert_eq!(prop_values(tint, "min"), vec![NodeValue::Float(0.0)]);
		assert_eq!(prop_values(tint, "max"), vec![NodeValue::Float(1.0)]);
		assert_eq!(
			prop_values(tint, "tooltip"),
			vec![NodeValue::Text("Pick me".into())]
		);

		let mode = core.get_input("mode").unwrap();
		assert_eq!(
			prop_values(mode, "combo_option"),
			vec![
				NodeValue::Text("B".into()),
				NodeValue::Text("C".into()),
				NodeValue::Text("A".into()),
			]
		);
		assert!(prop_values(mode, "combo_value").is_empty());

		// labels 有、enum 空 → values 回退 labels。
		let labels_only = core.get_input("labels_only").unwrap();
		assert_eq!(
			prop_values(labels_only, "combo_option"),
			vec![
				NodeValue::Text("OnlyA".into()),
				NodeValue::Text("OnlyB".into()),
			]
		);
		assert!(prop_values(labels_only, "combo_value").is_empty());

		// StrCombo：enum 回退 labels + combo_value 表。
		let sc = core.get_input("sc").unwrap();
		assert_eq!(
			prop_values(sc, "combo_option"),
			vec![NodeValue::Text("v1".into()), NodeValue::Text("v2".into()),]
		);
		assert_eq!(
			prop_values(sc, "combo_value"),
			vec![NodeValue::Text("v1".into()), NodeValue::Text("v2".into()),]
		);

		let plain = core.get_input("plain_rgb").unwrap();
		assert_eq!(plain.display_name, "plain_rgb");
		assert_eq!(
			prop_values(plain, "color_semantic"),
			vec![NodeValue::Text("color".into())]
		);
	}

	/// Breaks out of the loop when the parametric UI color table has less than one full dimension.
	#[test]
	fn build_core_parametric_partial_ui_colour() {
		let def = ParamDef::new("curve", ofx::TYPE_PARAMETRIC);
		def.props
			.set_one(ofx::P_PARAMETRIC_DIMENSION, PropValue::Int(2));
		def.props.define(
			ofx::P_PARAMETRIC_RANGE,
			vec![PropValue::Double(0.0), PropValue::Double(1.0)],
		);
		// 只够第 1 维（3 个 double）→ 第 2 维 break。
		def.props.define(
			ofx::P_PARAMETRIC_UI_COLOUR,
			vec![
				PropValue::Double(1.0),
				PropValue::Double(0.0),
				PropValue::Double(0.0),
			],
		);
		let inst = fake_instance(
			"test.parametric.partial",
			ok_entry,
			param_set(vec![def]),
			vec![],
		);
		let core = build_core(&inst);
		let input = core.get_input("curve").unwrap();
		assert_eq!(
			prop_values(input, "parametric_ui_colour"),
			vec![NodeValue::Color([1.0, 0.0, 0.0, 1.0])]
		);
		assert_eq!(
			prop_values(input, "parametric_dimension"),
			vec![NodeValue::Int(2)]
		);
		assert_eq!(
			prop_values(input, "parametric_range"),
			vec![NodeValue::Vec2([0.0, 1.0])]
		);
	}

	/// Display-name/description fallbacks and sub-category mapping.
	#[test]
	fn display_name_description_and_sub_category() {
		let inst = fake_instance("test.display", ok_entry, param_set(vec![]), vec![]);
		assert_eq!(plugin_display_name(&inst), "test.display");
		inst.plugin
			.descriptor
			.props
			.set_one(ofx::PROP_LABEL, text("Pretty"));
		assert_eq!(plugin_display_name(&inst), "Pretty");
		inst.plugin
			.descriptor
			.props
			.set_one(PROP_PLUGIN_DESCRIPTION, text("Desc"));
		assert_eq!(plugin_description(&inst), "Desc");
		// 未配置 → 空串。
		let bare = fake_instance("test.display.bare", ok_entry, param_set(vec![]), vec![]);
		assert_eq!(plugin_description(&bare), "");

		assert_eq!(sub_category_for("OfxImageEffectContextFilter"), "Filter");
		assert_eq!(
			sub_category_for("OfxImageEffectContextGenerator"),
			"Generator"
		);
		assert_eq!(
			sub_category_for("OfxImageEffectContextTransition"),
			"Transition"
		);
		assert_eq!(sub_category_for("OfxImageEffectContextGeneral"), "General");
		assert_eq!(sub_category_for("OfxImageEffectContextBogus"), "General");
	}

	/// set_text_param's type dispatch: String/StrChoice write-back, NUL rejection,
	/// non-text types and missing parameters silently ignored.
	#[test]
	fn set_text_param_type_dispatch_and_nul_rejection() {
		let s = ParamDef::new("s", ofx::TYPE_STRING);
		let sc = ParamDef::new("sc", ofx::TYPE_STRCHOICE);
		let d = ParamDef::new("d", ofx::TYPE_DOUBLE);
		let inst = fake_instance(
			"test.text.dispatch",
			ok_entry,
			param_set(vec![s, sc, d]),
			vec![],
		);

		// 查无参数 → no-op。
		set_text_param(&inst, "missing", "x");

		// String：NUL → 拒绝（值保持），有效 → 写回。
		let before = inst.params.find("s").unwrap().get();
		set_text_param(&inst, "s", "with\0nul");
		assert_eq!(inst.params.find("s").unwrap().get(), before);
		set_text_param(&inst, "s", "hello");
		assert!(matches!(
			inst.params.find("s").unwrap().get(),
			ParamValue::String(_)
		));

		// StrChoice：NUL → 拒绝，有效 → 写回。
		let before = inst.params.find("sc").unwrap().get();
		set_text_param(&inst, "sc", "with\0nul");
		assert_eq!(inst.params.find("sc").unwrap().get(), before);
		set_text_param(&inst, "sc", "pick");
		assert!(matches!(
			inst.params.find("sc").unwrap().get(),
			ParamValue::StrChoice(_)
		));

		// 非文本类型 → 静默忽略。
		let before = inst.params.find("d").unwrap().get();
		set_text_param(&inst, "d", "5");
		assert_eq!(inst.params.find("d").unwrap().get(), before);
	}

	/// Edge branches of the recording entry (empty action, non-instanceChanged,
	/// missing/mismatched change reason) — covered by calling the entry directly.
	#[test]
	fn push_button_recording_entry_edge_branches() {
		let _g = PUSH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// 空 action → 不记录。
		let rc = unsafe {
			recording_entry(
				std::ptr::null(),
				std::ptr::null(),
				std::ptr::null_mut(),
				std::ptr::null_mut(),
			)
		};
		assert_eq!(rc, 0);
		// 其它 action → 不记录。
		let other = CString::new("OfxActionSomethingElse").unwrap();
		let rc = unsafe {
			recording_entry(
				other.as_ptr(),
				std::ptr::null(),
				std::ptr::null_mut(),
				std::ptr::null_mut(),
			)
		};
		assert_eq!(rc, 0);

		// instanceChanged 但 inArgs 缺 reason → 记录空串。
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
		let changed = CString::new(crate::host::ACTION_INSTANCE_CHANGED).unwrap();
		let props = PropertySet::new();
		unsafe {
			recording_entry(
				changed.as_ptr(),
				std::ptr::null(),
				&props as *const PropertySet as *mut c_void,
				std::ptr::null_mut(),
			)
		};
		assert_eq!(
			*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()),
			vec!["OfxActionInstanceChanged".to_string(), String::new()]
		);

		// reason 类型不符 → 空串。
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
		props.set_one(crate::host::PROP_CHANGE_REASON, PropValue::Int(3));
		unsafe {
			recording_entry(
				changed.as_ptr(),
				std::ptr::null(),
				&props as *const PropertySet as *mut c_void,
				std::ptr::null_mut(),
			)
		};
		assert_eq!(
			*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()),
			vec!["OfxActionInstanceChanged".to_string(), String::new()]
		);
	}

	/// An instanceChanged failure in push_button_clicked only logs and still returns true.
	#[test]
	fn push_button_clicked_instance_changed_failure_returns_true() {
		unsafe extern "C" fn fail_changed(
			action: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> i32 {
			if action_is(action, crate::host::ACTION_INSTANCE_CHANGED) {
				crate::suites::status::ERR_FATAL
			} else {
				crate::suites::status::OK
			}
		}
		let button = ParamDef::new("button", ofx::TYPE_PUSHBUTTON);
		button.props.set_one(ofx::PROP_LABEL, text("Button"));
		let inst = fake_instance(
			"test.push.fail",
			fail_changed,
			param_set(vec![button]),
			vec![],
		);
		let id = register_fake(inst);
		assert!(push_button_clicked(id, "button"));
		unregister_instance(id);
	}

	/// execute_plugin_job: non-Plugin spec, unregistered instance, dummy/zero-size
	/// input, and the PushButton/None branches of parameter value dispatch.
	#[test]
	fn execute_plugin_job_spec_and_source_branches() {
		let inst = fake_instance("test.exec.branches", ok_entry, param_set(vec![]), vec![]);
		let id = register_fake(inst);

		fn plugin_spec(
			instance: u64,
			values: Vec<(String, NodeValue)>,
		) -> quercus_render::eval::JobSpec {
			quercus_render::eval::JobSpec::Plugin {
				instance,
				type_id: "org.quercus.test.exec".to_string(),
				time: 0.0,
				effect_input_id: Some(SOURCE_CLIP.to_string()),
				inputs: Vec::new(),
				values,
			}
		}

		// 非 Plugin spec → Invalid。
		let spec = quercus_render::eval::JobSpec::Generate;
		let req = quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::dummy(),
		};
		assert!(execute_plugin_job(&req).is_err());

		// 未登记实例 → Failed。
		let spec = plugin_spec(u64::MAX, Vec::new());
		let req = quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::dummy(),
		};
		assert!(execute_plugin_job(&req).is_err());

		// 登记实例 + dummy 源 → Failed。
		let spec = plugin_spec(id, Vec::new());
		let req = quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::dummy(),
		};
		assert!(execute_plugin_job(&req).is_err());

		// 登记实例 + 零尺寸（非 dummy）源 → generate_frame 失败。
		let pod = quercus_core::frame::VideoParamsPod {
			width: 1,
			height: 0,
			..Default::default()
		};
		let mut zero = quercus_core::texture::Frame::new();
		zero.set_video_params(pod);
		let spec = plugin_spec(id, Vec::new());
		let req = quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::wrap_frame(zero),
		};
		assert!(execute_plugin_job(&req).is_err());

		// 参数值分发：PushButton/None 跳过、数值走 POD、文本走 set_ofx；
		// 后续 render_frame 的结果不做断言（假插件没有真实渲染行为），
		// 这里只要求各分发分支被走到。
		let frame = quercus_render::eval::generate_frame(
			quercus_core::Rational::new(0, 1),
			(2, 2),
			quercus_core::PixelFormat::F32,
		)
		.expect("frame");
		let spec = plugin_spec(
			id,
			vec![
				("button".to_string(), NodeValue::PushButton),
				("none".to_string(), NodeValue::None),
				("gain".to_string(), NodeValue::Float(1.0)),
				("title".to_string(), NodeValue::Text("hello".to_string())),
			],
		);
		let req = quercus_render::eval::PluginJobRequest {
			spec: &spec,
			src: quercus_core::texture::Texture::wrap_frame(frame),
		};
		let _ = execute_plugin_job(&req);

		unregister_instance(id);
	}

	// ---- 真实测试插件（build.rs 产物）的宿主面覆盖 ----------------------

	/// Serialises host global state (real plugin scanning/instantiation).
	static HOST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn fixture_bundle_dir() -> Option<std::path::PathBuf> {
		let out = std::path::PathBuf::from(env!("OUT_DIR"));
		let lib = out.join(if cfg!(target_os = "macos") {
			"quercus_test_plugin.dylib"
		} else {
			"quercus_test_plugin.so"
		});
		if !lib.is_file() {
			return None;
		}
		let platform = if cfg!(target_os = "macos") {
			"MacOS"
		} else if cfg!(target_os = "windows") {
			"Win64"
		} else {
			"Linux-x86-64"
		};
		let dir = std::env::temp_dir().join(format!(
			"quercus-ofx-plugin-unit-fixture-{}",
			std::process::id()
		));
		let bin_dir = dir
			.join("quercus-test-plugin.ofx.bundle/Contents")
			.join(platform);
		std::fs::create_dir_all(&bin_dir).ok()?;
		let target = bin_dir.join(if cfg!(target_os = "windows") {
			"plugin.dll"
		} else {
			"plugin"
		});
		if !target.exists() {
			std::fs::copy(&lib, &target).ok()?;
		}
		Some(dir)
	}

	fn ensure_fixture_loaded() -> bool {
		let Some(dir) = fixture_bundle_dir() else {
			return false;
		};
		if Host::global().cache.scan_path(&dir).is_err() {
			return false;
		}
		Host::global()
			.cache
			.find("org.quercus.test-plugin")
			.is_some()
	}

	fn with_trace<R>(f: impl FnOnce() -> R) -> R {
		static TRACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let prev = std::env::var_os("QUERCUS_OFX_TRACE");
		std::env::set_var("QUERCUS_OFX_TRACE", "1");
		let out = f();
		match prev {
			Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
			None => std::env::remove_var("QUERCUS_OFX_TRACE"),
		}
		out
	}

	/// duplicate_instance: registry gets a new instance (real test plugin) + unknown handle → None.
	#[test]
	fn duplicate_instance_recreates_and_unknown_handle_is_none() {
		let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		if !ensure_fixture_loaded() {
			eprintln!("SKIP: 测试插件未构建");
			return;
		}
		let inst = Host::global()
			.create_instance(
				"org.quercus.test-plugin",
				Some("OfxImageEffectContextFilter"),
			)
			.expect("测试插件实例");
		let handle = PluginInstanceHandle(register_instance(inst));
		let dup = duplicate_instance(handle).expect("duplicate 应成功");
		assert_ne!(dup.0, handle.0);
		let dup_inst = instance_from_id(dup.0).expect("duplicate 已登记");
		assert_eq!(dup_inst.value.plugin.identifier, "org.quercus.test-plugin");
		assert_eq!(dup_inst.value.context, "OfxImageEffectContextFilter");
		unregister_instance(dup.0);
		assert!(duplicate_instance(PluginInstanceHandle(u64::MAX)).is_none());
	}

	/// shared_plugin_instance: first access creates and caches an instance, the
	/// next access hits the cache; unknown identifier → None.
	#[test]
	fn shared_plugin_instance_caches_and_misses() {
		let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		if !ensure_fixture_loaded() {
			eprintln!("SKIP: 测试插件未构建");
			return;
		}
		let first = shared_plugin_instance("org.quercus.test-plugin").expect("首访建实例");
		let second = shared_plugin_instance("org.quercus.test-plugin").expect("再访命中缓存");
		assert_eq!(first, second);
		assert!(shared_plugin_instance("com.example.no-such-plugin").is_none());
	}

	/// Runs the full render_real_plugin_smoke path with the test plugin produced by
	/// build.rs (including the QUERCUS_OFX_TRACE probe branches), no longer
	/// depending on a real plugin being installed on the machine.
	#[test]
	fn fixture_plugin_renders_through_the_executor() {
		let _g = HOST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		if !ensure_fixture_loaded() {
			eprintln!("SKIP: 测试插件未构建");
			return;
		}
		with_trace(|| render_real_plugin_smoke("org.quercus.test-plugin"));
	}
}
