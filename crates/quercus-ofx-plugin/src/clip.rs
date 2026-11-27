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
		// OfxImageClipPropConnected (ofxsImageEffect.cpp:1106's
		// `Clip::isConnected()` is a strong read without a default — when this property is missing,
		// plugins with an optional mask clip such as CImg immediately throw
		// PropertyUnknownToHost → MissingHostFeature purple frame). Default 0
		// (not connected); set_input_texture sets it to 1 when an input texture is attached.
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
			return Err(Error::Failed(
				"fetch_image sub-region not supported in Phase 1".into(),
			));
		}
		let texture = self
			.input_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.ok_or(Error::NotFound)?;
		// Placeholder texture (dummy): treated as no input.
		if texture.is_dummy() {
			return Err(Error::NotFound);
		}
		// Texture → CPU frame (GPU textures downloaded via the backend; frame released on drop).
		let frame = crate::render::texture_get_frame(&texture)?;
		let params = frame.video_params();
		let format = params.format;
		if format != PIXEL_FORMAT_F32
			&& format != crate::render::PIXEL_FORMAT_U8
			&& format != quercus_core::PixelFormat::U16 as i32
			&& format != quercus_core::PixelFormat::F16 as i32
		{
			return Err(Error::Failed(format!(
				"input frame format {format} not supported (only U8/U16/F16/F32)"
			)));
		}
		let (w, h) = (params.width as f64, params.height as f64);
		// Components per negotiation result (getClipPreferences already wrote clip.props).
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
			return Err(Error::Failed("frame has no data".into()));
		}
		// Row-major + format conversion (frame row stride read via linesize — real
		// quercusrender frames may have row padding; destination Image is always tightly packed F32).
		// U8/U16/F16 inputs are normalized to [0,1] floats (matching oliveclip.cpp
		// setInputTexture's swscale conversion path: the plugin always sees the negotiated
		// depth); NaN/Inf in F32/conversion results is scrubbed to 0 (oliveclip.cpp
		// copy_pixels' scrub — CImg has undefined behaviour for NaN).
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
		let dst = image.data.as_mut_slice();
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
		// Bit-depth negotiation (design constraint: pipeline is ACEScg + F32 end-to-end;
		// when the plugin does not support F32 the input image is converted to the negotiated
		// depth — the output side converts back to F32 in the render driver). The clip props'
		// PixelDepth is written by the negotiation flow (set_video_params); default = F32.
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
				"output frame format {} is not F32 (Phase 1 constraint)",
				params.format
			)));
		}
		let (w, h) = (params.width as usize, params.height as usize);
		// Image and frame must match in size (F32 end-to-end; validate width/height/row width/total length).
		let tight = w * image.components().channel_count() * 4;
		if tight != image.row_bytes() || tight * h != image.data.as_slice().len() {
			return Err(Error::Failed(
				"image size does not match the output frame".into(),
			));
		}
		let dst = frame.data_mut();
		if dst.is_null() {
			return Err(Error::Failed("output frame has no data".into()));
		}
		// Row-major copy (destination frame row stride read via linesize — real quercusrender
		// frames may have row padding; same M11 §4 fix as fetch_image).
		let row = frame.linesize_bytes();
		let row = if row > 0 { row } else { tight };
		let dst_bytes = unsafe { std::slice::from_raw_parts_mut(dst, row * h) };
		let pixels = image.data.as_slice();
		for y in 0..h {
			let d = y * row;
			let s = y * tight;
			dst_bytes[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
		}
		// Write back to the destination texture: for a CPU texture `to_frame` is a deep copy, so the
		// modified frame must be put back into the original (same value-model fix as
		// render_driver::write_output_frame); for a GPU destination texture the copy only lands
		// on the downloaded frame, so upload it back through the backend.
		match &mut texture {
			crate::render::Texture::Cpu(f) => {
				f.data = frame.data;
			}
			crate::render::Texture::Gpu { token, ctx, .. } => {
				ctx.upload(*token, &frame)
					.map_err(|e| Error::Failed(format!("output texture upload failed: {e}")))?;
			}
			// Unresolved planar textures never become a plugin output target (the decode path resolves them first).
			crate::render::Texture::Planar(_) => {
				return Err(Error::Failed(
					"planar texture cannot be a plugin output target".into(),
				));
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
			"frame_range pending renderer bridge".into(),
		))
	}
}

#[cfg(test)]
mod tests;
