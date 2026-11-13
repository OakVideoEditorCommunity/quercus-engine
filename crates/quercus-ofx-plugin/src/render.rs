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

//! quercusrender bridge (single-lib unification): texture/frame value types and the render call surface.
//!
//! quercusrender's C ABI has been removed (single-lib unification): textures are
//! [`quercus_core::texture::Texture`] (a value enum, no handle), CPU frames are
//! [`quercus_core::texture::Frame`]. This crate's render driver and GL suite
//! hold the value types directly:
//!
//! - [`Texture`] = [`quercus_core::texture::Texture`] (value alias;
//!   clone is reference semantics, drop frees the backend token automatically — the old `texture_free`/
//!   `frame_free` call surface is gone with the value model);
//! - [`Frame`] = [`quercus_core::texture::Frame`] (value alias);
//! - [`Renderer`] = `Arc<dyn quercus_core::backend::GpuContextLike>`
//!   (the renderer is the quercusrender backend context; the facade creates it via
//!   [`quercus_core::backend::GpuContext::create`]);
//! - [`VideoParams`] directly aliases quercusrender's
//!   [`quercus_core::frame::VideoParamsPod`] (same-layout POD);
//! - pixel-format constants directly alias [`quercus_core::PixelFormat`].
//!
//! Retained stubs (GPU-related, where the wgpu model has no direct Rust equivalent):
//! [`texture_id`] — quercusrender textures (wgpu/CPU) have no OpenGL texture name and
//! always return 0. **The real GL texture name in GL mode comes from [`crate::gl_bridge`]** (option B
//! has landed): the render driver creates the GL texture + FBO for the output frame, and the GL suite's
//! `OpenGLTextureIndex` property reports the real name from it; the use_opengl decision now uses
//! [`crate::gl_bridge::gl_available`] as the gate for "the target texture has a valid GL name".

/// `quercusrender_video_params` POD — single-lib unification: aliases the
/// quercusrender crate's struct (identical layout;
/// include/render/renderer.h:78).
pub type VideoParams = quercus_core::frame::VideoParamsPod;

/// The f32 value of olive::PixelFormat::Format.
pub const PIXEL_FORMAT_F32: i32 = quercus_core::PixelFormat::F32 as i32;
/// The u8 value of olive::PixelFormat::Format.
pub const PIXEL_FORMAT_U8: i32 = quercus_core::PixelFormat::U8 as i32;

/// The quercusrender renderer (the backend context; shared via Arc, GPU textures use it to upload/
/// download/blit — no separate renderer handle is needed).
pub type Renderer = std::sync::Arc<dyn quercus_core::backend::GpuContextLike>;

/// A quercusrender texture (a value type; a GPU or CPU wrapper).
pub type Texture = quercus_core::texture::Texture;

/// A quercusrender CPU frame (a value type).
pub type Frame = quercus_core::texture::Frame;

// ---- 桥调用面（值型实现；原 CHandle 桩随单库化重写）----------------------

/// Whether a texture is a placeholder (dummy: transparent black, never uploaded).
///
/// A placeholder texture has zero width/height and empty pixel data and represents the "no input / not yet rendered" slot;
/// the predicate lives in [`quercus_core::texture::Texture::is_dummy`] (GPU and
/// planar textures always return `false`).
pub fn texture_is_dummy(texture: &Texture) -> bool {
	texture.is_dummy()
}

/// The texture's CPU frame: a GPU texture is downloaded through the backend, a CPU texture is cloned
/// (the old `texture_get_frame` out-parameter form became a value return; an error is the old
/// "texture has no CPU frame" failure path).
pub fn texture_get_frame(texture: &Texture) -> crate::error::Result<Frame> {
	texture.to_frame().map_err(|e| {
		crate::error::Error::Failed(format!("纹理 readback 失败：{e}"))
	})
}

/// The texture's video parameters (size + pixel format; the old `texture_get_params`
/// out-parameter form became a value return).
///
/// The returned [`VideoParams`] is a POD value: width/height come from
/// [`quercus_core::texture::Texture::size`], the pixel format from
/// [`quercus_core::texture::Texture::format`] (stored as
/// [`quercus_core::PixelFormat`]'s f32 enum value), the other fields stay at their defaults.
/// A placeholder texture yields 0×0 and the default format.
pub fn texture_get_params(texture: &Texture) -> VideoParams {
	let (w, h) = texture.size();
	let mut p = VideoParams::default();
	p.width = w;
	p.height = h;
	p.format = texture.format() as i32;
	p
}

/// Build a CPU texture from video parameters and pixel data (the old `texture_create`
/// renderer+params+pixels form became a value form): rows are copied in row-major order using the `linesize` stride
/// (0 = tightly packed rows); F32/RGBA 4 channels is the pipeline norm. GPU upload is deferred
/// by the backend (wrapped-frame semantics, `Texture::wrap_frame`).
pub fn texture_create(
	params: &VideoParams,
	pixels: &[u8],
	linesize: i32,
) -> crate::error::Result<Texture> {
	use crate::error::Error;
	let mut frame = Frame::new();
	frame.set_video_params(*params);
	let w = frame.width.max(0) as usize;
	let h = frame.height.max(0) as usize;
	if w == 0 || h == 0 {
		return Err(Error::Invalid);
	}
	let bpc = frame.bytes_per_channel();
	if bpc == 0 {
		return Err(Error::Invalid);
	}
	// 行跨度推导通道数（RGB 3 通道等非常规分量）。
	if linesize > 0 && w > 0 && (linesize as usize).is_multiple_of(w * bpc) {
		frame.channels = (linesize as usize / (w * bpc)) as i32;
	}
	let tight = frame.linesize_bytes();
	let row = if linesize > 0 { linesize as usize } else { tight };
	if pixels.len() < row.saturating_mul(h) {
		return Err(Error::Invalid);
	}
	frame.data = vec![0u8; tight * h];
	for y in 0..h {
		let s = y * row;
		let d = y * tight;
		frame.data[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
	}
	Ok(Texture::wrap_frame(frame))
}

/// A quercusrender texture's texture name in the GL namespace. quercusrender textures (wgpu
/// Metal / CPU frames) have no OpenGL texture name and always return 0.
///
/// **The real texture name in GL mode is not provided by this function**: the host's own offscreen GL textures
/// (output frame + FBO attachment, input clip upload) are produced by [`crate::gl_bridge`] and
/// written out directly through the GL suite's `OpenGLTextureIndex` property; the use_opengl decision
/// uses [`crate::gl_bridge::gl_available`] as the gate for "the target texture has a valid GL name"
/// (the bridge can build a real GL texture for the target frame ⟺ it is available). See
/// [`crate::gl_bridge`] module docs for the evaluation history.
pub fn texture_id(_texture: &Texture) -> i32 {
	0
}

/// Whether the renderer is an OpenGL backend ([`quercus_core::backend::BackendKind::Gl`];
/// the old `renderer_is_open_gl` handle form became a backend-context kind query).
pub fn renderer_is_open_gl(renderer: &Renderer) -> bool {
	renderer.kind() == quercus_core::backend::BackendKind::Gl
}

/// A GL-kind marker context for [`RenderJob::renderer`]. The field's only
/// consumer is the use_opengl decision's kind check
/// ([`renderer_is_open_gl`]); the actual GL work (offscreen context, FBO,
/// readback) goes through [`crate::gl_bridge`], which needs no renderer
/// object. Passing this marker flips OpenGL-requiring plugins (the CImg
/// suite, which answers `kOfxStatErrMissingHostFeature` on the CPU path)
/// onto the real GL path in any process — including the headless
/// quercus-worker, where gl_bridge creates its own offscreen context.
pub struct GlKindMarker;

impl quercus_core::backend::GpuContextLike for GlKindMarker {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Gl
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

#[cfg(test)]
mod tests {
	use super::*;

	/// Test renderer: a minimal GpuContextLike fake (needs no GPU adapter).
	struct FakeGpu;
	impl quercus_core::backend::GpuContextLike for FakeGpu {
		fn kind(&self) -> quercus_core::backend::BackendKind {
			quercus_core::backend::BackendKind::Cpu
		}
		fn destroy_texture(&self, _token: u64) {}
		fn upload(&self, _token: u64, _frame: &Frame) -> quercus_render::error::Result<()> {
			Ok(())
		}
		fn download(&self, _token: u64) -> quercus_render::error::Result<Frame> {
			Ok(Frame::new())
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

	/// Texture creation: params + linesize-aware copy round trip (including RGB 3-channel inference).
	#[test]
	fn texture_create_linesize_aware() {
		let mut params = VideoParams::default();
		params.width = 3;
		params.height = 2;
		params.format = PIXEL_FORMAT_F32;
		// 紧凑 3×2 RGBA F32：3*4*4 = 48 B/行。
		let tight = 3 * 4 * 4;
		let mut pixels = vec![0u8; tight * 2];
		for (i, b) in pixels.iter_mut().enumerate() {
			*b = (i % 251) as u8;
		}
		let tex = texture_create(&params, &pixels, 0).expect("紧凑行应可创建");
		assert!(!texture_is_dummy(&tex));
		let frame = texture_get_frame(&tex).expect("CPU 帧应可读");
		assert_eq!(frame.width, 3);
		assert_eq!(frame.height, 2);
		assert_eq!(frame.channels, 4);
		assert_eq!(frame.linesize_bytes(), tight);
		assert_eq!(frame.data, pixels);
		// 行跨度变体（每行 4 字节填充）。
		let row = tight + 4;
		let mut padded = vec![0u8; row * 2];
		for y in 0..2 {
			padded[y * row..y * row + tight].copy_from_slice(&pixels[y * tight..(y + 1) * tight]);
		}
		let tex = texture_create(&params, &padded, row as i32).expect("带填充行应可创建");
		let frame = texture_get_frame(&tex).unwrap();
		assert_eq!(frame.data, pixels, "填充行应剥离为紧凑存储");
		// 像素不足 → Invalid。
		assert!(texture_create(&params, &padded[..row * 2 - 1], row as i32).is_err());
		// 无效尺寸 → Invalid。
		let mut bad = params;
		bad.width = 0;
		assert!(texture_create(&bad, &pixels, 0).is_err());
	}

	/// Texture parameter query and dummy semantics.
	#[test]
	fn texture_params_and_dummy() {
		let dummy = Texture::dummy();
		assert!(texture_is_dummy(&dummy));
		let p = texture_get_params(&dummy);
		assert_eq!((p.width, p.height), (0, 0));

		let mut params = VideoParams::default();
		params.width = 8;
		params.height = 4;
		params.format = PIXEL_FORMAT_F32;
		let tex = texture_create(&params, &[0u8; 8 * 4 * 4 * 4], 0).unwrap();
		let p = texture_get_params(&tex);
		assert_eq!((p.width, p.height), (8, 4));
		assert_eq!(p.format, PIXEL_FORMAT_F32);
	}

	/// Renderer kind query and GL decision (the wgpu backend has no GL texture name by design;
	/// the `texture_id` always-0 stub is not asserted here).
	#[test]
	fn renderer_kind_and_gl_id_stub() {
		let r: Renderer = std::sync::Arc::new(FakeGpu);
		assert!(!renderer_is_open_gl(&r));
	}
}
