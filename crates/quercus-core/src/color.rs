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

//! Color processing: OCIO processors and the process-wide default config
//! (C++ `ColorProcessor` + `ColorManager` statics).
//!
//! Implemented over the `ocio-rs` crate (safe Rust bindings for
//! OpenColorIO v2.5.2, bundled real-OCIO build). OCIO is never rewritten —
//! this module maps the C++ call surface onto ocio-rs.

use crate::colormath;
use crate::commonutil;
use std::collections::HashMap;
use std::env;
use std::ops;
use std::result;
use std::slice;
use std::sync::{LazyLock, Mutex};

use crate::PixelFormat;

use crate::error::{Error, Result};
use crate::texture::Frame;

/// A color processor (wraps an OCIO processor). A processor whose OCIO
/// lookup failed holds `None` — conversions then pass through, mirroring
/// the C++ non-fatal creation behavior.
pub struct ColorProcessor {
	/// The OCIO processor (None = pass-through).
	inner: Option<ocio_rs::Processor>,
	/// The cached CPU processor used for pixel conversions (C++
	/// `cpu_processor_`).
	cpu: Option<ocio_rs::CPUProcessor>,
}

/// The R/B channel swap as a 4x4 matrix transform (baked around the
/// display chain so BGRA buffers can be transformed through OCIO's RGBA
/// entry points).
fn rb_swap_matrix() -> Option<ocio_rs::transform::MatrixTransform> {
	let m = ocio_rs::transform::MatrixTransform::create().ok()?;
	m.set_matrix(&[
		0.0, 0.0, 1.0, 0.0, //
		0.0, 1.0, 0.0, 0.0, //
		1.0, 0.0, 0.0, 0.0, //
		0.0, 0.0, 0.0, 1.0,
	])
	.ok()?;
	Some(m)
}

/// Processor direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
	/// src -> dst.
	Normal,
	/// dst -> src.
	Inverse,
}

impl Direction {
	/// The ocio-rs transform direction for the swap already applied at the
	/// call site.
	fn to_ocio(self) -> ocio_rs::TransformDirection {
		match self {
			Direction::Normal => ocio_rs::TransformDirection::Forward,
			Direction::Inverse => ocio_rs::TransformDirection::Inverse,
		}
	}
}

// OCIO Config/Processor are refcounted handles over C++ objects that are
// thread-safe for CPU processing (the C++ ColorProcessor is shared across
// render threads the same way); the safe wrapper is Send+Sync.
unsafe impl Send for ColorProcessor {}
unsafe impl Sync for ColorProcessor {}

impl ColorProcessor {
	/// Creates a processor from two colorspace names on the default config
	/// (C++ `ColorProcessor::create(config, input, dest)`).
	///
	/// `src_space` is the input colorspace or role (e.g. "scene_linear");
	/// `dst_transform` is the destination colorspace or role name.
	/// `Direction::Inverse` builds the same pair in the opposite order, so
	/// the processor runs dst → src.
	///
	/// Role names are resolved to canonical names before building. OCIO
	/// failures are non-fatal: a processor with `inner == None` is returned
	/// and conversions pass through, so callers that must know check
	/// [`Self::is_valid`]. Returns `None` only when no default config is
	/// loaded.
	pub fn create(src_space: &str, dst_transform: &str, dir: Direction) -> Option<Self> {
		let config = default_config()?;
		let src = if config.has_role(src_space) {
			config
				.canonical_name(src_space)
				.unwrap_or_else(|| src_space.to_string())
		} else {
			src_space.to_string()
		};

		let processor = match dir {
			Direction::Normal => config.processor(&src, dst_transform),
			Direction::Inverse => config.processor(dst_transform, &src),
		}
		.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());

		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Creates a processor from a LUT file on the default config (C++
	/// `create_lut` semantics).
	///
	/// `path` is the LUT file, read by OCIO's `FileTransform` (any format
	/// OCIO supports: .cube, .spi3d, …); `dir` selects forward or inverse
	/// evaluation. Interpolation is linear. Returns `None` when the default
	/// config is missing or OCIO cannot open the file; when the transform
	/// builds but the processor is rejected, a pass-through processor is
	/// returned instead (check [`Self::is_valid`]).
	pub fn create_lut(path: &str, dir: Direction) -> Option<Self> {
		let config = default_config()?;
		let transform = ocio_rs::transform::FileTransform::create().ok()?;
		transform.set_src(path).ok()?;
		transform.set_interpolation(ocio_rs::Interpolation::Linear);
		transform.set_direction(dir.to_ocio());
		let processor = config
			.processor_from_transform(&transform, dir.to_ocio())
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create from a LUT file on a specific config (C++
	/// `ColorProcessor::create_lut` family with a ColorManager config).
	pub fn create_lut_on(config: &ocio_rs::Config, path: &str, dir: Direction) -> Option<Self> {
		let transform = ocio_rs::transform::FileTransform::create().ok()?;
		transform.set_src(path).ok()?;
		transform.set_interpolation(ocio_rs::Interpolation::Linear);
		transform.set_direction(dir.to_ocio());
		let processor = config
			.processor_from_transform(&transform, dir.to_ocio())
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create a dynamic grading-primary processor on the default config
	/// (C++ `ColorProcessor` grading-primary family).
	pub fn create_grading_primary(style: GradingStyle) -> Option<Self> {
		let config = default_config()?;
		let transform =
			ocio_rs::transform::GradingPrimaryTransform::create(style.to_ocio()).ok()?;
		transform.make_dynamic();
		transform.set_direction(ocio_rs::TransformDirection::Forward);
		let processor = config
			.processor_from_transform(&transform, ocio_rs::TransformDirection::Forward)
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create the display-output processor from a display ICC profile
	/// (macOS ColorSync / Windows ICM / Linux colord or `_ICC_PROFILE`).
	///
	/// OCIO's ICC reader (FileFormatICC, display-class profiles) builds the
	/// FileTransform's forward direction as "CIE XYZ (D65-adapted PCS) →
	/// device code values", so the chain is `<src_space> → linear Rec.709 →
	/// CIE XYZ D65 → ICC forward`. `src_space` is a colorspace or role of
	/// the default config (the pipeline reference, e.g. "scene_linear").
	pub fn create_display_icc(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::create_display_icc_impl(src_space, icc_path, false)
	}

	/// Create the display-output processor for BGRA8 buffers (the viewer's
	/// wire format): the [`create_display_icc`](Self::create_display_icc)
	/// chain with R/B-swapping matrices baked around it, so BGRA bytes are
	/// transformed in place through OCIO's RGBA entry points (swap∘chain∘swap
	/// is the identity-wrapped chain evaluated on swapped channels).
	///
	/// `src_space` is a colorspace or role of the default config (the
	/// pipeline reference, e.g. "scene_linear") and `icc_path` is the
	/// display ICC profile. Returns `None` when the default config is
	/// missing or the group cannot be created. Pair with
	/// [`Self::convert_bgra8`], which requires this chain.
	pub fn create_display_icc_bgra8(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::create_display_icc_impl(src_space, icc_path, true)
	}

	/// Create the display-output processor for content the caller has already
	/// converted to CIE XYZ (D65, unit luminance) — the non-sRGB project
	/// output gamut path. [`create_display_icc`](Self::create_display_icc)
	/// starts from an OCIO named space (sRGB and friends); P3/BT.2020
	/// targets have no named space in the builtin configs, so the caller
	/// linearizes and gamut-maps to XYZ itself
	/// (`crate::colormath::output_spec_to_xyz_d65`) and this chain only
	/// needs the ICC half: it runs the existing builder with the config's
	/// `cie_xyz_d65_interchange` role as the source space (XYZ → linear
	/// Rec.709, whose inverse the builder's leg 2 immediately undoes — a
	/// no-op round trip, leaving the ICC FileTransform to map the XYZ PCS
	/// to device values).
	///
	/// Returns `None` (not a pass-through) when the chain cannot build — e.g.
	/// the config rejects the interchange role — so callers can fall back to
	/// the sRGB chain instead of silently mapping wrong content.
	pub fn create_display_icc_xyz(icc_path: &str) -> Option<Self> {
		Self::create_display_icc_xyz_impl(icc_path, false)
	}

	/// The [`create_display_icc_xyz`](Self::create_display_icc_xyz) chain
	/// with R/B-swapping matrices baked around it for BGRA8 buffers (see
	/// [`create_display_icc_bgra8`](Self::create_display_icc_bgra8)).
	pub fn create_display_icc_xyz_bgra8(icc_path: &str) -> Option<Self> {
		Self::create_display_icc_xyz_impl(icc_path, true)
	}

	/// Shared builder behind the XYZ-input `create_display_icc_xyz*`
	/// wrappers.
	///
	/// Runs [`Self::create_display_icc_impl`] with the config's
	/// `cie_xyz_d65_interchange` role as the source space (`bgra` is
	/// forwarded) and returns `None` instead of a pass-through processor
	/// when the resulting chain is invalid, so callers can fall back to the
	/// sRGB chain rather than pushing XYZ values through a no-op.
	fn create_display_icc_xyz_impl(icc_path: &str, bgra: bool) -> Option<Self> {
		let p = Self::create_display_icc_impl("cie_xyz_d65_interchange", icc_path, bgra)?;
		// A failed OCIO lookup yields a pass-through processor (the
		// non-fatal convention above); for the XYZ chain that must surface as
		// `None` so the caller can fall back to the sRGB degradation instead
		// of feeding XYZ values through a no-op.
		p.is_valid().then_some(p)
	}

	/// Shared builder behind the `create_display_icc*` family.
	///
	/// Builds the group transform `src_space → Linear Rec.709 → CIE XYZ
	/// D65 → ICC forward` and, when `bgra` is true, brackets it with R/B
	/// swap matrices so a BGRA8 buffer can be pushed through OCIO's RGBA
	/// entry point (swap∘chain∘swap). `src_space` is a colorspace name or
	/// role of the default config; `icc_path` is a display-class ICC profile
	/// read by OCIO's ICC reader.
	///
	/// Returns `None` when the default config is missing or any OCIO object
	/// cannot be built. The returned processor may still be a pass-through
	/// (`inner == None`) when the group itself is rejected — the XYZ
	/// wrappers turn that into `None` via [`Self::is_valid`].
	fn create_display_icc_impl(src_space: &str, icc_path: &str, bgra: bool) -> Option<Self> {
		let config = default_config()?;
		// Leg 1: pipeline space -> linear Rec.709 (sRGB primaries, D65).
		let to_lin709 = ocio_rs::transform::ColorSpaceTransform::create().ok()?;
		to_lin709.set_src(src_space).ok()?;
		to_lin709.set_dst("Linear Rec.709 (sRGB)").ok()?;
		// Leg 2: linear Rec.709 -> CIE XYZ D65 (the ICC connection space as
		// OCIO's ICC reader adapts it, D50->D65 Bradford baked in). The
		// builtin configs carry no XYZ colorspace, so the conversion is an
		// explicit matrix (sRGB/Rec.709 primaries -> XYZ D65).
		let to_xyz = ocio_rs::transform::MatrixTransform::create().ok()?;
		to_xyz
			.set_matrix(&[
				0.4123908, 0.3575843, 0.1804808, 0.0, //
				0.2126390, 0.7151687, 0.0721923, 0.0, //
				0.0193308, 0.1191948, 0.9505322, 0.0, //
				0.0, 0.0, 0.0, 1.0,
			])
			.ok()?;
		// Leg 3: XYZ D65 -> device, per the ICC profile (OCIO's
		// FileFormatICC forward direction).
		let icc = ocio_rs::transform::FileTransform::create().ok()?;
		icc.set_src(icc_path).ok()?;
		icc.set_interpolation(ocio_rs::Interpolation::Linear);
		icc.set_direction(ocio_rs::TransformDirection::Forward);
		let group = ocio_rs::transform::GroupTransform::create().ok()?;
		if bgra {
			group.append_transform(&rb_swap_matrix()?).ok()?;
		}
		group.append_transform(&to_lin709).ok()?;
		group.append_transform(&to_xyz).ok()?;
		group.append_transform(&icc).ok()?;
		if bgra {
			group.append_transform(&rb_swap_matrix()?).ok()?;
		}
		let processor = config
			.processor_from_transform(&group, ocio_rs::TransformDirection::Forward)
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create from an explicit OCIO processor (C++
	/// `ColorProcessor::create(ConstProcessorRcPtr)`).
	pub fn from_processor(processor: ocio_rs::Processor) -> Self {
		let cpu = processor.default_cpu_processor().ok();
		Self {
			inner: Some(processor),
			cpu,
		}
	}

	/// Create a pass-through processor (invalid OCIO processor).
	pub fn pass_through() -> Self {
		Self {
			inner: None,
			cpu: None,
		}
	}

	/// The underlying OCIO processor handle, when valid.
	pub fn processor(&self) -> Option<&ocio_rs::Processor> {
		self.inner.as_ref()
	}

	/// Converts one RGBA color.
	///
	/// `rgba` is R,G,B,A as `f64`; alpha is converted too but normally
	/// stays 1.0. The conversion runs at `f32` precision, matching OCIO's
	/// CPU processor, then widens back to `f64`. A pass-through processor
	/// returns `rgba` unchanged. Used for single samples such as swatch
	/// previews; bulk image data goes through [`Self::convert_frame`].
	pub fn convert_color(&self, rgba: [f64; 4]) -> [f64; 4] {
		match &self.cpu {
			Some(cpu) => {
				let mut c = [
					rgba[0] as f32,
					rgba[1] as f32,
					rgba[2] as f32,
					rgba[3] as f32,
				];
				cpu.apply_rgba(&mut c);
				[c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64]
			}
			None => rgba,
		}
	}

	/// Converts a whole frame in place (row-major, tightly packed RGBA).
	///
	/// `frame` must carry `PixelFormat::F32`; every pixel is rewritten
	/// through OCIO with a stride of 4 floats, so the data must be tightly
	/// packed. A pass-through processor returns `Ok(())` without touching
	/// the frame, so callers need no special case.
	///
	/// # Errors
	///
	/// `Error::Invalid` when the frame is not F32; `Error::Failed` when the
	/// byte buffer cannot be viewed as `f32` (length not a multiple of 4) or
	/// OCIO rejects the apply.
	pub fn convert_frame(&self, frame: &mut Frame) -> Result<()> {
		let Some(cpu) = &self.cpu else {
			return Ok(()); // pass-through
		};
		if frame.format != PixelFormat::F32 {
			return Err(Error::Invalid);
		}
		// Tightly packed RGBA: stride is 4 f32 elements per pixel
		// (ocio-rs counts elements, not bytes).
		let pixels = frame.pixel_count() as i64;
		let buf: &mut [f32] = bytemuck_f32_slice(&mut frame.data)
			.ok_or_else(|| Error::Failed("pixel buffer not f32 aligned".to_string()))?;
		cpu.apply_rgba_pixels(buf, pixels, 4);
		Ok(())
	}

	/// True when the underlying OCIO processor is valid (C++
	/// `get_processor() != null`).
	pub fn is_valid(&self) -> bool {
		self.inner.is_some()
	}

	/// The OCIO processor cache id (C++ `ColorProcessor::id()`), or the
	/// empty string for a pass-through processor.
	pub fn cache_id(&self) -> String {
		match &self.inner {
			Some(p) => p.cache_id().unwrap_or_default(),
			None => String::new(),
		}
	}

	/// Converts `pixels` packed BGRA8 pixels in place (the viewer's wire
	/// format).
	///
	/// `data` holds `pixels * 4` bytes in B,G,R,A order and `pixels` counts
	/// pixels. The processor must have been built with
	/// [`create_display_icc_bgra8`](Self::create_display_icc_bgra8): the
	/// R/B swizzle is baked into the chain, so the bytes go through OCIO's
	/// RGBA entry point unchanged. A pass-through processor is a no-op
	/// (`Ok(())`, buffer untouched).
	///
	/// The conversion detours through F32: the default CPU processor is
	/// F32-finalized and rejects packed u8 buffers with a bit-depth
	/// mismatch, and the ocio-rs binding exposes no Uint8-finalized CPU
	/// processor. Bytes are un-scaled to 0..1, converted, then clamped and
	/// rounded back to u8; the round-trip cost is one small staging buffer.
	///
	/// # Errors
	///
	/// `Error::Invalid` when `data` is shorter than `pixels * 4` bytes;
	/// `Error::Failed` when OCIO rejects the apply.
	pub fn convert_bgra8(&self, data: &mut [u8], pixels: i64) -> Result<()> {
		let Some(cpu) = &self.cpu else {
			return Ok(());
		};
		let count = (pixels.max(0) as usize) * 4;
		if data.len() < count {
			return Err(Error::Invalid);
		}
		let mut f32s: Vec<f32> = data[..count].iter().map(|&v| v as f32 / 255.0).collect();
		cpu.try_apply_rgba_pixels(&mut f32s, pixels, 4)
			.map_err(|e| Error::Failed(format!("OCIO f32 apply (bgra8 detour): {e}")))?;
		for (dst, v) in data[..count].iter_mut().zip(f32s) {
			*dst = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
		}
		Ok(())
	}

	/// Converts `pixels` tightly packed RGBA `f32` pixels in place.
	///
	/// `samples` holds `pixels * 4` interleaved R,G,B,A values and is
	/// rewritten with the converted values; `pixels` counts pixels, not
	/// floats, and is passed straight to OCIO as the pixel count. A
	/// pass-through processor (or one whose CPU processor failed to build)
	/// leaves the buffer untouched and returns `Ok(())`.
	///
	/// # Errors
	///
	/// `Error::Failed` when the OCIO CPU processor rejects the buffer (for
	/// example a length/stride mismatch between `samples` and `pixels`),
	/// with OCIO's message in the payload.
	pub fn convert_f32_rgba(&self, samples: &mut [f32], pixels: i64) -> Result<()> {
		let Some(cpu) = &self.cpu else {
			return Ok(());
		};
		cpu.try_apply_rgba_pixels(samples, pixels, 4)
			.map_err(|e| Error::Failed(format!("OCIO f32 apply: {e}")))
	}
}

/// Grading-primary transform style (mirrors the C++ enum values: LIN=0,
/// LOG=1 in `include/render/color.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradingStyle {
	/// OCIO GRADING_LIN.
	Lin,
	/// OCIO GRADING_LOG.
	Log,
}

impl GradingStyle {
	fn to_ocio(self) -> ocio_rs::GradingStyle {
		match self {
			GradingStyle::Lin => ocio_rs::GradingStyle::Lin,
			GradingStyle::Log => ocio_rs::GradingStyle::Log,
		}
	}
}

/// Reinterpret a byte buffer as f32 when its length is a multiple of 4
/// (the pipeline guarantees F32 RGBA frames, so alignment is exact).
fn bytemuck_f32_slice(data: &mut [u8]) -> Option<&mut [f32]> {
	if !data.len().is_multiple_of(4) {
		return None;
	}
	// SAFETY: length is a multiple of 4 and `data` is byte-aligned; the
	// cast is valid for any alignment (f32 has no stricter requirement
	// than u8 for mutable slice casts at this length).
	Some(unsafe { slice::from_raw_parts_mut(data.as_mut_ptr() as *mut f32, data.len() / 4) })
}

// ---- process-wide default config (C++ ColorManager statics) ----------------

/// The process-wide pipeline color settings (the project properties that
/// drive the ACEScg + F32 pipeline): the working colorspace and the
/// output/delivery spec. Defaults to ACEScg working + sRGB output; the app
/// updates this from the open project's properties (and again on every
/// project-properties commit). Render and export paths read it — a single
/// project is open at a time, so a process global is the same shape as the
/// OCIO default config above.
static PIPELINE_COLOR: LazyLock<Mutex<(colormath::WorkingColorSpace, colormath::OutputColorSpec)>> =
	LazyLock::new(|| {
		Mutex::new((
			colormath::WorkingColorSpace::default(),
			colormath::OutputColorSpec::default(),
		))
	});

/// Set the pipeline color settings (working space + output spec).
pub fn set_pipeline_color_settings(
	working: colormath::WorkingColorSpace,
	output: colormath::OutputColorSpec,
) {
	*PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()) = (working, output);
}

/// The pipeline working colorspace (ACEScg unless the project says
/// otherwise).
///
/// Reads the process-wide settings installed by
/// [`set_pipeline_color_settings`]; render and export paths use it to know
/// which space their pixels are in. Read counterpart of that setter.
pub fn pipeline_working_space() -> colormath::WorkingColorSpace {
	PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()).0
}

/// The pipeline output/delivery spec.
pub fn pipeline_output_spec() -> colormath::OutputColorSpec {
	PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()).1
}

/// The pipeline working colorspace as an OFX colorspace name (the value
/// written to `kOfxImageClipPropColourspace` so plugins are told the
/// true space of the pixels they receive — ACEScg in the default pipeline,
/// sRGB in the legacy pass-through mode).
pub fn pipeline_working_ofx_name() -> &'static str {
	match pipeline_working_space() {
		colormath::WorkingColorSpace::AcesCg => "ACEScg",
		colormath::WorkingColorSpace::SrgbLegacy => "sRGB",
	}
}

/// Send+Sync wrapper around `ocio_rs::Config` (a `NonNull`-based handle;
/// the underlying OCIO config is a shared pointer safe for concurrent
/// reads).
pub struct SafeConfig(ocio_rs::Config);
unsafe impl Send for SafeConfig {}
unsafe impl Sync for SafeConfig {}

impl ops::Deref for SafeConfig {
	type Target = ocio_rs::Config;
	fn deref(&self) -> &ocio_rs::Config {
		&self.0
	}
}

static DEFAULT_CONFIG: LazyLock<Mutex<Option<std::sync::Arc<SafeConfig>>>> =
	LazyLock::new(|| Mutex::new(None));

/// Borrow the default config (C++ `ColorManager::get_default_config()`).
/// The `Arc` keeps the config alive across threads.
pub fn default_config() -> Option<std::sync::Arc<SafeConfig>> {
	DEFAULT_CONFIG
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Load the process-wide default config from $OCIO or the bundled config
/// (C++ `ColorManager::SetUpDefaultConfig`).
pub fn set_up_default_config() -> Result<()> {
	let env = env::var("OCIO").ok().filter(|p| !p.is_empty());
	set_up_default_config_from(env.as_deref())
}

/// Load the process-wide default config from an explicit path (`None` =
/// the bundled default config). The project-properties OCIO override and
/// the `$OCIO` startup path both land here.
pub fn set_up_default_config_from(path: Option<&str>) -> Result<()> {
	let config = match path {
		Some(path) => ocio_rs::Config::from_file(path)
			.map_err(|e| Error::Failed(format!("load OCIO config \"{path}\": {e}")))?,
		None => ocio_rs::Config::create_from_builtin_config("default")
			.or_else(|_| ocio_rs::Config::create_from_builtin_config("ocio-2.2-default"))
			.map_err(|e| Error::Failed(format!("load bundled config: {e}")))?,
	};
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) =
		Some(std::sync::Arc::new(SafeConfig(config)));
	Ok(())
}

/// The active default config's display transform cache id for
/// (display, view) — computed from the config's reference colorspace
/// (C++ `ColorManager::display_transform` cache-id semantics).
///
/// `Err(Error::State)` when no default config exists; `Ok(None)` when the
/// display or view is unknown.
pub fn display_transform_result(
	display: &str,
	view: &str,
) -> result::Result<Option<String>, Error> {
	let config = default_config().ok_or(Error::State)?;

	let mut display_found = false;
	for i in 0..config.num_displays_all() {
		if config.display_all(i).as_deref() == Some(display) {
			display_found = true;
			break;
		}
	}
	if !display_found {
		return Ok(None);
	}

	let mut view_found = false;
	let views =
		config.num_views_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, display);
	for i in 0..views {
		if config
			.view_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, display, i)
			.as_deref()
			== Some(view)
		{
			view_found = true;
			break;
		}
	}
	if !view_found {
		return Ok(None);
	}

	// OCIO's ROLE_REFERENCE role name ("reference"): `color_space`
	// resolves name-or-role (C++ `getColorSpace(ROLE_REFERENCE)`); the
	// role-name lookups are the fallback for configs that do not bind it.
	let src = config
		.color_space("reference")
		.and_then(|cs| cs.name())
		.filter(|s| !s.is_empty())
		.or_else(|| {
			config
				.role_color_space("reference")
				.filter(|s| !s.is_empty())
		})
		.or_else(|| {
			config
				.role_color_space("aces_interchange")
				.filter(|s| !s.is_empty())
		})
		.or_else(|| {
			config
				.role_color_space("scene_linear")
				.filter(|s| !s.is_empty())
		})
		.ok_or(Error::State)?;
	let processor = config
		.processor_display(src, display, view, ocio_rs::TransformDirection::Forward)
		.map_err(|_| Error::NotFound)?;
	Ok(processor.cache_id())
}

/// The display transform's cache id for (`display`, `view`), if it exists.
///
/// `display` is an OCIO display name (e.g. "sRGB") and `view` is one of
/// that display's views (e.g. "ACES 1.0 SDR-video"). The id is OCIO's
/// processor cache id, not a displayable name, and is meant for keying
/// caches.
///
/// Convenience wrapper over [`display_transform_result`] that collapses the
/// error case into `None`: `None` means no default config is loaded, the
/// display/view pair is unknown, or the transform could not be built.
pub fn display_transform(display: &str, view: &str) -> Option<String> {
	display_transform_result(display, view).ok().flatten()
}

/// The $OCIO path when set, otherwise the extracted default config path
/// (C++ `ColorManager::get_config_path`; the actual file extraction is
/// deferred to the app layer — the Rust default config stays in memory).
pub fn config_path() -> Option<String> {
	if let Ok(path) = env::var("OCIO") {
		if !path.is_empty() {
			return Some(path);
		}
	}
	default_config()?;
	Some(format!(
		"{}/ocioconf/config.ocio",
		commonutil::configuration_location()
	))
}

// ---- OCIO GPU function shaders (C++ colormanagement.cpp GetColorContext) --

/// Cache of generated GLSL stubs, keyed by function name + color spaces +
/// config cache id (so a swapped test config re-generates). `None` entries
/// are cached too — the lookup result is deterministic per key.
static OCIO_STUB_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// The GLSL ES 3.0 shader text implementing the OCIO function `fn_name`
/// between `from_space` and `to_space` (both resolved as color-space names
/// or roles, exactly like the C++ `getProcessor(src, dst)` calls).
///
/// This mirrors the C++ OCIO-node shader path: the node's
/// `GenerateProcessor` builds a `ColorTransform` between two spaces and
/// `GetShaderCode` receives the auto-generated stub from
/// `Renderer::GetColorContext` (colormanagement.cpp), which the node then
/// splices into its fragment shader at the `%1` marker.
///
/// `None` when no default config exists or the processor is LUT-based:
/// `extractGpuShaderInfo` reports lookup textures that the C++ renderer
/// uploads per LUT (the loops right after `GetColorContext`), but the Rust
/// renderer has no such upload path, so those transforms are skipped and
/// the node passes through.
pub fn ocio_function_shader(fn_name: &str, from_space: &str, to_space: &str) -> Option<String> {
	let config = default_config()?;
	let cache_key = format!(
		"{}:{}:{}:{}",
		fn_name,
		from_space,
		to_space,
		config.cache_id().unwrap_or_default()
	);
	if let Some(hit) = OCIO_STUB_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&cache_key)
	{
		return hit.clone();
	}
	let stub = build_ocio_function_shader(&config, fn_name, from_space, to_space);
	OCIO_STUB_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(cache_key, stub.clone());
	stub
}

/// Build (but do not cache) the GLSL stub for `fn_name` between the two
/// spaces (C++ `GpuShaderDesc::CreateShaderDesc` + `setLanguage` +
/// `extractGpuShaderInfo`, colormanagement.cpp `GetColorContext`).
fn build_ocio_function_shader(
	config: &SafeConfig,
	fn_name: &str,
	from_space: &str,
	to_space: &str,
) -> Option<String> {
	let processor = config.processor(from_space, to_space).ok()?;
	let gpu = processor.default_gpu_processor().ok()?;
	let mut desc = ocio_rs::GpuShaderDesc::create().ok()?;
	desc.set_language(ocio_rs::GpuLanguage::GlslEs3_0).ok()?;
	desc.set_function_name(fn_name).ok()?;
	desc.set_resource_prefix("ocio_").ok()?;
	gpu.try_extract_shader_info(&mut desc).ok()?;
	// LUT-based processors need their 1D/3D textures uploaded (the C++
	// `GetColorContext` caller loops over `getNum3DTextures`/`getNumTextures`
	// right after extraction); without a LUT upload path these cannot render.
	if desc.num_textures() > 0 || desc.num_3d_textures() > 0 {
		return None;
	}
	desc.shader_text()
}

/// Cache of generated grading GLSL stubs, keyed by style + config cache id
/// (same shape as [`OCIO_STUB_CACHE`]; grading is analytic so the result is
/// deterministic per key).
static OCIO_GRADING_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// The GLSL text of the dynamic grading-primary processor for `style`
/// (C++ quercusrender creates the same processor via
/// `quercusrender_color_processor_create_grading_primary` and the renderer
/// applies it between passes; the Rust equivalent generates the OCIO GPU
/// shader the node splices at its `%1` marker). `None` when no default
/// config exists or the processor is LUT-based (the same guard as
/// [`ocio_function_shader`] — grading is analytic, so this is purely a
/// config-availability check).
pub fn grading_primary_function_shader(style: GradingStyle) -> Option<String> {
	let config = default_config()?;
	let style_id = match style {
		GradingStyle::Lin => "lin",
		GradingStyle::Log => "log",
	};
	let cache_key = format!("{style_id}:{}", config.cache_id().unwrap_or_default());
	if let Some(hit) = OCIO_GRADING_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&cache_key)
	{
		return hit.clone();
	}
	let stub = build_ocio_grading_shader(&config, style);
	OCIO_GRADING_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(cache_key, stub.clone());
	stub
}

/// Build (but do not cache) the grading GLSL stub: a dynamic
/// grading-primary transform on `config`, extracted exactly like
/// [`build_ocio_function_shader`].
fn build_ocio_grading_shader(config: &SafeConfig, style: GradingStyle) -> Option<String> {
	let transform = ocio_rs::transform::GradingPrimaryTransform::create(style.to_ocio()).ok()?;
	transform.make_dynamic();
	transform.set_direction(ocio_rs::TransformDirection::Forward);
	let processor = config
		.processor_from_transform(&transform, ocio_rs::TransformDirection::Forward)
		.ok()?;
	let gpu = processor.default_gpu_processor().ok()?;
	let mut desc = ocio_rs::GpuShaderDesc::create().ok()?;
	desc.set_language(ocio_rs::GpuLanguage::GlslEs3_0).ok()?;
	desc.set_function_name("ove_grading_primary").ok()?;
	desc.set_resource_prefix("ocio_").ok()?;
	gpu.try_extract_shader_info(&mut desc).ok()?;
	if desc.num_textures() > 0 || desc.num_3d_textures() > 0 {
		return None;
	}
	desc.shader_text()
}

// ---- LUT library (C++ LUTLibrary) ------------------------------------------

/// Supported LUT extensions (C++ `LUTLibrary::supported_extensions()`).
pub const SUPPORTED_LUT_EXTENSIONS: [&str; 9] = [
	"cube", "3dl", "spi1d", "spi3d", "spimtx", "csp", "clf", "ctf", "cub",
];

/// 1 when `extension` (dot optional, case-insensitive) is supported.
pub fn is_supported_lut_extension(extension: &str) -> bool {
	let ext = extension.strip_prefix('.').unwrap_or(extension);
	SUPPORTED_LUT_EXTENSIONS
		.iter()
		.any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests;
