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

//! M5: zero-copy hardware-decode import.
//!
//! A decoded hardware surface (VAAPI/NVDEC/D3D11VA/VideoToolbox) is
//! imported into the engine's wgpu device as a pair of planar textures
//! (luma + interleaved chroma) instead of being downloaded with
//! `av_hwframe_transfer_data`. The render side turns the planes into
//! working-space RGBA with the M2 YUV→RGB pass; the CPU staging path
//! stays the per-frame fallback.
//!
//! Platform surface extraction lives here (FFmpeg types); the raw
//! Vulkan/Metal import lives in `quercus_core::backend::external`.
//!
//! ## Counters
//!
//! - [`HW_IMPORTS`] — frames that took the zero-copy path.
//! - [`HW_IMPORT_FALLBACKS`] — attempts that were tried and failed.
//! - [`HW_IMPORT_UNSUPPORTED`] — frames that skipped the import because
//!   no import path exists (NVDEC, a platform switch off, an
//!   unimportable layout); not failures.
//! - `hwdecode::HW_TRANSFERS` — frames downloaded to system memory; a
//!   zero-copy hardware frame must not move it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ffmpeg::ffi as sys;
use ffmpeg_next as ffmpeg;
use quercus_core::backend::{GpuContext, YuvTransform};
#[cfg(target_os = "linux")]
use quercus_core::backend::ImportGuard;
use quercus_core::colormath::YuvMatrix;
use quercus_core::texture::{PlanarDescriptor, PlanarFormat};

/// Frames that were imported to the GPU instead of downloaded (M5
/// acceptance: the hardware path keeps `hwdecode::HW_TRANSFERS` at 0).
pub static HW_IMPORTS: AtomicU64 = AtomicU64::new(0);

/// Import attempts that were actually attempted and failed (per-frame
/// fallback; feeds the consecutive-failure breaker). "Unsupported"
/// outcomes are deliberately NOT counted here — a missing import path is
/// not a failure (M5 audit).
pub static HW_IMPORT_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// Frames that took the staging path because no import path exists for
/// this device/machine/configuration: NVDEC/CUDA has no public handle
/// export, a per-platform switch is off, or the surface layout is not
/// importable. Observability only: neither counted as a fallback nor fed
/// to the breaker.
pub static HW_IMPORT_UNSUPPORTED: AtomicU64 = AtomicU64::new(0);

/// Consecutive import failures after which the process stops attempting
/// (the platform combination is evidently unsupported on this machine).
/// A single failure does not disable anything — the next frame is
/// attempted afresh, as the per-frame fallback contract requires.
const STICKY_FAILURE_LIMIT: u32 = 8;

static CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);

/// The config key of the zero-copy import switch (1 = import, 0 =
/// staging). Default ON; `QUERCUS_GPU_IMPORT=0` overrides both.
pub const CONFIG_KEY_GPU_DECODE_IMPORT: &str = "GpuDecodeImport";

/// The per-platform import switches (each platform's import is an
/// independent, revertable row of design §3.6).
pub const CONFIG_KEY_VAAPI_IMPORT: &str = "GpuDecodeImportVaapi";
/// Windows D3D11VA import switch.
pub const CONFIG_KEY_D3D11_IMPORT: &str = "GpuDecodeImportD3d11";
/// macOS VideoToolbox import switch.
pub const CONFIG_KEY_VIDEOTOOLBOX_IMPORT: &str = "GpuDecodeImportVideotoolbox";

/// Whether the zero-copy hardware import is preferred. `QUERCUS_GPU_IMPORT=0`
/// force-disables it (diagnostics/CI without importable surfaces), same
/// convention as `QUERCUS_HWACCEL`.
pub fn gpu_import_enabled() -> bool {
	if let Ok(v) = std::env::var("QUERCUS_GPU_IMPORT") {
		return v != "0";
	}
	switch_from_config(CONFIG_KEY_GPU_DECODE_IMPORT)
}

/// One platform's import switch (global import switch AND platform
/// switch). `env` overrides the config key, like the global switch.
fn platform_import_enabled(env: &str, config: &str) -> bool {
	if let Ok(v) = std::env::var(env) {
		return v != "0";
	}
	switch_from_config(config)
}

fn switch_from_config(key: &str) -> bool {
	match quercus_core::configstore::ConfigStore::instance().get(None, key) {
		Ok(value) => value != "false",
		Err(_) => true,
	}
}

/// Reset the import counters (tests).
pub fn reset_import_counters() {
	HW_IMPORTS.store(0, Ordering::Relaxed);
	HW_IMPORT_FALLBACKS.store(0, Ordering::Relaxed);
	HW_IMPORT_UNSUPPORTED.store(0, Ordering::Relaxed);
	CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
}

/// Everything the import needs from the decoder's frame and colorimetry.
pub struct HwImportRequest<'a> {
	/// The decoded frame (a hardware surface when import is possible).
	/// Borrowed for the duration of the attempt; the import guard takes
	/// its own reference to the underlying surface.
	pub frame: *const sys::AVFrame,
	/// Lifetime marker for the borrowed frame.
	pub _marker: std::marker::PhantomData<&'a ()>,
	/// The session's hardware device type (`None` = software decode).
	pub device_type: Option<sys::AVHWDeviceType>,
	/// Frame dimensions (luma).
	pub luma_size: (u32, u32),
	/// The frame's YUV→RGB luma matrix.
	pub matrix: YuvMatrix,
	/// Full/limited range.
	pub full_range: bool,
	/// Source color-primaries code point.
	pub color_primaries: i32,
	/// Source transfer-characteristic code point.
	pub color_trc: i32,
}

/// A successfully imported hardware frame: the plane tokens plus the
/// colorimetry the render side needs to resolve them to working-space
/// RGBA.
pub struct ImportedHwFrame {
	/// Plane tokens in descriptor order.
	pub planes: Vec<u64>,
	/// Generic plane layout (color model, subsampling, storage depth).
	pub descriptor: PlanarDescriptor,
	/// Luma dimensions.
	pub width: u32,
	/// Frame height.
	pub height: u32,
	/// The GPU device that owns the planes.
	pub ctx: Arc<GpuContext>,
	/// Extra lifetime guard for platforms whose GPU texture types cannot
	/// carry a drop callback (macOS) or for a reusable VMM slot.
	pub keep_alive: Option<Arc<dyn std::any::Any + Send + Sync>>,
	/// True when the plane tokens are owned by `keep_alive` (a reusable
	/// CUDA VMM slot), so the PlanarTexture must use only borrowed leases.
	pub borrowed_planes: bool,
}

impl ImportedHwFrame {
	/// The YUV→RGB transform for the planes (the bit depth picks the
	/// exact limited-range expansion: 8 for NV12, 16 for P010).
	pub fn transform(&self, matrix: YuvMatrix, full_range: bool) -> YuvTransform {
		YuvTransform::from_matrix_depth(matrix, full_range, self.descriptor.storage.bit_depth())
	}
}

/// Try to import `req.frame` as a pair of planar GPU textures. `None`
/// means "use the staging path for this frame" — the caller must then
/// call the CPU retrieval (the decoder session cache still holds the
/// decoded surface, so no re-decode happens).
pub fn try_import_hw_frame(req: &HwImportRequest<'_>) -> Option<ImportedHwFrame> {
	if !gpu_import_enabled() {
		return None;
	}
	if CONSECUTIVE_FAILURES.load(Ordering::Relaxed) >= STICKY_FAILURE_LIMIT {
		return None;
	}
	let device_type = req.device_type?;
	// Only a host-installed device (the app's UI/render context) may take
	// the frame: a lazily created worker/CLI context must keep decoding on
	// the CPU path. Checked before touching the frame so the common
	// "no import here" case costs nothing.
	if !GpuContext::host_gpu_installed() {
		return None;
	}
	if !crate::hwdecode::is_hw_format(frame_format(req.frame)) {
		return None;
	}
	let Some(ctx) = GpuContext::shared() else {
		// The host slot was cleared between the two checks (shutdown):
		// nothing was attempted, so this is not a failure.
		HW_IMPORT_UNSUPPORTED.fetch_add(1, Ordering::Relaxed);
		return None;
	};
	match platform_import(req, &ctx, device_type) {
		ImportOutcome::Imported {
			planes,
			descriptor,
			keep_alive,
			borrowed_planes,
		} => {
			CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
			HW_IMPORTS.fetch_add(1, Ordering::Relaxed);
			Some(ImportedHwFrame {
				planes,
				descriptor,
				width: req.luma_size.0,
				height: req.luma_size.1,
				ctx,
				keep_alive,
				borrowed_planes,
			})
		}
		ImportOutcome::Unsupported => {
			HW_IMPORT_UNSUPPORTED.fetch_add(1, Ordering::Relaxed);
			None
		}
		ImportOutcome::Failed => {
			bump_fallback();
			None
		}
	}
}

/// Outcome of one platform import attempt.
enum ImportOutcome {
	/// The planes were imported; the tokens and keep-alive guard.
	Imported {
		planes: Vec<u64>,
		descriptor: PlanarDescriptor,
		keep_alive: Option<Arc<dyn std::any::Any + Send + Sync>>,
		borrowed_planes: bool,
	},
	/// No import path exists for this device/machine/configuration (the
	/// frame takes the staging path without being a failure).
	Unsupported,
	/// The import path exists and the attempt failed: per-frame fallback,
	/// counted and fed to the consecutive-failure breaker.
	Failed,
}

fn bump_fallback() {
	HW_IMPORT_FALLBACKS.fetch_add(1, Ordering::Relaxed);
	CONSECUTIVE_FAILURES.fetch_add(1, Ordering::Relaxed);
}

/// The frame's raw pixel format (hardware variants included).
fn frame_format(frame: *const sys::AVFrame) -> sys::AVPixelFormat {
	// SAFETY: plain read of the frame's format field.
	unsafe { std::mem::transmute::<i32, sys::AVPixelFormat>((*frame).format) }
}

#[cfg(target_os = "linux")]
fn platform_import(
	req: &HwImportRequest<'_>,
	ctx: &Arc<GpuContext>,
	device_type: sys::AVHWDeviceType,
) -> ImportOutcome {
	use sys::AVHWDeviceType;
	if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA {
		// NVDEC's FFmpeg CUDA frames are linear `cuMemAlloc` buffers, not
		// private CUarrays: export the page-aligned allocation as a
		// DMA-BUF fd and import each plane through the same Vulkan path
		// VAAPI uses. The export API is driver-gated; any refusal falls
		// back to staging (never a permanent failure).
		if !platform_import_enabled("QUERCUS_GPU_IMPORT_CUDA", "GpuDecodeImportCuda") {
			return ImportOutcome::Unsupported;
		}
		return cuda_import(req, ctx);
	}
	if device_type != AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI {
		return ImportOutcome::Unsupported;
	}
	if !platform_import_enabled("QUERCUS_GPU_IMPORT_VAAPI", CONFIG_KEY_VAAPI_IMPORT) {
		return ImportOutcome::Unsupported;
	}
	vaapi_import(req, ctx)
}

#[cfg(target_os = "windows")]
fn platform_import(
	req: &HwImportRequest<'_>,
	ctx: &Arc<GpuContext>,
	device_type: sys::AVHWDeviceType,
) -> ImportOutcome {
	if device_type != sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA {
		return ImportOutcome::Unsupported;
	}
	if !platform_import_enabled("QUERCUS_GPU_IMPORT_D3D11", CONFIG_KEY_D3D11_IMPORT) {
		return ImportOutcome::Unsupported;
	}
	d3d11_import(req, ctx)
}

#[cfg(target_os = "macos")]
fn platform_import(
	req: &HwImportRequest<'_>,
	ctx: &Arc<GpuContext>,
	device_type: sys::AVHWDeviceType,
) -> ImportOutcome {
	if device_type != sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX {
		return ImportOutcome::Unsupported;
	}
	if !platform_import_enabled("QUERCUS_GPU_IMPORT_VIDEOTOOLBOX", CONFIG_KEY_VIDEOTOOLBOX_IMPORT) {
		return ImportOutcome::Unsupported;
	}
	videotoolbox_import(req, ctx)
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn platform_import(
	_req: &HwImportRequest<'_>,
	_ctx: &Arc<GpuContext>,
	_device_type: sys::AVHWDeviceType,
) -> ImportOutcome {
	ImportOutcome::Unsupported
}

// ---------------------------------------------------------------------------
// Linux: VAAPI surface -> DRM PRIME descriptor -> DMA-BUF plane imports
// ---------------------------------------------------------------------------

/// DRM format fourcc values (little-endian `fourcc_code`).
#[cfg(target_os = "linux")]
mod drm_fourcc {
	pub const R8: u32 = 0x2020_3852; // 'R','8',' ',' '
	pub const R16: u32 = 0x2036_3152; // 'R','1','6',' '
	pub const GR88: u32 = 0x3838_5252; // 'R','R','8','8'
	pub const RG88: u32 = 0x3838_4752; // 'R','G','8','8'
	pub const RG1616: u32 = 0x3631_4752; // 'R','G','1','6'
}


/// The frame's software layout from its `AVHWFramesContext.sw_format`.
#[cfg(target_os = "linux")]
fn frame_sw_format(frame: *const sys::AVFrame) -> Option<sys::AVPixelFormat> {
	let frames_ref = unsafe { (*frame).hw_frames_ctx };
	if frames_ref.is_null() {
		return None;
	}
	let frames = unsafe { &*((*frames_ref).data as *const HwFramesContextHead) };
	Some(unsafe { std::mem::transmute::<i32, sys::AVPixelFormat>(frames.sw_format) })
}

/// Map an FFmpeg software pixel format to the generic planar descriptor.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn av_format_descriptor(sw_format: sys::AVPixelFormat) -> Option<PlanarDescriptor> {
	use quercus_core::texture::{PlaneStorage, Subsampling};
	use sys::AVPixelFormat as F;
	let semi = |ss, st| PlanarDescriptor::yuv_semi_planar(ss, st);
	let planar = |ss, st| PlanarDescriptor::yuv_planar(ss, st);
	Some(match sw_format {
		F::AV_PIX_FMT_NV12 => semi(Subsampling::Cs420, PlaneStorage::U8),
		F::AV_PIX_FMT_P010LE => semi(Subsampling::Cs420, PlaneStorage::U16),
		F::AV_PIX_FMT_NV16 => semi(Subsampling::Cs422, PlaneStorage::U8),
		F::AV_PIX_FMT_P210LE => semi(Subsampling::Cs422, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV420P => planar(Subsampling::Cs420, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV420P10LE => planar(Subsampling::Cs420, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV422P => planar(Subsampling::Cs422, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV422P10LE => planar(Subsampling::Cs422, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV444P => planar(Subsampling::Cs444, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV444P10LE => planar(Subsampling::Cs444, PlaneStorage::U16),
		_ => return None,
	})
}

/// Map a VAAPI frame to its DRM PRIME descriptor (FFmpeg calls
/// `vaExportSurfaceHandle` internally; no CPU download happens).
#[cfg(target_os = "linux")]
fn vaapi_import(req: &HwImportRequest<'_>, ctx: &Arc<GpuContext>) -> ImportOutcome {
	let mut drm_frame = ffmpeg::frame::Video::empty();
	// SAFETY: `drm_frame` is a valid AVFrame; the map only writes into it
	// and takes references to the source frame's hwframe mapping.
	let rc = unsafe {
		let raw = drm_frame.as_mut_ptr();
		(*raw).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
		sys::av_hwframe_map(raw, req.frame, sys::AV_HWFRAME_MAP_READ as i32)
	};
	if rc < 0 {
		return ImportOutcome::Failed;
	}
	// SAFETY: on success `data[0]` is the descriptor FFmpeg allocated for
	// this mapping; it stays valid until `drm_frame` is unref'd.
	let desc = unsafe { &*((*drm_frame.as_ptr()).data[0] as *const sys::AVDRMFrameDescriptor) };
	let (lw, lh) = req.luma_size;
	if lw == 0 || lh == 0 {
		return ImportOutcome::Unsupported;
	}
	// Prefer the real software layout; fall back to the two-layer legacy
	// formats only when the hwframe metadata is unavailable.
	let descriptor = frame_sw_format(req.frame)
		.and_then(av_format_descriptor)
		.or_else(|| match desc.nb_layers {
			2 => {
				let a = desc.layers[0].format;
				let b = desc.layers[1].format;
				if a == drm_fourcc::R8 && (b == drm_fourcc::GR88 || b == drm_fourcc::RG88) {
					Some(PlanarFormat::Nv12.descriptor())
				} else if a == drm_fourcc::R16 && b == drm_fourcc::RG1616 {
					Some(PlanarFormat::P010.descriptor())
				} else {
					None
				}
			}
			_ => None,
		});
	let Some(descriptor) = descriptor else {
		return ImportOutcome::Unsupported;
	};
	if desc.nb_layers as usize != descriptor.plane_count() {
		return ImportOutcome::Unsupported;
	}
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!(
			"[vaapi-import] descriptor={descriptor:?} luma={lw}x{lh} layers={}",
			desc.nb_layers
		);
	}
	for layer in &desc.layers[..desc.nb_layers as usize] {
		if layer.nb_planes != 1 {
			return ImportOutcome::Unsupported;
		}
	}
	let plane = |layer: usize| {
		let l = &desc.layers[layer];
		let p = &l.planes[0];
		let obj = &desc.objects[p.object_index as usize];
		quercus_core::backend::external::DmaBufPlane {
			fd: obj.fd,
			offset: p.offset as u64,
			pitch: p.pitch as u64,
			modifier: obj.format_modifier,
		}
	};
	// Shared keep-alive: the source frame pins the VAAPI surface until
	// the last plane texture is destroyed.
	let Some(keep) = crate::ffmpeg::RefFrame::clone_raw(req.frame) else {
		return ImportOutcome::Failed;
	};
	let keep = Arc::new(keep);
	let mut planes = Vec::with_capacity(descriptor.plane_count());
	for index in 0..descriptor.plane_count() {
		let Some((pw, ph)) = descriptor.plane_size(index, lw as i32, lh as i32) else {
			for token in planes.drain(..) {
				ctx.destroy_texture(token);
			}
			return ImportOutcome::Unsupported;
		};
		let Some(format) = descriptor.plane_texture_format(index) else {
			for token in planes.drain(..) {
				ctx.destroy_texture(token);
			}
			return ImportOutcome::Unsupported;
		};
		match ctx.import_dmabuf_plane(
			pw as u32,
			ph as u32,
			format,
			plane(index),
			guard(keep.clone()),
		) {
			Ok(token) => planes.push(token),
			Err(err) => {
				let unsupported = matches!(
					err,
					quercus_core::error::Error::State | quercus_core::error::Error::Invalid
				);
				for token in planes.drain(..) {
					ctx.destroy_texture(token);
				}
				return if unsupported {
					ImportOutcome::Unsupported
				} else {
					ImportOutcome::Failed
				};
			}
		}
	}
	ImportOutcome::Imported {
		planes,
		descriptor,
		keep_alive: None,
		borrowed_planes: false,
	}
}

/// A keep-alive import guard holding one `Arc` of the decoder frame
/// (Linux/VAAPI path; Windows attaches its guard inline and macOS keeps
/// the frame on the planar texture).
#[cfg(target_os = "linux")]
fn guard(value: Arc<crate::ffmpeg::RefFrame>) -> Option<ImportGuard> {
	Some(Box::new(move || drop(value)))
}

// ---------------------------------------------------------------------------
// Linux: CUDA/NVDEC frame -> DMA-BUF export -> Vulkan plane imports
// ---------------------------------------------------------------------------

/// FFmpeg 8 `AVCUDADeviceContext` (hwcontext_cuda.h).
#[cfg(target_os = "linux")]
#[repr(C)]
struct CudaDeviceContext {
	cuda_ctx: *mut std::ffi::c_void,
	stream: *mut std::ffi::c_void,
	internal: *mut std::ffi::c_void,
}

/// The leading fields of `AVHWDeviceContext` / `AVHWFramesContext`; only
/// the fields needed to reach the CUDA context and the frame's sw format.
#[cfg(target_os = "linux")]
#[repr(C)]
struct HwDeviceContextHead {
	av_class: *const std::ffi::c_void,
	device_type: i32,
	hwctx: *mut std::ffi::c_void,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct HwFramesContextHead {
	av_class: *const std::ffi::c_void,
	device_ref: *mut sys::AVBufferRef,
	device_ctx: *mut HwDeviceContextHead,
	hwctx: *mut std::ffi::c_void,
	free: *mut std::ffi::c_void,
	user_opaque: *mut std::ffi::c_void,
	pool: *mut std::ffi::c_void,
	initial_pool_size: i32,
	format: i32,
	sw_format: i32,
	width: i32,
	height: i32,
}

#[cfg(target_os = "linux")]
mod cuda_driver {
	use std::ffi::{c_char, c_int, c_void};
	use std::sync::OnceLock;

	unsafe extern "C" {
		fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
		fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
	}
	const RTLD_NOW: c_int = 2;

	#[repr(C)]
	#[derive(Clone, Copy, Default)]
	pub struct MemLocation {
		pub type_: i32,
		pub id: i32,
	}
	#[repr(C)]
	#[derive(Clone, Copy)]
	pub struct MemAllocationProp {
		pub type_: i32,
		pub requested_handle_types: i32,
		pub location: MemLocation,
		pub win32_handle_metadata: *mut c_void,
		pub compression_type: u8,
		pub gpu_direct_rdma_capable: u8,
		pub usage: u16,
		pub reserved: [u8; 38],
	}
	impl Default for MemAllocationProp {
		fn default() -> Self {
			Self {
				type_: 0,
				requested_handle_types: 0,
				location: MemLocation::default(),
				win32_handle_metadata: std::ptr::null_mut(),
				compression_type: 0,
				gpu_direct_rdma_capable: 0,
				usage: 0,
				reserved: [0; 38],
			}
		}
	}
	#[repr(C)]
	#[derive(Clone, Copy, Default)]
	pub struct MemAccessDesc {
		pub location: MemLocation,
		pub flags: i32,
		pub pad: i32,
	}

	pub type CuCtxPushCurrent = unsafe extern "C" fn(*mut c_void) -> i32;
	pub type CuCtxPopCurrent = unsafe extern "C" fn(*mut *mut c_void) -> i32;
	pub type CuCtxGetDevice = unsafe extern "C" fn(*mut c_int) -> i32;
	pub type CuMemGetAllocationGranularity =
		unsafe extern "C" fn(*mut usize, *const MemAllocationProp, i32) -> i32;
	pub type CuMemCreate =
		unsafe extern "C" fn(*mut u64, usize, *const MemAllocationProp, u64) -> i32;
	pub type CuMemAddressReserve =
		unsafe extern "C" fn(*mut u64, usize, usize, u64, u64) -> i32;
	pub type CuMemMap = unsafe extern "C" fn(u64, usize, u64, u64, u64) -> i32;
	pub type CuMemSetAccess =
		unsafe extern "C" fn(u64, usize, *const MemAccessDesc, usize) -> i32;
	pub type CuMemExportToShareableHandle =
		unsafe extern "C" fn(*mut c_int, u64, i32, u64) -> i32;
	pub type CuMemcpyDtoD = unsafe extern "C" fn(u64, u64, usize) -> i32;
	pub type CuCtxSynchronize = unsafe extern "C" fn() -> i32;
	pub type CuMemUnmap = unsafe extern "C" fn(u64, usize) -> i32;
	pub type CuMemAddressFree = unsafe extern "C" fn(u64, usize) -> i32;
	pub type CuMemRelease = unsafe extern "C" fn(u64) -> i32;

	#[derive(Clone, Copy)]
	pub struct Driver {
		pub push: CuCtxPushCurrent,
		pub pop: CuCtxPopCurrent,
		pub get_device: CuCtxGetDevice,
		pub granularity: CuMemGetAllocationGranularity,
		pub create: CuMemCreate,
		pub reserve: CuMemAddressReserve,
		pub map: CuMemMap,
		pub access: CuMemSetAccess,
		pub export: CuMemExportToShareableHandle,
		pub copy_dtod: CuMemcpyDtoD,
		pub sync: CuCtxSynchronize,
		pub unmap: CuMemUnmap,
		pub address_free: CuMemAddressFree,
		pub release: CuMemRelease,
	}

	static DRIVER: OnceLock<Option<Driver>> = OnceLock::new();

	fn sym(lib: *mut c_void, name: &[u8]) -> *mut c_void {
		unsafe { dlsym(lib, name.as_ptr() as *const c_char) }
	}

	pub fn load() -> Option<Driver> {
		*DRIVER.get_or_init(|| unsafe {
			let lib = dlopen(b"libcuda.so.1\0".as_ptr() as *const c_char, RTLD_NOW);
			if lib.is_null() {
				return None;
			}
			macro_rules! get {
				($name:literal) => {{
					let p = sym(lib, $name);
					if p.is_null() { return None; }
					std::mem::transmute::<*mut c_void, _>(p)
				}};
			}
			Some(Driver {
				push: get!(b"cuCtxPushCurrent\0"),
				pop: get!(b"cuCtxPopCurrent\0"),
				get_device: get!(b"cuCtxGetDevice\0"),
				granularity: get!(b"cuMemGetAllocationGranularity\0"),
				create: get!(b"cuMemCreate\0"),
				reserve: get!(b"cuMemAddressReserve\0"),
				map: get!(b"cuMemMap\0"),
				access: get!(b"cuMemSetAccess\0"),
				export: get!(b"cuMemExportToShareableHandle\0"),
				copy_dtod: get!(b"cuMemcpyDtoD_v2\0"),
				sync: get!(b"cuCtxSynchronize\0"),
				unmap: get!(b"cuMemUnmap\0"),
				address_free: get!(b"cuMemAddressFree\0"),
				release: get!(b"cuMemRelease\0"),
			})
		})
	}
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
struct CudaPlaneLayout {
	offset: usize,
	pitch: usize,
	tex_w: usize,
	ph: usize,
}

#[cfg(target_os = "linux")]
pub(crate) struct CudaVmmSlot {
	driver: cuda_driver::Driver,
	ctx: *mut std::ffi::c_void,
	va: u64,
	size: usize,
	handle: u64,
	fd: i32,
	descriptor: PlanarDescriptor,
	layouts: Vec<CudaPlaneLayout>,
	gpu: Arc<GpuContext>,
	free: std::sync::atomic::AtomicBool,
}
#[cfg(target_os = "linux")]
unsafe impl Send for CudaVmmSlot {}
#[cfg(target_os = "linux")]
unsafe impl Sync for CudaVmmSlot {}
#[cfg(target_os = "linux")]
impl Drop for CudaVmmSlot {
	fn drop(&mut self) {
		if self.fd >= 0 {
			close_fd(self.fd);
		}
		unsafe {
			(self.driver.push)(self.ctx);
			let _ = (self.driver.unmap)(self.va, self.size);
			let _ = (self.driver.address_free)(self.va, self.size);
			let _ = (self.driver.release)(self.handle);
			_ = (self.driver.pop)(std::ptr::null_mut());
		}
	}
}

#[cfg(target_os = "linux")]
struct CudaSlotLease(Arc<CudaVmmSlot>);
#[cfg(target_os = "linux")]
impl Drop for CudaSlotLease {
	fn drop(&mut self) {
		let slot = self.0.clone();
		let gpu = slot.gpu.clone();
		gpu.on_submitted_work_done(move || {
			slot.free
				.store(true, std::sync::atomic::Ordering::Release);
		});
	}
}

#[cfg(target_os = "linux")]
static CUDA_SLOTS: std::sync::OnceLock<std::sync::Mutex<Vec<Arc<CudaVmmSlot>>>> =
	std::sync::OnceLock::new();
#[cfg(target_os = "linux")]
fn cuda_slots() -> &'static std::sync::Mutex<Vec<Arc<CudaVmmSlot>>> {
	CUDA_SLOTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}


#[cfg(target_os = "linux")]
unsafe extern "C" {
	fn close(fd: i32) -> i32;
}

#[cfg(target_os = "linux")]
fn close_fd(fd: i32) {
	// SAFETY: plain libc close.
	unsafe {
		close(fd);
	}
}


/// Map an FFmpeg software pixel format to the engine's generic descriptor.
#[cfg(target_os = "linux")]
fn cuda_descriptor(sw_format: sys::AVPixelFormat) -> Option<PlanarDescriptor> {
	use quercus_core::texture::{ColorModel, PlaneStorage, Subsampling};
	use sys::AVPixelFormat as F;
	let semi = |ss, st| PlanarDescriptor::yuv_semi_planar(ss, st);
	let planar = |ss, st| PlanarDescriptor::yuv_planar(ss, st);
	Some(match sw_format {
		F::AV_PIX_FMT_NV12 => semi(Subsampling::Cs420, PlaneStorage::U8),
		F::AV_PIX_FMT_P010LE => semi(Subsampling::Cs420, PlaneStorage::U16),
		F::AV_PIX_FMT_NV16 => semi(Subsampling::Cs422, PlaneStorage::U8),
		F::AV_PIX_FMT_P210LE => semi(Subsampling::Cs422, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV420P => planar(Subsampling::Cs420, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV420P10LE => planar(Subsampling::Cs420, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV422P => planar(Subsampling::Cs422, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV422P10LE => planar(Subsampling::Cs422, PlaneStorage::U16),
		F::AV_PIX_FMT_YUV444P => planar(Subsampling::Cs444, PlaneStorage::U8),
		F::AV_PIX_FMT_YUV444P10LE => planar(Subsampling::Cs444, PlaneStorage::U16),
		_ => return None,
	})
}

#[cfg(target_os = "linux")]
fn cuda_import(req: &HwImportRequest<'_>, ctx: &Arc<GpuContext>) -> ImportOutcome {
	use cuda_driver::{MemAccessDesc, MemAllocationProp, MemLocation};
	let driver = match cuda_driver::load() {
		Some(driver) => driver,
		None => return ImportOutcome::Unsupported,
	};
	let frames_ref = unsafe { (*req.frame).hw_frames_ctx };
	if frames_ref.is_null() {
		return ImportOutcome::Unsupported;
	}
	let frames = unsafe { &*((*frames_ref).data as *const HwFramesContextHead) };
	let device = unsafe { &*frames.device_ctx };
	let sw_format = unsafe { std::mem::transmute::<i32, sys::AVPixelFormat>(frames.sw_format) };
	let Some(descriptor) = cuda_descriptor(sw_format) else {
		return ImportOutcome::Unsupported;
	};
	let cuda_ctx = unsafe { (*(device.hwctx as *const CudaDeviceContext)).cuda_ctx };
	if cuda_ctx.is_null() {
		return ImportOutcome::Unsupported;
	}
	let (w, h) = req.luma_size;
	if w == 0 || h == 0 {
		return ImportOutcome::Unsupported;
	}
	let frame = unsafe { &*req.frame };
	let bytes = descriptor.storage.bytes_per_component() as i32;
	let mut layouts = Vec::with_capacity(descriptor.plane_count());
	let mut total = 0usize;
	for plane in 0..descriptor.plane_count() {
		let Some((pw, ph)) = descriptor.plane_size(plane, w as i32, h as i32) else {
			return ImportOutcome::Unsupported;
		};
		let Some(components) = descriptor.plane_source_components(plane) else {
			return ImportOutcome::Unsupported;
		};
		let pitch = frame.linesize[plane];
		if frame.data[plane].is_null() || pitch <= 0 {
			return ImportOutcome::Unsupported;
		}
		let texel = (components * bytes as usize).max(1);
		let tex_w = ((pitch as usize) / texel).max(pw as usize);
		layouts.push(CudaPlaneLayout {
			offset: total,
			pitch: pitch as usize,
			tex_w,
			ph: ph as usize,
		});
		total = total.saturating_add((pitch as usize).saturating_mul(ph as usize));
	}
	if total == 0 {
		return ImportOutcome::Unsupported;
	}
	let mut device_id = 0i32;
	unsafe {
		(driver.push)(cuda_ctx);
		_ = (driver.get_device)(&mut device_id);
	}
	let mut granularity = 2 * 1024 * 1024usize;
	let prop = MemAllocationProp {
		type_: 1,
		requested_handle_types: 1,
		location: MemLocation { type_: 1, id: device_id },
		win32_handle_metadata: std::ptr::null_mut(),
		compression_type: 0,
		gpu_direct_rdma_capable: 0,
		usage: 0,
		reserved: [0; 38],
	};
	unsafe { _ = (driver.granularity)(&mut granularity, &prop, 0) };
	if granularity == 0 { granularity = 2 * 1024 * 1024; }
	let slot = {
		let mut pool = cuda_slots().lock().unwrap_or_else(|e| e.into_inner());
		let found = pool.iter().position(|slot| {
			slot.free.load(std::sync::atomic::Ordering::Acquire)
				&& slot.descriptor == descriptor
				&& slot.layouts.as_slice() == layouts.as_slice()
		});
		match found {
			Some(index) => {
				let slot = pool[index].clone();
				slot.free.store(false, std::sync::atomic::Ordering::Release);
				Some(slot)
			}
			None if pool.len() >= 8 => {
				// If queue-completion callbacks stop advancing, do not grow
				// VMM allocations without bound (each 4K P210 slot is 32 MiB).
				// Fall back to staging for this frame instead of freezing or
				// exhausting GPU memory.
				if std::env::var_os("OAK_DEBUG_HW").is_some() {
					eprintln!("[cuda-import] slot pool exhausted; staging fallback");
				}
				None
			}
			None => {
				let mut handle = 0u64;
				let mut va = 0u64;
				let size = total.div_ceil(granularity) * granularity;
				let mut ok = unsafe {
					(driver.create)(&mut handle, size, &prop, 0) == 0
						&& (driver.reserve)(&mut va, size, 0, 0, 0) == 0
						&& (driver.map)(va, size, 0, handle, 0) == 0
				};
				if ok {
					let access = MemAccessDesc {
						location: MemLocation { type_: 1, id: device_id },
						flags: 3,
						pad: 0,
					};
					ok = unsafe { (driver.access)(va, size, &access, 1) == 0 };
				}
				let mut fd = -1i32;
				if ok {
					ok = unsafe { (driver.export)(&mut fd, handle, 1, 0) == 0 && fd >= 0 };
				}
				if ok {
					let slot = Arc::new(CudaVmmSlot {
						driver,
						ctx: cuda_ctx,
						va,
						size,
						handle,
						fd,
						descriptor,
						layouts: layouts.clone(),
						gpu: ctx.clone(),
						free: std::sync::atomic::AtomicBool::new(false),
					});
					pool.push(slot.clone());
					Some(slot)
				} else {
					unsafe {
						if va != 0 {
							_ = (driver.unmap)(va, size);
							_ = (driver.address_free)(va, size);
						}
						if handle != 0 { _ = (driver.release)(handle); }
						if fd >= 0 { close_fd(fd); }
					}
					None
				}
			}
		}
	};
	let Some(slot) = slot else {
		unsafe { _ = (driver.pop)(std::ptr::null_mut()) };
		return ImportOutcome::Unsupported;
	};
	let mut copied = true;
	unsafe {
		for (plane, layout) in layouts.iter().enumerate() {
			let src = frame.data[plane] as u64;
			let dst = slot.va + layout.offset as u64;
			if (driver.copy_dtod)(dst, src, layout.pitch * layout.ph) != 0 {
				copied = false;
				break;
			}
		}
		_ = (driver.sync)();
		_ = (driver.pop)(std::ptr::null_mut());
	}
	if !copied {
		return ImportOutcome::Unsupported;
	}
	let mut planes = Vec::with_capacity(descriptor.plane_count());
	for (plane, layout) in layouts.iter().enumerate() {
		let Some(format) = descriptor.plane_texture_format(plane) else {
			copied = false;
			break;
		};
		match ctx.import_opaque_fd_plane(
			layout.tex_w as u32,
			layout.ph as u32,
			format,
			slot.fd,
			layout.offset as u64,
			layout.pitch as u64,
			None,
		) {
			Ok(token) => planes.push(token),
			Err(_) => {
				copied = false;
				break;
			}
		}
	}
	if !copied {
		for token in planes.drain(..) {
			ctx.destroy_texture(token);
		}
		return ImportOutcome::Failed;
	}
	ImportOutcome::Imported {
		planes,
		descriptor,
		keep_alive: Some(Arc::new(CudaSlotLease(slot))),
		borrowed_planes: false,
	}
}

// ---------------------------------------------------------------------------
// Windows: D3D11VA texture -> shared NT handle -> Vulkan import
// ---------------------------------------------------------------------------

/// Import a D3D11VA texture (Windows): one NV12/P010 texture shared as an
/// NT handle, imported as a single multi-planar image whose two plane
/// views feed the planar YUV→RGB pass. The decoder's frame reference is
/// kept alive by the texture's drop guard.
#[cfg(target_os = "windows")]
fn d3d11_import(req: &HwImportRequest<'_>, ctx: &Arc<GpuContext>) -> ImportOutcome {
	use windows::core::Interface;
	use windows::Win32::Foundation::CloseHandle;
	use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
	use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
	use windows::Win32::Graphics::Dxgi::{IDXGIResource1, DXGI_SHARED_RESOURCE_READ};

	// AV_PIX_FMT_D3D11: data[0] = ID3D11Texture2D*, data[1] = subresource
	// index. D3D11VA frames cycle through the decoder's surface ARRAY, so
	// the slice index is part of the frame identity — the import selects
	// it via an array-layer view (M5 audit).
	let texture_ptr = unsafe { (*req.frame).data[0] } as *mut std::ffi::c_void;
	if texture_ptr.is_null() {
		return ImportOutcome::Unsupported;
	}
	let slice = unsafe { (*req.frame).data[1] } as usize as u32;
	// SAFETY: FFmpeg owns the texture reference for the frame's lifetime.
	let texture: &ID3D11Texture2D = unsafe { &*(texture_ptr as *const ID3D11Texture2D) };
	let mut desc = D3D11_TEXTURE2D_DESC::default();
	unsafe { texture.GetDesc(&mut desc) };
	let array_size = desc.ArraySize.max(1);
	if slice >= array_size {
		return ImportOutcome::Unsupported;
	}
	let (texture_format, planar) = match desc.Format {
		DXGI_FORMAT_NV12 => (quercus_core::wgpu::TextureFormat::NV12, PlanarFormat::Nv12),
		DXGI_FORMAT_P010 => (quercus_core::wgpu::TextureFormat::P010, PlanarFormat::P010),
		_ => return ImportOutcome::Unsupported,
	};
	// SAFETY: the D3D11 texture supports IDXGIResource1 (every shared
	// texture does); a cast failure means it is not a shareable resource.
	let Ok(resource) = texture.cast::<IDXGIResource1>() else {
		return ImportOutcome::Unsupported;
	};
	// Read-only NT handle; Vulkan references the resource, so the handle
	// is closed again right after the import.
	let handle = unsafe {
		resource.CreateSharedHandle(
			None,
			DXGI_SHARED_RESOURCE_READ.0,
			None::<&windows::core::PCWSTR>,
		)
	};
	let Ok(handle) = handle else {
		return ImportOutcome::Failed;
	};
	let Some(keep) = crate::ffmpeg::RefFrame::clone_raw(req.frame) else {
		unsafe {
			let _ = CloseHandle(handle);
		}
		return ImportOutcome::Failed;
	};
	let (w, h) = req.luma_size;
	let guard: Arc<crate::ffmpeg::RefFrame> = Arc::new(keep);
	let result = ctx.import_d3d11_shared_texture(
		handle.0 as isize,
		w,
		h,
		texture_format,
		array_size,
		slice,
		Some(Box::new(move || drop(guard))),
	);
	unsafe {
		let _ = CloseHandle(handle);
	}
	match result {
		Ok((y, uv)) => ImportOutcome::Imported {
			planes: vec![y, uv],
			descriptor: planar.descriptor(),
			keep_alive: None,
		},
		// State/Invalid are capability gaps (missing device feature, wrong
		// format), not failed attempts.
		Err(quercus_core::error::Error::State) | Err(quercus_core::error::Error::Invalid) => {
			ImportOutcome::Unsupported
		}
		Err(_) => ImportOutcome::Failed,
	}
}

// ---------------------------------------------------------------------------
// macOS: VideoToolbox CVPixelBuffer -> IOSurface -> Metal textures
// ---------------------------------------------------------------------------

// `CVPixelBufferGetIOSurface` (CoreVideo).
#[cfg(target_os = "macos")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
	fn CVPixelBufferGetIOSurface(pixel_buffer: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// Import a VideoToolbox frame (macOS): its CVPixelBuffer's IOSurface is
/// wrapped in one Metal texture per plane. Metal textures cannot carry a
/// drop callback, so the decoder's frame reference travels back in
/// `ImportedHwFrame::keep_alive` and is held by the planar texture.
#[cfg(target_os = "macos")]
fn videotoolbox_import(req: &HwImportRequest<'_>, ctx: &Arc<GpuContext>) -> ImportOutcome {
	// AV_PIX_FMT_VIDEOTOOLBOX keeps the CVPixelBuffer in data[3].
	let pixel_buffer = unsafe { (*req.frame).data[3] } as *mut std::ffi::c_void;
	if pixel_buffer.is_null() {
		return ImportOutcome::Unsupported;
	}
	let surface = unsafe { CVPixelBufferGetIOSurface(pixel_buffer) };
	if surface.is_null() {
		return ImportOutcome::Unsupported;
	}
	let frames = unsafe { (*req.frame).hw_frames_ctx };
	if frames.is_null() {
		return ImportOutcome::Unsupported;
	}
	// SAFETY: `hw_frames_ctx` is an AVHWFramesContext for this frame.
	let hw_frames = unsafe { (*frames).data as *const sys::AVHWFramesContext };
	let sw_format = unsafe { (*hw_frames).sw_format };
	let Some(descriptor) = av_format_descriptor(sw_format) else {
		return ImportOutcome::Unsupported;
	};
	let (w, h) = req.luma_size;
	if w == 0 || h == 0 {
		return ImportOutcome::Unsupported;
	}
	let mut planes = Vec::with_capacity(descriptor.plane_count());
	for plane in 0..descriptor.plane_count() {
		let Some((pw, ph)) = descriptor.plane_size(plane, w as i32, h as i32) else {
			for token in planes.drain(..) {
				ctx.destroy_texture(token);
			}
			return ImportOutcome::Unsupported;
		};
		let Some(format) = descriptor.plane_texture_format(plane) else {
			for token in planes.drain(..) {
				ctx.destroy_texture(token);
			}
			return ImportOutcome::Unsupported;
		};
		// SAFETY: `surface` is the CVPixelBuffer's IOSurface; plane slots
		// follow the YUV plane order of the descriptor.
		match unsafe { ctx.import_iosurface_plane(surface, plane as usize, pw as u32, ph as u32, format) } {
			Ok(token) => planes.push(token),
			Err(quercus_core::error::Error::State)
			| Err(quercus_core::error::Error::Invalid) => {
				for token in planes.drain(..) {
					ctx.destroy_texture(token);
				}
				return ImportOutcome::Unsupported;
			}
			Err(_) => {
				for token in planes.drain(..) {
					ctx.destroy_texture(token);
				}
				return ImportOutcome::Failed;
			}
		}
	}
	let Some(keep) = crate::ffmpeg::RefFrame::clone_raw(req.frame) else {
		for token in planes.drain(..) {
			ctx.destroy_texture(token);
		}
		return ImportOutcome::Failed;
	};
	ImportOutcome::Imported {
		planes,
		descriptor,
		keep_alive: Some(Arc::new(keep)),
		borrowed_planes: false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn empty_frame() -> ffmpeg::frame::Video {
		ffmpeg::frame::Video::empty()
	}

	fn request(frame: &ffmpeg::frame::Video, device_type: sys::AVHWDeviceType) -> HwImportRequest<'_> {
		HwImportRequest {
			frame: unsafe { frame.as_ptr() },
			_marker: std::marker::PhantomData,
			device_type: Some(device_type),
			luma_size: (64, 64),
			matrix: YuvMatrix::Bt709,
			full_range: false,
			color_primaries: 1,
			color_trc: 1,
		}
	}

	/// Test-only RAII override for a process-global environment variable:
	/// restores the previous value (or absence) on drop, so a panicking
	/// assertion cannot leak the override into the next serialized test and
	/// an externally preset value survives the test.
	struct EnvVarGuard {
		name: &'static str,
		previous: Option<std::ffi::OsString>,
	}

	impl EnvVarGuard {
		/// Set `name=value` for the guard's lifetime.
		fn set(name: &'static str, value: &str) -> Self {
			let previous = std::env::var_os(name);
			std::env::set_var(name, value);
			Self { name, previous }
		}

		/// Remove `name` for the guard's lifetime.
		fn remove(name: &'static str) -> Self {
			let previous = std::env::var_os(name);
			std::env::remove_var(name);
			Self { name, previous }
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match self.previous.take() {
				Some(value) => std::env::set_var(self.name, value),
				None => std::env::remove_var(self.name),
			}
		}
	}

	/// Without a host-installed device the import declines before any
	/// frame work (and is not counted as a fallback: nothing failed).
	#[test]
	fn import_requires_a_host_installed_context() {
		let _g = crate::lock_tests();
		reset_import_counters();
		assert!(!GpuContext::host_gpu_installed());
		let frame = empty_frame();
		let req = request(&frame, sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI);
		assert!(try_import_hw_frame(&req).is_none());
		assert_eq!(HW_IMPORTS.load(Ordering::Relaxed), 0);
		assert_eq!(HW_IMPORT_FALLBACKS.load(Ordering::Relaxed), 0);
		assert_eq!(HW_IMPORT_UNSUPPORTED.load(Ordering::Relaxed), 0);
	}

	/// The counter classification (M5 audit): a device type with no import
	/// path (NVDEC/CUDA) is "unsupported", never a failure — it must not
	/// feed the consecutive-failure breaker or `HW_IMPORT_FALLBACKS`.
	/// Exercised at the platform-function level so no real GPU is needed.
	///
	/// (`platform_import` is private; this test lives in the same module.)
	#[cfg(target_os = "linux")]
	#[test]
	fn non_importable_device_type_is_unsupported_not_failed() {
		let _g = crate::lock_tests();
		// A context is required by the platform function's signature;
		// the non-VAAPI device check happens before any of its fields are
		// touched, so a lazily-created local context is never used.
		let Some(ctx) = GpuContext::create(quercus_core::backend::BackendKind::Auto) else {
			eprintln!("no GPU adapter; skipping classification test");
			return;
		};
		let frame = empty_frame();
		let req = request(&frame, sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA);
		assert!(
			matches!(
				platform_import(&req, &ctx, sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA),
				ImportOutcome::Unsupported
			),
			"CUDA/NVDEC has no export path and must classify as unsupported"
		);
	}

	/// The `QUERCUS_GPU_IMPORT=0` escape hatch disables the path outright, and
	/// removing the override falls back to the default-on config. The
	/// override is RAII-scoped, so an externally preset value survives the
	/// test instead of being clobbered by `remove_var`.
	#[test]
	fn import_switch_disables_the_path() {
		let _g = crate::lock_tests();
		let previous = std::env::var_os("QUERCUS_GPU_IMPORT");
		{
			let _off = EnvVarGuard::set("QUERCUS_GPU_IMPORT", "0");
			assert!(!gpu_import_enabled(), "explicit 0 disables the import path");
		}
		assert_eq!(
			std::env::var_os("QUERCUS_GPU_IMPORT"),
			previous,
			"the guard restores the externally preset value"
		);
		if previous.is_none() {
			// No override: the config-store default keeps the import path
			// on. (With an external value the caller's setting governs, so
			// asserting the default would be wrong.)
			let _unset = EnvVarGuard::remove("QUERCUS_GPU_IMPORT");
			assert!(
				gpu_import_enabled(),
				"unset falls back to the default-on config"
			);
		}
	}

	/// Plane layouts carry the expansion depth the YUV transform needs.
	#[test]
	fn planar_formats_carry_their_bit_depth() {
		assert_eq!(PlanarFormat::Nv12.bit_depth(), 8);
		assert_eq!(PlanarFormat::P010.bit_depth(), 16);
		assert_eq!(PlanarFormat::Nv12.name(), "nv12");
	}
}
