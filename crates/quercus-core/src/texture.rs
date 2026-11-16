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

//! Textures and CPU frames.

use crate::PixelFormat;
use crate::Rational;
use std::sync::Arc;

use crate::backend::{BackendKind, GpuContextLike, YuvTransform};
use crate::error::{Error, Result};
use crate::frame::VideoParamsPod;

/// A CPU frame (the payload quercuscodec frames bridge into, and the value
/// `QuercusCodecFrame` handles box).
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
	/// Width of the pixel buffer (effective resolution).
	pub width: i32,
	/// Height of the pixel buffer (effective resolution).
	pub height: i32,
	/// Pixel format (F32 on the main pipeline).
	pub format: PixelFormat,
	/// Channel count (4 on the main pipeline).
	pub channels: i32,
	/// Timestamp in the sequence timebase.
	pub timestamp: Rational,
	/// Pixel payload (row-major, tightly packed).
	pub data: Vec<u8>,
	/// Full video metadata (divider/aspect/interlacing etc.).
	pub params: VideoParamsPod,
}

impl Default for Frame {
	fn default() -> Self {
		Self {
			width: 0,
			height: 0,
			format: PixelFormat::Invalid,
			channels: 0,
			timestamp: Rational::NULL,
			data: Vec::new(),
			params: VideoParamsPod::default(),
		}
	}
}

impl Frame {
	/// An empty frame (C++ `Frame::create()` before allocation).
	pub fn new() -> Self {
		Self::default()
	}

	/// The dummy frame: 0×0, transparent black, never uploaded
	/// (`Texture::dummy` semantics).
	pub fn dummy() -> Self {
		Self {
			width: 0,
			height: 0,
			format: PixelFormat::F32,
			channels: VideoParamsPod::INTERNAL_CHANNEL_COUNT,
			timestamp: Rational::new(0, 1),
			data: Vec::new(),
			params: VideoParamsPod::default(),
		}
	}

	/// Bytes per channel for the frame's format.
	pub fn bytes_per_channel(&self) -> usize {
		self.format.bytes_per_channel()
	}

	/// Line stride in bytes (tightly packed rows: `width * channels * bpc`).
	pub fn linesize_bytes(&self) -> usize {
		(self.width as usize)
			.saturating_mul(self.channels as usize)
			.saturating_mul(self.bytes_per_channel())
	}

	/// Total pixel payload size.
	pub fn allocated_size(&self) -> usize {
		(self.height as usize).saturating_mul(self.linesize_bytes())
	}

	/// True when the pixel buffer is allocated.
	pub fn is_allocated(&self) -> bool {
		!self.data.is_empty()
	}

	/// Set the frame's video metadata (dims, format, divider, aspect…).
	/// The channel count stays at the pipeline constant (4).
	pub fn set_video_params(&mut self, pod: VideoParamsPod) {
		self.params = pod;
		self.width = pod.effective_width();
		self.height = pod.effective_height();
		self.format = match pod.format {
			f if f == PixelFormat::U8 as i32 => PixelFormat::U8,
			f if f == PixelFormat::U10 as i32 => PixelFormat::U10,
			f if f == PixelFormat::U16 as i32 => PixelFormat::U16,
			f if f == PixelFormat::F16 as i32 => PixelFormat::F16,
			f if f == PixelFormat::F32 as i32 => PixelFormat::F32,
			_ => PixelFormat::Invalid,
		};
		self.channels = VideoParamsPod::INTERNAL_CHANNEL_COUNT;
	}

	/// The frame's video metadata as the public POD.
	pub fn video_params(&self) -> VideoParamsPod {
		let mut p = self.params;
		p.width = self.width;
		p.height = self.height;
		p.format = self.format as i32;
		p
	}

	/// Allocate (or re-allocate) the pixel buffer per the current metadata,
	/// zeroed. Returns false when the metadata is invalid
	/// (C++ `Frame::allocate`).
	pub fn allocate(&mut self) -> bool {
		if self.width <= 0 || self.height <= 0 || self.channels <= 0 {
			return false;
		}
		let size = self.allocated_size();
		if size == 0 {
			return false;
		}
		if self.data.len() != size {
			self.data = vec![0u8; size];
		} else {
			self.data.fill(0);
		}
		true
	}

	/// Borrowed pixel data pointer (empty when not allocated).
	pub fn data(&self) -> *const u8 {
		self.data.as_ptr()
	}

	/// Mutable pixel data pointer (empty when not allocated).
	pub fn data_mut(&mut self) -> *mut u8 {
		self.data.as_mut_ptr()
	}

	/// True for the dummy frame.
	pub fn is_dummy(&self) -> bool {
		self.width == 0 && self.height == 0 && self.data.is_empty()
	}

	/// True when the pixel format is a float type.
	pub fn is_float(&self) -> bool {
		matches!(self.format, PixelFormat::F16 | PixelFormat::F32)
	}

	/// Number of pixels.
	pub fn pixel_count(&self) -> usize {
		(self.width as usize).saturating_mul(self.height as usize)
	}
}

/// Shared ownership of a registry texture (M2). A `Texture::Gpu` value can
/// be cloned freely — the table handles, the graph compositor and the
/// present path all do — and the registry entry is destroyed exactly once,
/// when the last lease drops. Without it, dropping any one clone would
/// pull the texture out from under the others.
pub struct GpuLease {
	token: u64,
	ctx: Arc<dyn GpuContextLike>,
	/// False for a borrowed view: the token's lifetime is owned by another
	/// holder (the CUDA VMM slot pool), so dropping this lease must not
	/// destroy the registry entry.
	owned: bool,
}

impl GpuLease {
	/// Create an owning lease for registry texture `token` on `ctx`, returned in
	/// an `Arc` shared by every clone of the texture.
	///
	/// Dropping the last `Arc` calls `ctx.destroy_texture(token)`, so callers
	/// must not destroy the token themselves and must keep using the lease for
	/// as long as the texture lives. Use [`GpuLease::borrowed`] for tokens whose
	/// lifetime another holder (e.g. a CUDA VMM slot pool) manages.
	pub fn new(ctx: Arc<dyn GpuContextLike>, token: u64) -> Arc<Self> {
		Arc::new(Self {
			token,
			ctx,
			owned: true,
		})
	}

	/// A non-owning lease for a token whose lifetime is managed elsewhere
	/// (e.g. a reusable CUDA VMM slot that keeps its imported textures for
	/// many frames).
	pub fn borrowed(ctx: Arc<dyn GpuContextLike>, token: u64) -> Arc<Self> {
		Arc::new(Self {
			token,
			ctx,
			owned: false,
		})
	}

	/// The backend token this lease covers — a key in `ctx`'s texture registry,
	/// as passed to [`GpuLease::new`]/[`GpuLease::borrowed`].
	///
	/// Consumers store it in `Texture::Gpu`/`PlanarTexture` and hand it back to
	/// upload/download/destroy calls; the lease only decides when the token is
	/// destroyed.
	pub fn token(&self) -> u64 {
		self.token
	}
}

impl Drop for GpuLease {
	fn drop(&mut self) {
		if self.owned {
			self.ctx.destroy_texture(self.token);
		}
	}
}

/// How a frame's color components map onto GPU plane textures.
///
/// This is the layout axis of the pixel matrix. Plane order is always
/// `[Y, U, V]` for [`ColorModel::YuvPlanar`], `[Y, UV]` for
/// [`ColorModel::YuvSemiPlanar`] (interleaved U then V in `.r`/`.g`) and
/// `[RGB(A)]` for the interleaved models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorModel {
	/// Planar YUV: three single-channel planes (I420/YUV420P,
	/// YUV422P, YUV444P and their 10/16-bit variants).
	YuvPlanar,
	/// Semi-planar YUV: luma plane plus one interleaved two-channel
	/// chroma plane (NV12/P010, NV16/P216, and 4:4:4 equivalents).
	YuvSemiPlanar,
	/// Interleaved RGB: three source channels, stored in an
	/// `Rgba*` texture (alpha filled with 1.0 by the upload API).
	Rgb,
	/// Interleaved RGBA: four source channels, alpha preserved.
	Rgba,
}

impl ColorModel {
	/// True for the YUV color models (matrix/range conversion applies).
	pub fn is_yuv(self) -> bool {
		matches!(self, ColorModel::YuvPlanar | ColorModel::YuvSemiPlanar)
	}
}

/// Chroma subsampling of the YUV color models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subsampling {
	/// 4:2:0: chroma planes at half width and half height.
	Cs420,
	/// 4:2:2: chroma planes at half width and full height.
	Cs422,
	/// 4:4:4: chroma planes at full resolution.
	Cs444,
}

impl Subsampling {
	/// Chroma plane dimensions for a luma `width`×`height` frame
	/// (odd dimensions round up; the plane is at least 1×1).
	pub fn chroma_size(self, width: i32, height: i32) -> (i32, i32) {
		if width <= 0 || height <= 0 {
			return (0, 0);
		}
		match self {
			Subsampling::Cs420 => ((width + 1) / 2, (height + 1) / 2),
			Subsampling::Cs422 => ((width + 1) / 2, height),
			Subsampling::Cs444 => (width, height),
		}
	}

	/// Chroma/luma dimension ratio (the YUV pass `uv_scale` uniform):
	/// `(0.5, 0.5)` for 4:2:0, `(0.5, 1.0)` for 4:2:2, `(1.0, 1.0)`
	/// for 4:4:4.
	pub fn uv_scale(self) -> (f32, f32) {
		match self {
			Subsampling::Cs420 => (0.5, 0.5),
			Subsampling::Cs422 => (0.5, 1.0),
			Subsampling::Cs444 => (1.0, 1.0),
		}
	}

	/// Infer the subsampling from actual chroma and luma texture dimensions:
	/// the option whose [`Subsampling::chroma_size`] reproduces
	/// `(uv_width, uv_height)` for a `width`×`height` luma plane.
	///
	/// `uv_width`/`uv_height` are the chroma plane dimensions in pixels,
	/// `width`/`height` the luma dimensions; candidates are tried as 4:4:4,
	/// 4:2:2, then 4:2:0. Returns `None` when none matches (the chroma size
	/// follows no supported ratio) or the luma size is not positive. Ambiguous
	/// 1-pixel frames resolve to the larger (less subsampled) option, which is
	/// harmless because such a plane has one texel.
	pub fn from_chroma_size(
		uv_width: i32,
		uv_height: i32,
		width: i32,
		height: i32,
	) -> Option<Self> {
		if width <= 0 || height <= 0 {
			return None;
		}
		if (uv_width, uv_height) == (width, height) {
			Some(Subsampling::Cs444)
		} else if (uv_width, uv_height) == ((width + 1) / 2, height) {
			Some(Subsampling::Cs422)
		} else if (uv_width, uv_height) == ((width + 1) / 2, (height + 1) / 2) {
			Some(Subsampling::Cs420)
		} else {
			None
		}
	}
}

/// Per-component storage/decoding of every plane of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaneStorage {
	/// 8-bit normalized integer (`R8Unorm`/`Rg8Unorm`/`Rgba8Unorm`).
	U8,
	/// 16-bit normalized integer (`R16Unorm`/`Rg16Unorm`/`Rgba16Unorm`);
	/// this is also how 10-bit-in-16 samples are carried (P010-style).
	U16,
	/// IEEE half floats (`R16Float`/`Rg16Float`/`Rgba16Float`).
	F16,
	/// IEEE single floats (`R32Float`/`Rg32Float`/`Rgba32Float`).
	F32,
}

impl PlaneStorage {
	/// Storage bytes per component at this depth: 1 for `U8`, 2 for `U16`/`F16`
	/// and 4 for `F32`.
	///
	/// Used to size CPU frames, staging rows and GPU texel counts; callers
	/// multiply it by the component count and pixel count to get a byte size.
	pub fn bytes_per_component(self) -> usize {
		match self {
			PlaneStorage::U8 => 1,
			PlaneStorage::U16 | PlaneStorage::F16 => 2,
			PlaneStorage::F32 => 4,
		}
	}

	/// The normalization/conversion depth: 8, 16, 16, 32.
	pub fn bit_depth(self) -> u32 {
		match self {
			PlaneStorage::U8 => 8,
			PlaneStorage::U16 | PlaneStorage::F16 => 16,
			PlaneStorage::F32 => 32,
		}
	}

	/// Display name (diagnostics).
	pub fn name(self) -> &'static str {
		match self {
			PlaneStorage::U8 => "u8",
			PlaneStorage::U16 => "u16",
			PlaneStorage::F16 => "f16",
			PlaneStorage::F32 => "f32",
		}
	}

	/// The single-component texture format for this storage.
	pub fn texture_format(self) -> wgpu::TextureFormat {
		match self {
			PlaneStorage::U8 => wgpu::TextureFormat::R8Unorm,
			PlaneStorage::U16 => wgpu::TextureFormat::R16Unorm,
			PlaneStorage::F16 => wgpu::TextureFormat::R16Float,
			PlaneStorage::F32 => wgpu::TextureFormat::R32Float,
		}
	}

	/// Recover the storage type from a `wgpu` texture format (1, 2 or 4
	/// channel formats).
	pub fn from_texture_format(format: wgpu::TextureFormat) -> Option<Self> {
		use wgpu::TextureFormat as F;
		match format {
			F::R8Unorm | F::Rg8Unorm | F::Rgba8Unorm => Some(PlaneStorage::U8),
			F::R16Unorm | F::Rg16Unorm | F::Rgba16Unorm => Some(PlaneStorage::U16),
			F::R16Float | F::Rg16Float | F::Rgba16Float => Some(PlaneStorage::F16),
			F::R32Float | F::Rg32Float | F::Rgba32Float => Some(PlaneStorage::F32),
			_ => None,
		}
	}
}

/// Full plane layout of a decoded frame: color model, chroma subsampling
/// and per-component storage.
///
/// This is the pixel-matrix descriptor the GPU planar pass, the plane
/// texture allocator and the raw CPU upload API all consume. Every plane
/// of a frame shares the same [`PlaneStorage`]; the per-plane differences
/// are the channel count (1/2/4) and the chroma dimensions, both derived
/// from the color model + subsampling.
///
/// | model          | planes                          | 420 | 422 | 444 |
/// |----------------|---------------------------------|-----|-----|-----|
/// | `YuvPlanar`    | `[Y, U, V]`, 1 channel each     | ✓   | ✓   | ✓   |
/// | `YuvSemiPlanar`| `[Y, UV]`, 1 + 2 channels       | ✓   | ✓   | ✓   |
/// | `Rgb`          | `[RGB]`, stored 4-channel       | –   | –   | ✓   |
/// | `Rgba`         | `[RGBA]`, 4-channel             | –   | –   | ✓   |
///
/// Every cell exists for all four [`PlaneStorage`] depths (U8/U16/F16/F32);
/// U16 is how 10-bit-in-16 (P010/P216/YUV422P10/YUV444P10) is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlanarDescriptor {
	/// Plane layout (planar vs semi-planar vs interleaved RGB(A)).
	pub model: ColorModel,
	/// Chroma subsampling (ignored for the interleaved RGB models,
	/// where [`Subsampling::Cs444`] is conventional).
	pub subsampling: Subsampling,
	/// Per-component storage of every plane.
	pub storage: PlaneStorage,
}

impl PlanarDescriptor {
	/// Build a descriptor for any model/subsampling/storage combination.
	pub const fn new(model: ColorModel, subsampling: Subsampling, storage: PlaneStorage) -> Self {
		Self {
			model,
			subsampling,
			storage,
		}
	}

	/// Planar YUV descriptor: `[Y, U, V]`, three single-channel planes (I420 /
	/// YUV420P, YUV422P, YUV444P and their 10/16-bit variants).
	///
	/// `subsampling` selects the chroma dimensions (4:2:0 halves both axes,
	/// 4:2:2 only the width, 4:4:4 keeps full resolution); `storage` is the
	/// per-component depth shared by all three planes.
	pub const fn yuv_planar(subsampling: Subsampling, storage: PlaneStorage) -> Self {
		Self::new(ColorModel::YuvPlanar, subsampling, storage)
	}

	/// Semi-planar YUV descriptor: `[Y, UV]` — one single-channel luma plane
	/// plus one interleaved two-channel chroma plane (NV12/P010, NV16/P216
	/// style).
	///
	/// `subsampling` sets the chroma dimensions
	/// ([`Subsampling::Cs420`] halves both axes, [`Subsampling::Cs422`] only
	/// the width); `storage` is the depth shared by both planes, with U16
	/// carrying 10-bit-in-16 samples.
	pub const fn yuv_semi_planar(subsampling: Subsampling, storage: PlaneStorage) -> Self {
		Self::new(ColorModel::YuvSemiPlanar, subsampling, storage)
	}

	/// Interleaved RGB descriptor: one plane of three source components stored
	/// in a 4-channel RGBA texture, because `wgpu` has no 3-channel format.
	///
	/// The upload API fills alpha with 1.0. Subsampling is pinned to
	/// [`Subsampling::Cs444`] (ignored for this model); `storage` selects the
	/// per-component depth.
	pub const fn rgb(storage: PlaneStorage) -> Self {
		Self::new(ColorModel::Rgb, Subsampling::Cs444, storage)
	}

	/// Interleaved RGBA descriptor: one 4-channel plane with all four source
	/// components and alpha preserved.
	///
	/// Subsampling is pinned to [`Subsampling::Cs444`] because the interleaved
	/// models ignore it; `storage` selects the per-component depth.
	pub const fn rgba(storage: PlaneStorage) -> Self {
		Self::new(ColorModel::Rgba, Subsampling::Cs444, storage)
	}

	/// Number of GPU plane textures (3 planar YUV, 2 semi-planar, 1
	/// interleaved RGB/RGBA).
	pub fn plane_count(self) -> usize {
		match self.model {
			ColorModel::YuvPlanar => 3,
			ColorModel::YuvSemiPlanar => 2,
			ColorModel::Rgb | ColorModel::Rgba => 1,
		}
	}

	/// Source components per pixel of `plane` in CPU/import order: 1 for
	/// luma and planar chroma, 2 for the semi-planar chroma plane, 3 for
	/// RGB, 4 for RGBA. `None` when `plane` is out of range.
	pub fn plane_source_components(self, plane: usize) -> Option<usize> {
		match self.model {
			ColorModel::YuvPlanar => (plane < 3).then_some(1),
			ColorModel::YuvSemiPlanar => match plane {
				0 => Some(1),
				1 => Some(2),
				_ => None,
			},
			ColorModel::Rgb => (plane == 0).then_some(3),
			ColorModel::Rgba => (plane == 0).then_some(4),
		}
	}

	/// Components stored in the plane's GPU texture: identical to the
	/// source count except for RGB, which is expanded to a 4-channel
	/// texture (alpha 1.0) because `wgpu` has no 3-channel format.
	pub fn plane_texture_components(self, plane: usize) -> Option<usize> {
		match self.model {
			ColorModel::Rgb | ColorModel::Rgba => (plane == 0).then_some(4),
			_ => self.plane_source_components(plane),
		}
	}

	/// The `wgpu` texture format of this plane, or `None` when `plane` is out
	/// of range.
	///
	/// Combines the plane's texture component count
	/// ([`Self::plane_texture_components`], so RGB yields 4) with the shared
	/// [`PlaneStorage`]. Used by the plane texture allocator and by the legacy
	/// [`PlanarFormat`] luma/chroma accessors.
	pub fn plane_texture_format(self, plane: usize) -> Option<wgpu::TextureFormat> {
		let components = self.plane_texture_components(plane)?;
		Some(texture_format_for(self.storage, components))
	}

	/// Plane dimensions for a luma `width`×`height` frame. `None` when
	/// `plane` is out of range or the dimensions are not positive.
	pub fn plane_size(self, plane: usize, width: i32, height: i32) -> Option<(i32, i32)> {
		if width <= 0 || height <= 0 || plane >= self.plane_count() {
			return None;
		}
		match self.model {
			ColorModel::YuvPlanar | ColorModel::YuvSemiPlanar if plane > 0 => {
				Some(self.subsampling.chroma_size(width, height))
			}
			_ => Some((width, height)),
		}
	}

	/// The normalization depth of the storage (8/16/16/32); the argument
	/// for [`crate::backend::YuvTransform::from_matrix_depth`] when the
	/// source range must be expanded.
	pub fn bit_depth(self) -> u32 {
		self.storage.bit_depth()
	}

	/// Chroma/luma dimension ratio used by the descriptor-driven YUV
	/// pass: the subsampling ratio for the YUV models, `(1.0, 1.0)` for
	/// interleaved RGB(A).
	pub fn uv_scale(self) -> (f32, f32) {
		if self.model.is_yuv() {
			self.subsampling.uv_scale()
		} else {
			(1.0, 1.0)
		}
	}

	/// Chroma plane dimensions for a luma `width`×`height` frame (the
	/// luma size for interleaved RGB(A)).
	pub fn chroma_size(self, width: i32, height: i32) -> (i32, i32) {
		if self.model.is_yuv() {
			self.subsampling.chroma_size(width, height)
		} else if width > 0 && height > 0 {
			(width, height)
		} else {
			(0, 0)
		}
	}

	/// True for the YUV color models.
	pub fn is_yuv(self) -> bool {
		self.model.is_yuv()
	}

	/// The legacy [`PlanarFormat`] whose exact layout this descriptor
	/// matches (`NV12`/`P010`/`YUV422P10`), if any. General descriptor
	/// layouts (4:4:4, planar 4:2:2, RGB(A), float storage…) return
	/// `None`.
	pub fn legacy_format(self) -> Option<PlanarFormat> {
		[
			PlanarFormat::Nv12,
			PlanarFormat::P010,
			PlanarFormat::Yuv422P10,
		]
		.into_iter()
		.find(|format| format.descriptor() == self)
	}

	/// Short diagnostic name, e.g. `yuv-planar-422-u16` or `rgba-f32`.
	pub fn name(self) -> String {
		let model = match self.model {
			ColorModel::YuvPlanar => "yuv-planar",
			ColorModel::YuvSemiPlanar => "yuv-semi-planar",
			ColorModel::Rgb => "rgb",
			ColorModel::Rgba => "rgba",
		};
		let sub = match self.model {
			ColorModel::YuvPlanar | ColorModel::YuvSemiPlanar => match self.subsampling {
				Subsampling::Cs420 => "-420",
				Subsampling::Cs422 => "-422",
				Subsampling::Cs444 => "-444",
			},
			_ => "",
		};
		format!("{model}{sub}-{}", self.storage.name())
	}
}

/// The `wgpu` texture format for a `components`-channel plane of `storage`:
/// one normalized/float format each for 1, 2 and 4 channels.
///
/// Private helper behind [`PlanarDescriptor::plane_texture_format`] (and the
/// legacy [`PlanarFormat`] accessors); RGB is expanded to 4 channels
/// upstream, so `components == 3` is never requested.
///
/// # Panics
///
/// Panics (`unreachable!`) when `components` is not 1, 2 or 4 — the only
/// values [`PlanarDescriptor::plane_texture_components`] can produce.
fn texture_format_for(storage: PlaneStorage, components: usize) -> wgpu::TextureFormat {
	use wgpu::TextureFormat as F;
	match (components, storage) {
		(1, PlaneStorage::U8) => F::R8Unorm,
		(1, PlaneStorage::U16) => F::R16Unorm,
		(1, PlaneStorage::F16) => F::R16Float,
		(1, PlaneStorage::F32) => F::R32Float,
		(2, PlaneStorage::U8) => F::Rg8Unorm,
		(2, PlaneStorage::U16) => F::Rg16Unorm,
		(2, PlaneStorage::F16) => F::Rg16Float,
		(2, PlaneStorage::F32) => F::Rg32Float,
		(4, PlaneStorage::U8) => F::Rgba8Unorm,
		(4, PlaneStorage::U16) => F::Rgba16Unorm,
		(4, PlaneStorage::F16) => F::Rgba16Float,
		(4, PlaneStorage::F32) => F::Rgba32Float,
		// `plane_texture_components` only yields 1, 2 or 4.
		_ => unreachable!("unsupported planar component count {components}"),
	}
}

/// A hardware surface's legacy chroma layout (M5).
///
/// Kept as a source-compatible convenience enum for the hardware-import
/// producers; it maps onto the generalized [`PlanarDescriptor`] via
/// [`PlanarFormat::descriptor`]. New layouts should use the descriptor
/// directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanarFormat {
	/// 8-bit NV12: R8 luma + interleaved R8G8 chroma (4:2:0).
	Nv12,
	/// 10-bit P010: 16-bit left-aligned luma + interleaved 16-bit
	/// chroma (4:2:0).
	P010,
	/// 10-bit 4:2:2: 16-bit luma + interleaved 16-bit chroma at half
	/// width and full height (`R16Unorm` + `Rg16Unorm`).
	Yuv422P10,
}

impl PlanarFormat {
	/// The generalized descriptor for this legacy layout.
	pub fn descriptor(self) -> PlanarDescriptor {
		match self {
			PlanarFormat::Nv12 => {
				PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U8)
			}
			PlanarFormat::P010 => {
				PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U16)
			}
			PlanarFormat::Yuv422P10 => {
				PlanarDescriptor::yuv_semi_planar(Subsampling::Cs422, PlaneStorage::U16)
			}
		}
	}

	/// The single-channel luma plane format.
	pub fn y_format(self) -> wgpu::TextureFormat {
		self.descriptor()
			.plane_texture_format(0)
			.expect("a planar format always has a luma plane")
	}

	/// The interleaved two-channel chroma plane format.
	pub fn uv_format(self) -> wgpu::TextureFormat {
		self.descriptor()
			.plane_texture_format(1)
			.expect("a legacy planar format always has a chroma plane")
	}

	/// Display name (diagnostics).
	pub fn name(self) -> &'static str {
		match self {
			PlanarFormat::Nv12 => "nv12",
			PlanarFormat::P010 => "p010",
			PlanarFormat::Yuv422P10 => "yuv422p10",
		}
	}

	/// The source bit depth (the limited-range expansion depth for
	/// [`crate::backend::YuvTransform::from_matrix_depth`]).
	pub fn bit_depth(self) -> u32 {
		self.descriptor().bit_depth()
	}

	/// Storage bytes per component (1 for NV12, 2 for 10-bit layouts).
	pub fn bytes_per_component(self) -> usize {
		self.descriptor().storage.bytes_per_component()
	}

	/// Chroma plane dimensions for a luma `width`×`height` frame, delegated to
	/// this layout's [`PlanarDescriptor`].
	///
	/// The legacy layouts are all 4:2:0 or 4:2:2, so odd luma dimensions round
	/// the chroma size up (at least 1×1); non-positive input yields `(0, 0)`.
	pub fn chroma_size(self, width: i32, height: i32) -> (i32, i32) {
		self.descriptor().chroma_size(width, height)
	}

	/// Chroma/luma dimension ratio (`(0.5, 0.5)` or `(0.5, 1.0)`).
	pub fn uv_scale(self) -> (f32, f32) {
		self.descriptor().uv_scale()
	}

	/// True for the 4:2:2 layout (full-height chroma).
	pub fn is_422(self) -> bool {
		matches!(self, PlanarFormat::Yuv422P10)
	}
}

impl From<PlanarFormat> for PlanarDescriptor {
	fn from(format: PlanarFormat) -> Self {
		format.descriptor()
	}
}

/// A zero-copy hardware-decode (or raw-upload) frame: the decoder's
/// planes imported as GPU textures, plus everything the render thread
/// needs to turn it into a working-space RGBA texture —
/// [`GpuContext::run_planar_matrix_to_rgb`] for the matrix/range and the
/// raw source colorimetry for the source→working transform.
///
/// This is a *transient* texture: footage decode produces it and the
/// footage path resolves it to `Texture::Gpu` before any node samples
/// it. Every other consumer sees only `Gpu`/`Cpu`.
///
/// [`GpuContext::run_planar_matrix_to_rgb`]: crate::backend::GpuContext::run_planar_matrix_to_rgb
#[derive(Clone)]
pub struct PlanarTexture {
	/// Plane tokens in descriptor order: `[Y, U, V]` for planar YUV,
	/// `[Y, UV]` for semi-planar, `[RGB(A)]` for interleaved. Entries at
	/// or beyond [`PlanarTexture::plane_count`] are 0 and unused.
	pub planes: [u64; 3],
	/// Number of valid entries in [`PlanarTexture::planes`] (1, 2 or 3).
	pub plane_count: usize,
	/// The full plane layout (color model, subsampling, storage).
	pub descriptor: PlanarDescriptor,
	/// Luma/first plane token — legacy alias of `planes[0]`, valid for
	/// every color model.
	pub y: u64,
	/// Second plane token — legacy alias of `planes[1]` (interleaved
	/// chroma for semi-planar, U for planar YUV). For the 1-plane
	/// RGB/RGBA models it aliases `planes[0]`; use
	/// [`PlanarTexture::plane_tokens`] for those.
	pub uv: u64,
	/// Frame width (luma resolution).
	pub width: i32,
	/// Frame height (luma resolution).
	pub height: i32,
	/// The YUV→RGB matrix/range for the planes (ignored for RGB/RGBA).
	pub transform: YuvTransform,
	/// Source color-primaries code point (`AVCOL_PRI_*`).
	pub color_primaries: i32,
	/// Source transfer-characteristic code point (`AVCOL_TRC_*`).
	pub color_trc: i32,
	/// The context owning the plane textures.
	pub ctx: Arc<dyn GpuContextLike>,
	/// Plane-token ownership: dropping the last `PlanarTexture` clone
	/// releases the registry tokens (and with them the imported
	/// surfaces). Never read directly.
	#[allow(dead_code)]
	y_lease: Arc<GpuLease>,
	#[allow(dead_code)]
	uv_lease: Option<Arc<GpuLease>>,
	#[allow(dead_code)]
	v_lease: Option<Arc<GpuLease>>,
	/// Extra lifetime guard for platforms whose GPU texture types cannot
	/// carry a drop callback (macOS/Metal): the decoder's pixel-buffer
	/// reference is held here and released with the planar texture.
	#[allow(dead_code)]
	pub keep_alive: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl PlanarTexture {
	/// Build a planar texture from imported plane tokens: `planes` is
	/// `(luma, chroma)`, `size` the luma dimensions. Legacy two-plane
	/// (NV12/P010/YUV422P10) adapter over
	/// [`PlanarTexture::from_descriptor`].
	pub fn new(
		ctx: Arc<dyn GpuContextLike>,
		format: PlanarFormat,
		size: (i32, i32),
		planes: (u64, u64),
		transform: YuvTransform,
		color: (i32, i32),
	) -> Self {
		let (y, uv) = planes;
		let descriptor = format.descriptor();
		Self::build(ctx, descriptor, size, vec![y, uv], transform, color)
			.expect("a legacy planar format always has two planes")
	}

	/// Build a planar texture from a general descriptor and 1..3 plane
	/// tokens in descriptor order (`[Y,U,V]`, `[Y,UV]` or `[RGB(A)]`).
	/// The token count must equal [`PlanarDescriptor::plane_count`].
	pub fn from_descriptor(
		ctx: Arc<dyn GpuContextLike>,
		descriptor: PlanarDescriptor,
		size: (i32, i32),
		planes: &[u64],
		transform: YuvTransform,
		color: (i32, i32),
	) -> Result<Self> {
		if planes.len() != descriptor.plane_count() || planes.is_empty() || planes.len() > 3 {
			return Err(Error::Invalid);
		}
		if size.0 <= 0 || size.1 <= 0 {
			return Err(Error::Invalid);
		}
		Self::build(ctx, descriptor, size, planes.to_vec(), transform, color)
	}

	/// Borrowed-descriptor twin of [`PlanarTexture::from_descriptor`]: the
	/// plane tokens stay owned by the caller (a reusable CUDA VMM slot), so
	/// dropping this texture does not destroy them. `keep_alive` must keep
	/// the owner alive.
	pub fn from_descriptor_borrowed(
		ctx: Arc<dyn GpuContextLike>,
		descriptor: PlanarDescriptor,
		size: (i32, i32),
		planes: &[u64],
		transform: YuvTransform,
		color: (i32, i32),
		keep_alive: Arc<dyn std::any::Any + Send + Sync>,
	) -> Result<Self> {
		if planes.len() != descriptor.plane_count() || planes.is_empty() || planes.len() > 3 {
			return Err(Error::Invalid);
		}
		if size.0 <= 0 || size.1 <= 0 {
			return Err(Error::Invalid);
		}
		let (width, height) = size;
		let (color_primaries, color_trc) = color;
		let count = planes.len();
		let mut tokens = [0u64; 3];
		tokens[..count].copy_from_slice(planes);
		let y_lease = GpuLease::borrowed(ctx.clone(), tokens[0]);
		let uv_lease = (count > 1).then(|| GpuLease::borrowed(ctx.clone(), tokens[1]));
		let v_lease = (count > 2).then(|| GpuLease::borrowed(ctx.clone(), tokens[2]));
		let y = tokens[0];
		let uv = if count > 1 { tokens[1] } else { tokens[0] };
		Ok(Self {
			planes: tokens,
			plane_count: count,
			descriptor,
			y,
			uv,
			width,
			height,
			transform,
			color_primaries,
			color_trc,
			y_lease,
			uv_lease,
			v_lease,
			keep_alive: Some(keep_alive),
			ctx,
		})
	}

	/// Shared constructor behind [`PlanarTexture::new`],
	/// [`PlanarTexture::from_descriptor`] and the borrowed twin: copies the
	/// plane tokens into the fixed array, creates one owning [`GpuLease`] per
	/// plane and fills the alias fields.
	///
	/// `planes` must hold exactly [`PlanarDescriptor::plane_count`] tokens (1,
	/// 2 or 3) in descriptor order; `size` is the luma resolution in pixels and
	/// `color` the `(primaries, transfer)` pair kept for the source→working
	/// transform. `size` is not validated here — `from_descriptor` checks it,
	/// while [`PlanarTexture::new`] relies on the caller.
	///
	/// # Errors
	///
	/// `Error::Invalid` when `planes` is empty or its length disagrees with the
	/// descriptor's plane count.
	fn build(
		ctx: Arc<dyn GpuContextLike>,
		descriptor: PlanarDescriptor,
		size: (i32, i32),
		planes: Vec<u64>,
		transform: YuvTransform,
		color: (i32, i32),
	) -> Result<Self> {
		if planes.len() != descriptor.plane_count() || planes.is_empty() {
			return Err(Error::Invalid);
		}
		let (width, height) = size;
		let (color_primaries, color_trc) = color;
		let count = planes.len();
		let mut tokens = [0u64; 3];
		tokens[..count].copy_from_slice(&planes);
		let y_lease = GpuLease::new(ctx.clone(), tokens[0]);
		let uv_lease = (count > 1).then(|| GpuLease::new(ctx.clone(), tokens[1]));
		let v_lease = (count > 2).then(|| GpuLease::new(ctx.clone(), tokens[2]));
		let y = tokens[0];
		let uv = if count > 1 { tokens[1] } else { tokens[0] };
		Ok(Self {
			planes: tokens,
			plane_count: count,
			descriptor,
			y,
			uv,
			width,
			height,
			transform,
			color_primaries,
			color_trc,
			y_lease,
			uv_lease,
			v_lease,
			keep_alive: None,
			ctx,
		})
	}

	/// The valid plane tokens in descriptor order.
	pub fn plane_tokens(&self) -> &[u64] {
		&self.planes[..self.plane_count]
	}

	/// The chroma plane size for this frame (luma size for RGB/RGBA).
	pub fn uv_size(&self) -> (i32, i32) {
		self.descriptor.chroma_size(self.width, self.height)
	}

	/// The chroma/luma dimension ratio (the YUV pass `uv_scale`):
	/// `(0.5, 0.5)` for 4:2:0, `(0.5, 1.0)` for 4:2:2, `(1.0, 1.0)` for
	/// 4:4:4 and interleaved RGB(A).
	pub fn uv_scale(&self) -> (f32, f32) {
		self.descriptor.uv_scale()
	}

	/// The normalization depth of the plane storage (8/16/16/32).
	pub fn bit_depth(&self) -> u32 {
		self.descriptor.bit_depth()
	}

	/// The legacy [`PlanarFormat`] when this texture's descriptor matches
	/// one of the legacy layouts exactly; `None` for the general matrix
	/// descriptors.
	pub fn legacy_format(&self) -> Option<PlanarFormat> {
		self.descriptor.legacy_format()
	}

	/// Attach a non-CPU lifetime guard (macOS: the decoder's pixel
	/// buffer), released with this texture.
	pub fn set_keep_alive(&mut self, guard: Arc<dyn std::any::Any + Send + Sync>) {
		self.keep_alive = Some(guard);
	}
}

impl std::fmt::Debug for PlanarTexture {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("PlanarTexture")
			.field("planes", &self.plane_tokens())
			.field("descriptor", &self.descriptor)
			.field("width", &self.width)
			.field("height", &self.height)
			.finish_non_exhaustive()
	}
}

/// A texture: either backend-resident (GPU) or a CPU-frame wrapper.
///
/// GPU textures carry an `Arc` to their [`GpuContext`](crate::backend::GpuContext)
/// (the C++ `TexturePtr` keeps its renderer alive the same way), so a
/// texture value can upload/download/blit without a separate renderer
/// handle, plus a [`GpuLease`] that releases the backend token when the
/// last clone goes away.
#[derive(Clone)]
pub enum Texture {
	/// Backend GPU texture.
	Gpu {
		/// Backend token (wgpu texture registry key).
		token: u64,
		/// Owning backend.
		backend: BackendKind,
		/// Width.
		width: i32,
		/// Height.
		height: i32,
		/// Pixel format.
		format: PixelFormat,
		/// The context owning the texture (trait object so tests can fake
		/// the GPU side; `GpuContext` is the only production implementor).
		ctx: Arc<dyn GpuContextLike>,
		/// Shared token ownership (destroyed with the last clone).
		lease: Arc<GpuLease>,
		/// Presentation timestamp carried across the readback boundary.
		timestamp: Rational,
	},
	/// Imported hardware-decode YUV frame (M5); resolved to `Gpu` by the
	/// footage path before consumers run.
	Planar(PlanarTexture),
	/// CPU-frame wrapper (uploaded lazily by the backend).
	Cpu(Frame),
}

impl std::fmt::Debug for Texture {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Texture::Gpu {
				token,
				backend,
				width,
				height,
				format,
				..
			} => f
				.debug_struct("Texture::Gpu")
				.field("token", token)
				.field("backend", backend)
				.field("width", width)
				.field("height", height)
				.field("format", format)
				.finish(),
			Texture::Planar(p) => f.debug_tuple("Texture::Planar").field(p).finish(),
			Texture::Cpu(frame) => f.debug_tuple("Texture::Cpu").field(frame).finish(),
		}
	}
}

impl Texture {
	/// Wrap a registry texture (M2): builds the shared [`GpuLease`] so
	/// clones release the token exactly once.
	pub fn gpu(
		ctx: Arc<dyn GpuContextLike>,
		token: u64,
		width: i32,
		height: i32,
		format: PixelFormat,
	) -> Self {
		let lease = GpuLease::new(ctx.clone(), token);
		Texture::Gpu {
			token,
			backend: ctx.kind(),
			width,
			height,
			format,
			ctx,
			lease,
			timestamp: Rational::default(),
		}
	}

	/// Attach a presentation timestamp without changing texture ownership.
	pub fn with_timestamp(mut self, timestamp: Rational) -> Self {
		match &mut self {
			Texture::Gpu {
				timestamp: current, ..
			} => *current = timestamp,
			Texture::Cpu(frame) => frame.timestamp = timestamp,
			Texture::Planar(_) => {}
		}
		self
	}

	/// A dummy/empty texture (C++ `Texture::dummy` semantics): reads as
	/// transparent black, never uploaded.
	pub fn dummy() -> Self {
		Texture::Cpu(Frame::dummy())
	}

	/// True for dummy textures.
	pub fn is_dummy(&self) -> bool {
		match self {
			Texture::Gpu { .. } | Texture::Planar(_) => false,
			Texture::Cpu(f) => f.is_dummy(),
		}
	}

	/// Wrap a CPU `frame` as [`Texture::Cpu`] without copying pixel data.
	///
	/// The frame's pixels are moved into the texture and uploaded lazily by
	/// the backend on first use; [`Texture::size`]/[`Texture::format`] report
	/// the frame's own values.
	pub fn wrap_frame(frame: Frame) -> Self {
		Texture::Cpu(frame)
	}

	/// Wrap an imported hardware YUV frame (M5) as [`Texture::Planar`].
	///
	/// The result is transient: the footage path resolves it to
	/// `Texture::Gpu` before any consumer samples it, and
	/// [`Texture::to_frame`] on it returns `Error::State` until then.
	/// `planar` keeps the plane leases, so dropping the wrapper releases
	/// the plane tokens.
	pub fn wrap_planar(planar: PlanarTexture) -> Self {
		Texture::Planar(planar)
	}

	/// The imported planar frame, when this is one.
	pub fn as_planar(&self) -> Option<&PlanarTexture> {
		match self {
			Texture::Planar(p) => Some(p),
			_ => None,
		}
	}

	/// True for imported hardware YUV frames (M5) — the footage path's
	/// resolve trigger.
	pub fn is_planar(&self) -> bool {
		matches!(self, Texture::Planar(_))
	}

	/// Read back into a CPU frame (downloads for GPU textures). An
	/// unresolved planar texture is an error: the caller must resolve
	/// (YUV→RGB) through the footage path first.
	pub fn to_frame(&self) -> Result<Frame> {
		match self {
			Texture::Cpu(f) => Ok(f.clone()),
			Texture::Gpu {
				token,
				ctx,
				timestamp,
				..
			} => {
				let mut frame = ctx.download(*token)?;
				frame.timestamp = *timestamp;
				Ok(frame)
			}
			Texture::Planar(_) => Err(Error::State),
		}
	}

	/// Async form of [`Texture::to_frame`]: the GPU readback runs on the
	/// dedicated readback thread, so a render thread can submit the next
	/// frame before this one is mapped. CPU textures return immediately.
	pub fn to_frame_async(&self) -> Result<crate::backend::FrameReadback> {
		match self {
			Texture::Gpu { token, ctx, .. } => ctx.download_async(*token),
			Texture::Cpu(_) => Err(Error::State),
			Texture::Planar(_) => Err(Error::State),
		}
	}

	/// Dimensions (0x0 for dummy).
	pub fn size(&self) -> (i32, i32) {
		match self {
			Texture::Gpu { width, height, .. } => (*width, *height),
			Texture::Planar(p) => (p.width, p.height),
			Texture::Cpu(f) => (f.width, f.height),
		}
	}

	/// The texture's pixel format. A planar texture reports `F32`: it is
	/// transient, and the footage path always resolves it to an F32 RGBA
	/// GPU texture before any consumer sees it.
	pub fn format(&self) -> PixelFormat {
		match self {
			Texture::Gpu { format, .. } => *format,
			Texture::Planar(_) => PixelFormat::F32,
			Texture::Cpu(f) => f.format,
		}
	}

	/// The backend kind hosting the texture.
	pub fn backend(&self) -> BackendKind {
		match self {
			Texture::Gpu { backend, .. } => *backend,
			Texture::Planar(p) => p.ctx.kind(),
			Texture::Cpu(_) => BackendKind::Cpu,
		}
	}
}

#[cfg(test)]
mod tests;
