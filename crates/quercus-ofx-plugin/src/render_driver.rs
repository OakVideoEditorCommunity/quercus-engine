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

//! Render driver: the rendering flow of
//! `src/render/src/plugin/pluginrenderer.cpp`
//! `PluginRenderer::render_plugin` (part of 1857 lines of C++) is
//! incorporated here semantically (M11 §4).
//!
//! Goal: all quercusrender PluginJob semantics are incorporated into this
//! module (after the single-library unification the original
//! `quercusplugin_instance_render_job` C ABI was removed, and the facade
//! constructs a [`RenderJob`] directly and calls [`render_frame`]); this
//! module carries the entire OFX host rendering flow. The C++ line numbers
//! cross-referenced section by section are noted in comments.
//!
//! ## Flow (render_frame, corresponding to render_plugin)
//!
//! 1. Instance lock (pluginrenderer.cpp:1436-1444) + cancellation check;
//! 2. use_opengl decision (pluginrenderer.cpp:1446-1457): the plugin
//!    descriptor's kOfxImageEffectPropOpenGLRenderSupported ∈ {true, needed}
//!    and the renderer is OpenGL and the destination texture has a GL id;
//! 3. Input texture collection (pluginrenderer.cpp:1518-1552): effect_input_id
//!    match → job.src; otherwise the texture table keyed by clip name;
//!    SimpleSource fallback;
//! 4. getClipPreferences (pluginrenderer.cpp:1554-1594);
//! 5. RoI/RoD setup (pluginrenderer.cpp:1596-1607): region_of_interest
//!    = destination size (canonical coordinates); the output clip's RoD is
//!    attached along with the output texture;
//! 6. Input clip RoD and format (pluginrenderer.cpp:1627-1665; Phase 2
//!    F32 end to end → no conversion);
//! 7. getRegionOfInterest (pluginrenderer.cpp:1666-1680): RoI is a
//!    suggestion; any failure continues with the default full frame (C++
//!    does the same for BadHandle; Phase 2 likewise continues for the other
//!    statuses — RoI only affects the upstream render range, and this
//!    pipeline's input is provided full-frame by quercusrender);
//! 8. Output clip format (pluginrenderer.cpp:1686-1697);
//! 9. **isIdentity fast path** (ofxRendering "Identity Effects"):
//!    isIdentity hit → copy the referenced input clip's frame at the
//!    passthrough time straight into the output (without calling the render
//!    action);
//! 10. Parameter overrides (pluginrenderer.cpp:132-290 apply_param_overrides);
//! 11. render action: the CPU path goes through
//!     [`crate::instance::Instance::render`] (output assembly: image →
//!     destination texture frame, row-stride aware); the GL path goes
//!     through [`crate::instance::Instance::render_gl`] + [`crate::gl_bridge`]
//!     (option B: the host creates an offscreen context, the plugin draws
//!     into the output GL texture attached to an FBO, and after render
//!     returns glReadPixels reads it back for assembly — the output format
//!     matches the CPU path; GL failure falls back to CPU).
//!
//! ## begin/end sequence brackets
//!
//! ofxRendering documentation: "All calls to the render action are bracketed
//! by a pair of begin/end sequence render actions". quercusrender brackets a
//! batch of frames for the same instance with [`begin_sequence`] first and
//! [`end_sequence`] last, calling [`render_frame`] once per frame in between.

use crate::image::Image;
use crate::instance::{Instance, OfxRangeD, OfxRectD, RenderScale};
use crate::property::Value;
use crate::render::{self, Renderer, Texture};

/// Inputs for one frame's render job (the Rust-side view of a quercusrender PluginJob).
pub struct RenderJob {
	/// Frame time (seconds).
	pub time: f64,
	/// Destination texture (output; created on the quercusrender side and passed in by value).
	pub dst: Texture,
	/// Primary input texture (effect_input_id / SimpleSource; `None` when absent).
	pub src: Option<Texture>,
	/// The effect input clip name (where job.src lands; C++ `node->get_effect_input_id()`).
	pub effect_input_id: Option<String>,
	/// Texture table for the remaining input clips (clip name → texture).
	pub inputs: Vec<(String, Texture)>,
	/// Parameter overrides (parameter name → quercusnode_value POD; corresponds to NodeValueRow).
	pub values: Vec<(String, crate::node::Value)>,
	/// GL renderer (`None` → CPU path; `Some` → subject to the use_opengl decision).
	pub renderer: Option<Renderer>,
	/// Whether to clear the destination before rendering (informational — C++ render_plugin
	/// does not handle it either; the upper-layer renderer does; see the plugin renderer notes).
	pub clear_destination: bool,
	/// Interactive-render flag (informational; in Phase 2 the render in args are always 0, see
	/// [`crate::instance::Instance::render`]).
	pub interactive: bool,
}

impl Default for RenderJob {
	fn default() -> Self {
		Self {
			time: 0.0,
			dst: Texture::dummy(),
			src: None,
			effect_input_id: None,
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: None,
			clear_destination: false,
			interactive: false,
		}
	}
}

/// beginSequenceRender bracket (ofxRendering: "bracketed by a pair of
/// begin/end sequence render actions"; in ofxGPURender.h's GL mode this
/// action's in args also carry kOfxImageEffectPropOpenGLEnabled).
/// `gl` non-empty selects GL mode.
pub fn begin_sequence(
	inst: &Instance,
	range: OfxRangeD,
	gl: Option<Renderer>,
) -> crate::error::Result<()> {
	if gl.is_some() {
		// GL 模式：ofxGPURender.h "OpenGL Current Context" 要求
		// BeginSequenceRender 期间上下文 current（插件可能在此分配 GL
		// 资源）。acquire 覆盖整个 action；返回即清 current。
		let _guard = crate::gl_bridge::acquire()?;
		inst.begin_sequence_render_gl(range)
	} else {
		inst.begin_sequence_render(range)
	}
}

/// endSequenceRender bracket, paired with [`begin_sequence`] to close one
/// sequence render.
///
/// When `gl` is `Some`, runs in GL mode: first acquire the GL context
/// (`gl_bridge::acquire`, covering the whole action, since the plugin may
/// release GL resources during it), then call the plugin's GL
/// endSequenceRender; otherwise take the CPU path. Call once per successful
/// `begin_sequence`, with `range` matching that call.
///
/// # Arguments
///
/// * `inst` - the instance being rendered.
/// * `range` - the sequence's `[min, max]` frame times, in seconds.
/// * `gl` - `Some` selects the GL render path, `None` the CPU path.
///
/// # Errors
///
/// Returns an error when the GL context cannot be acquired or the plugin
/// returns a non-`OK` status.
pub fn end_sequence(
	inst: &Instance,
	range: OfxRangeD,
	gl: Option<Renderer>,
) -> crate::error::Result<()> {
	if gl.is_some() {
		let _guard = crate::gl_bridge::acquire()?;
		inst.end_sequence_render_gl(range)
	} else {
		inst.end_sequence_render(range)
	}
}

/// Render one frame (the Rust port of `render_plugin`; see the module docs
/// for the section-by-section line references).
///
/// Returns the assembled output texture and the RoI of each input clip
/// (clip name → rectangle; with value-type textures the output is written
/// into a local copy of `job.dst` and delivered with the return value —
/// CPU texture `to_frame` is a copy and cannot write back into the read-only
/// `job.dst`). The host does not crop inputs by RoI — inputs are provided
/// full-frame by quercusrender, matching the C++ renderer's behavior.
pub fn render_frame(
	inst: &Instance,
	job: &RenderJob,
) -> crate::error::Result<(Texture, Vec<(String, OfxRectD)>)> {
	use crate::error::Error;

	// 1. 实例锁（pluginrenderer.cpp:1436-1444：OlivePluginInstance 非
	// 线程安全，并发 setInputTexture/renderAction 会毁内部状态）。
	let _lock = inst.render_lock.lock().unwrap_or_else(|e| e.into_inner());
	// 取消检查必须在任何插件调用之前（shutdown 后入口已卸载）。
	if inst.cancel.load(std::sync::atomic::Ordering::Relaxed) {
		return Err(Error::Failed("已取消".into()));
	}

	// Before the GL context is first created, tell the bridge which GPU
	// the engine is on (multi-GPU Linux). This is a preference only; a
	// mismatch falls back to the first usable device.
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	if let Texture::Gpu { ctx, .. } = &job.dst {
		if let Some(gpu) = ctx
			.as_any()
			.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>())
		{
			if let Some(name) = gpu.adapter_name() {
				crate::gl_bridge::set_preferred_renderer(&name);
			}
		}
	}

	// 2. use_opengl（pluginrenderer.cpp:1446-1457）：插件声明 GL 支持
	// 且渲染器是 OpenGL 且像素深度协商可行（管线 F32 满足插件
	// kOfxOpenGLPropPixelDepth 声明）且目标纹理有有效 GL 名（= 桥能为
	// 目标帧建出真实 GL 纹理 ⟺ 离屏上下文可用，[`crate::gl_bridge`]）。
	// 任一不满足 → 回退 CPU 路径（GL 分支在下方真正渲染）。
	let use_opengl = match job.renderer.as_ref() {
		// CPU-only by default: the OpenFX GL path shares the engine's
		// external-memory/Vulkan state and has produced device-lost crashes
		// on NVIDIA. Plugins still render through the CPU image-effect API;
		// opt into GL explicitly with QUERCUS_OFX_GL=1 once that path is
		// validated.
		Some(r)
			if render::renderer_is_open_gl(r) && std::env::var_os("QUERCUS_OFX_GL").is_some() =>
		{
			let plugin_gl = plugin_supports_opengl(inst);
			let depth_ok =
				crate::suites::gl_render::pick_gl_pixel_depth(&inst.plugin.descriptor.props)
					.is_some();
			let gl_name_ok = crate::gl_bridge::gl_available();
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
				let raw = inst
					.plugin
					.descriptor
					.props
					.get(crate::host::PROP_GL_RENDER_SUPPORTED, 0);
				eprintln!(
					"[ofx] use_opengl decision: plugin_gl={plugin_gl} depth_ok={depth_ok} gl_available={gl_name_ok} (raw GL prop: {raw:?})"
				);
			}
			plugin_gl && depth_ok && gl_name_ok
		}
		_ => false,
	};

	// 目标帧与参数（F32 校验；输出装配的依据）。dst 取本地副本：
	// 值型纹理下输出装配写回副本，随返回值交付（job.dst 只读）。
	let (dst_params, w, h) = read_dst(&job.dst)?;
	let mut dst = job.dst.clone();
	let par = pixel_aspect(&dst_params);
	// 规范坐标的 RoI/RoD（pluginrenderer.cpp:1595-1603：x2 = 宽 × PAR）。
	let region_of_interest = OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: w * par,
		y2: h,
	};

	// 3. 输入纹理收集（pluginrenderer.cpp:1518-1552）。
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		if let Some(tex) = pick_input(&clip.name, job) {
			if usable(&tex) {
				clip.set_input_texture(Some(tex), job.time);
			}
		}
	}

	// 4. getClipPreferences（pluginrenderer.cpp:1554-1594）。
	let prefs = inst.get_clip_preferences()?;
	if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
		eprintln!(
			"[ofx] clip prefs: components={} (inst {})",
			prefs.output_components, inst.plugin.identifier
		);
	}
	let components = match prefs.output_components.as_str() {
		"OfxImageComponentRGBA" => crate::image::Components::Rgba,
		"OfxImageComponentRGB" => crate::image::Components::Rgb,
		"OfxImageComponentAlpha" => crate::image::Components::Alpha,
		_ => return Err(Error::Failed("协商分量未知".into())),
	};
	// 位深协商（设计约束：管线全链路 ACEScg + F32；插件不支持 F32
	// 时按插件协商的位深渲染——输入图像转低、输出转回 F32，而不是
	// 直接失败）。GL 路径强制 F32：GL 纹理按管线格式创建，位深由
	// kOfxOpenGLPropPixelDepth 另行协商，clip props 必须与插件实际
	// 见到的图像一致。olive 像素格式号：0=u8, 2=u16, 3=f16, 4=f32。
	let (depth, depth_format) = if use_opengl {
		(crate::image::BitDepth::Float, 4)
	} else {
		let d = crate::image::BitDepth::from_ofx(&prefs.output_bit_depth)
			.ok_or_else(|| Error::Failed(format!("协商位深未知：{}", prefs.output_bit_depth)))?;
		let f = match d {
			crate::image::BitDepth::Byte => 0,
			crate::image::BitDepth::Short => 2,
			crate::image::BitDepth::Half => 3,
			crate::image::BitDepth::Float => 4,
		};
		(d, f)
	};

	// 5. 输出 clip：RoD + 输出纹理挂接（pluginrenderer.cpp:1603-1606）。
	let output_clip = inst
		.clips
		.iter()
		.find(|c| c.name == "Output")
		.ok_or_else(|| Error::Failed("实例无 Output clip".into()))?;
	output_clip.set_region_of_definition(region_of_interest, job.time);
	output_clip.set_output_texture(Some(dst.clone()), job.time);

	// 6. 输入 clip：RoD 与格式（pluginrenderer.cpp:1627-1665；位深按
	// 协商结果——非 F32 插件的输入图像在 fetch 时转低）。
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		if pick_input(&clip.name, job).is_some_and(|t| usable(&t)) {
			clip.set_region_of_definition(region_of_interest, job.time);
			clip.set_video_params(depth_format, 4);
		}
	}

	// 7. getRegionOfInterest（pluginrenderer.cpp:1666-1680）。RoI 为
	// 建议值：失败按默认整帧继续（C++ 对 BadHandle 如此；本管线
	// 输入整帧提供，RoI 只作记录）。
	let rois = inst
		.get_regions_of_interest(job.time, RenderScale { x: 1.0, y: 1.0 }, region_of_interest)
		.unwrap_or_else(|_| inst.clips.iter().map(|_| region_of_interest).collect());

	// 8. 输出 clip 格式（pluginrenderer.cpp:1686-1697；位深按协商）。
	output_clip.set_video_params(depth_format, components.channel_count() as i32);

	// 渲染窗口（像素坐标；pluginrenderer.cpp:1699-1704）。
	let render_window = OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: w,
		y2: h,
	};

	// 9. isIdentity 短路（ofxRendering "Identity Effects"）：插件声明
	// 本帧等价于某输入 clip → 直接透传该 clip 在透传时间的帧。
	if let Some((t, clip_name)) = inst.is_identity(job.time)? {
		passthrough(inst, &clip_name, t, &mut dst)?;
		return Ok((dst, zip_rois(inst, &rois)));
	}

	// 10. 参数覆盖（pluginrenderer.cpp:1729-1731 + 132-290）。
	apply_param_overrides(inst, &job.values);

	// 11. render action。
	if !use_opengl {
		let output = std::sync::Arc::new(Image::allocate(
			depth,
			components,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w,
				y2: h,
			},
		));
		inst.render(
			job.time,
			RenderScale { x: 1.0, y: 1.0 },
			render_window,
			output.clone(),
		)?;
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() && depth == crate::image::BitDepth::Float
		{
			let p = output.pixels();
			let dump: Vec<f32> = p
				.chunks_exact(4)
				.take(4)
				.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
				.collect();
			eprintln!(
				"[ofx] render_frame: output buffer at {:p}, pixels[0..4] = {dump:?}",
				p.as_ptr()
			);
		}
		// 输出装配（pluginrenderer.cpp:1762-1834 的 CPU 路径）：插件
		// 协商了低位深时先转回管线工作格式（全链路 F32/ACEScg）。
		let f32_output;
		let output_f32 = if depth == crate::image::BitDepth::Float {
			&*output
		} else {
			f32_output = output.convert_depth(crate::image::BitDepth::Float);
			&f32_output
		};
		write_output_frame(&mut dst, output_f32)?;
	} else {
		// Linux/OpenFX 真正 GPU 零拷贝：输入/输出都是同一 Vulkan 分配，
		// 插件直接写输出 GL 纹理，无 glReadPixels / CPU frame。不可用
		// 时回退下面的旧 GL 回读路径。
		#[cfg(any(target_os = "linux", target_os = "windows"))]
		{
			match try_render_gl_zerocopy(
				inst,
				job,
				RenderScale { x: 1.0, y: 1.0 },
				render_window,
				w,
				h,
				&dst_params,
			) {
				Ok(true) => return Ok((dst, zip_rois(inst, &rois))),
				Ok(false) => {}
				Err(err) => {
					eprintln!("[ofx] GPU zero-copy render failed, falling back: {err}");
				}
			}
		}
		// GL 路径（方案 B，见 [`crate::gl_bridge`]）：宿主自建离屏
		// 上下文，为输出帧建 GL 纹理 + FBO（插件直接画进附着的输出
		// 纹理，等价 C++ attach_output_texture），render 返回后
		// glReadPixels 回读装帧（pluginrenderer.cpp:1784-1834 的 GL
		// 分支 + 本实现的回读装配）。GL 失败回退 CPU（对齐现有失败
		// 语义；最终失败仍上抛）。
		match render_gl_frame(
			inst,
			job,
			RenderScale { x: 1.0, y: 1.0 },
			render_window,
			w,
			h,
			&dst_params,
		) {
			Ok(image) => write_output_frame(&mut dst, &image)?,
			Err(gl_err) => {
				eprintln!("[PLUGIN] GL 渲染失败，回退 CPU：{gl_err}");
				let output = std::sync::Arc::new(Image::allocate(
					depth,
					components,
					OfxRectD {
						x1: 0.0,
						y1: 0.0,
						x2: w,
						y2: h,
					},
				));
				inst.render(
					job.time,
					RenderScale { x: 1.0, y: 1.0 },
					render_window,
					output.clone(),
				)?;
				// 同主 CPU 路径：低位深协商的输出先转回 F32 再装帧。
				let f32_output;
				let output_f32 = if depth == crate::image::BitDepth::Float {
					&*output
				} else {
					f32_output = output.convert_depth(crate::image::BitDepth::Float);
					&f32_output
				};
				write_output_frame(&mut dst, output_f32)?;
			}
		}
	}

	Ok((dst, zip_rois(inst, &rois)))
}

/// Render one frame on the GL path (the body of render_frame's GL branch;
/// returns the read-back, assembled F32 RGBA image).
///
/// Flow: acquire an offscreen context (current on this thread, globally
/// serialized) → create the output GL texture (size = destination frame,
/// format from `dst_params`) → attach and bind the FBO → viewport →
/// [`Instance::render_gl`] (the plugin draws into the attached texture; the
/// real texture name is injected through GlCtx and clipLoadTexture(Output)
/// returns it) → glReadPixels read-back for assembly (vertical flip + format
/// conversion) → clean up the FBO/texture. Any failing step returns Err and
/// the caller falls back to CPU.
fn render_gl_frame(
	inst: &Instance,
	job: &RenderJob,
	scale: RenderScale,
	window: OfxRectD,
	w: f64,
	h: f64,
	dst_params: &render::VideoParams,
) -> crate::error::Result<std::sync::Arc<Image>> {
	use crate::error::Error;

	// 离屏上下文（进程级共享；本线程 current 直到 guard drop）。
	let _guard = crate::gl_bridge::acquire()?;
	let (wpx, hpx) = (w as i32, h as i32);
	// 输出 GL 纹理 + FBO 挂载（GL_RGBA32F/GL_RGBA8 按目标帧格式）。
	let gl_tex = crate::gl_bridge::create_output_texture(wpx, hpx, dst_params)?;
	let fbo = crate::gl_bridge::create_fbo(gl_tex, wpx, hpx)?;
	crate::gl_bridge::bind_fbo(fbo);
	crate::gl_bridge::set_viewport(wpx, hpx);

	// render action（OpenGLEnabled=1；输出纹理真实名经 GlCtx 注入）。
	let render_res = inst.render_gl(
		job.time,
		scale,
		window,
		job.renderer.clone().unwrap(),
		job.dst.clone(),
		Some(gl_tex),
	);

	// 回读前防御性重绑 FBO + 视口（规范下插件不解除输出绑定，但个别
	// 插件可能改绑/改视口——重绑保证 glReadPixels 读的是输出纹理）。
	crate::gl_bridge::bind_fbo(fbo);
	crate::gl_bridge::set_viewport(wpx, hpx);

	// 回读（FBO 仍绑定、输出 GL 纹理仍存活）。
	let readback = crate::gl_bridge::read_pixels_to_image(wpx, hpx, dst_params);

	// 清理（guard drop 时清 current 并放锁）。
	crate::gl_bridge::delete_fbo(fbo);
	crate::gl_bridge::delete_gl_texture(gl_tex);

	render_res?;
	readback.map_err(|e| Error::Failed(format!("GL 回读失败：{e}")))
}

/// Linux/OpenFX GL zero-copy: import the destination and every GPU
/// input from the same Vulkan allocation (`OPAQUE_FD` -> GL memory
/// object), let the plugin render into the destination GL texture, and
/// skip `glReadPixels` entirely.
///
/// Returns `Ok(false)` when the path is not applicable or an allocation
/// lacks exportable memory; the caller then uses the existing GL-readback
/// or CPU path. `Ok(true)` means the plugin rendered into `job.dst`
/// in-place.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn try_render_gl_zerocopy(
	inst: &Instance,
	job: &RenderJob,
	scale: RenderScale,
	window: OfxRectD,
	w: f64,
	h: f64,
	dst_params: &render::VideoParams,
) -> crate::error::Result<bool> {
	use crate::error::Error;
	use quercus_core::backend::{GlMemoryExport, GpuContext};

	fn export_for(texture: &Texture) -> Option<GlMemoryExport> {
		let Texture::Gpu { token, ctx, .. } = texture else {
			return None;
		};
		let gpu = ctx.as_any()?.downcast_ref::<GpuContext>()?;
		if !gpu.supports_gl_zerocopy() {
			return None;
		}
		gpu.export_gl_memory(*token).ok()
	}

	// The destination must be an exportable GPU texture. We only need the
	// concrete context for the wgpu operations; retrieve it directly.
	let Texture::Gpu { token, ctx, .. } = &job.dst else {
		return Ok(false);
	};
	let gpu = match ctx.as_any().and_then(|a| a.downcast_ref::<GpuContext>()) {
		Some(g) if g.supports_gl_zerocopy() => g,
		_ => return Ok(false),
	};
	let dst_export = match gpu.export_gl_memory(*token) {
		Ok(e) => e,
		Err(_) => return Ok(false),
	};
	// Output tracker starts UNINITIALIZED; a wgpu clear moves it to
	// COLOR_TARGET/COLOR_ATTACHMENT_OPTIMAL before GL writes, so the later
	// sample transition does not discard the plugin's pixels.
	if !gpu.wait_idle_for(std::time::Duration::from_secs(2)) {
		return Ok(false);
	}
	gpu.clear_texture(*token)
		.map_err(|e| Error::Failed(format!("zero-copy output clear failed: {e}")))?;
	if !gpu.wait_idle_for(std::time::Duration::from_secs(2)) {
		return Ok(false);
	}

	let _guard = crate::gl_bridge::acquire()?;
	let (wpx, hpx) = (w as i32, h as i32);
	let dst_gl = crate::gl_bridge::import_memory_texture(&dst_export, wpx, hpx, dst_params)?;
	let fbo = crate::gl_bridge::create_fbo(dst_gl.name, wpx, hpx)?;
	crate::gl_bridge::bind_fbo(fbo);
	crate::gl_bridge::set_viewport(wpx, hpx);

	let mut imported: Vec<crate::gl_bridge::GlMemoryTexture> = Vec::new();
	let mut attached: Vec<&crate::clip::ClipInstance> = Vec::new();
	let mut unsupported = false;
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		let Some(tex) = pick_input(&clip.name, job) else {
			continue;
		};
		if !usable(&tex) {
			continue;
		}
		match &tex {
			Texture::Cpu(_) => {}
			Texture::Planar(_) => {
				unsupported = true;
				break;
			}
			Texture::Gpu { .. } => {
				let Some(export) = export_for(&tex) else {
					unsupported = true;
					break;
				};
				let params = render::texture_get_params(&tex);
				if params.format != render::PIXEL_FORMAT_F32 {
					unsupported = true;
					break;
				}
				match crate::gl_bridge::import_memory_texture(
					&export,
					params.width,
					params.height,
					&params,
				) {
					Ok(gl) => {
						clip.set_input_gl_texture(Some(gl.clone()));
						imported.push(gl);
						attached.push(clip);
					}
					Err(err) => {
						eprintln!("[ofx] zero-copy input import failed, falling back: {err}");
						unsupported = true;
						break;
					}
				}
			}
		}
	}

	if unsupported {
		for clip in attached {
			clip.set_input_gl_texture(None);
		}
		for gl in imported {
			crate::gl_bridge::delete_memory_texture(&gl);
		}
		crate::gl_bridge::delete_fbo(fbo);
		crate::gl_bridge::delete_memory_texture(&dst_gl);
		return Ok(false);
	}

	let render_res = inst.render_gl(
		job.time,
		scale,
		window,
		job.renderer.clone().unwrap(),
		job.dst.clone(),
		Some(dst_gl.name),
	);
	crate::gl_bridge::gl_memory_barrier();
	crate::gl_bridge::gl_finish();

	for clip in attached {
		clip.set_input_gl_texture(None);
	}
	for gl in imported {
		crate::gl_bridge::delete_memory_texture(&gl);
	}
	crate::gl_bridge::delete_fbo(fbo);
	crate::gl_bridge::delete_memory_texture(&dst_gl);
	if !gpu.wait_idle_for(std::time::Duration::from_secs(2)) {
		return Err(Error::Failed(
			"zero-copy GL render: GPU did not finish within 2s".into(),
		));
	}
	render_res.map_err(|e| Error::Failed(format!("zerocopy GL render: {e}")))?;
	Ok(true)
}

/// Pair input clip names with the RoI list, for passing render results back
/// to the engine.
///
/// Skips the clip named `Output`; the paired index is the **original** index
/// in `inst.clips` (still counted in the order that includes `Output`), and
/// `rois` must be supplied in the same order; missing RoIs are filled with an
/// all-zero rectangle, so a length mismatch is not an error.
///
/// # Arguments
///
/// * `inst` - an instance that has been created and has its clips set.
/// * `rois` - RoI list in clip order.
fn zip_rois(inst: &Instance, rois: &[OfxRectD]) -> Vec<(String, OfxRectD)> {
	inst.clips
		.iter()
		.enumerate()
		.filter(|(_, c)| c.name != "Output")
		.map(|(i, c)| (c.name.clone(), rois.get(i).copied().unwrap_or_default()))
		.collect()
}

/// Read the destination texture `dst`'s parameters and validate F32,
/// returning `(params, width, height)`.
///
/// Width and height are returned as `f64` so they can directly participate
/// in RoI and pixel-aspect arithmetic; only `PIXEL_FORMAT_F32` output frames
/// are accepted (a Phase 2 constraint).
///
/// # Errors
///
/// Returns `Error::Failed` when the frame format is not F32, and
/// `Error::Invalid` when width or height is not positive.
fn read_dst(dst: &Texture) -> crate::error::Result<(render::VideoParams, f64, f64)> {
	use crate::error::Error;
	let params = render::texture_get_params(dst);
	if params.format != render::PIXEL_FORMAT_F32 {
		return Err(Error::Failed(format!(
			"输出帧格式 {} 非 F32（Phase 2 约束）",
			params.format
		)));
	}
	let (w, h) = (params.width as f64, params.height as f64);
	if w <= 0.0 || h <= 0.0 {
		return Err(Error::Invalid);
	}
	Ok((params, w, h))
}

/// Pixel aspect ratio of the destination frame
/// (`pixel_aspect_num / pixel_aspect_den`).
///
/// Returns `1.0` (square pixels) when the denominator is 0, the default when
/// `VideoParams` has no pixel ratio set.
///
/// # Arguments
///
/// * `params` - destination frame parameters (as read by [`read_dst`]).
fn pixel_aspect(params: &render::VideoParams) -> f64 {
	if params.pixel_aspect_den != 0 {
		params.pixel_aspect_num as f64 / params.pixel_aspect_den as f64
	} else {
		1.0
	}
}

/// Whether the plugin descriptor declares GL render support
/// (ofxGPURender.h:397-408: "true"/"needed").
pub(crate) fn plugin_supports_opengl(inst: &Instance) -> bool {
	inst.plugin
		.descriptor
		.props
		.get(crate::host::PROP_GL_RENDER_SUPPORTED, 0)
		.map(|v| match v {
			Value::String(s) => {
				let s = s.to_string_lossy();
				s == "true" || s == "needed"
			}
			_ => false,
		})
		.unwrap_or(false)
}

/// Whether an input texture is usable (not a placeholder; is_usable_input
/// from pluginrenderer.cpp:1504-1513 — Phase 2 only checks that it is not a
/// dummy; frame/Renderer are guaranteed by quercusrender).
fn usable(tex: &Texture) -> bool {
	!tex.is_dummy()
}

/// Pick a texture for an input clip using the priority order of C++
/// pluginrenderer.cpp:1527-1543.
///
/// Order: if `clip_name` equals `job.effect_input_id` and `job.src` exists,
/// use `job.src`; otherwise look it up by name in `job.inputs`; finally, a
/// clip named `Source` falls back to `job.src` (the SimpleSource
/// convention).
///
/// # Arguments
///
/// * `clip_name` - the input clip name requested by the plugin.
/// * `job` - the input set for this render (`src`, `inputs` and
///   `effect_input_id`).
///
/// # Returns
///
/// A clone of the matched texture (shared frame data); `None` when none of
/// the three rules matches.
fn pick_input(clip_name: &str, job: &RenderJob) -> Option<Texture> {
	if job.effect_input_id.as_deref() == Some(clip_name) {
		if let Some(src) = &job.src {
			return Some(src.clone());
		}
	}
	for (name, tex) in &job.inputs {
		if name == clip_name {
			return Some(tex.clone());
		}
	}
	// SimpleSource 回退（pluginrenderer.cpp:1534-1543：
	// kOfxImageEffectSimpleSourceClipName 取 k_texture_input，再回退
	// job.src）。
	if clip_name == "Source" {
		return job.src.clone();
	}
	None
}

/// isIdentity fast path: copy the input clip referenced by `clip_name` at
/// time `t` straight into the output texture, skipping plugin rendering.
///
/// The image is fetched at a 1:1 render scale and then written into `dst`
/// through [`write_output_frame`] (a CPU copy; the destination frame must be
/// F32).
///
/// # Arguments
///
/// * `inst` - the instance that provides the clip.
/// * `clip_name` - the input clip name referenced by the plugin's
///   `isIdentity` action.
/// * `t` - frame fetch time, in seconds.
/// * `dst` - output frame texture, overwritten in place.
///
/// # Errors
///
/// Returns an error when the clip name does not exist, fetching the frame
/// fails, or the output frame is not F32 / its size does not match the
/// image.
fn passthrough(
	inst: &Instance,
	clip_name: &str,
	t: f64,
	dst: &mut Texture,
) -> crate::error::Result<()> {
	use crate::error::Error;
	let clip = inst
		.clips
		.iter()
		.find(|c| c.name == clip_name)
		.ok_or_else(|| Error::Failed(format!("isIdentity 引用未知 clip {clip_name}")))?;
	let image = clip.fetch_image(t, RenderScale { x: 1.0, y: 1.0 }, None)?;
	write_output_frame(dst, &image)
}

/// Parameter overrides (the Rust port of pluginrenderer.cpp:132-290
/// `apply_param_overrides`): inject each frame's node values into the
/// instance parameters so this render uses the node's current values.
///
/// For each `(key, node value)`: find the parameter by name, skipping
/// unknown parameters; convert the value to the parameter type with
/// `param_value_from_node`, skipping values that cannot be expressed (the
/// string family String/StrChoice goes through the dedicated
/// `set_param_string` path). Single-value Double parameters get extra
/// cleaning: NaN/Inf falls back to the property default and logs a warning,
/// then the value is clamped to `kOfxParamPropMin`/`kOfxParamPropMax`
/// (multi-dimensional Double families do not get this check, matching C++).
/// The function returns nothing; a failure is only visible as the
/// corresponding parameter not being overridden.
///
/// # Arguments
///
/// * `inst` - target instance whose parameter table is modified in place.
/// * `values` - the node's current value list `(parameter name, node value)`.
fn apply_param_overrides(inst: &Instance, values: &[(String, crate::node::Value)]) {
	for (key, v) in values {
		let Some(p) = inst.params.find(key) else {
			continue;
		};
		let Some(mut pv) = crate::param::param_value_from_node(v, &p.def.ofx_type) else {
			continue;
		};
		// Double 标量的 NaN/Inf 清洗 + Min/Max 钳制
		// （pluginrenderer.cpp:155-177：坏值回退默认并告警，再按
		// kOfxParamPropMin/Max 钳制；多维 Double 族 C++ 无此检查）。
		if p.def.ofx_type == crate::param::TYPE_DOUBLE {
			if let crate::param::ParamValue::Double(d, 1) = &mut pv {
				if d[0].is_nan() || d[0].is_infinite() {
					eprintln!("[PLUGIN] NaN/Inf in double param {key} replacing with default");
					d[0] = prop_double(&p.def.props, crate::param::P_DEFAULT, 0);
				}
				if let Some(Value::Double(min)) = p.def.props.get(crate::param::P_MIN, 0) {
					if d[0] < min {
						d[0] = min;
					}
				}
				if let Some(Value::Double(max)) = p.def.props.get(crate::param::P_MAX, 0) {
					if d[0] > max {
						d[0] = max;
					}
				}
			}
		}
		p.set_ofx(pv);
	}
}

/// Read the Double value of component `index` of property `name`.
///
/// Missing properties, non-numeric types and out-of-range indexes all fall
/// back to `0.0`; `Int` values are promoted to `f64`. Used to read numeric
/// properties such as default/min/max from parameter definitions.
///
/// # Arguments
///
/// * `props` - property set (usually a parameter definition or the plugin
///   descriptor's props).
/// * `name` - property name (a `kOfxParamProp*`-style key).
/// * `index` - component index for multi-value properties, 0-based; pass 0
///   for single-value properties.
fn prop_double(props: &crate::property::PropertySet, name: &str, index: usize) -> f64 {
	match props.get(name, index) {
		Some(Value::Double(v)) => v,
		Some(Value::Int(v)) => v as f64,
		_ => 0.0,
	}
}

/// Write a CPU image into the destination texture (row-major, row-stride
/// aware; F32 validated). The common landing point for Phase 2 output
/// assembly (shared by the CPU render path and isIdentity passthrough). GPU
/// destination textures are written back through the backend upload
/// (`Texture::Gpu` branch).
pub(crate) fn write_output_frame(dst: &mut Texture, image: &Image) -> crate::error::Result<()> {
	use crate::error::Error;
	// CPU 纹理就地写入；GPU 纹理经下载帧改写后 upload 回写。
	let mut frame = render::texture_get_frame(dst)?;
	let params = frame.video_params();
	if params.format != render::PIXEL_FORMAT_F32 {
		return Err(Error::Failed("输出帧格式非 F32（Phase 2 约束）".into()));
	}
	let (w, h) = (params.width as usize, params.height as usize);
	let tight = w * image.components().channel_count() * 4;
	if tight != image.row_bytes() || tight * h != image.pixels().len() {
		return Err(Error::Failed("图像尺寸与输出帧不一致".into()));
	}
	let dst_ptr = frame.data_mut();
	if dst_ptr.is_null() {
		return Err(Error::Failed("输出帧无数据".into()));
	}
	let row = frame.linesize_bytes();
	let row = if row > 0 { row } else { tight };
	let dst_bytes = unsafe { std::slice::from_raw_parts_mut(dst_ptr, row * h) };
	let pixels = image.pixels();
	for y in 0..h {
		let d = y * row;
		let s = y * tight;
		dst_bytes[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
	}
	match dst {
		// 就地写回（值型 CPU 纹理：to_frame 是拷贝，必须写回本体）。
		Texture::Cpu(f) => {
			f.data = frame.data;
		}
		// GPU 目标纹理：拷贝只落在下载帧上，经后端 upload 回写。
		Texture::Gpu { token, ctx, .. } => {
			ctx.upload(*token, &frame)
				.map_err(|e| Error::Failed(format!("输出纹理上传失败：{e}")))?;
		}
		// 未解析的平面纹理不会成为插件输出目标（解码路径会先解析）。
		Texture::Planar(_) => {
			return Err(Error::Failed("平面纹理不能作为插件输出目标".into()));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests;
