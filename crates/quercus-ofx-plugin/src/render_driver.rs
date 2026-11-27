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
	/// Non-empty keyframe tracks for animatable parameters (render-time snapshot).
	pub animations: Vec<(String, quercus_node::keyframe::KeyframeTrack)>,
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
			animations: Vec::new(),
			renderer: None,
			clear_destination: false,
			interactive: false,
		}
	}
}

/// Scoped guard that installs render-time keyframe track snapshots onto an
/// instance's parameters and restores their prior state on drop (preventing
/// cross-job leakage on shared worker instances and overriding bound nodes
/// or stale local tracks during `render_frame`).
struct AnimationSnapshotGuard<'a> {
	saved: Vec<(
		&'a crate::param::ParamInstance,
		Option<Option<quercus_node::keyframe::KeyframeTrack>>,
		Option<Value>,
	)>,
}

impl<'a> AnimationSnapshotGuard<'a> {
	fn install(
		inst: &'a Instance,
		animations: &[(String, quercus_node::keyframe::KeyframeTrack)],
		values: &[(String, crate::node::Value)],
	) -> Self {
		let mut saved = Vec::with_capacity(animations.len() + values.len());
		for (name, _) in values {
			let Some(param) = inst.params.find(name) else {
				continue;
			};
			if !animations.iter().any(|(anim_name, _)| anim_name == name)
				&& param.animation_track().is_some()
			{
				let prev_override = param.render_animation_override();
				let prev_anim = param.props.get(crate::param::P_IS_ANIMATING, 0);
				param.set_render_animation_override(Some(None));
				param.set_is_animating(false);
				saved.push((param, prev_override, prev_anim));
			}
		}
		for (name, track) in animations {
			let Some(param) = inst.params.find(name) else {
				continue;
			};
			let prev_override = param.render_animation_override();
			let prev_anim = param.props.get(crate::param::P_IS_ANIMATING, 0);
			let animating = !track.keys().is_empty();
			param.set_render_animation_override(Some(Some(track.clone())));
			param.set_is_animating(animating);
			saved.push((param, prev_override, prev_anim));
		}
		Self { saved }
	}
}

impl Drop for AnimationSnapshotGuard<'_> {
	fn drop(&mut self) {
		for (param, prev_override, prev_anim) in self.saved.drain(..).rev() {
			param.set_render_animation_override(prev_override);
			if let Some(v) = prev_anim {
				param.props.set_one(crate::param::P_IS_ANIMATING, v);
			}
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
		// GL mode: ofxGPURender.h "OpenGL Current Context" requires the
		// context to be current during BeginSequenceRender (the plugin may
		// allocate GL resources here). acquire covers the whole action; on return it clears current.
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

	// 1. Instance lock (pluginrenderer.cpp:1436-1444: OlivePluginInstance is not
	// thread-safe; concurrent setInputTexture/renderAction corrupts internal state).
	let _lock = inst.render_lock.lock().unwrap_or_else(|e| e.into_inner());
	// Cancel check must precede any plugin call (entry points are unloaded after shutdown).
	if inst.cancel.load(std::sync::atomic::Ordering::Relaxed) {
		return Err(Error::Failed("cancelled".into()));
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

	// 2. use_opengl (pluginrenderer.cpp:1446-1457): plugin declares GL support
	// and renderer is OpenGL and pixel-depth negotiation is viable (pipeline
	// F32 satisfies the plugin's kOfxOpenGLPropPixelDepth declaration) and the
	// destination texture has a valid GL name (= bridge can create a real GL
	// texture for the destination frame ⟺ offscreen context available, [`crate::gl_bridge`]).
	// Any unmet condition → fall back to CPU path (GL branch does the actual render below).
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

	// Destination frame and params (F32 validation; basis for output assembly). dst takes a local copy:
	// value-type textures: assembly writes back to the copy and returns it (job.dst read-only).
	let (dst_params, w, h) = read_dst(&job.dst)?;
	let mut dst = job.dst.clone();
	let par = pixel_aspect(&dst_params);
	// RoI/RoD in canonical coordinates (pluginrenderer.cpp:1595-1603: x2 = width × PAR).
	let region_of_interest = OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: w * par,
		y2: h,
	};

	// 3. Input texture collection (pluginrenderer.cpp:1518-1552).
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

	// Install render-time keyframe snapshots before any plugin actions
	// (getClipPreferences, getRoI, isIdentity, render) and restore on drop.
	let _anim_guard = AnimationSnapshotGuard::install(inst, &job.animations, &job.values);

	// 4. getClipPreferences (pluginrenderer.cpp:1554-1594).
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
		_ => return Err(Error::Failed("unknown negotiated components".into())),
	};
	// Bit-depth negotiation (design constraint: pipeline is ACEScg + F32 end-to-end;
	// when the plugin does not support F32, render at the depth the plugin negotiated — inputs
	// converted down, outputs converted back to F32, instead of failing outright). GL path forces
	// F32: GL textures are created in the pipeline format, depth is negotiated separately via
	// kOfxOpenGLPropPixelDepth, and clip props must match the images the plugin actually sees.
	// Olive pixel format numbers: 0=u8, 2=u16, 3=f16, 4=f32.
	let (depth, depth_format) = if use_opengl {
		(crate::image::BitDepth::Float, 4)
	} else {
		let d = crate::image::BitDepth::from_ofx(&prefs.output_bit_depth).ok_or_else(|| {
			Error::Failed(format!(
				"unknown negotiated bit depth: {}",
				prefs.output_bit_depth
			))
		})?;
		let f = match d {
			crate::image::BitDepth::Byte => 0,
			crate::image::BitDepth::Short => 2,
			crate::image::BitDepth::Half => 3,
			crate::image::BitDepth::Float => 4,
		};
		(d, f)
	};

	// 5. Output clip: RoD + output texture attachment (pluginrenderer.cpp:1603-1606).
	let output_clip = inst
		.clips
		.iter()
		.find(|c| c.name == "Output")
		.ok_or_else(|| Error::Failed("instance has no Output clip".into()))?;
	output_clip.set_region_of_definition(region_of_interest, job.time);
	output_clip.set_output_texture(Some(dst.clone()), job.time);

	// 6. Input clips: RoD and format (pluginrenderer.cpp:1627-1665; depth per
	// negotiation — input images for non-F32 plugins are converted down on fetch).
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		if pick_input(&clip.name, job).is_some_and(|t| usable(&t)) {
			clip.set_region_of_definition(region_of_interest, job.time);
			clip.set_video_params(depth_format, 4);
		}
	}

	// 7. getRegionOfInterest (pluginrenderer.cpp:1666-1680). RoI is advisory:
	// on failure continue with the default full frame (C++ does the same for BadHandle; this pipeline
	// supplies full-frame inputs, RoI is recorded only).
	let rois = inst
		.get_regions_of_interest(job.time, RenderScale { x: 1.0, y: 1.0 }, region_of_interest)
		.unwrap_or_else(|_| inst.clips.iter().map(|_| region_of_interest).collect());

	// 8. Output clip format (pluginrenderer.cpp:1686-1697; depth per negotiation).
	output_clip.set_video_params(depth_format, components.channel_count() as i32);

	// Render window (pixel coordinates; pluginrenderer.cpp:1699-1704).
	let render_window = OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: w,
		y2: h,
	};

	// 9. isIdentity short-circuit (ofxRendering "Identity Effects"): plugin declares
	// this frame equivalent to some input clip → pass through that clip's frame at the pass-through time.
	if let Some((t, clip_name)) = inst.is_identity(job.time)? {
		passthrough(inst, &clip_name, t, &mut dst)?;
		return Ok((dst, zip_rois(inst, &rois)));
	}

	// 10. Parameter overrides (pluginrenderer.cpp:1729-1731 + 132-290).
	apply_param_overrides(inst, &job.values);

	// 11. render action.
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
		// Output assembly (CPU path of pluginrenderer.cpp:1762-1834): when the plugin
		// negotiated a lower depth, convert back to the pipeline working format first (F32/ACEScg end-to-end).
		let f32_output;
		let output_f32 = if depth == crate::image::BitDepth::Float {
			&*output
		} else {
			f32_output = output.convert_depth(crate::image::BitDepth::Float);
			&f32_output
		};
		write_output_frame(&mut dst, output_f32)?;
	} else {
		// Linux/OpenFX true GPU zero-copy: input/output are the same Vulkan allocation,
		// the plugin writes the output GL texture directly, no glReadPixels / CPU frame. When
		// unavailable, fall back to the old GL read-back path below.
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
		// GL path (option B, see [`crate::gl_bridge`]): host creates its own offscreen
		// context, creates a GL texture + FBO for the output frame (plugin draws directly
		// into the attached output texture, equivalent to C++ attach_output_texture); after
		// render returns, glReadPixels read-back assembles the frame (GL branch of
		// pluginrenderer.cpp:1784-1834 + this implementation's read-back assembly). GL failure
		// falls back to CPU (matching existing failure semantics; final failure is still propagated).
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
				eprintln!("[PLUGIN] GL render failed, falling back to CPU: {gl_err}");
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
				// Same as the main CPU path: lower-depth negotiated outputs are converted back to F32 before assembly.
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

	// Offscreen context (process-wide shared; current on this thread until guard drop).
	let _guard = crate::gl_bridge::acquire()?;
	let (wpx, hpx) = (w as i32, h as i32);
	// Output GL texture + FBO attachment (GL_RGBA32F/GL_RGBA8 per destination frame format).
	let gl_tex = crate::gl_bridge::create_output_texture(wpx, hpx, dst_params)?;
	let fbo = crate::gl_bridge::create_fbo(gl_tex, wpx, hpx)?;
	crate::gl_bridge::bind_fbo(fbo);
	crate::gl_bridge::set_viewport(wpx, hpx);

	// render action (OpenGLEnabled=1; real output texture name injected via GlCtx).
	let render_res = inst.render_gl(
		job.time,
		scale,
		window,
		job.renderer.clone().unwrap(),
		job.dst.clone(),
		Some(gl_tex),
	);

	// Defensively rebind FBO + viewport before read-back (per spec the plugin does not unbind the output,
	// but some may rebind/change the viewport — rebinding ensures glReadPixels reads the output texture).
	crate::gl_bridge::bind_fbo(fbo);
	crate::gl_bridge::set_viewport(wpx, hpx);

	// Read-back (FBO still bound, output GL texture still alive).
	let readback = crate::gl_bridge::read_pixels_to_image(wpx, hpx, dst_params);

	// Cleanup (guard drop clears current and releases the lock).
	crate::gl_bridge::delete_fbo(fbo);
	crate::gl_bridge::delete_gl_texture(gl_tex);

	render_res?;
	readback.map_err(|e| Error::Failed(format!("GL readback failed: {e}")))
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
			"output frame format {} is not F32 (Phase 2 constraint)",
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
	// SimpleSource fallback (pluginrenderer.cpp:1534-1543:
	// kOfxImageEffectSimpleSourceClipName resolves to k_texture_input, then falls back
	// to job.src).
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
		.ok_or_else(|| Error::Failed(format!("isIdentity references unknown clip {clip_name}")))?;
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
		// Double scalar NaN/Inf cleaning + Min/Max clamping
		// (pluginrenderer.cpp:155-177: bad values fall back to the default with a warning, then clamp to
		// kOfxParamPropMin/Max; multi-dimensional Double families get no such check in C++).
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
	// CPU texture: write in place; GPU texture: modify the downloaded frame then upload it back.
	let mut frame = render::texture_get_frame(dst)?;
	let params = frame.video_params();
	if params.format != render::PIXEL_FORMAT_F32 {
		return Err(Error::Failed(
			"output frame format is not F32 (Phase 2 constraint)".into(),
		));
	}
	let (w, h) = (params.width as usize, params.height as usize);
	let tight = w * image.components().channel_count() * 4;
	if tight != image.row_bytes() || tight * h != image.pixels().len() {
		return Err(Error::Failed(
			"image size does not match the output frame".into(),
		));
	}
	let dst_ptr = frame.data_mut();
	if dst_ptr.is_null() {
		return Err(Error::Failed("output frame has no data".into()));
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
		// Write back in place (value-type CPU texture: to_frame is a copy, must write back into the original).
		Texture::Cpu(f) => {
			f.data = frame.data;
		}
		// GPU destination texture: the copy only lands in the downloaded frame, upload it back via the backend.
		Texture::Gpu { token, ctx, .. } => {
			ctx.upload(*token, &frame)
				.map_err(|e| Error::Failed(format!("output texture upload failed: {e}")))?;
		}
		// Unresolved planar textures never become a plugin output target (the decode path resolves them first).
		Texture::Planar(_) => {
			return Err(Error::Failed(
				"planar texture cannot be a plugin output target".into(),
			));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests;
