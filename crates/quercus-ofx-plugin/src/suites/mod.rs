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

//! Suite layer: the C function tables that plugins call into (the concentrated
//! unsafe trampoline area).
//!
//! Each suite is a `#[repr(C)]` static instance of a function table; plugins
//! obtain one through [`fetch_suite`]. Every entry point follows the same safety
//! pattern:
//!
//! 1. Unwrap the handle (`RefBox`/`PropertySet` pointer); a null pointer →
//!    kOfxStatErrBadHandle;
//! 2. Call the safe Rust implementation;
//! 3. `catch_unwind` as a backstop → kOfxStatFailed.
//!
//! Reference: HS: ofxhImageEffect.cpp:2776 (the fetchSuite dispatch table and
//! version negotiation).

pub mod draw;
pub mod gl_render;
pub mod image_effect;
pub mod interact;
pub mod memory;
pub mod message;
pub mod multithread;
pub mod param;
pub mod parametric;
pub mod progress;
pub mod property;
pub mod timeline;

/// OFX status codes (the whole table matches SDK ofxCore.h:895-953 verbatim;
/// plugins compare status codes exactly, so any deviation silently breaks
/// compatibility).
pub mod status {
	/// kOfxStatOK: success.
	pub const OK: i32 = 0;
	/// kOfxStatFailed: generic failure.
	pub const FAILED: i32 = 1;
	/// kOfxStatErrFatal: fatal error.
	pub const ERR_FATAL: i32 = 2;
	/// kOfxStatErrUnknown: unknown object/property.
	pub const ERR_UNKNOWN: i32 = 3;
	/// kOfxStatErrMissingHostFeature: the host lacks the feature.
	pub const ERR_MISSING_HOST_FEATURE: i32 = 4;
	/// kOfxStatErrUnsupported: unsupported operation.
	pub const ERR_UNSUPPORTED: i32 = 5;
	/// kOfxStatErrExists: the object already exists.
	pub const ERR_EXISTS: i32 = 6;
	/// kOfxStatErrFormat: format error.
	pub const ERR_FORMAT: i32 = 7;
	/// kOfxStatErrMemory: out of memory.
	pub const ERR_MEMORY: i32 = 8;
	/// kOfxStatErrBadHandle: invalid handle (including NULL).
	pub const ERR_BAD_HANDLE: i32 = 9;
	/// kOfxStatErrBadIndex: index out of bounds.
	pub const ERR_BAD_INDEX: i32 = 10;
	/// kOfxStatErrValue: invalid value.
	pub const ERR_VALUE: i32 = 11;
	/// kOfxStatReplyYes.
	pub const REPLY_YES: i32 = 12;
	/// kOfxStatReplyNo.
	pub const REPLY_NO: i32 = 13;
	/// kOfxStatReplyDefault.
	pub const REPLY_DEFAULT: i32 = 14;
}

/// Handle convention: every OFX handle handed to a plugin = the address of the
/// object's `props` field OR the low 3 tag bits.
///
/// Preconditions (this crate's internal discipline, obeyed by the host when
/// creating objects):
/// - the object is `#[repr(C)]` and `props: PropertySet` sits at **offset 0** —
///   the handle address is the property-set address, isomorphic to C++ "base
///   class at offset 0" (in HS the OfxPropertySetHandle *is* Property::Set*),
///   so the property suite can dereference it directly with no reverse lookup;
/// - the object is heap-stable: `Arc<RefBox<T>>` / `Box<T>` elements inside a
///   structure (a Vec reallocation moves only the Box pointers, not the
///   payloads).
///
/// Tag-bit encoding: the low 3 bits (objects containing a Mutex/Arc are aligned
/// to ≥8, so the low 3 bits are always 0).
pub mod tag {
	/// Tag mask (the low 3 bits).
	pub const MASK: usize = 0b111;
	/// A bare property set (used directly inside the host / passed as `&props` by
	/// tests).
	pub const PROPERTY_SET: usize = 0;
	/// [`crate::descriptor::EffectDescriptor`] (the effect/param-set handle during
	/// describe).
	pub const DESCRIPTOR: usize = 1;
	/// [`crate::instance::Instance`] (the effect/param-set handle during the
	/// instance phase).
	pub const INSTANCE: usize = 2;
	/// [`crate::param::ParamDef`] (the param handle during describe).
	pub const PARAM_DEF: usize = 3;
	/// [`crate::param::ParamInstance`] (the param handle during the instance phase).
	pub const PARAM_INSTANCE: usize = 4;
	/// [`crate::clip::ClipInstance`] (the clip handle during the instance phase;
	/// during describe [`crate::descriptor::ClipDescriptor`] shares this tag — each
	/// type is only used in its own phase and the property suite does not tell them
	/// apart).
	pub const CLIP: usize = 5;
	/// [`crate::image::Image`] (the product of clipGetImage).
	pub const IMAGE: usize = 6;
	/// [`crate::suites::interact::Interact`] (the interact instance handle; a
	/// standalone interaction object alongside the instance-phase effect/param-set).
	pub const INTERACT: usize = 7;

	/// Tag a property-set pointer with the object type to produce an OFX handle for
	/// plugins.
	///
	/// `props` is the address of the property set at offset 0 inside the object (see
	/// the handle convention at the top of the module) and `t` is an object tag from
	/// [`tag`] (only the low 3 bits are used); the handle is the bitwise OR of the
	/// two. Called by the host when creating object handles, and public so the host
	/// and tests can build handles.
	pub fn make(props: *const crate::property::PropertySet, t: usize) -> *mut std::ffi::c_void {
		(props as usize | t) as *mut std::ffi::c_void
	}

	/// Strip the low 3 tag bits off a handle and get the property-set pointer back.
	///
	/// A bare property-set pointer (tag 0) is returned unchanged (see
	/// [`tag::PROPERTY_SET`]); the inverse of [`tag::make`]. Only the bits are
	/// cleared — the address is not validated.
	pub fn strip(handle: *mut std::ffi::c_void) -> *const crate::property::PropertySet {
		((handle as usize) & !MASK) as *const crate::property::PropertySet
	}

	/// Read the object tag from the low 3 bits of a handle.
	///
	/// The return value is one of the constants in [`tag`] (`PROPERTY_SET`,
	/// `DESCRIPTOR` … `INTERACT`); only a bit mask is applied — the handle is not
	/// dereferenced or validated, and the caller must ensure it was built by this
	/// crate's [`tag::make`].
	pub fn kind(handle: *mut std::ffi::c_void) -> usize {
		(handle as usize) & MASK
	}
}

/// Render context (TLS): the render driver sets it before calling a plugin
/// action, and suite callbacks (timeline, clipGetImage's scale, progress) read
/// it; corresponds to the instance's rendering-phase state in HS (a subset of
/// ofxhImageEffect.cpp's Render in_args).
#[derive(Clone, Copy)]
pub struct RenderCtx {
	/// The current render time (seconds).
	pub time: f64,
	/// The render scale.
	pub scale: crate::instance::RenderScale,
	/// The project time domain (timeline getTimeBounds).
	pub range: crate::instance::OfxRangeD,
}

thread_local! {
	static RENDER_CTX: std::cell::RefCell<Option<RenderCtx>> = const { std::cell::RefCell::new(None) };
}

/// Set/clear the render context (called by the render driver; it must be cleared
/// before the action returns; public: the render driver lives in the host layer
/// and tests inject it directly too).
pub fn set_render_ctx(ctx: Option<RenderCtx>) {
	RENDER_CTX.with(|c| *c.borrow_mut() = ctx);
}

/// Read the render context (None when there is no context; without TLS a
/// plugin's time query gets the headless default of 0).
pub(crate) fn render_ctx() -> Option<RenderCtx> {
	RENDER_CTX.with(|c| *c.borrow())
}

thread_local! {
	/// The current render's output image (set by the render driver; the image effect
	/// suite's clipGetImage returns it for the Output clip).
	static CURRENT_OUTPUT: std::cell::RefCell<Option<std::sync::Arc<crate::image::Image>>> =
		const { std::cell::RefCell::new(None) };
}

/// Set/clear the current output image (called by the render driver; it must be
/// cleared before the action returns).
pub(crate) fn set_current_output(image: Option<std::sync::Arc<crate::image::Image>>) {
	CURRENT_OUTPUT.with(|c| *c.borrow_mut() = image);
}

/// Read the current output image.
pub(crate) fn current_output() -> Option<std::sync::Arc<crate::image::Image>> {
	CURRENT_OUTPUT.with(|c| c.borrow().clone())
}

/// GL render context (TLS): the GL render driver sets it before calling a plugin
/// action, and the OpenGLRender suite's clipLoadTexture takes the current
/// renderer and output texture from it (corresponding to the instance's
/// rendering-phase GL state in C++; the "OpenGL Current Context" section of
/// ofxGPURender.h requires the host to hold the GL context during Render — this
/// design's convention is that the caller (quercusrender's PluginJob path) has
/// already made the renderer context current before entering render_job, and this
/// table only carries the renderer reference and the output-texture value).
#[derive(Clone)]
pub struct GlCtx {
	/// The current renderer (a quercusrender backend context).
	pub renderer: crate::render::Renderer,
	/// The attached output texture (the render target; in GL mode the plugin draws
	/// its result into it).
	pub output_texture: crate::render::Texture,
	/// The actual pixel depth of the current GL texture (a kOfxBitDepth* static
	/// string; Phase 2 is F32 throughout the chain and render_gl fills this in after
	/// negotiating against the plugin's kOfxOpenGLPropPixelDepth — the texture
	/// handle's kOfxImageEffectPropPixelDepth follows it).
	pub gl_pixel_depth: &'static str,
	/// The attached **real GL output texture name** (in GL mode, the GL texture the
	/// host builds for the output frame plus the FBO color attachment;
	/// clipLoadTexture(Output)'s OpenGLTextureIndex follows it — ofxGPURender.h
	/// requires the host to bind the output texture as the current color buffer).
	/// None = no real GL name (CPU fallback).
	pub output_gl_texture: Option<i32>,
}

thread_local! {
	static GL_CTX: std::cell::RefCell<Option<GlCtx>> = const { std::cell::RefCell::new(None) };
}

/// Set/clear the GL render context (called by the GL render driver; it must be
/// cleared before the action returns; public: tests inject it directly).
///
/// `Some` replaces the calling thread's TLS context and `None` clears it; this
/// only affects the calling thread, and the reading side is the module's
/// `gl_ctx` (without a context the GL suite returns
/// kOfxStatErrMissingHostFeature as specified).
pub fn set_gl_ctx(ctx: Option<GlCtx>) {
	GL_CTX.with(|c| *c.borrow_mut() = ctx);
}

/// Read the GL render context (None when there is no context — a
/// clipLoadTexture call outside GL rendering returns
/// kOfxStatErrMissingHostFeature as specified).
pub(crate) fn gl_ctx() -> Option<GlCtx> {
	GL_CTX.with(|c| c.borrow().clone())
}

/// fetchSuite host entry: returns a function-table pointer by name and version;
/// `None` when unknown or when the version does not match (the FFI layer turns
/// that into NULL).
///
/// `name` is the canonical OFX suite name (case-sensitive, exact match) and
/// `version` is the major version the plugin requests; the version must equal
/// exactly the one registered in this table and is not treated as a minimum
/// compatibility level. The returned pointer refers to a process-wide static
/// function table; callers must not free or modify it.
///
/// Phase 1 registers: OfxPropertySuite v1, OfxMemorySuite v1,
/// OfxImageEffectSuite v1, OfxParameterSuite v1, OfxMessageSuite v1/v2,
/// OfxProgressSuite v1/v2, OfxTimeLineSuite v1, OfxMultiThreadSuite v1.
/// Phase 2 adds: OfxImageEffectOpenGLRenderSuite v1 (the GL path); ofxColour has
/// no suite table (properties only plus the GetOutputColourspace action).
/// Phase 3 adds: OfxInteractSuite v1, OfxDrawSuite v1 (the interact host).
/// Phase 4 adds: OfxParametricParameterSuite v1 (curve parameters,
/// suites/parametric.rs).
pub fn fetch_suite(name: &str, version: i32) -> Option<*const std::ffi::c_void> {
	let suite: *const std::ffi::c_void = match (name, version) {
		("OfxPropertySuite", 1) => ptr(property::suite_v1()),
		("OfxMemorySuite", 1) => ptr(memory::suite_v1()),
		("OfxImageEffectSuite", 1) => ptr(image_effect::suite_v1()),
		("OfxParameterSuite", 1) => ptr(param::suite_v1()),
		("OfxMessageSuite", 1) => ptr(message::suite_v1()),
		("OfxMessageSuite", 2) => ptr(message::suite_v2()),
		("OfxProgressSuite", 1) => ptr(progress::suite_v1()),
		("OfxProgressSuite", 2) => ptr(progress::suite_v2()),
		("OfxTimeLineSuite", 1) => ptr(timeline::suite_v1()),
		("OfxMultiThreadSuite", 1) => ptr(multithread::suite_v1()),
		("OfxImageEffectOpenGLRenderSuite", 1) => ptr(gl_render::suite_v1()),
		("OfxInteractSuite", 1) => ptr(interact::suite_v1()),
		("OfxDrawSuite", 1) => ptr(draw::suite_v1()),
		("OfxParametricParameterSuite", 1) => ptr(parametric::suite_v1()),
		_ => return None,
	};
	Some(suite)
}

/// Turn a suite function-table reference into an opaque pointer (the common exit
/// of [`fetch_suite`]).
///
/// `p` must be a `'static` suite table (the static tables returned by each
/// suite's `suite_v1`/`suite_v2`), so the returned pointer stays valid for the
/// lifetime of the process and callers must not free it; this function only
/// converts a reference to a pointer and does not copy the table.
fn ptr<T>(p: &'static T) -> *const std::ffi::c_void {
	p as *const T as *const std::ffi::c_void
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Suite dispatch table: exact version matching; unknown version/name → None.
	#[test]
	fn fetch_suite_dispatch() {
		assert!(fetch_suite("OfxPropertySuite", 1).is_some());
		assert!(fetch_suite("OfxMemorySuite", 1).is_some());
		assert!(fetch_suite("OfxImageEffectSuite", 1).is_some());
		assert!(fetch_suite("OfxParameterSuite", 1).is_some());
		assert!(fetch_suite("OfxMessageSuite", 1).is_some());
		assert!(fetch_suite("OfxMessageSuite", 2).is_some());
		assert!(fetch_suite("OfxProgressSuite", 1).is_some());
		assert!(fetch_suite("OfxProgressSuite", 2).is_some());
		assert!(fetch_suite("OfxTimeLineSuite", 1).is_some());
		assert!(fetch_suite("OfxMultiThreadSuite", 1).is_some());
		assert!(fetch_suite("OfxImageEffectOpenGLRenderSuite", 1).is_some());
		assert!(fetch_suite("OfxInteractSuite", 1).is_some());
		assert!(fetch_suite("OfxDrawSuite", 1).is_some());
		assert!(fetch_suite("OfxParametricParameterSuite", 1).is_some());

		assert!(fetch_suite("OfxPropertySuite", 2).is_none());
		assert!(fetch_suite("OfxMessageSuite", 3).is_none());
		assert!(fetch_suite("OfxImageEffectOpenGLRenderSuite", 2).is_none());
		assert!(fetch_suite("OfxInteractSuite", 2).is_none());
		assert!(fetch_suite("OfxDrawSuite", 2).is_none());
		assert!(fetch_suite("OfxParametricParameterSuite", 2).is_none());
		assert!(fetch_suite("OfxBogusSuite", 1).is_none());
		assert!(fetch_suite("", 1).is_none());
	}

	/// Handle tags: make/strip/read-kind round-trip; the alignment bits of the
	/// address are 0.
	#[test]
	fn handle_tag_roundtrip() {
		let set = crate::property::PropertySet::new();
		let props = &set as *const crate::property::PropertySet;
		assert_eq!(props as usize & tag::MASK, 0, "PropertySet 必须 ≥8 对齐");
		for t in [
			tag::PROPERTY_SET,
			tag::DESCRIPTOR,
			tag::INSTANCE,
			tag::PARAM_DEF,
			tag::PARAM_INSTANCE,
			tag::CLIP,
			tag::IMAGE,
			tag::INTERACT,
		] {
			let h = tag::make(props, t);
			assert_eq!(tag::kind(h), t);
			assert_eq!(tag::strip(h) as usize, props as usize);
		}
	}

	/// Render-context TLS: set/read/clear.
	#[test]
	fn render_ctx_tls() {
		assert!(render_ctx().is_none());
		let ctx = RenderCtx {
			time: 1.5,
			scale: crate::instance::RenderScale { x: 2.0, y: 2.0 },
			range: crate::instance::OfxRangeD {
				min: 0.0,
				max: 100.0,
			},
		};
		set_render_ctx(Some(ctx));
		let got = render_ctx().unwrap();
		assert_eq!(got.time, 1.5);
		assert_eq!(got.scale.x, 2.0);
		assert_eq!(got.range.max, 100.0);
		set_render_ctx(None);
		assert!(render_ctx().is_none());
	}

	/// GL-context TLS: set/read/clear (the rendering-phase value clipLoadTexture
	/// reads).
	#[test]
	fn gl_ctx_tls() {
		assert!(gl_ctx().is_none());
		// 最小 GpuContextLike 假实现（无 GPU 适配器需求）。
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
		let renderer: crate::render::Renderer = std::sync::Arc::new(FakeGpu);
		let tex = crate::render::Texture::dummy();
		set_gl_ctx(Some(GlCtx {
			renderer,
			output_texture: tex.clone(),
			gl_pixel_depth: "OfxBitDepthFloat",
			output_gl_texture: Some(42),
		}));
		let got = gl_ctx().unwrap();
		assert_eq!(
			got.renderer.kind(),
			quercus_core::backend::BackendKind::Cpu
		);
		assert!(got.output_texture.is_dummy());
		assert_eq!(got.gl_pixel_depth, "OfxBitDepthFloat");
		assert_eq!(got.output_gl_texture, Some(42));
		set_gl_ctx(None);
		assert!(gl_ctx().is_none());
	}
}
