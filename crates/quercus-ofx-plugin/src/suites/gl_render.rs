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

// ---- GL constants (ofxGPURender.h; the GLFormat strings are the canonical
// OpenFX 1.4 names from ofxOpenGLRender.h — the vendored header is a stub that
// omits them, so they are defined per the spec) ----

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

// ---- Live texture table --------------------------------------------------

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
		// Luminance family: phase 2 does not model it (no matching Components);
		// the host uploads as RGBA and reports it faithfully — the format check
		// takes the None branch.
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
		// GL context (TLS): only present during GL rendering (the
		// "OpenGL Current Context" rule in ofxGPURender.h).
		let gl = crate::suites::gl_ctx().ok_or(status::ERR_MISSING_HOST_FEATURE)?;

		let scale = crate::suites::render_ctx()
			.map(|ctx| ctx.scale)
			.unwrap_or(crate::instance::RenderScale { x: 1.0, y: 1.0 });

		if c.name == "Output" {
			// Output: returns the already-attached output texture (format is
			// ignored; render-target binding is guaranteed by the caller's
			// contract — equivalent to C++ attach_output_texture).
			let tex = gl.output_texture.clone();
			let (w, h) = texture_size(&tex);
			if w <= 0.0 || h <= 0.0 {
				return Err(status::ERR_BAD_HANDLE);
			}
			// OpenGLTextureIndex = the real GL texture name the host created for
			// the output frame (injected by the render driver's GL branch via GlCtx;
			// 0 = CPU fallback).
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

		// Linux GPU zero-copy: the render driver has already imported this
		// clip's input from the same Vulkan GPU memory as a GL texture; return
		// the real name directly, skipping the fetch_image download and the
		// create_input_texture upload.
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
			// gl_texture=None: releasing the handle does not delete the GL
			// texture; the render driver cleans up imported zero-copy textures
			// uniformly (avoids a duplicate glDeleteTextures).
			let addr = register(Box::new(props), tex, false, None);
			unsafe { *out = tag::make(addr as *const PropertySet, tag::PROPERTY_SET) };
			return Ok(());
		}

		// Input clip: phase 2 only supports full frames (phase-1 constraint of fetch_image).
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
		// A different component count was explicitly requested → phase 2 does
		// not convert → Failed (the spec requires satisfying the request; a
		// silent wrong format is worse than an explicit failure).
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
		// GL mode: upload the input image as a real GL texture (RGBA32F; the
		// context is current — the render driver already acquired it) and report
		// the real name via OpenGLTextureIndex; a GL upload failure → explicit
		// Failed (the plugin continues per the spec). Without a GL context (this
		// function is only reachable during GL rendering; this branch is the CPU
		// texture fallback): still create a CPU texture, index 0.
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
				// The input clip's real GL texture is deleted on release (context current).
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
mod tests;
