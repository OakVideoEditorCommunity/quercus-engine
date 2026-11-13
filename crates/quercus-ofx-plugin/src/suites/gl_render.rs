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

//! OfxImageEffectOpenGLRenderSuiteV1 (M11 phase 2): clipLoadTexture /
//! clipFreeTexture / flushResources.
//!
//! Semantics follow ofxGPURender.h (vendored, the OpenGL Render Suite
//! section) and the HostSupport plugin-side implementation
//! (HS: ofxhImageEffect.cpp:2296-2367):
//!
//! - `clipLoadTexture(clip, time, format, region, *texture)`: loads the clip's
//!   image at `time` as a GL texture. The texture handle is a **property set**
//!   (tag 0) holding the 12 properties prescribed by ofxGPURender.h
//!   (OpenGLTextureIndex/
//!   OpenGLTextureTarget/PixelDepth/Components/PreMultiplication/
//!   RenderScale/PixelAspectRatio/Bounds/RegionOfDefinition/RowBytes/
//!   Field/UniqueIdentifier); the host keeps a strong reference
//!   ([`LIVE_TEXTURES`]) and clipFreeTexture removes it, releasing the
//!   texture (matching HS's get/release pairing).
//!   **OpenGLTextureIndex is the real GL texture name** (GL mode, see
//!   [`crate::gl_bridge`] scheme B): for the Output clip it is the GL
//!   texture the host created for the output frame (injected via GlCtx;
//!   the render driver creates it, attaches it to an FBO, reads it back
//!   and deletes it); for an input clip it is the GL texture obtained by
//!   uploading the image through the bridge during GL rendering (deleted
//!   by clipFreeTexture). 0 = CPU fallback (no GL name; the plugin falls
//!   back as the spec allows).
//! - Output clip: `format` is ignored and the host returns the handle of
//!   the already-attached output texture — binding the render target
//!   (ofxGPURender.h "the host must bind the resulting texture as the
//!   current color buffer") is part of the caller's contract: the GL
//!   render driver has already attached the output GL texture to an FBO
//!   and bound it before entering render_gl (equivalent to C++'s
//!   `PluginRenderer::attach_output_texture`). clipFreeTexture only
//!   releases the Output handle without deleting the texture (the host
//!   still has to read it back; the render driver deletes it afterwards).
//! - Input textures in GL mode are uploaded through
//!   [`crate::gl_bridge::create_input_texture`] (CPU frame → GL texture);
//!   in CPU mode (no GL context) the CPU texture from [`crate::render`] is
//!   kept. Texture format: under the end-to-end F32 constraint the pixel
//!   depth follows the clip negotiation result (always F32); if the
//!   `format` argument (kOfxImageEffectGLFormat*) requests components that
//!   differ from the negotiated ones, phase 2 does not convert → Failed
//!   (the spec requires "host ensures it gives the requested
//!   format" — an explicit failure is better than silently returning the
//!   wrong format). ofxGPURender.h notes that the host need not remap the
//!   image to the plugin's requested bit depth per Clip Preferences, so
//!   the plugin goes by the texture handle's PixelDepth/Components.
//! - `flushResources`: the host does not cache GPU resources between
//!   renders (textures are released immediately by clipFreeTexture) →
//!   nothing to release → kOfxStatReplyDefault
//!   (spec: "nothing the host could do").
//! - GL context rule (ofxGPURender.h "OpenGL Current Context"): the host
//!   only requires the context to be current during
//!   Render/BeginSequenceRender/EndSequenceRender/Attach/Detach; this
//!   implementation's convention is that the render driver acquires once
//!   for the whole GL render action ([`crate::gl_bridge::acquire`]), so the
//!   context is always current during this suite's callbacks, and the
//!   renderer handle is taken from [`crate::suites::gl_ctx`] TLS.

use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::clip::ClipInstance;
use crate::image::Image;
use crate::property::{PropertySet, Value};
use crate::suites::{status, tag};

// ---- GL 常量（ofxGPURender.h；GLFormat 字符串为 ofxOpenGLRender.h
// OpenFX 1.4 的规范名，vendored 头文件是 stub 未收录，按规范定义）----

/// kOfxImageEffectPropOpenGLTextureIndex (ofxGPURender.h:135).
pub(crate) const GL_TEXTURE_INDEX: &str = "OfxImageEffectPropOpenGLTextureIndex";
/// kOfxImageEffectPropOpenGLTextureTarget (ofxGPURender.h:152).
pub(crate) const GL_TEXTURE_TARGET: &str = "OfxImageEffectPropOpenGLTextureTarget";

/// kOfxImageEffectGLFormatRGBA (ofxOpenGLRender.h 1.4).
pub(crate) const GL_FORMAT_RGBA: &str = "OfxImageEffectGLFormatRGBA";
/// kOfxImageEffectGLFormatRGB.
pub(crate) const GL_FORMAT_RGB: &str = "OfxImageEffectGLFormatRGB";
/// kOfxImageEffectGLFormatAlpha.
pub(crate) const GL_FORMAT_ALPHA: &str = "OfxImageEffectGLFormatAlpha";
/// kOfxImageEffectGLFormatLuminance.
pub(crate) const GL_FORMAT_LUMINANCE: &str = "OfxImageEffectGLFormatLuminance";
/// kOfxImageEffectGLFormatLuminanceAlpha.
pub(crate) const GL_FORMAT_LUMINANCE_ALPHA: &str = "OfxImageEffectGLFormatLuminanceAlpha";

/// GLenum value of GL_TEXTURE_2D (0x0DE1; the OpenGLTextureTarget
/// property of a texture handle. A GL spec value, not an OFX macro).
pub(crate) const GL_TEXTURE_2D: i32 = 0x0DE1;

/// The non-premultiplied default value of
/// kOfxImageEffectPropPreMultiplication.
pub(crate) const PREMULT_NONE: &str = "OfxImagePreMultipliedNone";

/// GL pixel-depth negotiation (ofxGPURender.h:65-89
/// kOfxOpenGLPropPixelDepth): the list of GL-render bit depths the plugin
/// descriptor declares as supported (optional).
///
/// A `Some(actual depth)` means GL mode is feasible; `None` means the
/// pipeline cannot satisfy the plugin's declaration → the host should fall
/// back to CPU rendering (ofxGPURender.h "the host will
/// try to provide buffers/textures in one of the supported formats";
/// phase 2 is end-to-end F32 and cannot provide other bit depths):
/// - list missing/empty → Some(Float) (host's choice, spec default);
/// - list contains Float → Some(Float);
/// - list present without Float → None (pipeline constraint; GL mode not
///   feasible).
pub(crate) fn pick_gl_pixel_depth(
	plugin_props: &crate::property::PropertySet,
) -> Option<&'static str> {
	use crate::property::Value;
	let dim = plugin_props.dimension(crate::host::PROP_GL_PIXEL_DEPTH);
	if dim == 0 {
		return Some("OfxBitDepthFloat");
	}
	for i in 0..dim {
		if let Some(Value::String(s)) = plugin_props.get(crate::host::PROP_GL_PIXEL_DEPTH, i) {
			if s.to_string_lossy() == "OfxBitDepthFloat" {
				return Some("OfxBitDepthFloat");
			}
		}
	}
	None
}

// ---- 存活纹理表 -----------------------------------------------------------

/// Live GL texture table: clipLoadTexture output (props address → property
/// set + texture value + whether it is the output clip + real GL texture
/// name [optional]); clipFreeTexture removes the entry and the texture is
/// released — matching HS's get/release pairing
/// (HS: ofxhImageEffect.cpp:2336-2351). A texture is a quercusrender value
/// (drop releases the backend token automatically; the old `texture_free`
/// call surface was removed with the value model).
///
/// `gl_texture`: in GL mode, the real GL texture name created for an input
/// clip through [`crate::gl_bridge`] (always None for the output clip — the
/// host creates it and owns its lifetime, see the [`crate::gl_bridge`]
/// module docs); it is deleted via [`delete_gl_texture_if_gl`] when the
/// entry is removed.
///
/// The property set must be **boxed** (Box gives a stable heap address):
/// the texture handle points at it and it must still be alive after the
/// function returns; a stack temporary would dangle (a bug in the initial
/// phase-2 implementation).
static LIVE_TEXTURES: std::sync::LazyLock<
	Mutex<HashMap<usize, (Box<PropertySet>, crate::render::Texture, bool, Option<i32>)>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers a texture (inside clipLoadTexture; the boxed `props` address
/// becomes the handle base). `gl_texture`: the real GL texture name (input
/// clip in GL mode; None if absent).
fn register(
	props: Box<PropertySet>,
	texture: crate::render::Texture,
	is_output: bool,
	gl_texture: Option<i32>,
) -> usize {
	let addr = &*props as *const PropertySet as usize;
	LIVE_TEXTURES
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(addr, (props, texture, is_output, gl_texture));
	addr
}

/// Deletes a GL texture (when the context is current; otherwise it is left
/// for the fallback before the next GL render — the normal paths
/// clipFreeTexture/purge all run during GL rendering, where the context is
/// necessarily current).
fn delete_gl_texture_if_gl(name: i32) {
	if crate::suites::gl_ctx().is_some() {
		crate::gl_bridge::delete_gl_texture(name);
	}
}

/// Releases all leftover input textures (the safety net after a GL render
/// action returns: the spec requires the plugin to clipFreeTexture every
/// handle before returning; missed input textures are released here, while
/// output textures are kept — the host still needs them). GL input textures
/// are deleted as well (context current; render_gl calls this before
/// clearing the GlCtx TLS).
pub(crate) fn purge_leftovers() {
	let mut live = LIVE_TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
	let dropped: Vec<Option<i32>> = live
		.iter()
		.filter(|(_, (_, _, is_output, _))| !*is_output)
		.map(|(_, (_, _, _, gl))| *gl)
		.collect();
	live.retain(|_, (_, _, is_output, _)| *is_output);
	drop(live);
	for gl in dropped.into_iter().flatten() {
		delete_gl_texture_if_gl(gl);
	}
}

/// Shared entry template: panic fallback.
///
/// `f` is the body of a suite function; it is wrapped in `catch_unwind`
/// (`AssertUnwindSafe`) so a panic returns `kOfxStatFailed` instead of
/// unwinding across the C ABI, `Ok(())` returns `kOfxStatOK`, and
/// `Err(code)` is returned to the plugin as-is. All `extern "C"` entry
/// points return their status code through it.
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	catch_unwind(AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	)
}

/// clip handle resolution (instance phase).
///
/// `handle` is the opaque clip handle the plugin received; when it is NULL
/// or its tag is not `tag::CLIP` it returns `Err(kOfxStatErrBadHandle)`,
/// otherwise the tag is stripped to yield `&'static ClipInstance` (instances
/// live for the session; the handle itself is a tagged pointer).
///
/// Precondition (the function is safe, but it dereferences a raw pointer
/// internally): `handle` must be a live clip handle registered by this
/// plugin, not a destroyed instance.
fn resolve_clip(handle: *mut c_void) -> Result<&'static ClipInstance, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::CLIP => Ok(&*(tag::strip(handle) as *const ClipInstance)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// Writes a rectangle as a 4-element Int property (used for Bounds/ROD; OFX
/// image properties are in pixel coordinates).
///
/// `props` is the target property set; `name` is the property name
/// (`OfxImagePropBounds` / `OfxImagePropRegionOfDefinition`); `rect` is an
/// f64 `OfxRectD` whose coordinates are rounded to `Int` in
/// `x1, y1, x2, y2` order and defined as a 4-element property. No bounds or
/// empty-rectangle validation is done; the caller guarantees `rect` is
/// already in pixel coordinates.
fn rect_props(props: &PropertySet, name: &str, rect: crate::instance::OfxRectD) {
	let b = |v: f64| Value::Int(v.round() as i32);
	props.define(name, vec![b(rect.x1), b(rect.y1), b(rect.x2), b(rect.y2)]);
}

/// Reads a string from a clip property (missing ones return the default).
///
/// `clip` is an instance-phase clip; `name` is the property name (e.g.
/// `OfxImageEffectPropPreMultiplication`); `default` is used when the
/// property is missing or is not a `Value::String`. The string is converted
/// lossily (invalid UTF-8 bytes are replaced, no error), so the returned
/// text is an approximation of the property content.
fn clip_string(clip: &ClipInstance, name: &str, default: &str) -> String {
	clip.props
		.get(name, 0)
		.map(|v| match v {
			Value::String(s) => s.to_string_lossy().into_owned(),
			_ => default.to_string(),
		})
		.unwrap_or_else(|| default.to_string())
}

/// Builds the texture property set (the properties prescribed by
/// ofxGPURender.h; shared by input and output, only the data source
/// differs). `OpenGLTextureIndex` is the real GL texture name
/// (`texture_index`; 0 = no GL name — under CPU fallback the plugin falls
/// back as the spec allows, and the texture content is still available via
/// clipGetImage).
#[allow(clippy::too_many_arguments)]
fn make_texture_props(
	width: f64,
	height: f64,
	components: crate::image::Components,
	depth: &str,
	premult: &str,
	par: f64,
	scale: crate::instance::RenderScale,
	row_bytes: i32,
	texture_index: i32,
) -> PropertySet {
	let props = PropertySet::new();
	props.set_one(GL_TEXTURE_INDEX, Value::Int(texture_index));
	props.set_one(GL_TEXTURE_TARGET, Value::Int(GL_TEXTURE_2D));
	props.set_one(
		crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
		Value::String(cs(depth)),
	);
	props.set_one(
		crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
		Value::String(cs(components.to_ofx())),
	);
	props.set_one(
		"OfxImageEffectPropPreMultiplication",
		Value::String(cs(premult)),
	);
	props.define(
		crate::host::PROP_RENDER_SCALE,
		vec![Value::Double(scale.x), Value::Double(scale.y)],
	);
	props.set_one("OfxImagePropPixelAspectRatio", Value::Double(par));
	let bounds = crate::instance::OfxRectD {
		x1: 0.0,
		y1: 0.0,
		x2: width,
		y2: height,
	};
	rect_props(&props, crate::image::K_IMAGE_PROP_BOUNDS, bounds);
	rect_props(&props, crate::image::K_IMAGE_PROP_ROD, bounds);
	props.set_one(crate::image::K_IMAGE_PROP_ROW_BYTES, Value::Int(row_bytes));
	props.set_one("OfxImagePropField", Value::String(cs("OfxFieldNone")));
	props.set_one(
		crate::image::K_IMAGE_PROP_UNIQUE_ID,
		Value::String(crate::image::unique_identifier()),
	);
	props
}

/// Texture request format → expected components (NULL/unknown → None =
/// host's choice).
///
/// `format` is the kOfxImageEffectGLFormat* name requested by the plugin
/// (may be NULL). Both RGBA and NULL map to `Some(Rgba)` (phase 2 always
/// provides RGBA), RGB / ALPHA map to the corresponding component; the
/// Luminance family has no matching `Components`, so it returns `None` like
/// unknown formats. The caller uses `None` to skip the format check (`None`
/// means "host's choice", not "request failed"); when a different
/// component count was explicitly requested [`clip_load_texture`] returns
/// Failed.
fn format_components(format: Option<&str>) -> Option<crate::image::Components> {
	match format {
		Some(GL_FORMAT_RGBA) | None => Some(crate::image::Components::Rgba),
		Some(GL_FORMAT_RGB) => Some(crate::image::Components::Rgb),
		Some(GL_FORMAT_ALPHA) => Some(crate::image::Components::Alpha),
		// Luminance 族：Phase 2 不建模（无对应 Components），宿主
		// 按 RGBA 上传并如实上报——格式核对走 None 分支。
		Some(GL_FORMAT_LUMINANCE) | Some(GL_FORMAT_LUMINANCE_ALPHA) => None,
		Some(_) => None,
	}
}

/// clipLoadTexture: loads the clip's image at `time` as a GL texture.
///
/// `format` is the requested texture format (kOfxImageEffectGLFormat*;
/// NULL = the host decides from the plugin's kOfxOpenGLPropPixelDepth —
/// phase 2 is end-to-end F32). `region` (spec coordinates, nullable) is
/// clipped to the clip's RoD — phase 2 supports only full frames (NULL); a
/// sub-region request returns Failed (the plugin continues per the spec,
/// treating it as black).
///
/// Parameters: `clip` is a clip handle registered in the instance phase
/// (`Output` takes a special branch); `time` is the requested time in
/// seconds on that clip's time base; `format` is a C string format name,
/// possibly NULL; `region` must currently be NULL; `out` receives the
/// property-set handle (`tag::PROPERTY_SET`, released by
/// [`clip_free_texture`]) and is written as NULL at call start.
///
/// The `Output` clip returns the already-attached output texture (`format`
/// is ignored; size comes from [`texture_size`]) with `OpenGLTextureIndex`
/// set to the real GL texture name created by the host (0 = CPU fallback).
/// Input clips prefer the zero-copy path on Linux/Windows: the render
/// driver has already imported the same GPU memory into GL, so the real
/// name is returned directly and download/upload is skipped (that entry
/// does not delete the GL texture on release; the driver cleans it up);
/// otherwise `fetch_image` fetches the image, uploads an RGBA32F GL texture
/// and creates a CPU texture; if the GL upload fails the texture index is
/// 0.
///
/// # Errors
///
/// `kOfxStatErrBadHandle`: `out` or `clip` is NULL, the handle is not CLIP,
/// or the Output texture size is non-positive.
/// `kOfxStatErrMissingHostFeature`: no GL context (not in GL rendering).
/// `kOfxStatFailed`: `region` is non-NULL, image fetch failed, the requested
/// format does not match the actual components (phase 2 does not convert),
/// or image size is non-positive.
/// `kOfxStatErrMemory`: CPU texture creation failed.
///
/// # Safety
///
/// `out` must be a writable `*mut *mut c_void`; `clip` must be a live clip
/// handle received by this suite; `format` if non-NULL must be a valid
/// NUL-terminated C string for the call duration; `region` if non-NULL must
/// point to a valid `OfxRectD` (currently always returns Failed and never
/// dereferences it). The GL context must be current at call time (acquired
/// by the render driver) and the call may only happen on the render thread;
/// the successfully written `*out` handle must be released exactly once.
unsafe extern "C" fn clip_load_texture(
	clip: *mut c_void,
	time: c_double,
	format: *const c_char,
	region: *const c_void,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *out = std::ptr::null_mut() };
		let c = resolve_clip(clip)?;
		// GL 上下文（TLS）：仅 GL 渲染期存在（ofxGPURender.h 的
		// "OpenGL Current Context" 规则）。
		let gl = crate::suites::gl_ctx().ok_or(status::ERR_MISSING_HOST_FEATURE)?;

		let scale = crate::suites::render_ctx()
			.map(|ctx| ctx.scale)
			.unwrap_or(crate::instance::RenderScale { x: 1.0, y: 1.0 });

		if c.name == "Output" {
			// Output：返回已附着的输出纹理（format 忽略；渲染目标
			// 绑定由调用方契约保证——等价 C++ attach_output_texture）。
			let tex = gl.output_texture.clone();
			let (w, h) = texture_size(&tex);
			if w <= 0.0 || h <= 0.0 {
				return Err(status::ERR_BAD_HANDLE);
			}
			// OpenGLTextureIndex = 宿主为输出帧建的真实 GL 纹理名
			// （render 驱动 GL 分支经 GlCtx 注入；0 = CPU 回退）。
			let index = gl.output_gl_texture.unwrap_or(0);
			let props = make_texture_props(
				w,
				h,
				crate::image::Components::Rgba,
				gl.gl_pixel_depth,
				&clip_string(c, "OfxImageEffectPropPreMultiplication", PREMULT_NONE),
				clip_par(c),
				scale,
				(w as i32) * 4 * 4,
				index,
			);
			let addr = register(Box::new(props), tex, true, None);
			unsafe { *out = tag::make(addr as *const PropertySet, tag::PROPERTY_SET) };
			return Ok(());
		}

		// Linux GPU 零拷贝：该 clip 的输入已由渲染驱动从同一块 Vulkan
		// 显存导入为 GL 纹理；直接返回真实名，跳过 fetch_image 下载
		// 与 create_input_texture 上传。
		#[cfg(any(target_os = "linux", target_os = "windows"))]
		if let Some((gl_name, gl_w, gl_h)) = c.input_gl_texture_info() {
			let tex = c
				.input_texture_value()
				.unwrap_or_else(crate::render::Texture::dummy);
			let premult = clip_string(c, "OfxImageEffectPropPreMultiplication", PREMULT_NONE);
			let row_bytes = gl_w.saturating_mul(16);
			let props = make_texture_props(
				gl_w as f64,
				gl_h as f64,
				crate::image::Components::Rgba,
				gl.gl_pixel_depth,
				&premult,
				clip_par(c),
				scale,
				row_bytes,
				gl_name,
			);
			// gl_texture=None：句柄释放时不删除 GL 纹理；渲染驱动统一
			// 清理导入的零拷贝纹理（避免 glDeleteTextures 重复）。
			let addr = register(Box::new(props), tex, false, None);
			unsafe { *out = tag::make(addr as *const PropertySet, tag::PROPERTY_SET) };
			return Ok(());
		}

		// 输入 clip：Phase 2 仅支持整帧（fetch_image 的 phase-1 约束）。
		if !region.is_null() {
			return Err(status::FAILED);
		}
		let format = if format.is_null() {
			None
		} else {
			unsafe { CStr::from_ptr(format) }.to_str().ok()
		};

		let image = c
			.fetch_image(time, scale, None)
			.map_err(|_| status::FAILED)?;
		let components = image.components();
		// 明确请求了不同分量 → Phase 2 不转换 → Failed（规范要求
		// 满足请求格式；静默给错格式比显式失败更糟）。
		if let Some(expected) = format_components(format) {
			if expected != components {
				return Err(status::FAILED);
			}
		}
		let premult = clip_string(c, "OfxImageEffectPropPreMultiplication", PREMULT_NONE);

		let (w, h) = (image_width(&image), image_height(&image));
		if w <= 0.0 || h <= 0.0 {
			return Err(status::FAILED);
		}
		// GL 模式：把输入图像上传成真实 GL 纹理（RGBA32F；上下文
		// current——render 驱动已 acquire），OpenGLTextureIndex 返回
		// 真实名；GL 上传失败 → 显式 Failed（插件按规范继续）。非 GL
		// 上下文（本函数仅在 GL 渲染期可达，此分支是 CPU 纹理兜底）：
		// 仍建 CPU 纹理、索引 0。
		let gl_tex =
			crate::gl_bridge::create_input_texture(w as i32, h as i32, image.pixels()).ok();
		let params = crate::render::VideoParams {
			width: w as i32,
			height: h as i32,
			format: crate::render::PIXEL_FORMAT_F32,
			..Default::default()
		};
		let tex = crate::render::texture_create(&params, image.pixels(), image.row_bytes() as i32)
			.map_err(|_| status::ERR_MEMORY)?;
		let props = make_texture_props(
			w,
			h,
			components,
			gl.gl_pixel_depth,
			&premult,
			clip_par(c),
			scale,
			image.row_bytes() as i32,
			gl_tex.unwrap_or(0),
		);
		let addr = register(Box::new(props), tex, false, gl_tex);
		unsafe { *out = tag::make(addr as *const PropertySet, tag::PROPERTY_SET) };
		Ok(())
	})
}

/// Texture size (via [`crate::render::texture_get_params`]; falls back to
/// 0,0 on failure).
///
/// `tex` is a CPU-side texture value; returns `(width, height)` as f64
/// pixels. Returns `(0.0, 0.0)` when the parameter query fails or the width
/// is non-positive (the caller treats that as unusable, e.g.
/// `clipLoadTexture` returns `kOfxStatErrBadHandle` when the Output texture
/// is empty).
fn texture_size(tex: &crate::render::Texture) -> (f64, f64) {
	let p = crate::render::texture_get_params(tex);
	if p.width > 0 {
		(p.width as f64, p.height as f64)
	} else {
		(0.0, 0.0)
	}
}

fn image_width(image: &Image) -> f64 {
	(image.bounds().x2 - image.bounds().x1).round()
}

fn image_height(image: &Image) -> f64 {
	(image.bounds().y2 - image.bounds().y1).round()
}

/// The clip's negotiated pixel aspect ratio.
///
/// `c` is an instance-phase clip; reads the `OfxImagePropPixelAspectRatio`
/// property (accepting only `Double`) and returns the OFX default 1.0 when
/// it is missing or of the wrong type. This is the host/plugin-negotiated
/// PAR and is written verbatim into `OfxImagePropPixelAspectRatio` by
/// [`make_texture_props`].
fn clip_par(c: &ClipInstance) -> f64 {
	c.props
		.get("OfxImagePropPixelAspectRatio", 0)
		.and_then(|v| match v {
			Value::Double(d) => Some(d),
			_ => None,
		})
		.unwrap_or(1.0)
}

/// clipFreeTexture: releases a texture (for an input clip it drops the
/// texture value and releases the real GL texture; for Output it only
/// releases the handle without deleting the texture — the host still has to
/// read it). A texture is a value: removing the table entry drops it (the
/// old `texture_free` call surface was removed with the value model).
///
/// `texture_handle` is the property-set handle returned by
/// [`clip_load_texture`] through `out`. A NULL handle or one not in the live
/// table → `kOfxStatErrBadHandle`; successfully removing the entry →
/// `kOfxStatOK`, and the real GL texture name registered in that entry (the
/// one uploaded/zero-copy imported for an input clip) is deleted with
/// `glDeleteTextures`; Output entries are not deleted (the host render still
/// reads it). The same handle must not be released twice.
///
/// # Safety
///
/// `texture_handle` must be a handle returned by this suite and not yet
/// released (or NULL); it must not be dereferenced after release. If the
/// entry carries a GL texture name, the GL context must be current at call
/// time (guaranteed by the render driver during GL rendering) and the
/// release may only happen on the render thread.
unsafe extern "C" fn clip_free_texture(texture_handle: *mut c_void) -> c_int {
	caught(|| {
		if texture_handle.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let addr = tag::strip(texture_handle) as usize;
		let entry = {
			let mut live = LIVE_TEXTURES.lock().unwrap_or_else(|e| e.into_inner());
			live.remove(&addr)
		};
		match entry {
			Some((_props, _texture, _is_output, gl_tex)) => {
				// 输入 clip 的真实 GL 纹理随释放删除（上下文 current）。
				if let Some(gl) = gl_tex {
					delete_gl_texture_if_gl(gl);
				}
				Ok(())
			}
			None => Err(status::ERR_BAD_HANDLE),
		}
	})
}

/// flushResources: the host caches no GPU resources → REPLY_DEFAULT (the
/// spec semantics "nothing the host could do").
///
/// No arguments and no state: the host has no cached textures/programs to
/// flush, so per ofxGPURender.h it replies `kOfxStatReplyDefault` and the
/// plugin should continue rendering normally. The function deletes no
/// resources (cleanup goes through [`clip_free_texture`] / `purge_leftovers`
/// at process exit).
///
/// # Safety
///
/// Only marked `unsafe` because of the C ABI function-pointer type: it
/// reads and writes no pointers and has no GL context precondition, so it
/// may be called on any thread.
unsafe extern "C" fn flush_resources() -> c_int {
	status::REPLY_DEFAULT
}

/// Function table layout (matches the SDK's
/// `OfxImageEffectOpenGLRenderSuiteV1` field by field;
/// ofxGPURender.h:181-310).
#[repr(C)]
pub struct GlRenderSuiteV1 {
	/// clipLoadTexture.
	pub clip_load_texture: unsafe extern "C" fn(
		*mut c_void,
		c_double,
		*const c_char,
		*const c_void,
		*mut *mut c_void,
	) -> c_int,
	/// clipFreeTexture.
	pub clip_free_texture: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// flushResources.
	pub flush_resources: unsafe extern "C" fn() -> c_int,
}

/// Function table instance.
pub fn suite_v1() -> &'static GlRenderSuiteV1 {
	static SUITE: std::sync::OnceLock<GlRenderSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| GlRenderSuiteV1 {
		clip_load_texture,
		clip_free_texture,
		flush_resources,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::property::{PropertySet, Value};

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	/// The GL render suite's live table is process-global state: every test in
	/// this module that touches it is serialized through this lock (otherwise
	/// purge_leftovers would step on other tests' registered entries).
	fn suite_lock() -> std::sync::MutexGuard<'static, ()> {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn clip(name: &str) -> ClipInstance {
		ClipInstance::from_descriptor(&crate::descriptor::ClipDescriptor::new(name))
	}

	fn clip_handle(c: &ClipInstance) -> *mut c_void {
		tag::make(&c.props as *const PropertySet, tag::CLIP)
	}

	fn f32_frame(w: i32, h: i32, fill: f32) -> crate::render::Frame {
		let params = crate::render::VideoParams {
			width: w,
			height: h,
			format: crate::render::PIXEL_FORMAT_F32,
			..Default::default()
		};
		let mut frame = crate::render::Frame::new();
		frame.set_video_params(params);
		assert!(frame.allocate());
		for c in frame.data.as_chunks_mut::<4>().0 {
			c.copy_from_slice(&fill.to_le_bytes());
		}
		frame
	}

	fn f32_texture(w: i32, h: i32, fill: f32) -> crate::render::Texture {
		crate::render::Texture::wrap_frame(f32_frame(w, h, fill))
	}

	struct FakeGpu;

	impl quercus_core::backend::GpuContextLike for FakeGpu {
		fn kind(&self) -> quercus_core::backend::BackendKind {
			quercus_core::backend::BackendKind::Cpu
		}
		fn destroy_texture(&self, _token: u64) {}
		fn upload(
			&self,
			_token: u64,
			_frame: &quercus_core::texture::Frame,
		) -> quercus_render::error::Result<()> {
			Ok(())
		}
		fn download(
			&self,
			_token: u64,
		) -> quercus_render::error::Result<quercus_core::texture::Frame> {
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

	fn gl_ctx(output: crate::render::Texture, index: Option<i32>) -> crate::suites::GlCtx {
		crate::suites::GlCtx {
			renderer: std::sync::Arc::new(FakeGpu),
			output_texture: output,
			gl_pixel_depth: "OfxBitDepthFloat",
			output_gl_texture: index,
		}
	}

	/// Reads the property set from a handle written by clipLoadTexture.
	fn handle_props(handle: *mut c_void) -> PropertySet {
		assert_eq!(tag::kind(handle), tag::PROPERTY_SET, "纹理句柄标签 0");
		unsafe { (*(tag::strip(handle))).clone() }
	}

	// `Value` 未实现 PartialEq（Pointer 无法比较）——按种类解包断言。

	fn prop_int(props: &PropertySet, name: &str, index: usize) -> i32 {
		match props.get(name, index) {
			Some(Value::Int(i)) => i,
			other => panic!("{name}[{index}] 应为 Int，got {other:?}"),
		}
	}

	fn prop_double(props: &PropertySet, name: &str, index: usize) -> f64 {
		match props.get(name, index) {
			Some(Value::Double(d)) => d,
			other => panic!("{name}[{index}] 应为 Double，got {other:?}"),
		}
	}

	fn prop_string(props: &PropertySet, name: &str, index: usize) -> String {
		match props.get(name, index) {
			Some(Value::String(s)) => s.to_string_lossy().into_owned(),
			other => panic!("{name}[{index}] 应为 String，got {other:?}"),
		}
	}

	/// GL pixel-depth negotiation matrix (ofxGPURender.h
	/// kOfxOpenGLPropPixelDepth): undeclared/contains Float → pipeline F32
	/// feasible; declared without Float → None (GL mode not feasible, fall back
	/// to CPU).
	#[test]
	fn pick_gl_pixel_depth_matrix() {
		let props = PropertySet::new();
		assert_eq!(pick_gl_pixel_depth(&props), Some("OfxBitDepthFloat"));

		let props2 = PropertySet::new();
		props2.define(
			crate::host::PROP_GL_PIXEL_DEPTH,
			vec![
				Value::String(cs("OfxBitDepthHalf")),
				Value::String(cs("OfxBitDepthByte")),
			],
		);
		assert_eq!(pick_gl_pixel_depth(&props2), None);

		let props3 = PropertySet::new();
		props3.define(
			crate::host::PROP_GL_PIXEL_DEPTH,
			vec![
				Value::String(cs("OfxBitDepthByte")),
				Value::String(cs("OfxBitDepthFloat")),
			],
		);
		assert_eq!(pick_gl_pixel_depth(&props3), Some("OfxBitDepthFloat"));

		let props4 = PropertySet::new();
		props4.define(
			crate::host::PROP_GL_PIXEL_DEPTH,
			vec![Value::String(cs("OfxBitDepthFloat"))],
		);
		assert_eq!(pick_gl_pixel_depth(&props4), Some("OfxBitDepthFloat"));

		// 列表存在但元素非字符串 → 跳过，不含 Float → None。
		let props5 = PropertySet::new();
		props5.define(crate::host::PROP_GL_PIXEL_DEPTH, vec![Value::Int(4)]);
		assert_eq!(pick_gl_pixel_depth(&props5), None);
	}

	/// Texture request format → expected components matrix (NULL/unknown →
	/// host's choice).
	#[test]
	fn format_components_matrix() {
		use crate::image::Components;
		assert_eq!(
			format_components(Some(GL_FORMAT_RGBA)),
			Some(Components::Rgba)
		);
		assert_eq!(format_components(None), Some(Components::Rgba));
		assert_eq!(
			format_components(Some(GL_FORMAT_RGB)),
			Some(Components::Rgb)
		);
		assert_eq!(
			format_components(Some(GL_FORMAT_ALPHA)),
			Some(Components::Alpha)
		);
		assert_eq!(format_components(Some(GL_FORMAT_LUMINANCE)), None);
		assert_eq!(format_components(Some(GL_FORMAT_LUMINANCE_ALPHA)), None);
		assert_eq!(format_components(Some("bogus")), None);
	}

	/// make_texture_props: all 12 properties prescribed by ofxGPURender.h are
	/// present and have the right values.
	#[test]
	fn make_texture_props_fields() {
		let props = make_texture_props(
			4.0,
			2.0,
			crate::image::Components::Rgb,
			"OfxBitDepthFloat",
			PREMULT_NONE,
			1.5,
			crate::instance::RenderScale { x: 2.0, y: 3.0 },
			48,
			7,
		);
		assert_eq!(prop_int(&props, GL_TEXTURE_INDEX, 0), 7);
		assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
		assert_eq!(
			prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0),
			"OfxBitDepthFloat"
		);
		assert_eq!(
			prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
			"OfxImageComponentRGB"
		);
		assert_eq!(
			prop_string(&props, "OfxImageEffectPropPreMultiplication", 0),
			PREMULT_NONE
		);
		assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 0), 2.0);
		assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 1), 3.0);
		assert_eq!(prop_double(&props, "OfxImagePropPixelAspectRatio", 0), 1.5);
		for (i, want) in [0, 0, 4, 2].iter().enumerate() {
			assert_eq!(
				prop_int(&props, crate::image::K_IMAGE_PROP_BOUNDS, i),
				*want,
				"Bounds[{i}]"
			);
			assert_eq!(
				prop_int(&props, crate::image::K_IMAGE_PROP_ROD, i),
				*want,
				"RoD[{i}]"
			);
		}
		assert_eq!(
			prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
			48
		);
		assert_eq!(prop_string(&props, "OfxImagePropField", 0), "OfxFieldNone");
		assert!(matches!(
			props.get(crate::image::K_IMAGE_PROP_UNIQUE_ID, 0),
			Some(Value::String(_))
		));
	}

	/// rect_props: a Double rectangle rounds to Int×4 (negative half away from
	/// zero).
	#[test]
	fn rect_props_rounds_to_int() {
		let props = PropertySet::new();
		rect_props(
			&props,
			"r",
			crate::instance::OfxRectD {
				x1: 1.4,
				y1: -0.6,
				x2: 3.5,
				y2: 2.49,
			},
		);
		for (i, want) in [1, -1, 4, 2].iter().enumerate() {
			assert_eq!(prop_int(&props, "r", i), *want, "r[{i}]");
		}
	}

	/// clip_string / clip_par: string and non-string fallbacks, PAR defaults to
	/// 1.0 when missing.
	#[test]
	fn clip_string_and_par_fallbacks() {
		let c = clip("Source");
		assert_eq!(clip_string(&c, "missing", "def"), "def");
		assert_eq!(clip_par(&c), 1.0);

		c.props.set_one("s", Value::String(cs("xyz")));
		assert_eq!(clip_string(&c, "s", "def"), "xyz");
		c.props.set_one("s2", Value::Int(3));
		assert_eq!(clip_string(&c, "s2", "def"), "def");

		c.props
			.set_one("OfxImagePropPixelAspectRatio", Value::Double(2.0));
		assert_eq!(clip_par(&c), 2.0);
		c.props
			.set_one("OfxImagePropPixelAspectRatio", Value::Int(2));
		assert_eq!(clip_par(&c), 1.0, "非 Double 回退 1.0");
	}

	/// texture_size / image_width / image_height.
	#[test]
	fn texture_size_and_image_dims() {
		assert_eq!(texture_size(&crate::render::Texture::dummy()), (0.0, 0.0));
		assert_eq!(texture_size(&f32_texture(3, 2, 0.0)), (3.0, 2.0));

		let img = Image::allocate(
			crate::image::BitDepth::Float,
			crate::image::Components::Rgba,
			crate::instance::OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: 5.0,
				y2: 4.0,
			},
		);
		assert_eq!(image_width(&img), 5.0);
		assert_eq!(image_height(&img), 4.0);
	}

	/// resolve_clip: null handle / non-CLIP tag → BadHandle; a CLIP tag is
	/// valid.
	#[test]
	fn resolve_clip_handle_rules() {
		assert!(matches!(
			resolve_clip(std::ptr::null_mut()),
			Err(status::ERR_BAD_HANDLE)
		));
		let props = PropertySet::new();
		let wrong = tag::make(&props as *const PropertySet, tag::PROPERTY_SET);
		assert!(matches!(resolve_clip(wrong), Err(status::ERR_BAD_HANDLE)));
		let c = clip("Source");
		assert!(resolve_clip(clip_handle(&c)).is_ok());
	}

	/// clipLoadTexture: entry guards (null out / null clip / non-clip handle /
	/// no GL context).
	#[test]
	fn clip_load_texture_entry_guards() {
		let _lock = suite_lock();
		crate::suites::set_gl_ctx(None);
		let s = suite_v1();
		let c = clip("Source");
		let name = cs(GL_FORMAT_RGBA);
		let mut sentinel = 0u8;
		let mut out: *mut c_void = &mut sentinel as *mut u8 as *mut c_void;
		unsafe {
			// 空 out → BadHandle（且不触碰返回值）。
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					name.as_ptr(),
					std::ptr::null(),
					std::ptr::null_mut(),
				),
				status::ERR_BAD_HANDLE
			);
			// 空 clip 句柄 → BadHandle。
			assert_eq!(
				(s.clip_load_texture)(
					std::ptr::null_mut(),
					0.0,
					name.as_ptr(),
					std::ptr::null(),
					&mut out,
				),
				status::ERR_BAD_HANDLE
			);
			assert!(out.is_null(), "入口先把 out 置空");
			// 非 clip 标签 → BadHandle。
			let wrong = tag::make(&c.props as *const PropertySet, tag::PROPERTY_SET);
			assert_eq!(
				(s.clip_load_texture)(wrong, 0.0, name.as_ptr(), std::ptr::null(), &mut out),
				status::ERR_BAD_HANDLE
			);
			// 无 GL 上下文 → MissingHostFeature（GL suite 只在 GL 渲染期可达）。
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					name.as_ptr(),
					std::ptr::null(),
					&mut out,
				),
				status::ERR_MISSING_HOST_FEATURE
			);
		}
	}

	/// clipLoadTexture(Output): ignores format and returns the handle of the
	/// attached output texture (the real GL name is reported through
	/// OpenGLTextureIndex); free only removes the handle.
	#[test]
	fn clip_load_texture_output_clip() {
		let _lock = suite_lock();
		let s = suite_v1();
		let out_clip = clip("Output");
		let tex = f32_texture(4, 2, 0.25);
		crate::suites::set_gl_ctx(Some(gl_ctx(tex, Some(9))));
		let rgb = cs(GL_FORMAT_RGB); // Output 忽略请求格式。
		let mut handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&out_clip),
					0.0,
					rgb.as_ptr(),
					std::ptr::null(),
					&mut handle,
				),
				status::OK
			);
		}
		let props = handle_props(handle);
		assert_eq!(prop_int(&props, GL_TEXTURE_INDEX, 0), 9);
		assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
		assert_eq!(
			prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
			"OfxImageComponentRGBA"
		);
		assert_eq!(prop_int(&props, crate::image::K_IMAGE_PROP_BOUNDS, 2), 4);
		// 行字节 = w × RGBA × F32；OpenGLTextureIndex=0 变体（CPU 回退）。
		assert_eq!(
			prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
			4 * 4 * 4
		);
		assert!(LIVE_TEXTURES
			.lock()
			.unwrap()
			.contains_key(&(tag::strip(handle) as usize)));

		// free：输出 clip 只释放句柄、不删纹理（宿主还要回读）。
		unsafe {
			assert_eq!((s.clip_free_texture)(handle), status::OK);
			// 二次释放 / 空句柄 → BadHandle。
			assert_eq!((s.clip_free_texture)(handle), status::ERR_BAD_HANDLE);
			assert_eq!(
				(s.clip_free_texture)(std::ptr::null_mut()),
				status::ERR_BAD_HANDLE
			);
		}
		assert!(!LIVE_TEXTURES
			.lock()
			.unwrap()
			.contains_key(&(tag::strip(handle) as usize)));

		// output_gl_texture = None → 索引 0（CPU 回退语义）。
		crate::suites::set_gl_ctx(Some(gl_ctx(f32_texture(2, 2, 0.0), None)));
		let mut h0: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&out_clip),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut h0,
				),
				status::OK
			);
		}
		assert_eq!(prop_int(&handle_props(h0), GL_TEXTURE_INDEX, 0), 0);
		unsafe {
			assert_eq!((s.clip_free_texture)(h0), status::OK);
		}
		crate::suites::set_gl_ctx(None);
	}

	/// clipLoadTexture(Output): attached texture is dummy/0-sized → BadHandle.
	#[test]
	fn clip_load_texture_output_rejects_empty_texture() {
		let _lock = suite_lock();
		let s = suite_v1();
		let out_clip = clip("Output");
		crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
		let mut handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&out_clip),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut handle,
				),
				status::ERR_BAD_HANDLE
			);
		}
		crate::suites::set_gl_ctx(None);
	}

	/// clipLoadTexture(input clip): full-frame restriction, format matching,
	/// negotiated properties and release; OpenGLTextureIndex=0 when GL is
	/// unavailable (CPU fallback).
	#[test]
	fn clip_load_texture_input_clip() {
		let _lock = suite_lock();
		// Linux/macOS 真实 GL 桥：clipLoadTexture 的 GL 上传要求上下文
		// current（生产路径由 render_driver 的 GlGuard 保证）。
		let _gl = crate::gl_bridge::acquire().ok();
		let s = suite_v1();
		let c = clip("Source");
		c.set_input_texture(Some(f32_texture(2, 2, 0.5)), 0.0);
		crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));

		// 子区域请求（region 非空）→ Failed（Phase 2 仅整帧）。
		let region = crate::instance::OfxRectD::default();
		unsafe {
			let mut h: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					std::ptr::null(),
					&region as *const crate::instance::OfxRectD as *const c_void,
					&mut h,
				),
				status::FAILED
			);
		}

		// 未挂输入纹理 → Failed（fetch_image NotFound 映射）。
		let empty = clip("Source");
		unsafe {
			let mut h: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&empty),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut h,
				),
				status::FAILED
			);
		}

		// 分量不符：请求 RGB 但图像 RGBA（协商默认）→ Failed。
		let rgb = cs(GL_FORMAT_RGB);
		unsafe {
			let mut h: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				(s.clip_load_texture)(clip_handle(&c), 0.0, rgb.as_ptr(), std::ptr::null(), &mut h,),
				status::FAILED
			);
		}

		// 成功（format=NULL）：纹理句柄属性齐全。
		let mut handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut handle,
				),
				status::OK
			);
		}
		let props = handle_props(handle);
		assert_eq!(
			prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0),
			"OfxImageComponentRGBA"
		);
		assert_eq!(
			prop_string(&props, crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0),
			"OfxBitDepthFloat"
		);
		assert_eq!(
			prop_int(&props, crate::image::K_IMAGE_PROP_ROW_BYTES, 0),
			2 * 4 * 4
		);
		assert_eq!(prop_int(&props, GL_TEXTURE_TARGET, 0), GL_TEXTURE_2D);
		// 非 GL（Linux/Windows stub）→ 索引 0；macOS 有 GL 时 > 0。
		let index = match props.get(GL_TEXTURE_INDEX, 0) {
			Some(Value::Int(i)) => i,
			other => panic!("OpenGLTextureIndex 应为 Int，got {other:?}"),
		};
		if crate::gl_bridge::gl_available() {
			assert!(index > 0, "GL 可用时输入纹理应有真实 GL 名");
		} else {
			assert_eq!(index, 0, "无 GL 时索引为 0");
		}

		// 请求 RGBA（显式）与请求 Luminance 族（None = 宿主自选）都成功。
		let rgba = cs(GL_FORMAT_RGBA);
		let lum = cs(GL_FORMAT_LUMINANCE);
		let lum_a = cs(GL_FORMAT_LUMINANCE_ALPHA);
		let mut h2: *mut c_void = std::ptr::null_mut();
		let mut h3: *mut c_void = std::ptr::null_mut();
		let mut h4: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					rgba.as_ptr(),
					std::ptr::null(),
					&mut h2
				),
				status::OK
			);
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					lum.as_ptr(),
					std::ptr::null(),
					&mut h3
				),
				status::OK
			);
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					lum_a.as_ptr(),
					std::ptr::null(),
					&mut h4
				),
				status::OK
			);
			// 非 UTF-8 format 串按 None 处理（宿主自选）→ OK。
			let bad = CString::new(vec![0xFFu8]).unwrap();
			let mut h5: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					bad.as_ptr(),
					std::ptr::null(),
					&mut h5,
				),
				status::OK
			);
			// free 输入纹理：摘表（GL 模式同时删真实 GL 纹理）。
			assert_eq!((s.clip_free_texture)(h2), status::OK);
			assert_eq!((s.clip_free_texture)(h3), status::OK);
			assert_eq!((s.clip_free_texture)(h4), status::OK);
			assert_eq!((s.clip_free_texture)(h5), status::OK);
			assert_eq!((s.clip_free_texture)(handle), status::OK);
		}
		crate::suites::set_gl_ctx(None);
	}

	/// clipLoadTexture: the render context's scale flows into the texture
	/// properties; a 0-sized image → Failed.
	#[test]
	fn clip_load_texture_uses_render_scale_and_rejects_zero_image() {
		let _lock = suite_lock();
		let s = suite_v1();

		// 有 render ctx：scale 写入纹理句柄的 RenderScale 属性。
		let c = clip("Source");
		c.set_input_texture(Some(f32_texture(2, 2, 0.0)), 0.0);
		crate::suites::set_render_ctx(Some(crate::suites::RenderCtx {
			time: 1.0,
			scale: crate::instance::RenderScale { x: 2.0, y: 2.0 },
			range: crate::instance::OfxRangeD { min: 0.0, max: 1.0 },
		}));
		crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
		let mut handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&c),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut handle,
				),
				status::OK
			);
		}
		let props = handle_props(handle);
		assert_eq!(prop_double(&props, crate::host::PROP_RENDER_SCALE, 0), 2.0);
		unsafe {
			assert_eq!((s.clip_free_texture)(handle), status::OK);
		}
		crate::suites::set_render_ctx(None);

		// 0×0 帧（非 dummy：data 非空）→ image_width == 0 → Failed。
		let zero = clip("Source");
		let params = crate::render::VideoParams {
			format: crate::render::PIXEL_FORMAT_F32,
			..Default::default()
		};
		let mut frame = crate::render::Frame::new();
		frame.set_video_params(params);
		frame.data = vec![0u8; 16];
		zero.set_input_texture(Some(crate::render::Texture::wrap_frame(frame)), 0.0);
		let mut h: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&zero),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut h,
				),
				status::FAILED
			);
		}
		crate::suites::set_gl_ctx(None);
	}

	/// purge_leftovers: releases leftover input handles and keeps the output
	/// handle (the host still has to read it back).
	#[test]
	fn purge_leftovers_keeps_outputs() {
		let _lock = suite_lock();
		let s = suite_v1();
		let input = clip("Source");
		input.set_input_texture(Some(f32_texture(2, 2, 0.0)), 0.0);
		let out_clip = clip("Output");
		crate::suites::set_gl_ctx(Some(gl_ctx(f32_texture(2, 2, 0.0), Some(1))));
		let mut input_handle: *mut c_void = std::ptr::null_mut();
		let mut out_handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&input),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut input_handle,
				),
				status::OK
			);
			assert_eq!(
				(s.clip_load_texture)(
					clip_handle(&out_clip),
					0.0,
					std::ptr::null(),
					std::ptr::null(),
					&mut out_handle,
				),
				status::OK
			);
		}
		assert!(LIVE_TEXTURES
			.lock()
			.unwrap()
			.contains_key(&(tag::strip(input_handle) as usize)));
		assert!(LIVE_TEXTURES
			.lock()
			.unwrap()
			.contains_key(&(tag::strip(out_handle) as usize)));

		purge_leftovers();
		let live = LIVE_TEXTURES.lock().unwrap();
		assert!(
			!live.contains_key(&(tag::strip(input_handle) as usize)),
			"输入句柄应被兜底释放"
		);
		assert!(
			live.contains_key(&(tag::strip(out_handle) as usize)),
			"输出句柄应保留"
		);
		drop(live);
		unsafe {
			assert_eq!((s.clip_free_texture)(out_handle), status::OK);
		}
		crate::suites::set_gl_ctx(None);
	}

	/// delete_gl_texture_if_gl: no-op without a GL context; with a context it
	/// deletes through the bridge (the bridge is a stub when GL is unavailable,
	/// so this only verifies it does not panic).
	#[test]
	fn delete_gl_texture_if_gl_branches() {
		let _lock = suite_lock();
		crate::suites::set_gl_ctx(None);
		delete_gl_texture_if_gl(3);
		crate::suites::set_gl_ctx(Some(gl_ctx(crate::render::Texture::dummy(), None)));
		delete_gl_texture_if_gl(4);
		crate::suites::set_gl_ctx(None);
	}

	/// flushResources: the host caches no GPU resources → ReplyDefault.
	#[test]
	fn flush_resources_replies_default() {
		unsafe {
			assert_eq!((suite_v1().flush_resources)(), status::REPLY_DEFAULT);
		}
	}
}
