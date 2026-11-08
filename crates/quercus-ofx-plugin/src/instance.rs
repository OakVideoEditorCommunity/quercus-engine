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

//! Plugin instance: the action-invocation surface.
//!
//! Corresponds to C++ `OlivePluginInstance`. Semantics follow:
//! HS: ofxhImageEffect.cpp `Instance` (action call sequencing and argument-set assembly).

use std::ffi::CString;
use std::sync::Arc;

use crate::clip::ClipInstance;
use crate::host::Plugin;
use crate::param::ParamSetInstance;
use crate::property::PropertySet;

/// Edit-transaction state for paramEditBegin/End (undo grouping). Corresponds
/// to `edit_depth_`/`edit_command_` in C++
/// oliveplugininstance.cpp: multiple parameter write-backs inside the
/// transaction merge into one multi command, which is redone as a whole and
/// released on editEnd.
#[repr(C)]
pub struct EditTransaction {
	/// Nesting depth (editBegin/End must be paired).
	depth: i32,
	/// The multi command accumulated by the transaction (None = no sub-command yet).
	multi: Option<quercus_undo::undocommand::UndoCommand>,
	/// Number of write-backs inside the transaction (label count).
	param_count: i32,
	/// Label of the first write-back.
	first_label: String,
}

impl EditTransaction {
	/// An empty transaction (no nesting, no accumulated command).
	pub fn new() -> Self {
		Self {
			depth: 0,
			multi: None,
			param_count: 0,
			first_label: String::new(),
		}
	}
}

/// A time range (OFX canonical double seconds).
#[derive(Clone, Copy, Debug, Default)]
pub struct OfxRangeD {
	/// Start (inclusive).
	pub min: f64,
	/// End (inclusive).
	pub max: f64,
}

/// A rectangle (canonical coordinates, double).
#[derive(Clone, Copy, Debug, Default)]
pub struct OfxRectD {
	/// Left.
	pub x1: f64,
	/// Top.
	pub y1: f64,
	/// Right.
	pub x2: f64,
	/// Bottom.
	pub y2: f64,
}

/// Rendering parameters such as pixel aspect and field order (the in_args
/// subset of the render action).
#[derive(Clone, Copy, Debug)]
pub struct RenderScale {
	/// Render scale on the x axis.
	pub x: f64,
	/// Render scale on the y axis.
	pub y: f64,
}

/// A plugin instance. `Arc<RefBox<Instance>>` owns the lifetime (`RefBox` is
/// the facade boundary type, see [`crate::handle`]); the param bridge looks
/// instances up by props address ([`crate::suites::param`]), and node binding
/// goes through the [`crate::node`] identity registry.
///
/// `#[repr(C)]` with props at offset 0 (handle convention, see
/// [`crate::suites::tag`]; during the instance lifetime the effect/param-set
/// handle is `&props`).
#[repr(C)]
pub struct Instance {
	/// Instance-level property set (offset 0, handle convention).
	pub props: PropertySet,
	/// The owning plugin.
	pub plugin: Arc<Plugin>,
	/// Context (kOfxImageEffectContext*).
	pub context: String,
	/// Parameter instance set (instantiated from the describe output after createInstance).
	pub params: ParamSetInstance,
	/// Clip instance set (including "Output"; `Box` keeps the handle addresses stable).
	pub clips: Vec<Box<ClipInstance>>,
	/// Bound quercusnode node identity (written by the param bridge; 0 = unbound).
	pub node_identity: std::sync::atomic::AtomicUsize,
	/// Whether the destroyInstance action has been announced (destruction
	/// idempotence gate; driven by `Drop`; public because host shutdown and
	/// tests both need to set it).
	pub destroyed: std::sync::atomic::AtomicBool,
	/// Time domain of beginSequenceRender (used by the timeline
	/// getTimeBounds; cleared by endSequenceRender).
	pub sequence_range: std::sync::Mutex<Option<OfxRangeD>>,
	/// Progress callback registered by the facade
	/// (`quercusplugin_instance_set_progress_cb`).
	pub progress_cb: std::sync::Mutex<Option<(crate::progress::ProgressFn, usize)>>,
	/// Facade cancellation flag (`quercusplugin_instance_cancel`).
	pub cancel: std::sync::atomic::AtomicBool,
	/// Edit transaction for paramEditBegin/End (undo grouping).
	pub edit: std::sync::Mutex<EditTransaction>,
	/// Instance-level render serialization (corresponds to C++
	/// `OlivePluginInstance::mutex`; the instance_lock at
	/// pluginrenderer.cpp:1436-1444 — the render path is not thread-safe, so
	/// concurrent renders must exclude each other).
	pub render_lock: std::sync::Mutex<()>,
	/// The associated interact (created by `new_interact`; lives as long as
	/// the instance and is destroyed along with it in `notify_destroy`).
	pub interact: std::sync::Mutex<Option<std::sync::Arc<crate::suites::interact::Interact>>>,
}

/// Instance destruction path: announce the destroyInstance action first, then
/// remove the param write-back registrations. (Triggered by `Drop` when
/// `Arc<RefBox<Instance>>` reaches zero — the action announcement must be
/// emitted before the object is destroyed.)
impl Drop for Instance {
	fn drop(&mut self) {
		if !self
			.destroyed
			.swap(true, std::sync::atomic::Ordering::Relaxed)
		{
			self.notify_destroy();
		}
		crate::suites::param::unregister_params_of(&self.props as *const _ as usize);
	}
}

impl Instance {
	/// Binds a quercusnode node (the Rust side of C++ `set_node_handle`;
	/// called by the facade/tests during assembly). `identity` is the
	/// quercusnode node identity returned by [`crate::node::register_node`]
	/// (the packed value of [`quercus_node::id::NodeId::identity`], resolved
	/// back through [`crate::node::node_from_identity`]); 0 unbinds.
	pub fn bind_node(&self, identity: usize) {
		self.node_identity
			.store(identity, std::sync::atomic::Ordering::Relaxed);
	}

	/// paramEditBegin: enters the edit transaction (nestable; the first entry
	/// resets the transaction state).
	pub fn edit_begin(&self) {
		let mut e = self.edit.lock().unwrap_or_else(|e| e.into_inner());
		e.depth += 1;
		if e.depth == 1 {
			e.multi = None;
			e.param_count = 0;
			e.first_label.clear();
		}
	}

	/// paramEditEnd: leaves the edit transaction; when the outermost level
	/// ends, the accumulated multi command is redone into effect and released
	/// (this crate has no undo stack — commands are submitted on the
	/// quercusundo side, see the fallback branch of C++
	/// `submit_undo_command`).
	pub fn edit_end(&self) {
		let mut e = self.edit.lock().unwrap_or_else(|e| e.into_inner());
		if e.depth > 0 {
			e.depth -= 1;
		}
		if e.depth == 0 {
			if let Some(mut multi) = e.multi.take() {
				multi.redo_now();
				// drop(multi)：释放 multi 与子命令（命令值语义）。
				drop(multi);
			}
			e.param_count = 0;
			e.first_label.clear();
		}
	}

	/// Whether an edit transaction is active (the param bridge consults this to
	/// decide whether to pack a write-back into the multi command).
	pub fn in_edit(&self) -> bool {
		self.edit.lock().unwrap_or_else(|e| e.into_inner()).depth > 0
	}

	/// Transaction-aware undo submission (used by the param bridge for
	/// write-backs; corresponds to C++
	/// oliveplugininstance.cpp:390-414 `submit_undo_command`): inside an edit
	/// transaction the command joins the multi (the sub-command is redone
	/// immediately, so the value is visible at once); otherwise the single
	/// command is redone and released.
	pub(crate) fn submit_undo_command(&self, mut cmd: quercus_undo::undocommand::UndoCommand, label: &str) {
		if self.in_edit() {
			let mut e = self.edit.lock().unwrap_or_else(|e| e.into_inner());
			if e.multi.is_none() {
				e.multi = Some(quercus_undo::undocommand::UndoCommand::multi());
			}
			e.param_count += 1;
			if e.first_label.is_empty() {
				e.first_label = label.to_string();
			}
			cmd.redo_now();
			e.multi
				.as_mut()
				.expect("multi 已在上面初始化")
				.multi_add_child(cmd);
		} else {
			cmd.redo_now();
			drop(cmd);
		}
	}

	/// getClipPreferences action, returning the negotiation result (the
	/// output clip's components/bit depth/pixel aspect/field and the frame
	/// rate). The negotiation order follows HS: ofxhImageEffect.cpp
	/// `Instance::getClipPreferences` line by line — the historical debugging
	/// hot spot, so the implementation must carry line-number comments.
	///
	/// Negotiation flow (HS: ofxhImageEffect.cpp:1686-1740):
	/// 1. Pre-set clipPrefsStuffs in the out args (frameRate=1, premult,
	///    fieldOrder, continuousSamples, frameVarying, HS:1697-1710);
	/// 2. Pre-set the per-clip "OfxImageClipPropComponents_<name>" etc.
	///    (defaults RGBA/Float/PAR=1, HS:1717-1727);
	/// 3. Call the action (the out args are the negotiation product,
	///    HS:1708-1718);
	/// 4. Feed back: the per-clip components/bit depth/pixel aspect are
	///    written back to the clip instance properties, and the output frame
	///    rate/fielding/premult are recorded on the instance (HS:1721-1740).
	pub fn get_clip_preferences(&self) -> crate::error::Result<ClipPreferences> {
		use crate::host::{
			ACTION_GET_CLIP_PREFERENCES, CLIP_PREF_COMPONENTS, CLIP_PREF_DEPTH, CLIP_PREF_PAR,
			PROP_CONTINUOUS_SAMPLES, PROP_FIELD_ORDER, PROP_FRAME_RATE, PROP_FRAME_VARYING,
			PROP_PREMULT,
		};
		use crate::property::Value;

		// 1. clipPrefsStuffs（HS:1697-1706 的默认值）。
		let out = PropertySet::new();
		out.set_one(PROP_FRAME_RATE, Value::Double(1.0));
		out.set_one(PROP_PREMULT, Value::String(CString::new("").unwrap()));
		out.set_one(PROP_FIELD_ORDER, Value::String(CString::new("").unwrap()));
		out.set_one(PROP_CONTINUOUS_SAMPLES, Value::Int(0));
		out.set_one(PROP_FRAME_VARYING, Value::Int(0));

		// 2. per-clip 预置（HS:1717-1727；phase 1 全链路 F32+RGBA）。
		for clip in &self.clips {
			out.set_one(
				&crate::host::clip_pref_prop(CLIP_PREF_COMPONENTS, &clip.name),
				Value::String(CString::new("OfxImageComponentRGBA").unwrap()),
			);
			out.set_one(
				&crate::host::clip_pref_prop(CLIP_PREF_DEPTH, &clip.name),
				Value::String(CString::new("OfxBitDepthFloat").unwrap()),
			);
			out.set_one(
				&crate::host::clip_pref_prop(CLIP_PREF_PAR, &clip.name),
				Value::Double(1.0),
			);
		}

		// 3. action。
		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let empty = PropertySet::new();
		Self::add_action_arg_compat_props(&empty);
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[probe-args] getClipPreferences in_args {:p} out {:p}", &empty, &out);
		}
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_GET_CLIP_PREFERENCES, inst_handle, &empty, &out)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!(
				"getClipPreferences 失败：{stat}"
			)));
		}

		// 4. 回灌（HS:1721-1740：每 clip 的协商结果写回实例属性）。
		let mut output_components = "OfxImageComponentRGBA".to_string();
		let mut output_bit_depth = "OfxBitDepthFloat".to_string();
		let mut output_par = 1.0;
		for clip in &self.clips {
			let comp_name = crate::host::clip_pref_prop(CLIP_PREF_COMPONENTS, &clip.name);
			let depth_name = crate::host::clip_pref_prop(CLIP_PREF_DEPTH, &clip.name);
			let par_name = crate::host::clip_pref_prop(CLIP_PREF_PAR, &clip.name);
			let comps = out
				.get(&comp_name, 0)
				.map(|v| match v {
					Value::String(s) => s.to_string_lossy().into_owned(),
					_ => "OfxImageComponentRGBA".to_string(),
				})
				.unwrap_or_else(|| "OfxImageComponentRGBA".to_string());
			let depth = out
				.get(&depth_name, 0)
				.map(|v| match v {
					Value::String(s) => s.to_string_lossy().into_owned(),
					_ => "OfxBitDepthFloat".to_string(),
				})
				.unwrap_or_else(|| "OfxBitDepthFloat".to_string());
			let par = out
				.get(&par_name, 0)
				.and_then(|v| match v {
					Value::Double(d) => Some(d),
					_ => None,
				})
				.unwrap_or(1.0);
			clip.props.set_one(
				crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
				Value::String(CString::new(comps.clone()).unwrap()),
			);
			clip.props.set_one(
				crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
				Value::String(CString::new(depth.clone()).unwrap()),
			);
			clip.props
				.set_one("OfxImagePropPixelAspectRatio", Value::Double(par));
			if clip.name == "Output" {
				output_components = comps;
				output_bit_depth = depth;
				output_par = par;
			}
		}

		let frame_rate = out
			.get(PROP_FRAME_RATE, 0)
			.and_then(|v| match v {
				Value::Double(d) => Some(d),
				_ => None,
			})
			.unwrap_or(24.0);
		let field = out
			.get(PROP_FIELD_ORDER, 0)
			.map(|v| match v {
				Value::String(s) => s.to_string_lossy().into_owned(),
				_ => String::new(),
			})
			.unwrap_or_default();

		Ok(ClipPreferences {
			output_components,
			output_bit_depth,
			pixel_aspect_ratio: output_par,
			frame_rate,
			field,
		})
	}

	/// getRegionOfDefinition action（HS: ofxhImageEffect.cpp:1087-1130：
	/// in args = time + renderScale；out args = RegionOfDefinition）。
	pub fn get_region_of_definition(
		&self,
		time: f64,
		scale: RenderScale,
	) -> crate::error::Result<OfxRectD> {
		use crate::host::{ACTION_GET_ROD, PROP_RENDER_SCALE, PROP_ROD, PROP_TIME};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.set_one(PROP_TIME, Value::Double(time));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(scale.x), Value::Double(scale.y)],
		);
		Self::add_action_arg_compat_props(&in_args);
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[probe-args] getRod in_args {:p}", &in_args);
		}
		// out args 预定义（HS 行为：宿主先建属性表，插件只管写——
		// 缺失属性上插件的 propSet 会失败被忽略）。
		let out = PropertySet::new();
		out.define(
			PROP_ROD,
			vec![
				Value::Double(0.0),
				Value::Double(0.0),
				Value::Double(0.0),
				Value::Double(0.0),
			],
		);

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_GET_ROD, inst_handle, &in_args, &out)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!("getRoD 失败：{stat}")));
		}
		read_rect(&out, PROP_ROD).ok_or(crate::error::Error::Failed("getRoD 无输出".into()))
	}

	/// `getRegionsOfInterest` action: asks the plugin for the region of
	/// interest each input clip needs.
	///
	/// `time` is the effect time (seconds), `scale` is the render scale (1/1
	/// for the main timeline), and `region` is the output region the host is
	/// currently requesting (pixel coordinates). The host pre-defines
	/// `kOfxImageEffectPropRegionOfInterest_<clip name>` in the out args,
	/// calls the plugin, then reads each clip's RoI in `self.clips` order
	/// (matching the `clips` order); when the plugin does not write a clip's
	/// property, OFX semantics fall back to the whole `region`.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the action returns
	/// anything other than OK/ReplyDefault (for example when the plugin does
	/// not know the action), with the status code in the message.
	pub fn get_regions_of_interest(
		&self,
		time: f64,
		scale: RenderScale,
		region: OfxRectD,
	) -> crate::error::Result<Vec<OfxRectD>> {
		use crate::host::{ACTION_GET_ROI, PROP_RENDER_SCALE, PROP_ROI, PROP_TIME};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.set_one(PROP_TIME, Value::Double(time));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(scale.x), Value::Double(scale.y)],
		);
		Self::add_action_arg_compat_props(&in_args);
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[probe-args] getRoi in_args {:p}", &in_args);
		}
		in_args.define(
			PROP_ROI,
			vec![
				Value::Double(region.x1),
				Value::Double(region.y1),
				Value::Double(region.x2),
				Value::Double(region.y2),
			],
		);
		// out args 预定义 per-clip ROI 属性（HS 约定名前缀）。
		let out = PropertySet::new();
		for clip in &self.clips {
			let name = format!("OfxImageEffectPropRegionOfInterest_{}", clip.name);
			out.define(
				&name,
				vec![
					Value::Double(0.0),
					Value::Double(0.0),
					Value::Double(0.0),
					Value::Double(0.0),
				],
			);
		}

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_GET_ROI, inst_handle, &in_args, &out)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!("getRoI 失败：{stat}")));
		}
		let mut rois = Vec::with_capacity(self.clips.len());
		for clip in &self.clips {
			let name = format!("OfxImageEffectPropRegionOfInterest_{}", clip.name);
			let rect = read_rect(&out, &name)
				// 未设置 → 默认整个 region（OFX 语义：插件可不写）。
				.unwrap_or(region);
			rois.push(rect);
		}
		Ok(rois)
	}

	/// isIdentity action: returns Some((time, input_clip_name)) meaning this
	/// frame. Properties that the OFX 1.4+/Natron frameworks strongly read on
	/// action in-args: when missing they are thrown as PropertyUnknownToHost
	/// → MissingHostFeature (the root cause of purple frames). Every action
	/// in-args carries it.
	fn add_action_arg_compat_props(in_args: &PropertySet) {
		use crate::property::Value;
		// NatronOfxPropNativeOverlays：Natron 支持库强读（0 = 宿主不
		// 支持原生 overlay，插件走自己的回退）。OfxImageEffectPropRenderPlanes
		// 不放 in-args：Natron 框架按**字符串**读它，存在但类型是 Int
		// 反而会抛 PropertyUnknownToHost（缺失它才容忍）——它只属于
		// 图像/clip 属性（OFX 1.4 语义在 clipGetImagePlane 一侧）。
		in_args.set_one("NatronOfxPropNativeOverlays", Value::Int(0));
	}

	/// Passes the input clip through directly; None means a real render is
	/// needed.
	///
	/// Follows HS: ofxhImageEffect.cpp:1378-1450: in args = time + scale +
	/// renderWindow + fieldToRender; out args =
	/// kOfxImageEffectPropIsIdentity (the input clip name) + kOfxPropTime
	/// (the pass-through time, which may be changed).
	pub fn is_identity(&self, time: f64) -> crate::error::Result<Option<(f64, String)>> {
		use crate::host::{
			ACTION_IS_IDENTITY, PROP_FIELD_TO_RENDER, PROP_IS_IDENTITY, PROP_RENDER_SCALE,
			PROP_RENDER_WINDOW, PROP_TIME,
		};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.set_one(PROP_TIME, Value::Double(time));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.define(
			PROP_RENDER_WINDOW,
			vec![
				Value::Int(0),
				Value::Int(0),
				Value::Int(0),
				Value::Int(0),
			],
		);
		in_args.set_one(
			PROP_FIELD_TO_RENDER,
			Value::String(CString::new("OfxFieldNone").unwrap()),
		);
		in_args.set_one("NatronOfxPropNativeOverlays", Value::Int(0));
		// out args 预定义：IsIdentity（String）+ Time（Double）。
		let out = PropertySet::new();
		out.set_one(PROP_IS_IDENTITY, Value::String(CString::new("").unwrap()));
		out.set_one(PROP_TIME, Value::Double(time));

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[ofx] isIdentity in_args {:p} out_args {:p}", &in_args, &out);
		}
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_IS_IDENTITY, inst_handle, &in_args, &out)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			// A failed isIdentity is NOT fatal: the C++ plugin renderer
			// never calls this action at all (plugins that error on it —
			// the CImg suite answers MissingHostFeature — simply render
			// normally). Treat any non-OK as "not identity" and let the
			// real render decide.
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
				eprintln!("[ofx] isIdentity stat {stat}: proceeding to render");
			}
			return Ok(None);
		}
		let identity = out.get(PROP_IS_IDENTITY, 0);
		match identity {
			Some(Value::String(s)) if !s.is_empty() => {
				let clip_name = s.to_string_lossy().into_owned();
				let t = out
					.get(PROP_TIME, 0)
					.and_then(|v| match v {
						Value::Double(d) => Some(d),
						_ => None,
					})
					.unwrap_or(time);
				Ok(Some((t, clip_name)))
			}
			_ => Ok(None),
		}
	}

	/// Assembles the in args for the render action (shared by the CPU/GL
	/// paths).
	///
	/// `time` is the effect time (seconds), `scale` is the render scale
	/// (x/y), and `window` is the render window in pixel coordinates
	/// (rounded to `Int×4` — the support library type-checks the render
	/// in-args, and a Double would fail validation); when `gl_enabled` is
	/// true, `kOfxImageEffectPropOpenGLEnabled=1` is written as well
	/// (ofxGPURender.h:117).
	///
	/// Fixed flags such as sequentialRender/interactive/draft and the Natron
	/// compatibility property (`NatronOfxPropNativeOverlays=0`) are filled in
	/// too, so [`Instance::render`]/[`Instance::render_gl`] can hand the args
	/// straight to the plugin.
	fn render_in_args(
		time: f64,
		scale: RenderScale,
		window: OfxRectD,
		gl_enabled: bool,
	) -> PropertySet {
		use crate::host::{
			PROP_FIELD_TO_RENDER, PROP_INTERACTIVE_RENDER, PROP_NO_SPATIAL_AWARENESS,
			PROP_RENDER_QUALITY_DRAFT, PROP_RENDER_SCALE, PROP_RENDER_WINDOW,
			PROP_SEQUENTIAL_RENDER, PROP_TIME,
		};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.set_one(PROP_TIME, Value::Double(time));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(scale.x), Value::Double(scale.y)],
		);
		// RenderWindow 必须是 Int x4（支持库 ofxsPropertyValidation 对
		// render in-args 的强制类型；像素坐标本就是整型——Double 会校验
		// 失败成 kOfxStatFailed）。
		in_args.define(
			PROP_RENDER_WINDOW,
			vec![
				Value::Int(window.x1.round() as i32),
				Value::Int(window.y1.round() as i32),
				Value::Int(window.x2.round() as i32),
				Value::Int(window.y2.round() as i32),
			],
		);
		in_args.set_one(
			PROP_FIELD_TO_RENDER,
			Value::String(CString::new("OfxFieldNone").unwrap()),
		);
		in_args.set_one(PROP_SEQUENTIAL_RENDER, Value::Int(0));
		in_args.set_one(PROP_INTERACTIVE_RENDER, Value::Int(0));
		in_args.set_one(PROP_RENDER_QUALITY_DRAFT, Value::Int(0));
		in_args.set_one(PROP_NO_SPATIAL_AWARENESS, Value::Int(0));
		// Natron 支持库对 NatronOfxPropNativeOverlays 的强读（0 = 宿主
		// 不支持原生 overlay——插件据此走自己的回退路径）。
		in_args.set_one("NatronOfxPropNativeOverlays", Value::Int(0));
		if gl_enabled {
			in_args.set_one(crate::host::PROP_GL_ENABLED, Value::Int(1));
		}
		in_args
	}

	/// render action (CPU path). `output` is the output image already
	/// allocated in the negotiated format (Arc: the plugin obtains it via
	/// clipGetImage(Output), and the render driver and the suite each hold a
	/// strong reference); the input clips' images are prepared by the caller
	/// through [`ClipInstance::fetch_image`]. For the GL path see
	/// [`Instance::render_gl`].
	///
	/// The signature was originally `&mut Image`: the output image has to be
	/// shared with the suite as a strong reference (TLS + the LIVE_IMAGES
	/// table), which `&mut` cannot express — hence `Arc<Image>`.
	pub fn render(
		&self,
		time: f64,
		scale: RenderScale,
		window: OfxRectD,
		output: std::sync::Arc<crate::image::Image>,
	) -> crate::error::Result<()> {
		use crate::host::ACTION_RENDER;

		let in_args = Self::render_in_args(time, scale, window, false);

		// facade 取消标记：render 入口即短路（多帧循环的帧间取消）。
		if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
			return Err(crate::error::Error::Failed("已取消".into()));
		}

		// 渲染上下文（TLS）：timeline/progress/clipGetImage 读取。
		let range = self
			.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.unwrap_or(OfxRangeD { min: 0.0, max: 0.0 });
		crate::suites::set_render_ctx(Some(crate::suites::RenderCtx { time, scale, range }));
		crate::suites::set_current_output(Some(output.clone()));

		// 进度报告器（facade 回调优先；无回调而 app 注册了 UI 工厂
		// 时装静默报告器，progressStart 再经工厂现造 UI 报告器）。
		if let Some((cb, userdata)) = *self
			.progress_cb
			.lock()
			.unwrap_or_else(|e| e.into_inner())
		{
			crate::suites::progress::set_current(Some(unsafe {
				crate::progress::ProgressReporter::new(cb, userdata as *mut std::ffi::c_void)
			}));
		} else if crate::progress::has_reporter_factory() {
			crate::suites::progress::set_current(Some(
				crate::progress::ProgressReporter::silent(),
			));
		}

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let out = PropertySet::new();
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_RENDER, inst_handle, &in_args, &out)
		};

		// 清理 TLS（action 返回后渲染上下文必须消失）。
		crate::suites::set_current_output(None);
		crate::suites::set_render_ctx(None);
		crate::suites::progress::set_current(None);

		if stat != crate::suites::status::OK {
			return Err(crate::error::Error::Failed(format!("render 失败：{stat}")));
		}
		Ok(())
	}

	/// GL render action (M11 §4; the GL mode of ofxGPURender.h's Render
	/// action). Coexists with [`Instance::render`] (CPU).
	///
	/// Preconditions (honored by the render driver): the caller has already
	/// made the `renderer`'s GL context current (ofxGPURender.h "OpenGL
	/// Current Context": the host only requires a current context during
	/// Render/Begin/EndSequenceRender/Attach/Detach — this implementation's
	/// convention is that quercusrender's PluginJob path does so on entry),
	/// and `output_texture` is already attached as the renderer's output
	/// target (equivalent to C++ `PluginRenderer::attach_output_texture`).
	/// `output_gl_texture` is the real GL texture name the host created for
	/// the output frame (in GL mode clipLoadTexture(Output)'s
	/// OpenGLTextureIndex uses it); None = CPU fallback semantics.
	///
	/// Action sequence: kOfxActionOpenGLContextAttached → render (with
	/// kOfxImageEffectPropOpenGLEnabled=1 in the in args) →
	/// kOfxActionOpenGLContextDetached (ofxGPURender.h:345-371; attach and
	/// detach must be paired). The render result stays on the GL output
	/// texture (the plugin draws straight into the attached target) and the
	/// host reads it back via glReadPixels (done by the render driver, see
	/// [`crate::gl_bridge`]); in GL mode clipGetImage(Output) is unavailable
	/// (per the spec the plugin goes through the OpenGL suite —
	/// ofxGPURender.h "the effect SHOULD access all its images through
	/// the OpenGL suite"). Before render returns, unreleased input GL
	/// textures are given a last-chance cleanup
	/// ([`crate::suites::gl_render::purge_leftovers`]).
	pub fn render_gl(
		&self,
		time: f64,
		scale: RenderScale,
		window: OfxRectD,
		renderer: crate::render::Renderer,
		output_texture: crate::render::Texture,
		output_gl_texture: Option<i32>,
	) -> crate::error::Result<()> {
		use crate::host::ACTION_RENDER;

		let in_args = Self::render_in_args(time, scale, window, true);

		if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
			return Err(crate::error::Error::Failed("已取消".into()));
		}

		let range = self
			.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.unwrap_or(OfxRangeD { min: 0.0, max: 0.0 });
		crate::suites::set_render_ctx(Some(crate::suites::RenderCtx { time, scale, range }));
		// GL 纹理位深协商（插件 kOfxOpenGLPropPixelDepth → 管线 F32；
		// 调用方已按协商门（render 驱动）决定走 GL）。
		let gl_pixel_depth =
			crate::suites::gl_render::pick_gl_pixel_depth(&self.plugin.descriptor.props)
				.unwrap_or("OfxBitDepthFloat");
		crate::suites::set_gl_ctx(Some(crate::suites::GlCtx {
			renderer,
			output_texture,
			gl_pixel_depth,
			output_gl_texture,
		}));
		// GL 模式无 CPU 输出图像（current_output 保持 None）。

		if let Some((cb, userdata)) = *self
			.progress_cb
			.lock()
			.unwrap_or_else(|e| e.into_inner())
		{
			crate::suites::progress::set_current(Some(unsafe {
				crate::progress::ProgressReporter::new(cb, userdata as *mut std::ffi::c_void)
			}));
		} else if crate::progress::has_reporter_factory() {
			crate::suites::progress::set_current(Some(
				crate::progress::ProgressReporter::silent(),
			));
		}

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let empty = PropertySet::new();
		// attach（失败仍继续——插件可忽略；规范允许 ReplyDefault）。
		let stat = unsafe {
			self.plugin.call_action(
				crate::host::ACTION_GL_CONTEXT_ATTACHED,
				inst_handle,
				&empty,
				&empty,
			)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			crate::suites::set_gl_ctx(None);
			crate::suites::set_render_ctx(None);
			crate::suites::progress::set_current(None);
			return Err(crate::error::Error::Failed(format!(
				"OpenGLContextAttached 失败：{stat}"
			)));
		}

		let out = PropertySet::new();
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_RENDER, inst_handle, &in_args, &out)
		};

		// detach（必须与 attach 配对；ofxGPURender.h:345-346）。
		unsafe {
			self.plugin.call_action(
				crate::host::ACTION_GL_CONTEXT_DETACHED,
				inst_handle,
				&empty,
				&empty,
			)
		};

		// 兜底：插件遗漏 clipFreeTexture 的输入纹理在此释放。
		crate::suites::gl_render::purge_leftovers();
		crate::suites::set_gl_ctx(None);
		crate::suites::set_render_ctx(None);
		crate::suites::progress::set_current(None);

		if stat != crate::suites::status::OK {
			return Err(crate::error::Error::Failed(format!(
				"GL render 失败：{stat}"
			)));
		}
		Ok(())
	}

	/// GetOutputColourspace action (M11 §4; ofxColour.h:243-283): the host
	/// gives the plugin a list of preferred colour spaces and the plugin
	/// writes back the colour space of the output clip (which may be an
	/// "OfxColourspace_<clip>" cross-reference).
	///
	/// Returns the resolved output colour space: a cross-reference resolves
	/// to the referenced clip's actual colour space (the cross-clip reference
	/// convention of ofxColour.h:142-141); REPLY_DEFAULT → the first input
	/// clip's colour space (ofxColour.h:278-279). The caller (render driver)
	/// then writes the result back to the output clip
	/// ([`Instance::set_output_colourspace`]).
	pub fn get_output_colourspace(&self, preferred: &[String]) -> crate::error::Result<String> {
		use crate::host::{
			ACTION_GET_OUTPUT_COLOURSPACE, PROP_CLIP_COLOURSPACE, PROP_CLIP_PREFERRED_COLOURSPACES,
		};
		use crate::property::Value;

		let in_args = PropertySet::new();
		let values: Vec<Value> = preferred
			.iter()
			.map(|s| Value::String(CString::new(s.as_str()).unwrap()))
			.collect();
		in_args.define(PROP_CLIP_PREFERRED_COLOURSPACES, values);
		let out = PropertySet::new();
		out.set_one(
			PROP_CLIP_COLOURSPACE,
			Value::String(CString::new("").unwrap()),
		);

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_GET_OUTPUT_COLOURSPACE, inst_handle, &in_args, &out)
		};
		if stat == crate::suites::status::OK {
			let value = out
				.get(PROP_CLIP_COLOURSPACE, 0)
				.map(|v| match v {
					Value::String(s) => s.to_string_lossy().into_owned(),
					_ => String::new(),
				})
				.unwrap_or_default();
			if value.is_empty() {
				return Err(crate::error::Error::Failed(
					"GetOutputColourspace 未回写色彩空间".into(),
				));
			}
			Ok(self.resolve_colourspace(value))
		} else if stat == crate::suites::status::REPLY_DEFAULT {
			// 插件未实现 → 用第一个输入 clip 的色彩空间（规范默认）。
			let first_input = self
				.clips
				.iter()
				.find(|c| c.name != "Output")
				.map(|c| {
					c.props
						.get(PROP_CLIP_COLOURSPACE, 0)
						.map(|v| match v {
							Value::String(s) => s.to_string_lossy().into_owned(),
							_ => String::new(),
						})
						.unwrap_or_default()
				})
				.unwrap_or_default();
			Ok(first_input)
		} else {
			Err(crate::error::Error::Failed(format!(
				"GetOutputColourspace 失败：{stat}"
			)))
		}
	}

	/// Writes the output clip's colour space back (the landing point after
	/// GetOutputColourspace; ofxColour.h:262-264 "the host must set
	/// kOfxImageClipPropColourspace on the instance's output clip to the
	/// value from outArgs").
	pub fn set_output_colourspace(&self, colourspace: &str) {
		use crate::property::Value;
		if let Some(output) = self.clips.iter().find(|c| c.name == "Output") {
			output.props.set_one(
				crate::host::PROP_CLIP_COLOURSPACE,
				Value::String(CString::new(colourspace).unwrap()),
			);
		}
	}

	/// Resolves an `OfxColourspace_<clip>` cross-reference to the referenced
	/// clip's actual colour space.
	///
	/// If `value` starts with that prefix, the clip is looked up by name in
	/// `self.clips` and its `OfxImageEffectPropColourspace` property is
	/// returned as a string; when the clip does not exist or the property is
	/// not a string, `value` is returned unchanged. A non-cross-reference is
	/// returned unchanged as well. The consumer is the clip preferences
	/// negotiation path that lands the plugin-declared colour space on a
	/// concrete clip.
	pub fn resolve_colourspace(&self, value: String) -> String {
		const PREFIX: &str = "OfxColourspace_";
		if let Some(clip_name) = value.strip_prefix(PREFIX) {
			self.clips
				.iter()
				.find(|c| c.name == clip_name)
				.map(|c| {
					c.props
						.get(crate::host::PROP_CLIP_COLOURSPACE, 0)
						.map(|v| match v {
							crate::property::Value::String(s) => s.to_string_lossy().into_owned(),
							_ => value.clone(),
						})
						.unwrap_or(value.clone())
				})
				.unwrap_or(value)
		} else {
			value
		}
	}

	/// begin/endSequenceRender pair (the opening/closing brackets of a frame
	/// sequence render; in args per HS: ofxhImageEffect.cpp:473-513:
	/// frameRange + frameStep + renderScale + sequentialRenderStatus). The
	/// GL-mode variant [`Instance::begin_sequence_render_gl`] additionally
	/// carries kOfxImageEffectPropOpenGLEnabled (ofxGPURender.h:117).
	pub fn begin_sequence_render(&self, range: OfxRangeD) -> crate::error::Result<()> {
		self.begin_sequence_inner(range, false)
	}

	/// `beginSequenceRender` in GL mode.
	///
	/// Same as [`Instance::begin_sequence_render`], except that the in args
	/// additionally carry `kOfxImageEffectPropOpenGLEnabled=1`
	/// (ofxGPURender.h:117) for pairing around the GL render path. `range`
	/// is the `[min, max]` frame range of this sequence render.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK.
	pub fn begin_sequence_render_gl(&self, range: OfxRangeD) -> crate::error::Result<()> {
		self.begin_sequence_inner(range, true)
	}

	/// Common implementation of `beginSequenceRender` (shared by CPU/GL).
	///
	/// First assembles the in args from `range` and records the range in
	/// `sequence_range` (for pairing with
	/// [`Instance::end_sequence_render`]), then sends the begin action to the
	/// plugin.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK; `sequence_range` has already been written at
	/// that point, so the caller should still invoke the matching end to
	/// clean up.
	fn begin_sequence_inner(&self, range: OfxRangeD, gl_enabled: bool) -> crate::error::Result<()> {
		use crate::host::ACTION_BEGIN_SEQUENCE;

		let in_args = Self::sequence_in_args(range, gl_enabled);
		self.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.replace(range);

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let out = PropertySet::new();
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_BEGIN_SEQUENCE, inst_handle, &in_args, &out)
		};
		if stat != crate::suites::status::OK {
			return Err(crate::error::Error::Failed(format!(
				"beginSequenceRender 失败：{stat}"
			)));
		}
		Ok(())
	}

	/// Assembles the in args shared by begin/endSequenceRender.
	///
	/// `range` is the `[min, max]` frame range of the sequence render
	/// (written to `kOfxImageEffectPropFrameRange`, Double×2); when
	/// `gl_enabled` is true, `kOfxImageEffectPropOpenGLEnabled=1` is added.
	/// frameStep is fixed at 1.0, renderScale at 1/1 and sequentialRender at
	/// 1 (the in args contract of HS: ofxhImageEffect.cpp:473-513).
	fn sequence_in_args(range: OfxRangeD, gl_enabled: bool) -> PropertySet {
		use crate::host::{
			PROP_FRAME_RANGE, PROP_FRAME_STEP, PROP_RENDER_SCALE, PROP_SEQUENTIAL_RENDER,
		};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.define(
			PROP_FRAME_RANGE,
			vec![Value::Double(range.min), Value::Double(range.max)],
		);
		in_args.set_one(PROP_FRAME_STEP, Value::Double(1.0));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.set_one(PROP_SEQUENTIAL_RENDER, Value::Int(1));
		if gl_enabled {
			in_args.set_one(crate::host::PROP_GL_ENABLED, Value::Int(1));
		}
		in_args
	}

	/// Ends the sequence-render bracket opened by
	/// [`Instance::begin_sequence_render`] (CPU path).
	///
	/// `range` should be the same as at begin time and is used to assemble
	/// the end in args; whatever the plugin returns, the recorded
	/// `sequence_range` is cleared.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK.
	pub fn end_sequence_render(&self, range: OfxRangeD) -> crate::error::Result<()> {
		self.end_sequence_inner(range, false)
	}

	/// `endSequenceRender` in GL mode.
	///
	/// Same as [`Instance::end_sequence_render`], except that the in args
	/// additionally carry `kOfxImageEffectPropOpenGLEnabled=1`
	/// (ofxGPURender.h:117); `range` should match the GL begin.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK.
	pub fn end_sequence_render_gl(&self, range: OfxRangeD) -> crate::error::Result<()> {
		self.end_sequence_inner(range, true)
	}

	/// Common implementation of `endSequenceRender` (shared by CPU/GL).
	///
	/// Assembles the in args from `range` and sends the end action, then
	/// clears the recorded `sequence_range` unconditionally so the next begin
	/// cannot misjudge the pairing.
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK.
	fn end_sequence_inner(&self, range: OfxRangeD, gl_enabled: bool) -> crate::error::Result<()> {
		use crate::host::ACTION_END_SEQUENCE;

		let in_args = Self::sequence_in_args(range, gl_enabled);

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let out = PropertySet::new();
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_END_SEQUENCE, inst_handle, &in_args, &out)
		};
		self.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.take();
		if stat != crate::suites::status::OK {
			return Err(crate::error::Error::Failed(format!(
				"endSequenceRender 失败：{stat}"
			)));
		}
		Ok(())
	}

	/// Host-side dispatch of `kOfxActionInstanceChanged` (the inArgs contract
	/// of ofxCore.h:405-435).
	///
	/// `param_name` is the name of the changed parameter; `reason` determines
	/// `kOfxPropChangeReason` (UserEdited / PluginEdited / Time); `time` is
	/// the effect time at which the change happened (seconds); `scale` is the
	/// current render scale (x/y). The in args additionally carry
	/// `kOfxPropType=kOfxTypeParameter`. After the host presses a push
	/// button it calls this with UserEdited (a push button has no value, so
	/// the plugin's reaction lives entirely in instanceChanged; real plugins
	/// such as CImg rely on this action to refresh their internal state).
	///
	/// # Errors
	///
	/// Returns [`crate::error::Error::Failed`] when the plugin returns a
	/// status other than OK/ReplyDefault.
	pub fn instance_changed(
		&self,
		param_name: &str,
		reason: crate::param::ChangeReason,
		time: f64,
		scale: RenderScale,
	) -> crate::error::Result<()> {
		use crate::host::{
			ACTION_INSTANCE_CHANGED, CHANGE_PLUGIN_EDITED, CHANGE_TIME, CHANGE_USER_EDITED,
			PROP_CHANGE_REASON, PROP_RENDER_SCALE, PROP_TIME,
		};
		use crate::property::Value;

		let in_args = PropertySet::new();
		in_args.set_one(
			crate::param::PROP_TYPE,
			Value::String(CString::new(crate::param::TYPE_PARAMETER).unwrap()),
		);
		in_args.set_one(
			crate::param::PROP_NAME,
			Value::String(CString::new(param_name).unwrap()),
		);
		let reason_str = match reason {
			crate::param::ChangeReason::UserEdited => CHANGE_USER_EDITED,
			crate::param::ChangeReason::PluginEdited => CHANGE_PLUGIN_EDITED,
			crate::param::ChangeReason::TimeChanged => CHANGE_TIME,
		};
		in_args.set_one(
			PROP_CHANGE_REASON,
			Value::String(CString::new(reason_str).unwrap()),
		);
		in_args.set_one(PROP_TIME, Value::Double(time));
		in_args.define(
			PROP_RENDER_SCALE,
			vec![Value::Double(scale.x), Value::Double(scale.y)],
		);

		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let out = PropertySet::new();
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_INSTANCE_CHANGED, inst_handle, &in_args, &out)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!(
				"instanceChanged 失败：{stat}"
			)));
		}
		Ok(())
	}

	/// Creates the associated interact (the UI-event host in the main
	/// process; `new_interact(instance)` from the task contract). It coexists
	/// with the render instance in the worker process — OFX allows multiple
	/// instances of the same plugin, and the interact is an independent
	/// object (independent handle), distinguished from the effect instance by
	/// [`crate::suites::interact::Interact::handle`].
	///
	/// Flow: the plugin-declared overlay interact entry (V2 preferred, then
	/// V1; ofxImageEffect.h:825/812) becomes the interact's entry — when it
	/// is not declared, the plugin's main entry is used (task contract) →
	/// create the [`Interact`] (property set + tagged handle) → send
	/// `kOfxActionNewInteract` to the entry.
	///
	/// Return value:
	/// - `Some(interact)`: creation succeeded (NewInteract returned OK, or
	///   the plugin does not implement NewInteract but declares an overlay
	///   interact — real overlay plugins do not know NewInteract and go
	///   through the describe/create sequence).
	/// - `None`: the plugin has no interact (NewInteract returned
	///   kOfxStatReplyDefault and no overlay entry is declared) or returned
	///   an error status.
	///
	/// After creation the caller continues with [`Interact::describe`] →
	/// [`Interact::create_instance`] to finish instantiation; destruction
	/// follows the instance automatically ([`Instance::notify_destroy`]).
	pub fn new_interact(&self) -> Option<std::sync::Arc<crate::suites::interact::Interact>> {
		use crate::host::overlay_interact_entry;
		use crate::suites::interact::Interact;

		let overlay = overlay_interact_entry(&self.plugin.descriptor.props);
		let entry = overlay.unwrap_or(self.plugin.entry);
		let has_overlay = overlay.is_some();

		let effect_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let interact = Interact::new(self.plugin.clone(), entry, effect_handle);

		let empty = PropertySet::new();
		let st = interact.call(crate::host::ACTION_NEW_INTERACT, &empty, &empty);
		let accepted = match st {
			crate::suites::status::OK => true,
			// 插件未实现 NewInteract 但声明了 overlay interact → 仍创建
			//（走 describe/create 的官方序列）。
			crate::suites::status::REPLY_DEFAULT if has_overlay => true,
			_ => false,
		};
		if !accepted {
			return None;
		}
		*self
			.interact
			.lock()
			.unwrap_or_else(|e| e.into_inner()) = Some(interact.clone());
		Some(interact)
	}

	/// Returns the already-created interact (the UI-event host in the main
	/// process).
	///
	/// Registered by [`Instance::new_interact`] after a successful creation;
	/// returns `None` when it has not been created yet, creation failed, or
	/// it has already been taken by [`Instance::notify_destroy`]. The
	/// returned `Arc` clone can be held safely by the caller.
	pub fn interact(&self) -> Option<std::sync::Arc<crate::suites::interact::Interact>> {
		self.interact
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
	}

	/// describe_interact (task contract): sends `kOfxActionDescribe`
	/// (kOfxActionDescribeInteract) to the already-created interact. Not
	/// created → kOfxStatErrBadHandle.
	pub fn describe_interact(&self) -> i32 {
		match self.interact() {
			Some(i) => i.describe(),
			None => crate::suites::status::ERR_BAD_HANDLE,
		}
	}

	/// Destruction (the destroyInstance action). Teardown is driven by
	/// `Arc<RefBox<Instance>>` reaching zero; this only announces the
	/// action, idempotently (the `destroyed` gate in [`Instance::drop`]
	/// guarantees a single send).
	pub(crate) fn notify_destroy(&self) {
		use crate::host::ACTION_DESTROY_INSTANCE;
		let inst_handle = crate::suites::tag::make(
			&self.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let empty = PropertySet::new();
		// interact 与实例同生命周期：实例销毁前先销毁 interact
		//（ofxInteract.h kOfxActionDestroyInstanceInteract 的 \pre 要求
		// 实例成员尚未销毁）。
		if let Some(i) = self
			.interact
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.take()
		{
			i.destroy();
		}
		// 通知失败只记日志（销毁路径不可回滚）。
		let stat = unsafe {
			self.plugin
				.call_action(ACTION_DESTROY_INSTANCE, inst_handle, &empty, &empty)
		};
		if stat != crate::suites::status::OK && stat != crate::suites::status::REPLY_DEFAULT {
			eprintln!("destroyInstance 通知失败：{stat}");
		}
	}
}

/// Reads an `OfxRectD` rectangle from a property set (as the Double
/// components x1, y1, x2, y2).
///
/// The property `name` must exist and its first four components must all be
/// `Value::Double`, otherwise `None` is returned (a type mismatch is handled
/// the same as a missing property). Used to parse the out args of
/// `getRoD`/`getRegionsOfInterest`.
fn read_rect(props: &PropertySet, name: &str) -> Option<OfxRectD> {
	use crate::property::Value;
	let x1 = match props.get(name, 0)? {
		Value::Double(d) => d,
		_ => return None,
	};
	let y1 = match props.get(name, 1)? {
		Value::Double(d) => d,
		_ => return None,
	};
	let x2 = match props.get(name, 2)? {
		Value::Double(d) => d,
		_ => return None,
	};
	let y2 = match props.get(name, 3)? {
		Value::Double(d) => d,
		_ => return None,
	};
	Some(OfxRectD { x1, y1, x2, y2 })
}

/// The negotiation result of getClipPreferences.
#[derive(Clone, Debug)]
pub struct ClipPreferences {
	/// Output components (kOfxImageComponentRGBA etc.).
	pub output_components: String,
	/// Output bit depth (kOfxBitDepthFloat etc.; always float on the all-F32 pipeline).
	pub output_bit_depth: String,
	/// Pixel aspect ratio.
	pub pixel_aspect_ratio: f64,
	/// Frame rate.
	pub frame_rate: f64,
	/// Field handling mode (kOfxImageField*).
	pub field: String,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::descriptor::{ClipDescriptor, EffectDescriptor};
	use crate::image::{BitDepth, Components, Image};
	use crate::param::{ChangeReason, ParamDef, ParamInstance};
	use crate::property::Value as PropValue;
	use crate::suites::status;
	use std::ffi::{c_char, c_int, c_void, CString};
	use std::sync::atomic::{AtomicBool, AtomicUsize};
	use std::sync::{Arc, Mutex};

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn action_is(action: *const c_char, name: &str) -> bool {
		!action.is_null()
			&& unsafe { std::ffi::CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
	}

	fn prop_string(props: &PropertySet, name: &str) -> Option<String> {
		match props.get(name, 0) {
			Some(PropValue::String(s)) => Some(s.to_string_lossy().into_owned()),
			_ => None,
		}
	}

	unsafe extern "C" fn entry_ok(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}

	unsafe extern "C" fn entry_default(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::REPLY_DEFAULT
	}

	unsafe extern "C" fn entry_err(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::ERR_FATAL
	}

	fn param_set(defs: Vec<ParamDef>) -> ParamSetInstance {
		ParamSetInstance {
			params: defs
				.into_iter()
				.map(|d| Box::new(ParamInstance::from_def(d)))
				.collect(),
		}
	}

	fn clip(name: &str) -> ClipInstance {
		ClipInstance::from_descriptor(&ClipDescriptor::new(name))
	}

	fn make_instance_with(
		entry: crate::host::EntryPoint,
		params: ParamSetInstance,
		clips: Vec<ClipInstance>,
	) -> Arc<Instance> {
		let plugin = Arc::new(Plugin {
			identifier: "test.instance".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: AtomicBool::new(false),
		});
		Arc::new(Instance {
			props: PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: clips.into_iter().map(Box::new).collect(),
			node_identity: AtomicUsize::new(0),
			destroyed: AtomicBool::new(false),
			sequence_range: Mutex::new(None),
			progress_cb: Mutex::new(None),
			cancel: AtomicBool::new(false),
			edit: Mutex::new(EditTransaction::new()),
			render_lock: Mutex::new(()),
			interact: Mutex::new(None),
		})
	}

	fn make_instance(entry: crate::host::EntryPoint, clips: Vec<ClipInstance>) -> Arc<Instance> {
		make_instance_with(entry, param_set(vec![]), clips)
	}

	fn scale() -> RenderScale {
		RenderScale { x: 1.0, y: 1.0 }
	}

	fn window() -> OfxRectD {
		OfxRectD {
			x1: 0.0,
			y1: 0.0,
			x2: 2.0,
			y2: 2.0,
		}
	}

	fn out_image() -> Arc<Image> {
		Arc::new(Image::allocate(
			BitDepth::Float,
			Components::Rgba,
			window(),
		))
	}

	// ---- parameter edit transaction --------------------------------------

	#[test]
	fn edit_transaction_nesting_and_multi() {
		use quercus_undo::undocommand::UndoCommand;
		let inst = make_instance(entry_ok, vec![]);
		assert!(!inst.in_edit());

		// 嵌套 begin：首次进入重置事务状态。
		inst.edit_begin();
		inst.edit_begin();
		inst.submit_undo_command(UndoCommand::from_closures(|| {}, || {}), "one");
		{
			let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
			assert_eq!(e.depth, 2);
			assert_eq!(e.param_count, 1);
			assert_eq!(e.first_label, "one");
			assert_eq!(e.multi.as_ref().expect("multi").multi_child_count(), 1);
		}
		inst.submit_undo_command(UndoCommand::from_closures(|| {}, || {}), "two");
		{
			let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
			assert_eq!(e.multi.as_ref().expect("multi").multi_child_count(), 2);
			assert_eq!(e.first_label, "one", "首条标签保持");
		}
		// 内层 end：仍在事务内，multi 保留。
		inst.edit_end();
		assert!(inst.in_edit());
		// 外层 end：redo 并清空。
		inst.edit_end();
		assert!(!inst.in_edit());
		{
			let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
			assert!(e.multi.is_none());
			assert_eq!(e.param_count, 0);
			assert!(e.first_label.is_empty());
		}
		// 事务外 end：depth 不越界。
		inst.edit_end();

		// 新事务重新计数（首进入重置分支）。
		inst.edit_begin();
		{
			let e = inst.edit.lock().unwrap_or_else(|e| e.into_inner());
			assert!(e.multi.is_none());
			assert_eq!(e.param_count, 0);
		}
		inst.edit_end();

		// 事务外提交 → 立即 redo。
		let ran = Arc::new(AtomicBool::new(false));
		let flag = ran.clone();
		inst.submit_undo_command(
			UndoCommand::from_closures(
				move || flag.store(true, std::sync::atomic::Ordering::Relaxed),
				|| {},
			),
			"solo",
		);
		assert!(ran.load(std::sync::atomic::Ordering::Relaxed));
	}

	// ---- clip preferences -------------------------------------------------

	unsafe extern "C" fn entry_prefs_wrong_types(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_CLIP_PREFERENCES) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.define(
				&crate::host::clip_pref_prop(crate::host::CLIP_PREF_COMPONENTS, "Output"),
				vec![PropValue::Int(1)],
			);
			out.define(
				&crate::host::clip_pref_prop(crate::host::CLIP_PREF_DEPTH, "Output"),
				vec![PropValue::Int(1)],
			);
			out.define(
				&crate::host::clip_pref_prop(crate::host::CLIP_PREF_PAR, "Output"),
				vec![PropValue::String(cs("x"))],
			);
			out.set_one(crate::host::PROP_FRAME_RATE, PropValue::Int(30));
			out.set_one(crate::host::PROP_FIELD_ORDER, PropValue::Int(1));
		}
		status::OK
	}

	#[test]
	fn clip_preferences_default_and_reply_default_paths() {
		let inst = make_instance(entry_ok, vec![clip("Source"), clip("Output")]);
		let prefs = inst.get_clip_preferences().expect("默认协商");
		assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		assert_eq!(prefs.pixel_aspect_ratio, 1.0);
		// 插件未回写 → out args 的预置帧率 1.0。
		assert_eq!(prefs.frame_rate, 1.0);
		assert_eq!(prefs.field, "");
		// per-clip 回灌（分量/位深/PAR）。
		let source = inst.clips.iter().find(|c| c.name == "Source").unwrap();
		assert_eq!(
			prop_string(&source.props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS).as_deref(),
			Some("OfxImageComponentRGBA")
		);
		assert_eq!(
			prop_string(&source.props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH).as_deref(),
			Some("OfxBitDepthFloat")
		);
		assert!(matches!(
			source.props.get("OfxImagePropPixelAspectRatio", 0),
			Some(PropValue::Double(d)) if d == 1.0
		));

		// ReplyDefault 同样被接受。
		let inst = make_instance(entry_default, vec![clip("Output")]);
		assert!(inst.get_clip_preferences().is_ok());
		// 失败状态 → Err。
		let inst = make_instance(entry_err, vec![clip("Output")]);
		assert!(inst.get_clip_preferences().is_err());
	}

	#[test]
	fn clip_preferences_wrong_out_types_fall_back() {
		let inst = make_instance(entry_prefs_wrong_types, vec![clip("Output")]);
		let prefs = inst.get_clip_preferences().expect("类型不符应回退");
		assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		assert_eq!(prefs.pixel_aspect_ratio, 1.0);
		assert_eq!(prefs.frame_rate, 24.0);
		assert_eq!(prefs.field, "");
	}

	// ---- region of definition / interest ---------------------------------

	unsafe extern "C" fn entry_rod_ok(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_ROD) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.define(
				crate::host::PROP_ROD,
				vec![
					PropValue::Double(1.0),
					PropValue::Double(2.0),
					PropValue::Double(3.0),
					PropValue::Double(4.0),
				],
			);
		}
		status::OK
	}

	unsafe extern "C" fn entry_roi_source(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_ROI) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.define(
				"OfxImageEffectPropRegionOfInterest_Source",
				vec![
					PropValue::Double(1.0),
					PropValue::Double(2.0),
					PropValue::Double(3.0),
					PropValue::Double(4.0),
				],
			);
		}
		status::OK
	}

	#[test]
	fn region_of_definition_and_roi_paths() {
		let inst = make_instance(entry_rod_ok, vec![]);
		let rod = inst.get_region_of_definition(0.5, scale()).expect("rod");
		assert_eq!((rod.x1, rod.y1, rod.x2, rod.y2), (1.0, 2.0, 3.0, 4.0));
		let inst = make_instance(entry_err, vec![]);
		assert!(inst.get_region_of_definition(0.0, scale()).is_err());

		let inst = make_instance(entry_roi_source, vec![clip("Source"), clip("Output")]);
		let region = OfxRectD {
			x1: 10.0,
			y1: 11.0,
			x2: 12.0,
			y2: 13.0,
		};
		let rois = inst.get_regions_of_interest(0.0, scale(), region).expect("rois");
		assert_eq!(rois.len(), 2);
		assert_eq!(
			(rois[0].x1, rois[0].y1, rois[0].x2, rois[0].y2),
			(1.0, 2.0, 3.0, 4.0)
		);
		// 插件未回写的 clip 保持 out args 的预置零值（per-clip 预定义）。
		assert_eq!(
			(rois[1].x1, rois[1].y1, rois[1].x2, rois[1].y2),
			(0.0, 0.0, 0.0, 0.0)
		);
		let inst = make_instance(entry_err, vec![clip("Source")]);
		assert!(inst.get_regions_of_interest(0.0, scale(), region).is_err());
	}

	// ---- isIdentity -------------------------------------------------------

	unsafe extern "C" fn entry_identity_source(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_IS_IDENTITY) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.set_one(crate::host::PROP_IS_IDENTITY, PropValue::String(cs("Source")));
			out.set_one(crate::host::PROP_TIME, PropValue::Double(2.5));
		}
		status::OK
	}

	unsafe extern "C" fn entry_identity_time_int(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_IS_IDENTITY) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.set_one(crate::host::PROP_IS_IDENTITY, PropValue::String(cs("Source")));
			out.set_one(crate::host::PROP_TIME, PropValue::Int(3));
		}
		status::OK
	}

	#[test]
	fn is_identity_paths() {
		let inst = make_instance(entry_identity_source, vec![]);
		let (time, name) = inst.is_identity(0.0).unwrap().expect("identity");
		assert_eq!(time, 2.5);
		assert_eq!(name, "Source");

		// 透传时间非 Double → 回退输入时间。
		let inst = make_instance(entry_identity_time_int, vec![]);
		let (time, name) = inst.is_identity(1.25).unwrap().expect("identity");
		assert_eq!(time, 1.25);
		assert_eq!(name, "Source");

		// 空 identity → None。
		let inst = make_instance(entry_ok, vec![]);
		assert!(inst.is_identity(0.0).unwrap().is_none());
		// 插件失败 → 视为非透传（不致命）。
		let inst = make_instance(entry_err, vec![]);
		assert!(inst.is_identity(0.0).unwrap().is_none());
	}

	// ---- render / render_gl ----------------------------------------------

	unsafe extern "C" fn progress_cb(_: f64, _: *mut c_void) -> c_int {
		0
	}

	struct NoopUi;
	impl crate::progress::UiProgressReporter for NoopUi {
		fn update(&mut self, _progress: f64) -> bool {
			true
		}
	}

	struct FakeGpu;
	impl quercus_core::backend::GpuContextLike for FakeGpu {
		fn kind(&self) -> quercus_core::backend::BackendKind {
			quercus_core::backend::BackendKind::Cpu
		}
		fn destroy_texture(&self, _token: u64) {}
		fn upload(&self, _token: u64, _frame: &quercus_core::texture::Frame) -> quercus_render::error::Result<()> {
			Ok(())
		}
		fn download(&self, _token: u64) -> quercus_render::error::Result<quercus_core::texture::Frame> {
			Ok(quercus_core::texture::Frame::new())
		}
		fn blit(
			&self,
			_src: u64,
			_dst: u64,
			_processor: Option<&quercus_core::color::ColorProcessor>,
		) -> quercus_render::error::Result<()> {
			Ok(())
		}
	}

	fn fake_renderer() -> crate::render::Renderer {
		Arc::new(FakeGpu)
	}

	#[test]
	fn render_paths_and_progress_reporters() {
		// facade 取消标记 → 入口即短路。
		let inst = make_instance(entry_err, vec![]);
		inst.cancel
			.store(true, std::sync::atomic::Ordering::Relaxed);
		assert!(inst.render(0.0, scale(), window(), out_image()).is_err());

		// facade 进度回调分支。
		let inst = make_instance(entry_ok, vec![]);
		*inst.progress_cb.lock().unwrap_or_else(|e| e.into_inner()) =
			Some((progress_cb, 0));
		inst.render(0.0, scale(), window(), out_image())
			.expect("带回调的 render");

		// UI 工厂分支（无回调 + factory 已注册）。
		static FACTORY_LOCK: Mutex<()> = Mutex::new(());
		let _g = FACTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		crate::progress::set_reporter_factory(Some(Arc::new(|_, _| Box::new(NoopUi))));
		let inst = make_instance(entry_ok, vec![]);
		let result = inst.render(0.0, scale(), window(), out_image());
		crate::progress::set_reporter_factory(None);
		result.expect("带 UI 工厂的 render");

		// 插件 render 失败 → Err。
		let inst = make_instance(entry_err, vec![]);
		assert!(inst.render(0.0, scale(), window(), out_image()).is_err());

		// in-args：GL 开关两态。
		let gl = Instance::render_in_args(0.0, scale(), window(), true);
		assert!(matches!(
			gl.get(crate::host::PROP_GL_ENABLED, 0),
			Some(PropValue::Int(1))
		));
		let cpu = Instance::render_in_args(0.0, scale(), window(), false);
		assert!(cpu.get(crate::host::PROP_GL_ENABLED, 0).is_none());
	}

	unsafe extern "C" fn entry_gl_attach_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GL_CONTEXT_ATTACHED) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	unsafe extern "C" fn entry_gl_render_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_RENDER) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	#[test]
	fn render_gl_paths() {
		// 成功：attach → render → detach；位深列表空 → 默认 F32。
		let inst = make_instance(entry_ok, vec![]);
		inst.render_gl(
			0.0,
			scale(),
			window(),
			fake_renderer(),
			crate::render::Texture::dummy(),
			Some(7),
		)
		.expect("GL render");
		assert!(crate::suites::gl_ctx().is_none(), "GL 上下文已清理");

		// 插件声明 F32 位深列表 → pick 命中分支。
		let inst = make_instance(entry_ok, vec![]);
		inst.plugin.descriptor.props.define(
			crate::host::PROP_GL_PIXEL_DEPTH,
			vec![PropValue::String(cs("OfxBitDepthFloat"))],
		);
		inst.render_gl(
			0.0,
			scale(),
			window(),
			fake_renderer(),
			crate::render::Texture::dummy(),
			None,
		)
		.expect("声明位深的 GL render");

		// attach 失败 → 清理 + Err。
		let inst = make_instance(entry_gl_attach_fails, vec![]);
		assert!(inst
			.render_gl(
				0.0,
				scale(),
				window(),
				fake_renderer(),
				crate::render::Texture::dummy(),
				None,
			)
			.is_err());
		assert!(crate::suites::gl_ctx().is_none());

		// render 失败 → detach 仍发 + Err。
		let inst = make_instance(entry_gl_render_fails, vec![]);
		assert!(inst
			.render_gl(
				0.0,
				scale(),
				window(),
				fake_renderer(),
				crate::render::Texture::dummy(),
				None,
			)
			.is_err());

		// 取消 → 入口即短路。
		let inst = make_instance(entry_ok, vec![]);
		inst.cancel
			.store(true, std::sync::atomic::Ordering::Relaxed);
		assert!(inst
			.render_gl(
				0.0,
				scale(),
				window(),
				fake_renderer(),
				crate::render::Texture::dummy(),
				None,
			)
			.is_err());
	}

	// ---- output colourspace ----------------------------------------------

	unsafe extern "C" fn entry_colourspace_value(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.set_one(
				crate::host::PROP_CLIP_COLOURSPACE,
				PropValue::String(cs("ACEScg")),
			);
		}
		status::OK
	}

	unsafe extern "C" fn entry_colourspace_cross(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.set_one(
				crate::host::PROP_CLIP_COLOURSPACE,
				PropValue::String(cs("OfxColourspace_Source")),
			);
		}
		status::OK
	}

	unsafe extern "C" fn entry_colourspace_int(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		out_args: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_GET_OUTPUT_COLOURSPACE) {
			let out = unsafe { &*(out_args as *const PropertySet) };
			out.set_one(crate::host::PROP_CLIP_COLOURSPACE, PropValue::Int(1));
		}
		status::OK
	}

	#[test]
	fn output_colourspace_paths() {
		// OK + 直值。
		let inst = make_instance(entry_colourspace_value, vec![clip("Source"), clip("Output")]);
		assert_eq!(
			inst.get_output_colourspace(&["sRGB".to_string()]).unwrap(),
			"ACEScg"
		);
		// 写回 Output clip。
		inst.set_output_colourspace("sRGB");
		assert_eq!(
			prop_string(
				&inst.clips.iter().find(|c| c.name == "Output").unwrap().props,
				crate::host::PROP_CLIP_COLOURSPACE
			)
			.as_deref(),
			Some("sRGB")
		);

		// 交叉引用解析为所引 clip 的实际色彩空间。
		let inst = make_instance(entry_colourspace_cross, vec![clip("Source"), clip("Output")]);
		let expected = prop_string(&inst.clips[0].props, crate::host::PROP_CLIP_COLOURSPACE).unwrap();
		assert_eq!(inst.get_output_colourspace(&[]).unwrap(), expected);
		// 所引 clip 的色彩空间类型不符 → 原样返回引用串。
		inst.clips[0]
			.props
			.set_one(crate::host::PROP_CLIP_COLOURSPACE, PropValue::Int(1));
		assert_eq!(
			inst.get_output_colourspace(&[]).unwrap(),
			"OfxColourspace_Source"
		);
		// 查无引用 clip / 非引用原样返回。
		assert_eq!(
			inst.resolve_colourspace("OfxColourspace_Nope".into()),
			"OfxColourspace_Nope"
		);
		assert_eq!(inst.resolve_colourspace("sRGB".into()), "sRGB");

		// OK 但空值 → Err；OK 但非 String → Err。
		let inst = make_instance(entry_ok, vec![clip("Output")]);
		assert!(inst.get_output_colourspace(&[]).is_err());
		let inst = make_instance(entry_colourspace_int, vec![clip("Output")]);
		assert!(inst.get_output_colourspace(&[]).is_err());

		// ReplyDefault：第一个输入 clip 的色彩空间。
		let inst = make_instance(entry_default, vec![clip("Source"), clip("Output")]);
		let expected = prop_string(&inst.clips[0].props, crate::host::PROP_CLIP_COLOURSPACE).unwrap();
		assert_eq!(inst.get_output_colourspace(&[]).unwrap(), expected);
		// 无输入 clip → 空串。
		let inst = make_instance(entry_default, vec![clip("Output")]);
		assert_eq!(inst.get_output_colourspace(&[]).unwrap(), "");
		// 输入 clip 无 colourspace 属性 → 空串。
		let inst = make_instance(entry_default, vec![clip("Source")]);
		inst.clips[0]
			.props
			.remove(crate::host::PROP_CLIP_COLOURSPACE);
		assert_eq!(inst.get_output_colourspace(&[]).unwrap(), "");

		// 其它状态码 → Err。
		let inst = make_instance(entry_err, vec![clip("Output")]);
		assert!(inst.get_output_colourspace(&[]).is_err());

		// 无 Output clip → set_output_colourspace no-op。
		let inst = make_instance(entry_ok, vec![clip("Source")]);
		inst.set_output_colourspace("x");
	}

	// ---- sequence render --------------------------------------------------

	unsafe extern "C" fn entry_begin_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_BEGIN_SEQUENCE) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	unsafe extern "C" fn entry_end_fails(
		action: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_END_SEQUENCE) {
			status::ERR_FATAL
		} else {
			status::OK
		}
	}

	#[test]
	fn sequence_render_paths() {
		let range = OfxRangeD {
			min: 1.0,
			max: 10.0,
		};

		let inst = make_instance(entry_ok, vec![]);
		inst.begin_sequence_render(range).expect("begin");
		assert_eq!(
			inst.sequence_range
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.map(|r| r.min),
			Some(1.0)
		);
		inst.end_sequence_render(range).expect("end");
		assert!(inst
			.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.is_none());

		// GL 变体（in args 带 OpenGLEnabled）。
		let inst = make_instance(entry_ok, vec![]);
		inst.begin_sequence_render_gl(range).expect("gl begin");
		inst.end_sequence_render_gl(range).expect("gl end");

		// 失败状态 → Err（end 仍清 range）。
		let inst = make_instance(entry_begin_fails, vec![]);
		assert!(inst.begin_sequence_render(range).is_err());
		let inst = make_instance(entry_end_fails, vec![]);
		assert!(inst.end_sequence_render(range).is_err());
		assert!(inst
			.sequence_range
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.is_none());

		// in-args：GL 开关两态。
		let gl = Instance::sequence_in_args(range, true);
		assert!(matches!(
			gl.get(crate::host::PROP_GL_ENABLED, 0),
			Some(PropValue::Int(1))
		));
		let cpu = Instance::sequence_in_args(range, false);
		assert!(cpu.get(crate::host::PROP_GL_ENABLED, 0).is_none());
	}

	// ---- instanceChanged / interact --------------------------------------

	static CHANGED_REASONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

	unsafe extern "C" fn entry_record_reason(
		action: *const c_char,
		_: *const c_void,
		in_args: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		if action_is(action, crate::host::ACTION_INSTANCE_CHANGED) {
			let props = unsafe { &*(in_args as *const PropertySet) };
			if let Some(PropValue::String(s)) = props.get(crate::host::PROP_CHANGE_REASON, 0) {
				CHANGED_REASONS
					.lock()
					.unwrap_or_else(|e| e.into_inner())
					.push(s.to_string_lossy().into_owned());
			}
		}
		status::OK
	}

	#[test]
	fn instance_changed_reasons_and_failure() {
		CHANGED_REASONS
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
		let inst = make_instance(entry_record_reason, vec![]);
		inst.instance_changed("gain", ChangeReason::UserEdited, 0.0, scale())
			.unwrap();
		inst.instance_changed("gain", ChangeReason::PluginEdited, 0.0, scale())
			.unwrap();
		inst.instance_changed("gain", ChangeReason::TimeChanged, 0.0, scale())
			.unwrap();
		assert_eq!(
			*CHANGED_REASONS.lock().unwrap_or_else(|e| e.into_inner()),
			vec![
				"OfxChangeUserEdited".to_string(),
				"OfxChangePluginEdited".to_string(),
				"OfxChangeTime".to_string(),
			]
		);

		let inst = make_instance(entry_err, vec![]);
		assert!(inst
			.instance_changed("gain", ChangeReason::UserEdited, 0.0, scale())
			.is_err());
	}

	unsafe extern "C" fn overlay_default(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::REPLY_DEFAULT
	}

	#[test]
	fn new_interact_and_describe_paths() {
		// 未创建 → interact None / describe BadHandle。
		let inst = make_instance(entry_ok, vec![]);
		assert!(inst.interact().is_none());
		assert_eq!(inst.describe_interact(), status::ERR_BAD_HANDLE);

		// ReplyDefault 且未声明 overlay → 无 interact。
		let inst = make_instance(entry_default, vec![]);
		assert!(inst.new_interact().is_none());

		// main entry 接受 NewInteract → 创建；describe 走 main entry。
		let inst = make_instance(entry_ok, vec![]);
		let interact = inst.new_interact().expect("new_interact");
		assert_eq!(inst.describe_interact(), status::OK);
		assert!(inst.interact().is_some());
		assert_eq!(Arc::strong_count(&interact), 2);

		// 声明 overlay 入口 + ReplyDefault → 仍创建（官方 describe/create 序列）。
		let inst = make_instance(overlay_default, vec![]);
		inst.plugin.descriptor.props.set_one(
			crate::host::PROP_OVERLAY_INTERACT_V2,
			PropValue::Pointer(overlay_default as *const () as *mut c_void),
		);
		assert!(inst.new_interact().is_some());
		assert_eq!(inst.describe_interact(), status::REPLY_DEFAULT);
	}

	// ---- read_rect --------------------------------------------------------

	#[test]
	fn read_rect_rejects_wrong_types_and_missing() {
		let props = PropertySet::new();
		assert!(read_rect(&props, "missing").is_none());
		props.define(
			"r",
			vec![
				PropValue::Int(1),
				PropValue::Double(2.0),
				PropValue::Double(3.0),
				PropValue::Double(4.0),
			],
		);
		assert!(read_rect(&props, "r").is_none());
		props.define(
			"r",
			vec![
				PropValue::Double(1.0),
				PropValue::Int(2),
				PropValue::Double(3.0),
				PropValue::Double(4.0),
			],
		);
		assert!(read_rect(&props, "r").is_none());
		props.define(
			"r",
			vec![
				PropValue::Double(1.0),
				PropValue::Double(2.0),
				PropValue::Int(3),
				PropValue::Double(4.0),
			],
		);
		assert!(read_rect(&props, "r").is_none());
		props.define(
			"r",
			vec![
				PropValue::Double(1.0),
				PropValue::Double(2.0),
				PropValue::Double(3.0),
				PropValue::Int(4),
			],
		);
		assert!(read_rect(&props, "r").is_none());
		props.define(
			"r",
			vec![
				PropValue::Double(1.0),
				PropValue::Double(2.0),
				PropValue::Double(3.0),
				PropValue::Double(4.0),
			],
		);
		assert_eq!(read_rect(&props, "r").map(|r| r.x2), Some(3.0));
	}

	// ---- trace probes -----------------------------------------------------

	/// Trace-probe branches enabled by QUERCUS_OFX_TRACE (logging for clip
	/// prefs/RoD/RoI/isIdentity and the isIdentity failure path).
	///
	/// The probe's only effect besides the normal action result is a trace
	/// line on stderr (no test-installable sink), so each call is pinned by
	/// its return value; the env override is restored before the assertions
	/// run so a failing expectation cannot leak `QUERCUS_OFX_TRACE` into the
	/// next serialized test.
	#[test]
	fn trace_probe_branches() {
		static TRACE_LOCK: Mutex<()> = Mutex::new(());
		let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let prev = std::env::var_os("QUERCUS_OFX_TRACE");
		std::env::set_var("QUERCUS_OFX_TRACE", "1");

		let inst = make_instance(entry_ok, vec![clip("Source"), clip("Output")]);
		let prefs = inst.get_clip_preferences();
		let rod = inst.get_region_of_definition(0.0, scale());
		let region = OfxRectD {
			x1: 1.0,
			y1: 2.0,
			x2: 3.0,
			y2: 4.0,
		};
		let rois = inst.get_regions_of_interest(0.0, scale(), region);
		let identity = inst.is_identity(0.0);
		let bad = make_instance(entry_err, vec![]);
		let bad_identity = bad.is_identity(0.0);

		match prev {
			Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
			None => std::env::remove_var("QUERCUS_OFX_TRACE"),
		}

		// entry_ok writes nothing back: every negotiated value stays at the
		// host pre-set default, which is what the probe branch observes.
		let prefs = prefs.expect("entry_ok answers OK");
		assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		assert_eq!(prefs.pixel_aspect_ratio, 1.0);
		assert_eq!(prefs.frame_rate, 1.0);
		assert_eq!(prefs.field, "");

		let rod = rod.expect("getRoD answers OK");
		assert_eq!(
			(rod.x1, rod.y1, rod.x2, rod.y2),
			(0.0, 0.0, 0.0, 0.0),
			"an untouched pre-defined RoD stays at the zero default"
		);

		// The plugin leaves the pre-defined per-clip ROI properties alone:
		// the result is those zero defaults, not the input region.
		let rois = rois.expect("getRoI answers OK");
		assert_eq!(rois.len(), 2, "one RoI per clip");
		assert!(
			rois.iter()
				.all(|r| (r.x1, r.y1, r.x2, r.y2) == (0.0, 0.0, 0.0, 0.0)),
			"untouched RoIs stay at the pre-defined zeros: {rois:?}"
		);

		// entry_ok leaves kOfxImageEffectPropIsIdentity empty: an OK action
		// without an identity declaration means "not an identity".
		assert!(
			identity.expect("isIdentity answers OK").is_none(),
			"an empty identity property means render"
		);

		// entry_err fails the action: the failure is logged and answered
		// None (never propagated as an error).
		assert!(
			bad_identity.expect("failed isIdentity is tolerated").is_none(),
			"a failed isIdentity falls back to render"
		);
	}
}
