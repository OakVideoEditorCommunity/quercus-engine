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

//! Clip instance: the clip ↔ quercusrender texture bridge.
//!
//! Corresponds to C++ `OliveClipInstance`. Texture data flows through
//! [`crate::render`] (the quercusrender value types: `Texture`/`Frame`); the
//! OFX side only ever sees [`crate::image::Image`] (the CPU path).
//!
//! `#[repr(C)]` + props at offset 0 (the handle convention, see
//! [`crate::suites::tag`]; the clip handle is `&props`).
//!
//! Since the single-library merge quercusrender's ffi is gone: frame access
//! goes through the [`quercus_core::texture::Texture::to_frame`] value path
//! (GPU textures are downloaded through the backend, CPU textures cloned), and
//! frames are released automatically when the value drops (the former
//! `texture_get_frame`/`frame_free` handle-call surface is gone with the stub).

use crate::instance::{OfxRangeD, OfxRectD, RenderScale};
use crate::property::PropertySet;

/// A clip instance.
#[repr(C)]
pub struct ClipInstance {
	/// Instance-level clip properties (current components/bit depth/pixel aspect;
	/// negotiated results are written here; offset 0, handle convention).
	pub props: PropertySet,
	/// The clip name.
	pub name: String,
	/// The current input texture (a quercusrender value; None for the output clip).
	input_texture: std::sync::Mutex<Option<crate::render::Texture>>,
	/// Linux zero-copy: the GL texture imported from the input's
	/// `OPAQUE_FD` memory (same pixels, no CPU download/upload).
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	input_gl_texture: std::sync::Mutex<Option<crate::gl_bridge::GlMemoryTexture>>,
	/// The current output texture (phase 1's single slot for C++
	/// `output_textures_`; the write-back target of
	/// [`store_output_image`](Self::store_output_image); None for the input clip).
	output_texture: std::sync::Mutex<Option<crate::render::Texture>>,
}

/// Read the negotiated component layout back from the clip properties.
///
/// Reads `OfxImageEffectPropComponents`, written by `getClipPreferences`
/// (`OfxImageComponentRGBA` / `RGB` / `Alpha`); returns `None` when the property
/// is missing, the value is not a string, or the string is unknown. The caller
/// ([`ClipInstance::fetch_image`]) falls back to `Rgba` on `None`.
fn components_from_props(props: &PropertySet) -> Option<crate::image::Components> {
	use crate::property::Value;
	match props.get(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0)? {
		Value::String(s) => match s.to_string_lossy().as_ref() {
			"OfxImageComponentRGBA" => Some(crate::image::Components::Rgba),
			"OfxImageComponentRGB" => Some(crate::image::Components::Rgb),
			"OfxImageComponentAlpha" => Some(crate::image::Components::Alpha),
			_ => None,
		},
		_ => None,
	}
}

/// IEEE 754 half precision → single precision (a local alias for
/// [`crate::image::f16_to_f32`] that keeps the call sites readable).
fn f16_to_f32(bits: u16) -> f32 {
	crate::image::f16_to_f32(bits)
}

impl ClipInstance {
	/// Instantiate from a descriptor (called by the createInstance path; public:
	/// both the host and tests need to construct clip instances). The instance props
	/// are a deep copy of the descriptor props (HS: ClipBase's instance constructor,
	/// ofxhClip.cpp:57-70 — plugins read supported components etc. during the
	/// instance phase).
	///
	/// ofxColour (M11 §4): the host writes the input clip's
	/// kOfxImageClipPropColourspace as the working space (ACEScg, ofxColour.h
	/// "Hosts should set this property to the colourspace of the input clip.
	/// Typically it will be set to the working colourspace"); the output clip
	/// is written after the GetOutputColourspace action.
	pub fn from_descriptor(desc: &crate::descriptor::ClipDescriptor) -> Self {
		let props = desc.props.clone();
		let name = desc.name.clone();
		if name != "Output" {
			// The working colorspace follows the pipeline setting (project
			// property): ACEScg in the default pipeline, sRGB in the legacy
			// pass-through mode — plugins must be told the true space of the
			// pixels they receive.
			props.set_one(
				crate::host::PROP_CLIP_COLOURSPACE,
				crate::property::Value::String(
					std::ffi::CString::new(quercus_core::color::pipeline_working_ofx_name())
						.unwrap(),
				),
			);
		}
		// OfxImageClipPropConnected（ofxsImageEffect.cpp:1106 的
		// `Clip::isConnected()` 是无默认值强读——缺这个属性时，CImg 这类
		// 带可选 mask clip 的插件直接抛
		// PropertyUnknownToHost → MissingHostFeature 紫帧）。默认 0
		// （未连接），挂接输入纹理时由 set_input_texture 置 1。
		props.set_one(
			crate::host::PROP_CLIP_CONNECTED,
			crate::property::Value::Int(0),
		);
		Self {
			props,
			name,
			input_texture: std::sync::Mutex::new(None),
			#[cfg(any(target_os = "linux", target_os = "windows"))]
			input_gl_texture: std::sync::Mutex::new(None),
			output_texture: std::sync::Mutex::new(None),
		}
	}

	/// Write the negotiated pixel format (the props side of C++
	/// `OliveClipInstance::setParams`: setPixelDepth/setComponents,
	/// oliveclip.cpp:700-709). `format` is olive::PixelFormat::Format
	/// (0=u8, 2=u16, 3=f16, 4=f32); `channels` is the component count.
	pub fn set_video_params(&self, format: i32, channels: i32) {
		let depth = match format {
			0 => "OfxBitDepthByte",
			2 => "OfxBitDepthShort",
			3 => "OfxBitDepthHalf",
			_ => "OfxBitDepthFloat",
		};
		let comps = match channels {
			1 => "OfxImageComponentAlpha",
			3 => "OfxImageComponentRGB",
			_ => "OfxImageComponentRGBA",
		};
		self.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
			crate::property::Value::String(std::ffi::CString::new(depth).unwrap()),
		);
		self.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
			crate::property::Value::String(std::ffi::CString::new(comps).unwrap()),
		);
	}

	/// Write the negotiated RoD (region of definition).
	///
	/// `rod` holds the four bounds in canonical (OFX canonical) pixel coordinates,
	/// written as `x1, y1, x2, y2` into `OfxImageEffectPropRegionOfDefinition`; the
	/// image effect suite's `clipGetRegionOfDefinition` reads them back, so the
	/// written value must match the image bounds [`Self::fetch_image`] actually
	/// returns. C++ `setRegionOfDefinition` stores a map keyed by time, whereas this
	/// driver has one slot per frame and `_time` is currently unused; repeated calls
	/// overwrite the previous value.
	pub fn set_region_of_definition(&self, rod: OfxRectD, _time: f64) {
		use crate::property::Value;
		self.props.define(
			"OfxImageEffectPropRegionOfDefinition",
			vec![
				Value::Double(rod.x1),
				Value::Double(rod.y1),
				Value::Double(rod.x2),
				Value::Double(rod.y2),
			],
		);
	}

	/// Attach this clip's input texture and sync OFX's "connected" state.
	///
	/// `Some` sets `OfxImageClipPropConnected` to 1 and `None` sets it to 0 (an
	/// optional mask clip has no input, so the plugin side's `Clip::isConnected()`
	/// returns false). Called by the param/render bridge when the input value of
	/// this clip changes on the quercusnode side; texture ownership moves into the
	/// clip. `_time` is currently unused (the single-slot model).
	pub fn set_input_texture(&self, texture: Option<crate::render::Texture>, _time: f64) {
		// The connection state follows the texture hand-off (the render
		// driver only feeds clips that have input; optional mask clips
		// stay 0, so `Clip::isConnected()` answers false for them).
		let connected = i32::from(texture.is_some());
		self.props.set_one(
			crate::host::PROP_CLIP_CONNECTED,
			crate::property::Value::Int(connected),
		);
		*self.input_texture.lock().unwrap_or_else(|e| e.into_inner()) = texture;
	}

	/// Linux zero-copy: attach the GL texture imported from this clip's
	/// GPU input. `clipLoadTexture` then returns its real name without a
	/// `fetch_image` download.
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	pub fn set_input_gl_texture(&self, texture: Option<crate::gl_bridge::GlMemoryTexture>) {
		*self
			.input_gl_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner()) = texture;
	}

	/// Linux zero-copy: the attached GL texture's `(name, width, height)`.
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	pub fn input_gl_texture_info(&self) -> Option<(i32, i32, i32)> {
		self.input_gl_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.as_ref()
			.map(|t| (t.name, t.width, t.height))
	}

	/// A clone of the current input texture (keeps the backend token alive; used by
	/// the zero-copy input branch).
	pub fn input_texture_value(&self) -> Option<crate::render::Texture> {
		self.input_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
	}

	/// Attach this clip's output texture (passed in by the render driver after it
	/// creates one per frame).
	///
	/// `texture` ownership moves into the clip; `None` disconnects the output (this
	/// clip no longer produces image). C++ `setOutputTexture` stores multiple slots
	/// keyed by time; this driver's Phase 1 has a single slot, so `_time` is
	/// currently unused and per-frame texture selection is left to `// [P2]`.
	///
	/// Symmetric with the input side of [`Self::fetch_image`]: the output side is
	/// read by the image effect suite's `clipGetOutputTexture`/write-back path.
	pub fn set_output_texture(&self, texture: Option<crate::render::Texture>, _time: f64) {
		*self
			.output_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner()) = texture;
	}

	/// Fetch this clip's image at `time` (the host side of OFX clipGetImage).
	/// CPU path: read back the quercusrender texture into a [`crate::image::Image`]
	/// (pixel format per the negotiated result, F32 throughout the chain).
	/// `// [P2]` GL path: clipLoadTexture semantics are extended here.
	///
	/// Input frames may be U8/U16/F16/F32: non-F32 input is normalized to F32
	/// (matching oliveclip.cpp setInputTexture's format-conversion path); NaN/Inf
	/// values encountered during the conversion are scrubbed to 0 (oliveclip.cpp
	/// copy_pixels' scrub). `region` only supports None (the whole frame) —
	/// sub-regions arrive with the renderer bridge.
	pub fn fetch_image(
		&self,
		time: f64,
		scale: RenderScale,
		region: Option<OfxRectD>,
	) -> crate::error::Result<crate::image::Image> {
		use crate::error::Error;
		use crate::render::PIXEL_FORMAT_F32;

		let _ = (time, scale);
		if region.is_some() {
			return Err(Error::Failed("fetch_image 子区域第 1 期不支持".into()));
		}
		let texture = self
			.input_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.ok_or(Error::NotFound)?;
		// 占位纹理（dummy）：视作无输入。
		if texture.is_dummy() {
			return Err(Error::NotFound);
		}
		// 纹理 → CPU 帧（GPU 纹理后端下载；帧随 drop 释放）。
		let frame = crate::render::texture_get_frame(&texture)?;
		let params = frame.video_params();
		let format = params.format;
		if format != PIXEL_FORMAT_F32
			&& format != crate::render::PIXEL_FORMAT_U8
			&& format != quercus_core::PixelFormat::U16 as i32
			&& format != quercus_core::PixelFormat::F16 as i32
		{
			return Err(Error::Failed(format!(
				"输入帧格式 {format} 不支持（仅 U8/U16/F16/F32）"
			)));
		}
		let (w, h) = (params.width as f64, params.height as f64);
		// 分量按协商结果（getClipPreferences 已写入 clip.props）。
		let components =
			components_from_props(&self.props).unwrap_or(crate::image::Components::Rgba);

		let mut image = crate::image::Image::allocate(
			crate::image::BitDepth::Float,
			components,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w,
				y2: h,
			},
		);
		let src = frame.data();
		if src.is_null() {
			return Err(Error::Failed("帧无数据".into()));
		}
		// 行优先 + 格式转换（帧行跨度经 linesize 读取——真实
		// quercusrender 帧可有行填充；目标 Image 恒紧凑 F32）。
		// U8/U16/F16 输入归一化到 [0,1] 浮点（对齐 oliveclip.cpp
		// setInputTexture 的 swscale 转换路径：插件侧永远见到协商位
		// 深）；F32/转换结果中的 NaN/Inf 清洗为 0（oliveclip.cpp
		// copy_pixels 的 scrub——CImg 对 NaN 未定义行为）。
		let channels = components.channel_count();
		let samples_per_row = (w as usize) * channels;
		let src_bpc = match format {
			f if f == crate::render::PIXEL_FORMAT_U8 => 1,
			f if f == quercus_core::PixelFormat::U16 as i32 => 2,
			f if f == quercus_core::PixelFormat::F16 as i32 => 2,
			_ => 4,
		};
		let tight_src = samples_per_row * src_bpc;
		let row = frame.linesize_bytes();
		let row = if row > 0 { row } else { tight_src };
		let src_bytes = unsafe { std::slice::from_raw_parts(src, row * h as usize) };
		let dst = image.pixels_mut();
		let mut scrubbed = false;
		for y in 0..h as usize {
			let s = y * row;
			for i in 0..samples_per_row {
				let v = match format {
					f if f == crate::render::PIXEL_FORMAT_U8 => src_bytes[s + i] as f32 / 255.0,
					f if f == quercus_core::PixelFormat::U16 as i32 => {
						let off = s + i * 2;
						let bits = u16::from_le_bytes([src_bytes[off], src_bytes[off + 1]]);
						bits as f32 / 65535.0
					}
					f if f == quercus_core::PixelFormat::F16 as i32 => {
						let off = s + i * 2;
						let bits = u16::from_le_bytes([src_bytes[off], src_bytes[off + 1]]);
						let v = f16_to_f32(bits);
						if v.is_nan() || v.is_infinite() {
							scrubbed = true;
							0.0
						} else {
							v
						}
					}
					_ => {
						let off = s + i * 4;
						let v = f32::from_le_bytes([
							src_bytes[off],
							src_bytes[off + 1],
							src_bytes[off + 2],
							src_bytes[off + 3],
						]);
						if v.is_nan() || v.is_infinite() {
							scrubbed = true;
							0.0
						} else {
							v
						}
					}
				};
				let d = (y * samples_per_row + i) * 4;
				dst[d..d + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		if scrubbed {
			eprintln!("[PLUGIN] NaN/Inf scrubbed from input frame data during fetch");
		}
		// 位深协商（设计约束：管线全链路 ACEScg + F32；插件不支持
		// F32 时输入图像转成协商位深——输出端在 render 驱动转回
		// F32）。clip props 的 PixelDepth 由协商流程
		// （set_video_params）写入；缺省 = F32。
		let negotiated = self
			.props
			.get(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0)
			.and_then(|v| match v {
				crate::property::Value::String(s) => {
					crate::image::BitDepth::from_ofx(&s.to_string_lossy())
				}
				_ => None,
			})
			.unwrap_or(crate::image::BitDepth::Float);
		if negotiated != crate::image::BitDepth::Float {
			image = image.convert_depth(negotiated);
		}
		Ok(image)
	}

	/// Write the output image back into the quercusrender texture (used by the
	/// caller of [`crate::instance::Instance::render`] once the render finishes).
	///
	/// The output texture is created on the quercusrender side and attached through
	/// [`Self::set_output_texture`] — this function takes that texture's CPU frame
	/// (GPU textures are downloaded through the backend and, after write-back,
	/// uploaded again for `Texture::Gpu` through
	/// [`quercus_core::backend::GpuContextLike::upload`]), validates F32 and the
	/// size against the frame parameters, then copies the image pixels for the whole
	/// frame (F32 throughout the chain; C++ pluginrenderer's `readback/wrap` path is
	/// expressed as a CPU copy in phase 1, GL goes through [`crate::render`]'s
	/// `// [P2]`). With no output texture attached or a dummy texture →
	/// [`crate::error::Error::NotFound`]. On success the texture value is returned
	/// (a clone, released on drop).
	pub fn store_output_image(
		&self,
		image: &crate::image::Image,
	) -> crate::error::Result<crate::render::Texture> {
		use crate::error::Error;
		use crate::render::{texture_get_frame, PIXEL_FORMAT_F32};

		let mut texture = self
			.output_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.ok_or(Error::NotFound)?;
		if texture.is_dummy() {
			return Err(Error::NotFound);
		}
		let mut frame = texture_get_frame(&texture)?;
		let params = frame.video_params();
		if params.format != PIXEL_FORMAT_F32 {
			return Err(Error::Failed(format!(
				"输出帧格式 {} 非 F32（第 1 期约束）",
				params.format
			)));
		}
		let (w, h) = (params.width as usize, params.height as usize);
		// 图像与帧必须同尺寸（全链路 F32；宽高/行宽/总长逐项校验）。
		let tight = w * image.components().channel_count() * 4;
		if tight != image.row_bytes() || tight * h != image.pixels().len() {
			return Err(Error::Failed("图像尺寸与输出帧不一致".into()));
		}
		let dst = frame.data_mut();
		if dst.is_null() {
			return Err(Error::Failed("输出帧无数据".into()));
		}
		// 行优先拷贝（目标帧行跨度经 linesize 读取——真实 quercusrender
		// 帧可有行填充；M11 §4 修复同 fetch_image）。
		let row = frame.linesize_bytes();
		let row = if row > 0 { row } else { tight };
		let dst_bytes = unsafe { std::slice::from_raw_parts_mut(dst, row * h) };
		let pixels = image.pixels();
		for y in 0..h {
			let d = y * row;
			let s = y * tight;
			dst_bytes[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
		}
		// 写回目标纹理：CPU 纹理的 `to_frame` 是深拷贝，必须把改写
		// 后的帧放回本体（同 render_driver::write_output_frame 的值
		// 模型修复）；GPU 目标纹理：拷贝只落在下载帧上，经后端
		// upload 回写。
		match &mut texture {
			crate::render::Texture::Cpu(f) => {
				f.data = frame.data;
			}
			crate::render::Texture::Gpu { token, ctx, .. } => {
				ctx.upload(*token, &frame)
					.map_err(|e| Error::Failed(format!("输出纹理上传失败：{e}")))?;
			}
			// 未解析的平面纹理不会成为插件输出目标（解码路径会先解析）。
			crate::render::Texture::Planar(_) => {
				return Err(Error::Failed("平面纹理不能作为插件输出目标".into()));
			}
		}
		Ok(texture)
	}

	/// This clip's time domain (clipGetFrameRange).
	///
	/// `// TODO(value-model)`: the input range is derived from the quercusrender
	/// frame's time base (time_base) — to be implemented along with the clip's
	/// migration to the `quercus_core::texture::Texture` value model.
	pub fn frame_range(&self) -> crate::error::Result<OfxRangeD> {
		let _ = OfxRangeD::default();
		Err(crate::error::Error::Failed(
			"frame_range 待 renderer 桥".into(),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn f16_to_f32_covers_special_values() {
		// 常规值：1.0 = 0x3C00，-2.0 = 0xC000，0.5 = 0x3800。
		assert_eq!(f16_to_f32(0x3C00), 1.0);
		assert_eq!(f16_to_f32(0xC000), -2.0);
		assert_eq!(f16_to_f32(0x3800), 0.5);
		// 零与负零。
		assert_eq!(f16_to_f32(0x0000), 0.0);
		assert_eq!(f16_to_f32(0x8000).to_bits(), (0.0f32).to_bits() | (1 << 31));
		// 非规格数：最小正规格数 2^-14 ≈ 0.00006104；2^-24 是最小非
		// 规格数之一。
		assert!((f16_to_f32(0x0400) - 2f32.powi(-14)).abs() < 1e-12);
		assert!((f16_to_f32(0x0001) - 2f32.powi(-24)).abs() < 1e-12);
		// Inf/NaN。
		assert!(f16_to_f32(0x7C00).is_infinite() && f16_to_f32(0x7C00) > 0.0);
		assert!(f16_to_f32(0xFC00).is_infinite() && f16_to_f32(0xFC00) < 0.0);
		assert!(f16_to_f32(0x7E00).is_nan());
	}

	// ---- fetch_image / store_output_image / 协商矩阵 ----------------------

	fn cs(s: &str) -> std::ffi::CString {
		std::ffi::CString::new(s).unwrap()
	}

	fn test_clip(name: &str) -> ClipInstance {
		ClipInstance::from_descriptor(&crate::descriptor::ClipDescriptor::new(name))
	}

	fn video_params(w: i32, h: i32, format: i32) -> crate::render::VideoParams {
		crate::render::VideoParams {
			width: w,
			height: h,
			format,
			..Default::default()
		}
	}

	fn frame_with(
		w: i32,
		h: i32,
		format: quercus_core::PixelFormat,
		bytes: &[u8],
	) -> crate::render::Frame {
		let mut frame = crate::render::Frame::new();
		frame.set_video_params(video_params(w, h, format as i32));
		assert!(frame.allocate(), "测试帧应可分配");
		assert_eq!(frame.data.len(), bytes.len(), "测试字节长度");
		frame.data.copy_from_slice(bytes);
		frame
	}

	fn f32_frame(w: i32, h: i32, fill: f32) -> crate::render::Frame {
		let mut frame = crate::render::Frame::new();
		frame.set_video_params(video_params(w, h, crate::render::PIXEL_FORMAT_F32));
		assert!(frame.allocate());
		for c in frame.data.as_chunks_mut::<4>().0 {
			c.copy_from_slice(&fill.to_le_bytes());
		}
		frame
	}

	fn f32_image(w: i32, h: i32, fill: f32) -> crate::image::Image {
		let mut img = crate::image::Image::allocate(
			crate::image::BitDepth::Float,
			crate::image::Components::Rgba,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w as f64,
				y2: h as f64,
			},
		);
		for c in img.pixels_mut().as_chunks_mut::<4>().0 {
			c.copy_from_slice(&fill.to_le_bytes());
		}
		img
	}

	fn connect(clip: &ClipInstance, frame: crate::render::Frame) {
		clip.set_input_texture(Some(crate::render::Texture::wrap_frame(frame)), 0.0);
	}

	fn samples(image: &crate::image::Image) -> Vec<f32> {
		image
			.pixels()
			.as_chunks::<4>()
			.0
			.iter()
			.map(|c| f32::from_le_bytes(*c))
			.collect()
	}

	/// components_from_props: all three OFX component strings hit; unknown strings
	/// and non-string values return None.
	#[test]
	fn components_from_props_matrix() {
		use crate::property::Value;
		for (s, want) in [
			(
				"OfxImageComponentRGBA",
				Some(crate::image::Components::Rgba),
			),
			("OfxImageComponentRGB", Some(crate::image::Components::Rgb)),
			(
				"OfxImageComponentAlpha",
				Some(crate::image::Components::Alpha),
			),
			("OfxImageComponentBogus", None),
		] {
			let set = PropertySet::new();
			set.set_one(
				crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
				Value::String(cs(s)),
			);
			assert_eq!(components_from_props(&set), want, "{s}");
		}
		let set = PropertySet::new();
		set.set_one(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, Value::Int(1));
		assert_eq!(components_from_props(&set), None);
	}

	/// set_video_params: every bit-depth number and component-count branch is
	/// written back into the clip props (0/2/3 official mapping + Float fallback;
	/// 1/3 components + RGBA fallback).
	#[test]
	fn set_video_params_depth_and_component_matrix() {
		use crate::property::Value;
		for (fmt, want) in [
			(0, "OfxBitDepthByte"),
			(2, "OfxBitDepthShort"),
			(3, "OfxBitDepthHalf"),
			(4, "OfxBitDepthFloat"),
			(99, "OfxBitDepthFloat"),
		] {
			let c = test_clip("Source");
			c.set_video_params(fmt, 4);
			let got = c
				.props
				.get(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0);
			assert!(
				matches!(got, Some(Value::String(s)) if s.to_string_lossy() == want),
				"format {fmt}"
			);
		}
		for (ch, want) in [
			(1, "OfxImageComponentAlpha"),
			(3, "OfxImageComponentRGB"),
			(4, "OfxImageComponentRGBA"),
			(0, "OfxImageComponentRGBA"),
		] {
			let c = test_clip("Source");
			c.set_video_params(4, ch);
			let got = c.props.get(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0);
			assert!(
				matches!(got, Some(Value::String(s)) if s.to_string_lossy() == want),
				"channels {ch}"
			);
		}
	}

	/// fetch_image: sub-region requests, missing input and dummy placeholders all
	/// fail per contract.
	#[test]
	fn fetch_image_rejects_region_and_unusable_input() {
		let c = test_clip("Source");
		assert!(matches!(
			c.fetch_image(
				0.0,
				RenderScale { x: 1.0, y: 1.0 },
				Some(OfxRectD::default())
			),
			Err(crate::error::Error::Failed(_))
		));
		assert!(matches!(
			c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None),
			Err(crate::error::Error::NotFound)
		));
		c.set_input_texture(Some(crate::render::Texture::dummy()), 0.0);
		assert!(matches!(
			c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None),
			Err(crate::error::Error::NotFound)
		));
	}

	/// fetch_image: all four input conversion paths U8/U16/F16/F32 plus NaN/Inf
	/// scrubbing.
	#[test]
	fn fetch_image_converts_all_input_depths() {
		// U8：归一化到 [0,1]。
		let c = test_clip("Source");
		connect(
			&c,
			frame_with(1, 1, quercus_core::PixelFormat::U8, &[0, 255, 128, 64]),
		);
		let got = samples(
			&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
				.unwrap(),
		);
		assert_eq!(got[0], 0.0);
		assert_eq!(got[1], 1.0);
		assert!((got[2] - 128.0 / 255.0).abs() < 1e-7);
		assert!((got[3] - 64.0 / 255.0).abs() < 1e-7);

		// U16。
		let c = test_clip("Source");
		let mut bytes = Vec::new();
		for v in [0u16, 65535, 32768, 1] {
			bytes.extend_from_slice(&v.to_le_bytes());
		}
		connect(&c, frame_with(1, 1, quercus_core::PixelFormat::U16, &bytes));
		let got = samples(
			&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
				.unwrap(),
		);
		assert_eq!(got[0], 0.0);
		assert_eq!(got[1], 1.0);
		assert!((got[2] - 32768.0 / 65535.0).abs() < 1e-7);
		assert!((got[3] - 1.0 / 65535.0).abs() < 1e-7);

		// F16：NaN/Inf 清洗为 0，普通值与 -0 保留。
		let c = test_clip("Source");
		let mut bytes = Vec::new();
		for v in [0x3C00u16, 0x7E00, 0x7C00, 0x8000] {
			bytes.extend_from_slice(&v.to_le_bytes());
		}
		connect(&c, frame_with(1, 1, quercus_core::PixelFormat::F16, &bytes));
		let got = samples(
			&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
				.unwrap(),
		);
		assert_eq!(got[0], 1.0);
		assert_eq!(got[1], 0.0, "F16 NaN 应清洗为 0");
		assert_eq!(got[2], 0.0, "F16 Inf 应清洗为 0");
		assert_eq!(got[3].to_bits(), 0x8000_0000, "-0 应保留");

		// F32：NaN/Inf 清洗为 0。
		let c = test_clip("Source");
		let mut bytes = Vec::new();
		for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.5] {
			bytes.extend_from_slice(&v.to_le_bytes());
		}
		connect(&c, frame_with(1, 1, quercus_core::PixelFormat::F32, &bytes));
		let got = samples(
			&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
				.unwrap(),
		);
		assert_eq!(got[0], 0.0);
		assert_eq!(got[1], 0.0);
		assert_eq!(got[2], 0.0);
		assert_eq!(got[3], 1.5);
	}

	/// fetch_image: an unsupported input format (U10) fails explicitly.
	#[test]
	fn fetch_image_rejects_unsupported_format() {
		let c = test_clip("Source");
		// U10 = 4 字节/通道打包格式；管线仅支持 U8/U16/F16/F32。
		connect(
			&c,
			frame_with(1, 1, quercus_core::PixelFormat::U10, &[0u8; 16]),
		);
		let res = c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None);
		assert!(matches!(res, Err(crate::error::Error::Failed(_))));
	}

	/// fetch_image: the negotiated bit depth is read from the clip props — Byte
	/// lowers the depth; unknown strings and non-string values fall back to F32.
	#[test]
	fn fetch_image_negotiated_depth() {
		use crate::property::Value;

		let c = test_clip("Source");
		connect(&c, f32_frame(1, 1, 0.5));
		c.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
			Value::String(cs("OfxBitDepthByte")),
		);
		let img = c
			.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap();
		assert_eq!(img.depth(), crate::image::BitDepth::Byte);

		let c = test_clip("Source");
		connect(&c, f32_frame(1, 1, 0.5));
		c.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
			Value::String(cs("OfxBitDepthBogus")),
		);
		let img = c
			.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap();
		assert_eq!(img.depth(), crate::image::BitDepth::Float);

		let c = test_clip("Source");
		connect(&c, f32_frame(1, 1, 0.5));
		c.props
			.set_one(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, Value::Int(4));
		let img = c
			.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap();
		assert_eq!(img.depth(), crate::image::BitDepth::Float);
	}

	/// store_output_image: the no-output/dummy/non-F32/size-mismatch error branches
	/// plus the CPU whole-frame copy.
	#[test]
	fn store_output_image_cpu_paths() {
		let image = f32_image(2, 2, 0.25);

		// 未挂输出纹理 → NotFound。
		let c = test_clip("Output");
		assert!(matches!(
			c.store_output_image(&image),
			Err(crate::error::Error::NotFound)
		));

		// dummy 输出纹理 → NotFound。
		c.set_output_texture(Some(crate::render::Texture::dummy()), 0.0);
		assert!(matches!(
			c.store_output_image(&image),
			Err(crate::error::Error::NotFound)
		));

		// 非 F32 输出帧 → Failed。
		let c = test_clip("Output");
		c.set_output_texture(
			Some(crate::render::Texture::wrap_frame(frame_with(
				2,
				2,
				quercus_core::PixelFormat::U8,
				&[0u8; 16],
			))),
			0.0,
		);
		assert!(matches!(
			c.store_output_image(&image),
			Err(crate::error::Error::Failed(_))
		));

		// 尺寸不符 → Failed。
		let c = test_clip("Output");
		c.set_output_texture(
			Some(crate::render::Texture::wrap_frame(f32_frame(3, 1, 0.0))),
			0.0,
		);
		assert!(matches!(
			c.store_output_image(&image),
			Err(crate::error::Error::Failed(_))
		));

		// 成功：整帧拷贝回输出纹理。
		let c = test_clip("Output");
		c.set_output_texture(
			Some(crate::render::Texture::wrap_frame(f32_frame(2, 2, 0.0))),
			0.0,
		);
		let stored = c.store_output_image(&image).unwrap();
		let frame = crate::render::texture_get_frame(&stored).unwrap();
		assert_eq!(frame.data.as_slice(), image.pixels());
	}

	struct FakeGpu {
		uploads: std::sync::Mutex<Vec<(u64, crate::render::Frame)>>,
		fail_upload: bool,
	}

	impl quercus_core::backend::GpuContextLike for FakeGpu {
		fn kind(&self) -> quercus_core::backend::BackendKind {
			quercus_core::backend::BackendKind::Cpu
		}
		fn destroy_texture(&self, _token: u64) {}
		fn upload(
			&self,
			token: u64,
			frame: &crate::render::Frame,
		) -> quercus_render::error::Result<()> {
			if self.fail_upload {
				return Err(quercus_render::error::Error::State);
			}
			self.uploads
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.push((token, frame.clone()));
			Ok(())
		}
		fn download(&self, _token: u64) -> quercus_render::error::Result<crate::render::Frame> {
			Ok(f32_frame(2, 2, 0.0))
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

	/// store_output_image: a GPU output texture is written back through the backend
	/// upload; an upload failure is propagated.
	#[test]
	fn store_output_image_gpu_upload() {
		let image = f32_image(2, 2, 0.5);

		let ctx = std::sync::Arc::new(FakeGpu {
			uploads: std::sync::Mutex::new(Vec::new()),
			fail_upload: false,
		});
		let c = test_clip("Output");
		let tex = crate::render::Texture::gpu(ctx.clone(), 7, 2, 2, quercus_core::PixelFormat::F32);
		c.set_output_texture(Some(tex), 0.0);
		c.store_output_image(&image).unwrap();
		{
			let uploads = ctx.uploads.lock().unwrap();
			assert_eq!(uploads.len(), 1);
			assert_eq!(uploads[0].0, 7);
			assert_eq!(uploads[0].1.data.as_slice(), image.pixels());
		}

		// 上传失败 → Failed。
		let ctx = std::sync::Arc::new(FakeGpu {
			uploads: std::sync::Mutex::new(Vec::new()),
			fail_upload: true,
		});
		let c = test_clip("Output");
		c.set_output_texture(
			Some(crate::render::Texture::gpu(
				ctx,
				8,
				2,
				2,
				quercus_core::PixelFormat::F32,
			)),
			0.0,
		);
		assert!(matches!(
			c.store_output_image(&image),
			Err(crate::error::Error::Failed(_))
		));
	}

	/// frame_range: explicitly unsupported in phase 1 (the renderer bridge is
	/// pending).
	#[test]
	fn frame_range_is_unsupported() {
		let c = test_clip("Source");
		assert!(matches!(
			c.frame_range(),
			Err(crate::error::Error::Failed(_))
		));
	}
}
