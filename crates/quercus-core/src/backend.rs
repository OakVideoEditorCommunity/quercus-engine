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

//! GPU backend: **wgpu**, used directly.
//!
//! The C++ tree splits GL/Vulkan into dlopened backend plugins behind
//! `renderbackend_c.h` because C++ had no portable GPU abstraction.
//! wgpu (Metal/Vulkan/GL/DX12 in one safe API) removes that whole
//! layer: no backend plugins, no C interface, no unsafe dlopen glue.
//!
//! This module owns the `wgpu::Instance`/`Device`/`Queue` lifecycle and
//! the texture/shader operations the rest of the crate needs. It is the
//! only module that talks to wgpu, and the only module with `unsafe`
//! (the wgpu map_async callback marshalling) besides `bridge/`.
//!
//! Headless status: `GpuContext::create` requests an adapter without any
//! surface; when no adapter is available (headless CI, VMs) or the only
//! candidates cannot render the pipeline's canonical Rgba32Float target
//! (downlevel GL/GLES), it returns `None` and every consumer falls back
//! to the CPU path. GPU tests skip with no adapter unless
//! `QUERCUS_REQUIRE_GPU` is set (CI). Verified on macOS Metal and Linux
//! lavapipe (wgpu 29).

use crate::color;
use crate::colormath;
use crate::commonutil;
use crate::lut;
use std::any;
use std::borrow;
use std::collections::HashMap;
use std::env;
use std::ffi;
use std::fmt;
use std::future;
use std::iter;
use std::panic;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time;
use wgpu::AddressMode;
use wgpu::Backend;
use wgpu::Backends;
use wgpu::BindingResource;
use wgpu::BindingType;
use wgpu::BufferBindingType;
use wgpu::BufferUsages;
use wgpu::Color;
use wgpu::ColorWrites;
use wgpu::ErrorFilter;
use wgpu::ExperimentalFeatures;
use wgpu::Features;
use wgpu::FilterMode;
use wgpu::Instance;
use wgpu::InstanceDescriptor;
use wgpu::Limits;
use wgpu::LoadOp;
use wgpu::MapMode;
use wgpu::MemoryHints;
use wgpu::MultisampleState;
use wgpu::Origin3d;
use wgpu::PollStatus;
use wgpu::PollType;
use wgpu::PowerPreference;
use wgpu::PrimitiveState;
use wgpu::SamplerBindingType;
use wgpu::ShaderSource;
use wgpu::ShaderStages;
use wgpu::StoreOp;
use wgpu::TextureAspect;
use wgpu::TextureDimension;
use wgpu::TextureFormat;
use wgpu::TextureSampleType;
use wgpu::TextureUsages;
use wgpu::TextureViewDescriptor;
use wgpu::TextureViewDimension;
use wgpu::Trace;

use crate::PixelFormat;

use crate::error::{Error, Result};
use crate::frame::VideoParamsPod;
use crate::texture::{Frame, PlanarDescriptor, PlanarFormat, PlaneStorage, Subsampling, Texture};

pub mod external;

#[cfg(target_os = "linux")]
pub use external::DmaBufPlane;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use external::GlMemoryExport;
pub use external::ImportGuard;

/// Backend selection preference (mapped onto wgpu backends).
///
/// The choice is user-visible: the settings panel exposes a renderer
/// dropdown (Auto/Metal/Vulkan/OpenGL/CPU) persisted through the
/// quercus_core config C ABI under the "GraphicsBackend" key
/// (C++ parity: `RenderManager::backend_from_string` config round-trip).
/// "auto" resolves Metal → Vulkan → GL → CPU at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
	/// Resolve automatically (Metal → Vulkan → GL → CPU).
	Auto,
	/// Metal (macOS primary).
	Metal,
	/// Vulkan.
	Vulkan,
	/// OpenGL (legacy fallback).
	Gl,
	/// CPU fallback (no adapter found; dummy textures + CPU blits).
	Cpu,
}

impl BackendKind {
	/// Parse the persisted backend settings string into a [`BackendKind`].
	///
	/// Matching trims whitespace and ignores case: `""`/`"auto"` → Auto,
	/// `"metal"` → Metal, `"vulkan"` → Vulkan, `"opengl"`/`"gl"` → Gl and
	/// `"cpu"`/`"dummy"`/`"multiprocess"` → Cpu (the legacy C++
	/// `backend_from_string` values). Unknown strings fall back to
	/// [`BackendKind::Auto`]. Counterpart of [`BackendKind::to_config_string`].
	pub fn from_config_string(s: &str) -> BackendKind {
		match s.trim().to_ascii_lowercase().as_str() {
			"" | "auto" => BackendKind::Auto,
			"metal" => BackendKind::Metal,
			"vulkan" => BackendKind::Vulkan,
			"opengl" | "gl" => BackendKind::Gl,
			// C++ "dummy" backend and the multiprocess pool: no GPU path in
			// this pass — the CPU fallback is the honest equivalent.
			"cpu" | "dummy" | "multiprocess" => BackendKind::Cpu,
			_ => BackendKind::Auto,
		}
	}

	/// Serialize for the settings panel.
	pub fn to_config_string(self) -> &'static str {
		match self {
			BackendKind::Auto => "auto",
			BackendKind::Metal => "metal",
			BackendKind::Vulkan => "vulkan",
			BackendKind::Gl => "opengl",
			BackendKind::Cpu => "cpu",
		}
	}

	/// Read the user's persisted choice through the quercus_core config C ABI
	/// ("GraphicsBackend"). `QUERCUS_RENDER_BACKEND` overrides the config
	/// (tests / headless environments).
	pub fn from_user_config() -> BackendKind {
		if let Ok(v) = env::var("QUERCUS_RENDER_BACKEND") {
			if !v.is_empty() {
				return BackendKind::from_config_string(&v);
			}
		}
		let configured = commonutil::config_get_string(None, "GraphicsBackend").unwrap_or_default();
		BackendKind::from_config_string(&configured)
	}

	/// The wgpu backends this kind resolves to, in fallback order.
	fn wgpu_fallbacks(self) -> Vec<wgpu::Backends> {
		match self {
			BackendKind::Auto | BackendKind::Metal => {
				vec![Backends::METAL, Backends::VULKAN, Backends::GL]
			}
			BackendKind::Vulkan => {
				vec![Backends::VULKAN, Backends::METAL, Backends::GL]
			}
			BackendKind::Gl => {
				vec![Backends::GL, Backends::METAL, Backends::VULKAN]
			}
			BackendKind::Cpu => Vec::new(),
		}
	}

	/// Map a concrete [`wgpu::Backend`] id to the [`BackendKind`] this build
	/// reports for it.
	///
	/// Anything other than Metal, Vulkan or GL (e.g. DX12 or a browser backend)
	/// maps to [`BackendKind::Cpu`], because those adapters are never created
	/// here.
	fn from_wgpu_backend(b: wgpu::Backend) -> BackendKind {
		match b {
			Backend::Metal => BackendKind::Metal,
			Backend::Vulkan => BackendKind::Vulkan,
			Backend::Gl => BackendKind::Gl,
			_ => BackendKind::Cpu,
		}
	}
}

/// The config-store key for the on-screen display bit depth ("10" or
/// "8"). The window swapchain format is chosen once at surface creation,
/// so a change takes effect after a restart.
pub const CONFIG_KEY_DISPLAY_BIT_DEPTH: &str = "DisplayBitDepth";

/// On-screen presentation bit depth, persisted under the
/// `DisplayBitDepth` config key. The choice is user-visible: the
/// preferences dialog exposes a 10-bit/8-bit dropdown, and it decides
/// the formats the window layer presents with (see
/// [`DisplayBitDepth::present_formats`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayBitDepth {
	/// 8-bit per channel (Bgra8/Rgba8 swapchain).
	Bit8,
	/// 10-bit per channel (RGB10A2 — the default; RGBA16F for HDR).
	Bit10,
}

impl DisplayBitDepth {
	/// Parse the settings string; unknown values yield the default 10-bit.
	pub fn from_config_string(s: &str) -> DisplayBitDepth {
		match s.trim() {
			"8" => DisplayBitDepth::Bit8,
			_ => DisplayBitDepth::Bit10,
		}
	}

	/// The config-store value persisted for this depth.
	pub fn to_config_string(self) -> &'static str {
		match self {
			DisplayBitDepth::Bit8 => "8",
			DisplayBitDepth::Bit10 => "10",
		}
	}

	/// Read the user's persisted choice through the quercus_core config C ABI
	/// ("DisplayBitDepth").
	pub fn from_user_config() -> DisplayBitDepth {
		let configured =
			commonutil::config_get_string(None, CONFIG_KEY_DISPLAY_BIT_DEPTH).unwrap_or_default();
		DisplayBitDepth::from_config_string(&configured)
	}

	/// The wgpu surface formats the window layer should present with, in
	/// preference order (the platform picks the first the surface
	/// supports). 10-bit prefers RGB10A2 with an RGBA16F (HDR) fallback;
	/// 8-bit mirrors the engine's current default preference list.
	///
	/// The actual swapchain is configured by the window layer
	/// (gpui_wgpu's surface setup intersects these with the surface
	/// capabilities); this crate is headless and never creates a surface
	/// itself, so the mapping is the format-selection contract the window
	/// layer consumes at surface creation.
	pub fn present_formats(self) -> &'static [wgpu::TextureFormat] {
		match self {
			DisplayBitDepth::Bit10 => &[TextureFormat::Rgb10a2Unorm, TextureFormat::Rgba16Float],
			DisplayBitDepth::Bit8 => &[TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm],
		}
	}
}

/// A GPU-resident texture in the context registry. `Arc` so the present
/// path can hand the very same `wgpu::Texture` to the UI without a copy
/// (M2 zero-copy present): the registry keeps its own reference as long
/// as the engine token lives.
#[derive(Clone)]
struct GpuTexture {
	texture: Arc<wgpu::Texture>,
	width: u32,
	height: u32,
	format: wgpu::TextureFormat,
	/// The view aspect to use for sampling. Plane-imported textures
	/// (Windows NV12: one texture, two plane views) register one token
	/// per plane with the matching aspect; everything else is `All`.
	aspect: wgpu::TextureAspect,
	/// Selected array layer range for array-imported textures (Windows
	/// D3D11VA frames are slices of one array texture): `Some((base,
	/// count))` when this token aliases one slice. `None` = the whole
	/// (single-layer) texture.
	layer: Option<(u32, u32)>,
	/// Linux/Vulkan external memory backing (OpenFX GL zero-copy). The
	/// texture was allocated with `OPAQUE_FD` exportable memory, so the
	/// plugin's GL context can import the very same allocation.
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	external: Option<Arc<external::ExternalTexture>>,
	/// Optional lifetime guard (CUDA VMM slot): held until the token is
	/// destroyed, so a resolved texture cannot outlive its source surface.
	keep_alive: Option<Arc<dyn any::Any + Send + Sync>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serializes wgpu instance/device creation process-wide.
///
/// The Vulkan loader (libvulkan + ICD) is not safe against concurrent
/// `vkCreateInstance` on every driver stack: parallel test threads each
/// calling [`GpuContext::create`] crashed with a null call inside the
/// loader (SEGV in `vkCreateInstance`). Context creation is rare (test
/// setup and first renderer init), so the lock costs nothing in
/// practice.
fn create_lock() -> MutexGuard<'static, ()> {
	static LOCK: Mutex<()> = Mutex::new(());
	lock(&LOCK)
}

fn backend_diagnostic(message: impl fmt::Display) {
	if env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!("[gpu-backend] {message}");
	}
}

/// The context surface textures depend on (trait object in
/// [`Texture::Gpu`]). `GpuContext` is the production implementor; tests
/// can supply a fake.
pub trait GpuContextLike: Send + Sync {
	/// The backend kind in use.
	fn kind(&self) -> BackendKind;
	/// Destroy a texture token (idempotent).
	fn destroy_texture(&self, token: u64);
	/// Upload a CPU frame into a texture (F32 RGBA).
	fn upload(&self, token: u64, frame: &Frame) -> Result<()>;
	/// Download a texture into a CPU frame.
	fn download(&self, token: u64) -> Result<Frame>;
	/// Async download (default falls back to a synchronous implementation).
	fn download_async(&self, token: u64) -> Result<FrameReadback> {
		Err(Error::State)
	}
	/// Blit texture → texture (plain copy; color-managed deferred).
	fn blit(&self, src: u64, dst: u64, processor: Option<&color::ColorProcessor>) -> Result<()>;

	/// Concrete-context downcast hook (M2). The present path uses it to
	/// reach the LUT/display-pass API and the raw `wgpu::Texture`;
	/// trait-only fakes return `None` and callers fall back to CPU
	/// delivery.
	fn as_any(&self) -> Option<&dyn any::Any> {
		None
	}

	/// The raw texture for a token, for zero-copy presentation. `None`
	/// when the context cannot expose it (fakes) or the token is unknown.
	fn texture_handle(&self, _token: u64) -> Option<Arc<wgpu::Texture>> {
		None
	}
}

/// The GPU context: one wgpu instance/device/queue for the process,
/// plus the texture registry and the blit pipeline.
///
/// The device may be **adopted** from the host application instead of
/// created here (M2): when the UI and the render thread share one wgpu
/// device, engine textures are directly sampleable by the presenter and
/// the whole present path is zero-copy. `_instance`/`_adapter` are then
/// `None` (the adopter owns them).
pub struct GpuContext {
	// Kept alive for the whole context: the instance must outlive the
	// adapter on native backends. `None` for an adopted context.
	_instance: Option<wgpu::Instance>,
	_adapter: Option<wgpu::Adapter>,
	device: wgpu::Device,
	queue: wgpu::Queue,
	kind: BackendKind,
	textures: Mutex<HashMap<u64, GpuTexture>>,
	next_token: AtomicU64,
	blit: Mutex<Option<wgpu::RenderPipeline>>,
	/// Copy pipeline targeting `Rgba16Float` (the gpui surface pipeline is
	/// filterable and expects half-float, not `Rgba32Float`).
	blit16: Mutex<Option<wgpu::RenderPipeline>>,
	/// FLOAT32_FILTERABLE was available (linear sampling on F32 textures).
	filterable: bool,
	/// Compiled effect pipelines, keyed by the shaderfx cache key.
	programs: Mutex<HashMap<String, Arc<ShaderProgram>>>,
	/// The lazily created 1×1 placeholder texture (unconnected inputs).
	placeholder: Mutex<Option<u64>>,
	/// The display LUT texture (M2): the app-installed working-space →
	/// display-device transform, applied by [`GpuContext::present_texture`].
	display_lut: Mutex<Option<DisplayLutState>>,
	/// The compiled display-LUT passes, keyed by output format.
	present: Mutex<Vec<(wgpu::TextureFormat, PresentPipeline)>>,
	/// The compiled YUV→RGB pass (M5 decode import dependency).
	yuv: Mutex<Option<PresentPipeline>>,
	/// The compiled planar (imported hardware NV12/P010) YUV→RGB pass
	/// (M5): luma + interleaved chroma plane bindings.
	yuv_planar: Mutex<Option<PresentPipeline>>,
	/// The compiled descriptor-driven planar-matrix → Rgba32Float pass
	/// (full pixel matrix: 1–3 planes, YUV planar/semi-planar and
	/// interleaved RGB(A), bilinear target scaling).
	planar_matrix: Mutex<Option<PresentPipeline>>,
	/// Caller-keyed 3D LUT textures (per-node color transforms; M2).
	color_luts: Mutex<Vec<(String, u64)>>,
	/// Set once this context has created a GPU resource (texture or
	/// pipeline). A context that has never touched the GPU can still be
	/// replaced by the UI's adopted device (`install_shared`).
	used: AtomicBool,
	/// Set after a fatal wgpu poll/device-lost error. Further readbacks
	/// must fail cleanly instead of polling the lost device again.
	device_lost: Arc<AtomicBool>,
	/// Serializes CUDA external-memory import with GPU passes and readbacks.
	external_lock: Arc<Mutex<()>>,
	/// Linux/Vulkan OpenFX GL zero-copy: allocate `Rgba32Float` textures
	/// with exportable `OPAQUE_FD` memory. Disabled by
	/// `QUERCUS_OFX_GPU_ZEROCOPY=0`; every allocation failure falls back
	/// to a normal wgpu texture.
	ofx_zerocopy: bool,
}

/// The installed display transform: a 3D LUT texture plus its input domain.
struct DisplayLutState {
	token: u64,
	edge: u32,
	lo: [f32; 3],
	hi: [f32; 3],
}

/// Process-wide GPU↔CPU transfer counters (M2 acceptance, modeled on
/// `procpool::main_heap_frame_copies`). `uploads` counts
/// [`GpuContext::upload`] calls (CPU→GPU), `downloads` counts
/// [`GpuContext::download`] calls (GPU→CPU). The GPU graph/present path
/// must not download: readbacks happen only at the three explicit
/// boundaries (CPU OpenFX, export encoder, disk cache) and in tests.
static GPU_UPLOADS: AtomicU64 = AtomicU64::new(0);
static GPU_DOWNLOADS: AtomicU64 = AtomicU64::new(0);

/// The current (uploads, downloads) GPU transfer counters.
pub fn gpu_transfer_counters() -> (u64, u64) {
	(
		GPU_UPLOADS.load(Ordering::Relaxed),
		GPU_DOWNLOADS.load(Ordering::Relaxed),
	)
}

/// Reset the GPU transfer counters (tests).
pub fn reset_gpu_transfer_counters() {
	GPU_UPLOADS.store(0, Ordering::Relaxed);
	GPU_DOWNLOADS.store(0, Ordering::Relaxed);
}

/// The process-wide shared-context slot (`shared` / `install_shared`).
struct SharedSlot {
	decided: bool,
	/// The HOST (app/UI) explicitly installed the context, as opposed to
	/// the engine lazily creating one from the user config. External
	/// hardware-surface imports (M5) only run on a host-installed device:
	/// a lazily created private context in a worker/CLI/test must not
	/// silently switch decode onto the GPU.
	host_installed: bool,
	ctx: Option<Arc<GpuContext>>,
}

fn shared_slot() -> &'static Mutex<SharedSlot> {
	static SLOT: std::sync::OnceLock<Mutex<SharedSlot>> = std::sync::OnceLock::new();
	SLOT.get_or_init(|| {
		Mutex::new(SharedSlot {
			decided: false,
			host_installed: false,
			ctx: None,
		})
	})
}

/// Whether Vulkan OpenFX GL zero-copy allocation is requested.
///
/// Linux is the first-class platform: default on, disabled with
/// `QUERCUS_OFX_GPU_ZEROCOPY=0` / `false`.
///
/// Windows is **experimental and unverified**: default off, enabled only
/// with `QUERCUS_OFX_GPU_ZEROCOPY=1` / `true`. The runtime still checks
/// `VK_KHR_external_memory_win32` and the GL memory-object extension; any
/// failure falls back to the existing CPU/GL-readback path.
fn zerocopy_requested() -> bool {
	#[cfg(target_os = "linux")]
	{
		!matches!(
			env::var("QUERCUS_OFX_GPU_ZEROCOPY").ok().as_deref(),
			Some("0") | Some("false") | Some("FALSE") | Some("False")
		)
	}
	#[cfg(target_os = "windows")]
	{
		matches!(
			env::var("QUERCUS_OFX_GPU_ZEROCOPY").ok().as_deref(),
			Some("1") | Some("true") | Some("TRUE") | Some("True")
		)
	}
	#[cfg(not(any(target_os = "linux", target_os = "windows")))]
	{
		false
	}
}

/// Parse an `QUERCUS_REQUIRE_GPU` value: unset means "skipping is allowed";
/// only `0`/`false` (case-insensitive) disable the requirement. Kept pure
/// so the policy test needs no process-wide environment mutation (which
/// would race the GPU acceptance tests reading the variable in parallel).
fn require_gpu_from_value(value: Option<&str>) -> bool {
	match value {
		Some(v) => !(v == "0" || v.eq_ignore_ascii_case("false")),
		None => false,
	}
}

/// Whether GPU-dependent tests must hard-fail when no adapter is
/// available. CI sets `QUERCUS_REQUIRE_GPU=1` on the software-Vulkan runner
/// (lavapipe is present), so a degraded environment fails the suite
/// instead of silently losing the GPU acceptance signal.
pub fn require_gpu_adapter() -> bool {
	require_gpu_from_value(env::var("QUERCUS_REQUIRE_GPU").ok().as_deref())
}

/// Handle a missing adapter: panic when `required`, otherwise log the
/// skip (the caller returns). Taking the flag as a parameter lets the
/// policy test drive the hard-fail arm without flipping the real
/// environment variable of a running test process.
fn skip_or_fail_gpu_with(required: bool, what: &str) {
	if required {
		panic!("no GPU adapter available for {what} (QUERCUS_REQUIRE_GPU is set)");
	}
	eprintln!("no GPU adapter; skipping {what}");
}

/// Handle a missing adapter in a GPU acceptance test: panic when
/// `QUERCUS_REQUIRE_GPU` is set, otherwise log the skip (the caller returns).
pub fn skip_or_fail_gpu(what: &str) {
	skip_or_fail_gpu_with(require_gpu_adapter(), what);
}

/// A fresh [`GpuContext`] for a test, or `None` when no adapter exists
/// (panics instead when `QUERCUS_REQUIRE_GPU` is set).
pub fn gpu_or_skip(what: &str) -> Option<Arc<GpuContext>> {
	let ctx = GpuContext::create(BackendKind::Auto);
	if ctx.is_none() {
		skip_or_fail_gpu(what);
	}
	ctx
}

/// The process-wide shared context for a test, with the same
/// `QUERCUS_REQUIRE_GPU` policy as [`gpu_or_skip`].
pub fn shared_gpu_or_skip(what: &str) -> Option<Arc<GpuContext>> {
	let ctx = GpuContext::shared();
	if ctx.is_none() {
		skip_or_fail_gpu(what);
	}
	ctx
}

// SAFETY check: wgpu Device/Queue/Instance are Send+Sync; the rest is
// behind Mutexes. The context is shared across worker threads.
unsafe impl Send for GpuContext {}
unsafe impl Sync for GpuContext {}

/// Raw CPU bytes for one plane of a descriptor-driven frame, consumed by
/// [`GpuContext::upload_planar_matrix_planes`].
#[derive(Clone, Copy, Debug)]
pub struct PlaneData<'a> {
	/// Source plane bytes (row-major).
	pub data: &'a [u8],
	/// Source bytes per row; `0` means tightly packed (derived from the
	/// plane width and source component count).
	pub stride: usize,
}

impl<'a> PlaneData<'a> {
	/// Plane bytes whose rows are packed back-to-back (`stride == 0`, derived
	/// from the plane width when uploaded). Counterpart of
	/// [`PlaneData::strided`].
	pub fn tight(data: &'a [u8]) -> Self {
		Self { data, stride: 0 }
	}

	/// Plane bytes with an explicit source row stride, in bytes.
	///
	/// `stride` is the distance between the starts of consecutive rows and may
	/// exceed the packed row length when the decoder pads rows. Counterpart of
	/// [`PlaneData::tight`].
	pub fn strided(data: &'a [u8], stride: usize) -> Self {
		Self { data, stride }
	}
}

/// The `wgpu` copy row alignment for texture uploads (256 bytes).
const PLANE_ROW_ALIGNMENT: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;

/// Round a packed row up to [`PLANE_ROW_ALIGNMENT`] (256 bytes), the row
/// stride wgpu requires for buffer↔texture copies. `0` stays `0`.
fn aligned_plane_row_bytes(row_bytes: usize) -> usize {
	row_bytes.div_ceil(PLANE_ROW_ALIGNMENT) * PLANE_ROW_ALIGNMENT
}

/// Copy `height` rows of `row_bytes` from `data` (source stride
/// `src_stride`, `0` = tightly packed) into a staging buffer whose rows
/// are [`PLANE_ROW_ALIGNMENT`]-aligned, as `queue.write_texture`
/// expects. Short buffers and invalid strides are rejected.
fn repack_plane_rows(
	data: &[u8],
	src_stride: usize,
	row_bytes: usize,
	height: usize,
) -> Result<Vec<u8>> {
	if row_bytes == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	let stride = if src_stride == 0 {
		row_bytes
	} else {
		src_stride
	};
	if stride < row_bytes {
		return Err(Error::Invalid);
	}
	let needed = height
		.checked_sub(1)
		.and_then(|rows| rows.checked_mul(stride))
		.and_then(|n| n.checked_add(row_bytes))
		.ok_or(Error::Invalid)?;
	if data.len() < needed {
		return Err(Error::Invalid);
	}
	let staged_row = aligned_plane_row_bytes(row_bytes);
	let staged_len = staged_row.checked_mul(height).ok_or(Error::Invalid)?;
	let mut staged = vec![0u8; staged_len];
	for row in 0..height {
		let src_start = row * stride;
		let dst_start = row * staged_row;
		staged[dst_start..dst_start + row_bytes]
			.copy_from_slice(&data[src_start..src_start + row_bytes]);
	}
	Ok(staged)
}

/// Expand 3-channel interleaved rows into the 4-channel RGBA texture
/// layout, filling alpha with 1.0 for the storage depth. Returns a
/// tightly packed RGBA buffer.
fn expand_rgb_to_rgba(
	data: &[u8],
	src_stride: usize,
	width: usize,
	height: usize,
	storage: PlaneStorage,
) -> Result<Vec<u8>> {
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	let bpc = storage.bytes_per_component();
	let src_row = width.checked_mul(3 * bpc).ok_or(Error::Invalid)?;
	let dst_row = width.checked_mul(4 * bpc).ok_or(Error::Invalid)?;
	let stride = if src_stride == 0 { src_row } else { src_stride };
	if stride < src_row {
		return Err(Error::Invalid);
	}
	let needed = height
		.checked_sub(1)
		.and_then(|rows| rows.checked_mul(stride))
		.and_then(|n| n.checked_add(src_row))
		.ok_or(Error::Invalid)?;
	if data.len() < needed {
		return Err(Error::Invalid);
	}
	let alpha: &[u8] = match storage {
		PlaneStorage::U8 => &[0xFF],
		PlaneStorage::U16 => &[0xFF, 0xFF],
		PlaneStorage::F16 => &[0x00, 0x3C],
		PlaneStorage::F32 => &[0x00, 0x00, 0x80, 0x3F],
	};
	let mut out = vec![0u8; dst_row.checked_mul(height).ok_or(Error::Invalid)?];
	for row in 0..height {
		let src_start = row * stride;
		let dst_start = row * dst_row;
		for pixel in 0..width {
			let s = src_start + pixel * 3 * bpc;
			let d = dst_start + pixel * 4 * bpc;
			out[d..d + 3 * bpc].copy_from_slice(&data[s..s + 3 * bpc]);
			out[d + 3 * bpc..d + 4 * bpc].copy_from_slice(&alpha[..bpc]);
		}
	}
	Ok(out)
}

/// One asynchronous readback request: the caller waits on this while the
/// dedicated readback thread performs the GPU wait/map.
pub struct FrameReadback {
	rx: std::sync::mpsc::Receiver<Result<Frame>>,
	/// Source lifetime guard (CUDA VMM slot): held until the asynchronous
	/// readback completes, not just until the original Texture drops.
	_guard: Option<Arc<dyn any::Any + Send + Sync>>,
}

impl FrameReadback {
	/// Block until the readback thread has produced the frame or an error.
	pub fn wait(self) -> Result<Frame> {
		self.rx
			.recv()
			.unwrap_or_else(|_| Err(Error::Failed("readback worker stopped".into())))
	}
}

struct ReadbackJob {
	device: wgpu::Device,
	queue: wgpu::Queue,
	texture: Arc<wgpu::Texture>,
	format: wgpu::TextureFormat,
	width: u32,
	height: u32,
	external_lock: Arc<Mutex<()>>,
	tx: std::sync::mpsc::Sender<Result<Frame>>,
}

/// The process-wide readback thread, spawned lazily on first use.
///
/// Returns the sender side of the job channel. The `quercus-readback`
/// thread takes each job's external lifecycle lock, runs
/// [`download_texture`] and sends the result back on the job's own channel.
/// One thread serialises every [`GpuContext::download_async`] request,
/// keeping the render thread free but delaying a queued readback behind a
/// slow one.
fn readback_worker() -> &'static std::sync::mpsc::Sender<ReadbackJob> {
	static TX: std::sync::OnceLock<std::sync::mpsc::Sender<ReadbackJob>> =
		std::sync::OnceLock::new();
	TX.get_or_init(|| {
		let (tx, rx) = std::sync::mpsc::channel::<ReadbackJob>();
		let _ = thread::Builder::new()
			.name("quercus-readback".into())
			.spawn(move || {
				while let Ok(job) = rx.recv() {
					let _external = job.external_lock.lock().unwrap_or_else(|e| e.into_inner());
					let result = download_texture(
						&job.device,
						&job.queue,
						&job.texture,
						job.format,
						job.width,
						job.height,
					);
					let _ = job.tx.send(result);
				}
			});
		tx
	})
}

/// Copy a GPU texture back into a CPU [`Frame`] of `PixelFormat::F32`.
///
/// `Rgba32Float` is copied as-is and `Rgba16Float` is converted half→f32;
/// any other format is rejected. The copy goes through a `MAP_READ` staging
/// buffer whose rows are padded to 256 bytes and then repacked tightly into
/// `frame.data`. Because this may run on the readback thread, the device is
/// polled incrementally (never a blocking `PollType::Wait`) with a 30-second
/// deadline. Counted in [`gpu_transfer_counters`] as a GPU→CPU transfer.
///
/// # Errors
///
/// `Error::Failed` for an unsupported format, a failed/timed-out map, or a
/// device lost while polling (a `panic` from `device.poll` counts as lost).
fn download_texture(
	device: &wgpu::Device,
	queue: &wgpu::Queue,
	texture: &wgpu::Texture,
	format: wgpu::TextureFormat,
	width: u32,
	height: u32,
) -> Result<Frame> {
	GPU_DOWNLOADS.fetch_add(1, Ordering::Relaxed);
	// The packed formats are the export boundary's readback: the GPU has
	// already applied the delivery color transform, so the frame carries
	// 8-bit or 16-bit (10-bit-capable) RGBA instead of float RGBA.
	let (bpp, convert_half, frame_format) = match format {
		TextureFormat::Rgba32Float => (16usize, false, PixelFormat::F32),
		TextureFormat::Rgba16Float => (8usize, true, PixelFormat::F32),
		TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {
			(4usize, false, PixelFormat::U8)
		}
		TextureFormat::Rgba16Unorm => (8usize, false, PixelFormat::U16),
		other => {
			return Err(Error::Failed(format!(
				"texture download: unsupported format {other:?}"
			)))
		}
	};
	let w = width as usize;
	let h = height as usize;
	let linesize = w * bpp;
	// copy_texture_to_buffer requires a 256-byte-aligned row stride.
	let padded = (linesize + 255) & !255;
	let buffer = device.create_buffer(&wgpu::BufferDescriptor {
		label: Some("quercusrender-download"),
		size: (padded * h) as u64,
		usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
		mapped_at_creation: false,
	});
	let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
		label: Some("quercusrender-download"),
	});
	encoder.copy_texture_to_buffer(
		wgpu::TexelCopyTextureInfo {
			texture,
			mip_level: 0,
			origin: Origin3d::ZERO,
			aspect: TextureAspect::All,
		},
		wgpu::TexelCopyBufferInfo {
			buffer: &buffer,
			layout: wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(padded as u32),
				rows_per_image: None,
			},
		},
		wgpu::Extent3d {
			width: w as u32,
			height: h as u32,
			depth_or_array_layers: 1,
		},
	);
	queue.submit(Some(encoder.finish()));
	let (tx, rx) = std::sync::mpsc::channel();
	buffer.slice(..).map_async(MapMode::Read, move |result| {
		let _ = tx.send(result.is_ok());
	});
	// Poll incrementally: a blocking `PollType::Wait` on the readback
	// thread can serialize against the render thread's submits. Polling
	// keeps the queue lock held only for the maintenance call, and the
	// map callback is still driven by the same device.
	let deadline = time::Instant::now() + time::Duration::from_secs(30);
	let mapped_ok = loop {
		let poll = panic::catch_unwind(panic::AssertUnwindSafe(|| {
			let _ = device.poll(PollType::Poll);
		}));
		if poll.is_err() {
			return Err(Error::Failed(
				"wgpu device lost during texture download".into(),
			));
		}
		match rx.try_recv() {
			Ok(ok) => break ok,
			Err(std::sync::mpsc::TryRecvError::Disconnected) => break false,
			Err(std::sync::mpsc::TryRecvError::Empty) => {}
		}
		if time::Instant::now() > deadline {
			return Err(Error::Failed("texture download map timed out".into()));
		}
		thread::sleep(time::Duration::from_millis(1));
	};
	if !mapped_ok {
		return Err(Error::Failed("texture download map failed".into()));
	}
	let mapped = buffer.slice(..).get_mapped_range();
	let mut data = if padded == linesize {
		// Tight rows: one bulk copy instead of `height` per-row memcpys.
		mapped.to_vec()
	} else {
		let mut data = vec![0u8; linesize * h];
		for row in 0..h {
			let src = &mapped[row * padded..row * padded + linesize];
			data[row * linesize..(row + 1) * linesize].copy_from_slice(src);
		}
		data
	};
	drop(mapped);
	buffer.unmap();
	if convert_half {
		let mut f32_data = vec![0u8; w * h * 16];
		for i in 0..w * h * 4 {
			let bits = u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
			let v = half::f16::from_bits(bits).to_f32();
			f32_data[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
		data = f32_data;
	}
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w as i32;
	pod.height = h as i32;
	pod.format = frame_format as i32;
	frame.set_video_params(pod);
	frame.data = data;
	Ok(frame)
}

impl GpuContext {
	/// Create for the preferred backend; falls back across the backend
	/// order. `None` when no adapter is available (callers use the CPU
	/// path).
	pub fn create(prefer: BackendKind) -> Option<Arc<Self>> {
		// Held for the whole probe/create sequence: the Vulkan loader
		// crashes on concurrent instance creation (see `create_lock`).
		let _create_guard = create_lock();
		for backends in prefer.wgpu_fallbacks() {
			backend_diagnostic(format_args!("trying {backends:?}"));
			let instance = Instance::new(wgpu::InstanceDescriptor {
				backends,
				..InstanceDescriptor::new_without_display_handle()
			});
			let adapter =
				match pollster_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
					power_preference: PowerPreference::HighPerformance,
					compatible_surface: None,
					force_fallback_adapter: false,
				})) {
					Ok(a) => a,
					Err(error) => {
						backend_diagnostic(format_args!(
							"request_adapter({backends:?}) failed: {error}"
						));
						continue;
					}
				};
			let info = adapter.get_info();
			backend_diagnostic(format_args!(
				"adapter: name={:?} backend={:?} vendor=0x{:x} device=0x{:x} driver={:?} {}",
				info.name, info.backend, info.vendor, info.device, info.driver, info.driver_info
			));
			// The pipeline's canonical render target is Rgba32Float;
			// adapters that cannot render it (downlevel GL/GLES without
			// GL_EXT_color_buffer_float — e.g. some software Mesa
			// setups) fail every pipeline/texture-usage validation, so
			// treat them as unavailable and let the CPU path take over.
			if !adapter
				.get_texture_format_features(TextureFormat::Rgba32Float)
				.allowed_usages
				.contains(TextureUsages::RENDER_ATTACHMENT)
			{
				backend_diagnostic(format_args!(
					"skipping {}: Rgba32Float is not renderable",
					info.name
				));
				continue;
			}
			// Linear sampling on Rgba32Float needs FLOAT32_FILTERABLE
			// (widely available on desktop GPUs); without it effect
			// shaders sample nearest — a quality degradation, not a
			// failure (logged once by the shaderfx runner).
			let filterable = adapter.features().contains(Features::FLOAT32_FILTERABLE);
			let mut required_features = Features::empty();
			if filterable {
				required_features |= Features::FLOAT32_FILTERABLE;
			}
			// M5 (Windows): the D3D11VA import wraps a multi-planar
			// NV12/P010 texture; those formats need their features
			// enabled at device creation (no-op on adapters without
			// them — the import then falls back per frame).
			for feature in [Features::TEXTURE_FORMAT_NV12, Features::TEXTURE_FORMAT_P010] {
				if adapter.features().contains(feature) {
					required_features |= feature;
				}
			}
			// Normalized 16-bit plane textures (P010, YUV422P10,
			// YUV444P16, and every CPU raw-plane upload at U16 storage)
			// are native-only formats gated behind this feature. Request
			// it when the adapter advertises it; the upload path reports
			// a clean error when an adopted host device lacks it.
			if adapter
				.features()
				.contains(Features::TEXTURE_FORMAT_16BIT_NORM)
			{
				required_features |= Features::TEXTURE_FORMAT_16BIT_NORM;
			}
			// Windows OpenFX GL zero-copy needs VK_KHR_external_memory_win32
			// enabled at device creation (OPAQUE_WIN32 export). Request it
			// only when the adapter advertises it; the runtime gate then
			// disables zero-copy on drivers without it.
			#[cfg(target_os = "windows")]
			if adapter
				.features()
				.contains(Features::VULKAN_EXTERNAL_MEMORY_WIN32)
			{
				required_features |= Features::VULKAN_EXTERNAL_MEMORY_WIN32;
			}
			let (device, queue) =
				match pollster_block_on(adapter.request_device(&wgpu::DeviceDescriptor {
					label: Some("quercusrender"),
					required_features,
					required_limits: Limits::default(),
					experimental_features: ExperimentalFeatures::disabled(),
					memory_hints: MemoryHints::default(),
					trace: Trace::Off,
				})) {
					Ok(dq) => dq,
					Err(error) => {
						backend_diagnostic(format_args!(
							"request_device({} / {:?}) failed: {error}",
							info.name, info.backend
						));
						continue;
					}
				};
			let kind = BackendKind::from_wgpu_backend(info.backend);
			if kind == BackendKind::Cpu {
				backend_diagnostic(format_args!(
					"skipping {}: unsupported reported backend {:?}",
					info.name, info.backend
				));
				continue;
			}
			backend_diagnostic(format_args!(
				"selected {} with {:?}",
				info.name, info.backend
			));
			let device_lost = Arc::new(AtomicBool::new(false));
			install_device_lost_logging(&device, device_lost.clone());
			return Some(Arc::new(Self {
				_instance: Some(instance),
				_adapter: Some(adapter),
				device,
				queue,
				kind,
				textures: Mutex::new(HashMap::new()),
				next_token: AtomicU64::new(1),
				blit: Mutex::new(None),
				blit16: Mutex::new(None),
				filterable,
				programs: Mutex::new(HashMap::new()),
				placeholder: Mutex::new(None),
				display_lut: Mutex::new(None),
				present: Mutex::new(Vec::new()),
				yuv: Mutex::new(None),
				yuv_planar: Mutex::new(None),
				planar_matrix: Mutex::new(None),
				color_luts: Mutex::new(Vec::new()),
				used: AtomicBool::new(false),
				device_lost,
				external_lock: Arc::new(Mutex::new(())),
				ofx_zerocopy: zerocopy_requested(),
			}));
		}
		None
	}

	/// Adopt an existing wgpu device/queue (M2): the host UI creates the
	/// device (gpui_wgpu) and the engine renders on it, so the textures
	/// the render thread produces are directly sampleable by the UI's
	/// renderer — the zero-copy present path. `kind` labels the backend
	/// (the device does not expose it); `BackendKind::Auto` is acceptable.
	pub fn adopt(
		device: Arc<wgpu::Device>,
		queue: Arc<wgpu::Queue>,
		kind: BackendKind,
	) -> Arc<Self> {
		// The adopted device's feature set decides filtering; the render
		// pipeline's `Rgba32Float` render target works on every adapter
		// whose device made it this far (the adopter only hands over a
		// live, validated device).
		let filterable = device.features().contains(Features::FLOAT32_FILTERABLE);
		Arc::new(Self {
			_instance: None,
			_adapter: None,
			device: (*device).clone(),
			queue: (*queue).clone(),
			kind,
			textures: Mutex::new(HashMap::new()),
			next_token: AtomicU64::new(1),
			blit: Mutex::new(None),
			blit16: Mutex::new(None),
			filterable,
			programs: Mutex::new(HashMap::new()),
			placeholder: Mutex::new(None),
			display_lut: Mutex::new(None),
			present: Mutex::new(Vec::new()),
			yuv: Mutex::new(None),
			yuv_planar: Mutex::new(None),
			planar_matrix: Mutex::new(None),
			color_luts: Mutex::new(Vec::new()),
			used: AtomicBool::new(false),
			device_lost: Arc::new(AtomicBool::new(false)),
			external_lock: Arc::new(Mutex::new(())),
			ofx_zerocopy: zerocopy_requested(),
		})
	}

	/// True when this context wraps a device it did not create (the UI
	/// shared its device; presentation is zero-copy on this context).
	pub fn is_adopted(&self) -> bool {
		self._adapter.is_none()
	}

	/// The adapter description, when this context created its own device
	/// (adopted contexts do not own an adapter).
	pub fn adapter_name(&self) -> Option<String> {
		self._adapter.as_ref().map(|a| a.get_info().name)
	}

	/// True once the context has created a GPU resource (texture or
	/// pipeline). An unused context is replaceable by
	/// [`GpuContext::install_shared`].
	pub fn is_used(&self) -> bool {
		self.used.load(Ordering::Acquire)
	}

	/// The underlying wgpu device/queue. Handing these out lets another
	/// context (or the UI) wrap the same device: contexts sharing a device
	/// can present each other's textures zero-copy.
	pub fn device_queue(&self) -> (Arc<wgpu::Device>, Arc<wgpu::Queue>) {
		(Arc::new(self.device.clone()), Arc::new(self.queue.clone()))
	}

	/// Wait until the wgpu queue has finished all submitted work. Used as
	/// the coarse cross-API synchronization boundary for the OpenFX GL
	/// zero-copy path (the first version does not yet use external
	/// semaphores).
	pub fn wait_idle(&self) {
		// Flush any queued `Queue::write_texture` staging copies first:
		// they are only submitted with the next `Queue::submit`, and a
		// bare `poll` does not necessarily submit them.
		let _ = self.queue.submit(iter::empty());
		// `Device::poll` panics via wgpu's fatal-error hook once the device
		// has been lost. A plugin/host readback must fail that frame, not
		// abort the whole editor.
		if panic::catch_unwind(panic::AssertUnwindSafe(|| {
			let _ = self.device.poll(PollType::wait_indefinitely());
		}))
		.is_err()
		{
			self.mark_device_lost();
		}
	}

	/// Bounded variant of [`GpuContext::wait_idle`]. Returns false when the
	/// GPU did not finish within `timeout` (external-memory/OFX paths must
	/// not block the render thread forever on a driver stall).
	pub fn wait_idle_for(&self, timeout: time::Duration) -> bool {
		let _ = self.queue.submit(iter::empty());
		panic::catch_unwind(panic::AssertUnwindSafe(|| {
			matches!(
				self.device.poll(PollType::Wait {
					submission_index: None,
					timeout: Some(timeout),
				}),
				Ok(PollStatus::WaitSucceeded) | Ok(PollStatus::QueueEmpty)
			)
		}))
		.unwrap_or_else(|_| {
			self.mark_device_lost();
			false
		})
	}

	/// Run `callback` once all GPU work submitted so far has completed.
	/// External-memory producers use this to know when a reusable VMM
	/// buffer is no longer read by the renderer.
	/// Lock shared with the CUDA import path: CUDA writes into exported
	/// Vulkan memory must not run concurrently with a readback using that
	/// device.
	pub fn external_lifecycle_lock(&self) -> Arc<Mutex<()>> {
		self.external_lock.clone()
	}

	/// Run `callback` once every command buffer submitted so far has finished
	/// on the GPU.
	///
	/// Thin forwarding to [`wgpu::Queue::on_submitted_work_done`]: `callback`
	/// runs on wgpu's internal thread, so it must capture everything it needs
	/// and must not block.
	pub fn on_submitted_work_done(&self, callback: impl FnOnce() + Send + 'static) {
		self.queue.on_submitted_work_done(callback);
	}

	/// True after a fatal wgpu device-lost/poll error.
	pub fn device_lost(&self) -> bool {
		self.device_lost.load(std::sync::atomic::Ordering::Acquire)
	}

	fn mark_device_lost(&self) {
		self.device_lost
			.store(true, std::sync::atomic::Ordering::Release);
	}

	/// The backend actually in use.
	pub fn kind(&self) -> BackendKind {
		self.kind
	}

	/// True when this context hosts a Metal/Vulkan/GL adapter (never true
	/// for the CPU fallback).
	pub fn is_gpu(&self) -> bool {
		self.kind != BackendKind::Cpu
	}

	/// Create an `Rgba32Float` texture with the pipeline's canonical usage flags
	/// and register it.
	///
	/// `width`/`height` are in pixels and must be positive. The texture can be
	/// sampled, rendered to, uploaded into and copied from; this is the format
	/// every working-space pass produces. Returns the registry token consumed by
	/// [`GpuContext::upload`], [`GpuContext::blit`], [`GpuContext::download`]
	/// and the rest of the texture API.
	///
	/// # Errors
	///
	/// `Error::Invalid` when `width` or `height` is not positive.
	pub fn create_texture(&self, width: i32, height: i32) -> Result<u64> {
		self.create_texture_format(
			width,
			height,
			1,
			TextureFormat::Rgba32Float,
			TextureUsages::TEXTURE_BINDING
				| TextureUsages::RENDER_ATTACHMENT
				| TextureUsages::COPY_DST
				| TextureUsages::COPY_SRC,
		)
	}

	/// Create a texture of an arbitrary format/depth (M2): the display
	/// LUT is a 3D `R32Float` texture and the YUV→RGB pass reads
	/// single-channel plane textures. `depth` > 1 selects `D3`.
	pub fn create_texture_format(
		&self,
		width: i32,
		height: i32,
		depth: u32,
		format: wgpu::TextureFormat,
		usage: wgpu::TextureUsages,
	) -> Result<u64> {
		if width <= 0 || height <= 0 || depth == 0 {
			return Err(Error::Invalid);
		}
		#[cfg(any(target_os = "linux", target_os = "windows"))]
		if self.ofx_zerocopy && depth == 1 && format == TextureFormat::Rgba32Float {
			match self.create_exportable_texture(width, height, format, usage) {
				Ok(token) => return Ok(token),
				Err(err) => {
					log::debug!("exportable Rgba32Float texture unavailable, falling back: {err}");
				}
			}
		}
		self.used.store(true, Ordering::Release);
		let size = wgpu::Extent3d {
			width: width as u32,
			height: height as u32,
			depth_or_array_layers: depth,
		};
		let texture = self.device.create_texture(&wgpu::TextureDescriptor {
			label: Some("quercusrender-texture"),
			size,
			mip_level_count: 1,
			sample_count: 1,
			dimension: if depth > 1 {
				TextureDimension::D3
			} else {
				TextureDimension::D2
			},
			format,
			usage,
			view_formats: &[],
		});
		let token = self.next_token.fetch_add(1, Ordering::Relaxed);
		lock(&self.textures).insert(
			token,
			GpuTexture {
				texture: Arc::new(texture),
				width: width as u32,
				height: height as u32,
				format,
				aspect: TextureAspect::All,
				layer: None,
				#[cfg(any(target_os = "linux", target_os = "windows"))]
				external: None,
				keep_alive: None,
			},
		);
		Ok(token)
	}

	/// Drop `token` from the registry, releasing the GPU texture once the last
	/// reference is gone. Unknown tokens are ignored, so repeated calls are
	/// harmless (idempotent).
	pub fn destroy_texture(&self, token: u64) {
		lock(&self.textures).remove(&token);
	}

	/// Attach a lifetime guard to a token (e.g. the CUDA VMM slot backing an
	/// imported frame). Dropped when the token is destroyed.
	pub fn set_texture_keep_alive(&self, token: u64, guard: Arc<dyn any::Any + Send + Sync>) {
		if let Some(entry) = lock(&self.textures).get_mut(&token) {
			entry.keep_alive = Some(guard);
		}
	}

	/// The `(width, height)` of the texture behind `token`, in pixels; `None`
	/// when the token is unknown.
	pub fn texture_size(&self, token: u64) -> Option<(u32, u32)> {
		lock(&self.textures)
			.get(&token)
			.map(|t| (t.width, t.height))
	}

	/// Whether the registry still holds `token`.
	///
	/// Tokens leave the registry via [`GpuContext::destroy_texture`] and
	/// [`GpuContext::take_texture_handle`], so `false` means the texture has
	/// already been released.
	pub fn has_texture(&self, token: u64) -> bool {
		lock(&self.textures).contains_key(&token)
	}

	/// The raw `wgpu::Texture` behind a token (M2 zero-copy present): the
	/// UI hands this very texture to gpui's surface path. The registry
	/// keeps its reference, so the caller may drop/destroy its token as
	/// soon as the returned `Arc` is stored elsewhere.
	pub fn texture_handle(&self, token: u64) -> Option<Arc<wgpu::Texture>> {
		lock(&self.textures).get(&token).map(|t| t.texture.clone())
	}

	/// Remove a token from the registry and return its texture. Used for
	/// transient conversion/presentation textures handed to the host.
	pub fn take_texture_handle(&self, token: u64) -> Option<Arc<wgpu::Texture>> {
		lock(&self.textures).remove(&token).map(|t| t.texture)
	}

	/// The format `token` was created with, or `None` when the token is unknown.
	/// Used by plane uploads and tests.
	pub fn texture_format(&self, token: u64) -> Option<wgpu::TextureFormat> {
		lock(&self.textures).get(&token).map(|t| t.format)
	}

	/// Upload tightly packed raw bytes into a plain single-channel texture (the
	/// YUV→RGB planes).
	///
	/// `data` must be at least `width * bytes-per-texel * height` bytes with no
	/// row padding; accepted texture formats are `R8Unorm`, `R16Unorm`,
	/// `R16Float` and `R32Float` (1, 2, 2 and 4 bytes per texel). Counted in
	/// [`gpu_transfer_counters`] as a CPU→GPU transfer.
	///
	/// # Errors
	///
	/// `Error::NotFound` for an unknown `token`; `Error::Invalid` for an
	/// unsupported texture format or a buffer shorter than the texture needs.
	pub fn upload_plane(&self, token: u64, data: &[u8]) -> Result<()> {
		GPU_UPLOADS.fetch_add(1, Ordering::Relaxed);
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let w = entry.width;
		let h = entry.height;
		let bpp = match entry.format {
			TextureFormat::R8Unorm => 1usize,
			TextureFormat::R16Unorm | TextureFormat::R16Float => 2,
			TextureFormat::R32Float => 4,
			_ => return Err(Error::Invalid),
		};
		let row = w as usize * bpp;
		if data.len() < row * h as usize {
			return Err(Error::Invalid);
		}
		self.queue.write_texture(
			wgpu::TexelCopyTextureInfo {
				texture: &entry.texture,
				mip_level: 0,
				origin: Origin3d::ZERO,
				aspect: TextureAspect::All,
			},
			data,
			wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(row as u32),
				rows_per_image: None,
			},
			wgpu::Extent3d {
				width: w,
				height: h,
				depth_or_array_layers: 1,
			},
		);
		Ok(())
	}

	/// True when the device enabled the normalized 16-bit texture
	/// formats (`R16Unorm`/`Rg16Unorm`/`Rgba16Unorm`). Required for U16
	/// plane storage (P010, YUV422P10, 10-bit-in-16 raw uploads).
	pub fn supports_16bit_norm_planes(&self) -> bool {
		self.device
			.features()
			.contains(Features::TEXTURE_FORMAT_16BIT_NORM)
	}

	/// Create the plane textures for a descriptor-driven frame: three
	/// single-channel planes for planar YUV, luma + interleaved two-channel
	/// chroma for semi-planar YUV, one RGBA plane for interleaved RGB(A).
	/// Chroma planes follow the descriptor's subsampling (odd dimensions
	/// round up). Every plane is `TEXTURE_BINDING | COPY_DST`; upload CPU
	/// bytes with [`GpuContext::upload_planar_matrix_planes`]. The caller
	/// owns the returned tokens (wrap them in a `PlanarTexture` or destroy
	/// them).
	pub fn create_planar_matrix_textures(
		&self,
		descriptor: PlanarDescriptor,
		width: i32,
		height: i32,
	) -> Result<Vec<u64>> {
		if width <= 0 || height <= 0 || descriptor.plane_count() == 0 {
			return Err(Error::Invalid);
		}
		if descriptor.storage == PlaneStorage::U16 && !self.supports_16bit_norm_planes() {
			return Err(Error::Failed(
				"16-bit normalized texture formats are not enabled on this device".into(),
			));
		}
		let usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
		let mut tokens = Vec::with_capacity(descriptor.plane_count());
		for plane in 0..descriptor.plane_count() {
			let (plane_width, plane_height) = descriptor
				.plane_size(plane, width, height)
				.ok_or(Error::Invalid)?;
			let format = descriptor
				.plane_texture_format(plane)
				.ok_or(Error::Invalid)?;
			match self.create_texture_format(plane_width, plane_height, 1, format, usage) {
				Ok(token) => tokens.push(token),
				Err(err) => {
					for token in &tokens {
						self.destroy_texture(*token);
					}
					return Err(err);
				}
			}
		}
		Ok(tokens)
	}

	/// Upload raw CPU plane bytes for a descriptor-driven frame. `planes`
	/// are the tokens from [`GpuContext::create_planar_matrix_textures`]
	/// in descriptor order and `data` has exactly one entry per plane.
	///
	/// Rows may be tightly packed (`stride == 0`) or strided; each row is
	/// copied into a 256-byte (`COPY_BYTES_PER_ROW_ALIGNMENT`) aligned
	/// staging row before `queue.write_texture`, and 16-bit values are
	/// preserved verbatim. Interleaved RGB source data (3 channels) is
	/// expanded into the RGBA texture layout with alpha = 1.0. A failure
	/// after an earlier plane was written leaves that plane uploaded; the
	/// frame-level helpers destroy the tokens on failure.
	pub fn upload_planar_matrix_planes(
		&self,
		planes: &[u64],
		descriptor: PlanarDescriptor,
		data: &[PlaneData<'_>],
	) -> Result<()> {
		if planes.len() != descriptor.plane_count() || data.len() != planes.len() {
			return Err(Error::Invalid);
		}
		for (plane, (token, plane_data)) in planes.iter().zip(data).enumerate() {
			self.write_planar_plane(*token, descriptor, plane, plane_data)?;
		}
		Ok(())
	}

	/// Create and upload one descriptor-driven frame in one call.
	/// Returns the plane tokens in descriptor order; the caller owns them.
	/// On failure every created token is destroyed before returning.
	pub fn upload_planar_matrix_frame(
		&self,
		descriptor: PlanarDescriptor,
		width: i32,
		height: i32,
		data: &[PlaneData<'_>],
	) -> Result<Vec<u64>> {
		if data.len() != descriptor.plane_count() {
			return Err(Error::Invalid);
		}
		let planes = self.create_planar_matrix_textures(descriptor, width, height)?;
		if let Err(err) = self.upload_planar_matrix_planes(&planes, descriptor, data) {
			for token in &planes {
				self.destroy_texture(*token);
			}
			return Err(err);
		}
		Ok(planes)
	}

	/// Legacy adapter: create the `(y, uv)` semi-planar plane textures for a
	/// [`PlanarFormat`].
	///
	/// `width`/`height` are the luma dimensions; the chroma plane is sized by
	/// the format's subsampling. Returns the two tokens, or `Error::Invalid`
	/// after destroying whatever was created when the format does not describe
	/// exactly two planes. Counterpart of
	/// [`GpuContext::upload_planar_frame`].
	pub fn create_planar_textures(
		&self,
		format: PlanarFormat,
		width: i32,
		height: i32,
	) -> Result<(u64, u64)> {
		let mut planes = self.create_planar_matrix_textures(format.descriptor(), width, height)?;
		if planes.len() == 2 {
			Ok((planes[0], planes[1]))
		} else {
			for token in &planes {
				self.destroy_texture(*token);
			}
			planes.clear();
			Err(Error::Invalid)
		}
	}

	/// Legacy adapter: upload tightly packed `y`/`uv` CPU bytes into
	/// textures created by [`GpuContext::create_planar_textures`].
	pub fn upload_planar_planes(
		&self,
		y: u64,
		uv: u64,
		y_bytes: &[u8],
		uv_bytes: &[u8],
	) -> Result<()> {
		self.upload_planar_planes_strided(y, uv, y_bytes, 0, uv_bytes, 0)
	}

	/// Legacy stride-aware adapter: `y_stride`/`uv_stride` are the source
	/// bytes per row (`0` = tightly packed).
	pub fn upload_planar_planes_strided(
		&self,
		y: u64,
		uv: u64,
		y_bytes: &[u8],
		y_stride: usize,
		uv_bytes: &[u8],
		uv_stride: usize,
	) -> Result<()> {
		let descriptor = self.infer_semi_planar_descriptor(y, uv, None)?;
		self.upload_planar_matrix_planes(
			&[y, uv],
			descriptor,
			&[
				PlaneData::strided(y_bytes, y_stride),
				PlaneData::strided(uv_bytes, uv_stride),
			],
		)
	}

	/// Legacy adapter: create and upload one complete [`PlanarFormat`] frame,
	/// returning the `(y, uv)` tokens.
	///
	/// `width`/`height` are the luma dimensions; `y_bytes`/`uv_bytes` carry the
	/// plane data and `y_stride`/`uv_stride` the source bytes per row (`0` =
	/// tightly packed). Both textures are created first and destroyed again if
	/// the upload fails, so an `Err` leaks nothing. The caller owns the returned
	/// tokens (wrap them in a `PlanarTexture` or destroy them).
	///
	/// # Errors
	///
	/// `Error::Invalid` for non-positive dimensions or plane bytes too short for
	/// the given stride and size; `Error::Failed` when the device lacks 16-bit
	/// normalized plane formats.
	#[allow(clippy::too_many_arguments)]
	pub fn upload_planar_frame(
		&self,
		format: PlanarFormat,
		width: i32,
		height: i32,
		y_bytes: &[u8],
		y_stride: usize,
		uv_bytes: &[u8],
		uv_stride: usize,
	) -> Result<(u64, u64)> {
		let descriptor = format.descriptor();
		let planes = self.create_planar_matrix_textures(descriptor, width, height)?;
		if let Err(err) = self.upload_planar_matrix_planes(
			&planes,
			descriptor,
			&[
				PlaneData::strided(y_bytes, y_stride),
				PlaneData::strided(uv_bytes, uv_stride),
			],
		) {
			for token in &planes {
				self.destroy_texture(*token);
			}
			return Err(err);
		}
		Ok((planes[0], planes[1]))
	}

	/// Write one plane's CPU bytes into a registry texture: validate the
	/// descriptor matches the texture, repack rows to the copy alignment
	/// and expand RGB to RGBA where needed.
	fn write_planar_plane(
		&self,
		token: u64,
		descriptor: PlanarDescriptor,
		plane: usize,
		data: &PlaneData<'_>,
	) -> Result<()> {
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let expected_format = descriptor
			.plane_texture_format(plane)
			.ok_or(Error::Invalid)?;
		if entry.format != expected_format
			|| !entry.texture.usage().contains(TextureUsages::COPY_DST)
		{
			return Err(Error::Invalid);
		}
		let source_components = descriptor
			.plane_source_components(plane)
			.ok_or(Error::Invalid)?;
		let texture_components = descriptor
			.plane_texture_components(plane)
			.ok_or(Error::Invalid)?;
		let bpc = descriptor.storage.bytes_per_component();
		let width = entry.width as usize;
		let height = entry.height as usize;
		if width == 0 || height == 0 {
			return Err(Error::Invalid);
		}
		let source_row_bytes = width
			.checked_mul(source_components * bpc)
			.ok_or(Error::Invalid)?;
		let texture_row_bytes = width
			.checked_mul(texture_components * bpc)
			.ok_or(Error::Invalid)?;
		let staged = if source_components == texture_components {
			repack_plane_rows(data.data, data.stride, source_row_bytes, height)?
		} else if source_components == 3 && texture_components == 4 {
			let expanded =
				expand_rgb_to_rgba(data.data, data.stride, width, height, descriptor.storage)?;
			repack_plane_rows(&expanded, 0, texture_row_bytes, height)?
		} else {
			return Err(Error::Invalid);
		};
		let staged_row = aligned_plane_row_bytes(texture_row_bytes);
		GPU_UPLOADS.fetch_add(1, Ordering::Relaxed);
		self.queue.write_texture(
			wgpu::TexelCopyTextureInfo {
				texture: &entry.texture,
				mip_level: 0,
				origin: Origin3d::ZERO,
				aspect: TextureAspect::All,
			},
			&staged,
			wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(staged_row as u32),
				rows_per_image: Some(entry.height),
			},
			wgpu::Extent3d {
				width: entry.width,
				height: entry.height,
				depth_or_array_layers: 1,
			},
		);
		Ok(())
	}

	/// RGB to display-encoded RGB (the output node plus the display ICC
	/// chain, baked on the CPU by the app); [`present_texture`] applies it
	/// entirely on the GPU. Replaces (and destroys) any previous LUT.
	pub fn set_display_lut(&self, lut: &lut::Lut3d) -> Result<()> {
		let token = self.upload_lut(lut)?;
		let mut slot = lock(&self.display_lut);
		if let Some(old) = slot.take() {
			self.destroy_texture(old.token);
		}
		*slot = Some(DisplayLutState {
			token,
			edge: lut.edge,
			lo: lut.lo,
			hi: lut.hi,
		});
		Ok(())
	}

	/// Upload a [`crate::lut::Lut3d`] as an `edge³` `Rgba32Float` 3D texture.
	///
	/// `lut.data` holds RGB triplets ordered z-major, then y, then x; they are
	/// expanded to RGBA with alpha 1.0 and rows padded to wgpu's 256-byte copy
	/// alignment. Counted in [`gpu_transfer_counters`] as a CPU→GPU transfer.
	/// Callers cache the returned token per LUT (see
	/// [`GpuContext::apply_color_lut`] and [`GpuContext::set_display_lut`]).
	///
	/// # Errors
	///
	/// `Error::Invalid` when `lut.data.len() != edge³ * 3`.
	fn upload_lut(&self, lut: &lut::Lut3d) -> Result<u64> {
		let expected = (lut.edge as usize).pow(3) * 3;
		if lut.data.len() != expected {
			return Err(Error::Invalid);
		}
		let token = self.create_texture_format(
			lut.edge as i32,
			lut.edge as i32,
			lut.edge,
			TextureFormat::Rgba32Float,
			TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
		)?;
		// `write_texture` on a 3D texture: one row per (z,y), each row
		// `edge` RGBA f32 values, padded to the copy alignment.
		let row = lut.edge as usize * 16;
		let padded = (row + 255) & !255;
		let mut bytes = vec![0u8; padded * lut.edge as usize * lut.edge as usize];
		for z in 0..lut.edge as usize {
			for y in 0..lut.edge as usize {
				let src = &lut.data[(z * lut.edge as usize + y) * lut.edge as usize * 3..];
				let dst_off = (z * lut.edge as usize + y) * padded;
				for x in 0..lut.edge as usize {
					bytes[dst_off + x * 16..dst_off + x * 16 + 4]
						.copy_from_slice(&src[x * 3].to_le_bytes());
					bytes[dst_off + x * 16 + 4..dst_off + x * 16 + 8]
						.copy_from_slice(&src[x * 3 + 1].to_le_bytes());
					bytes[dst_off + x * 16 + 8..dst_off + x * 16 + 12]
						.copy_from_slice(&src[x * 3 + 2].to_le_bytes());
					bytes[dst_off + x * 16 + 12..dst_off + x * 16 + 16]
						.copy_from_slice(&1.0f32.to_le_bytes());
				}
			}
		}
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		GPU_UPLOADS.fetch_add(1, Ordering::Relaxed);
		self.queue.write_texture(
			wgpu::TexelCopyTextureInfo {
				texture: &entry.texture,
				mip_level: 0,
				origin: Origin3d::ZERO,
				aspect: TextureAspect::All,
			},
			&bytes,
			wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(padded as u32),
				rows_per_image: Some(lut.edge),
			},
			wgpu::Extent3d {
				width: lut.edge,
				height: lut.edge,
				depth_or_array_layers: lut.edge,
			},
		);
		Ok(token)
	}

	/// Whether a display LUT is installed.
	pub fn has_display_lut(&self) -> bool {
		lock(&self.display_lut).is_some()
	}

	/// Apply the installed display LUT to `src` (a working-space
	/// `Rgba32Float` texture) and return the resulting `Rgba32Float`
	/// display texture token. GPU→GPU: no upload, no download. The caller
	/// retrieves the raw handle with [`GpuContext::texture_handle`].
	pub fn present_texture(&self, src: u64) -> Result<u64> {
		let lut = {
			let slot = lock(&self.display_lut);
			let Some(lut) = slot.as_ref() else {
				return Err(Error::Failed("no display LUT installed".into()));
			};
			(lut.token, lut.edge, lut.lo, lut.hi)
		};
		self.apply_lut_to(src, lut.0, lut.1, lut.2, lut.3, TextureFormat::Rgba32Float)
	}

	/// Apply a caller-keyed 3D LUT to `src` and return a new
	/// `Rgba32Float` texture token — the GPU color transform for graph
	/// nodes (`ColorTransformJob`, M2). The LUT texture is uploaded once
	/// per key (the caller passes a stable processor cache id); applying
	/// it is GPU→GPU.
	pub fn apply_color_lut(&self, src: u64, key: &str, lut: &lut::Lut3d) -> Result<u64> {
		let token = {
			let mut cache = lock(&self.color_luts);
			if let Some((_, token)) = cache.iter().find(|(k, _)| k == key) {
				*token
			} else {
				let token = self.upload_lut(lut)?;
				if cache.len() >= 8 {
					let (_, old) = cache.remove(0);
					self.destroy_texture(old);
				}
				cache.push((key.to_string(), token));
				token
			}
		};
		self.apply_lut_to(
			src,
			token,
			lut.edge,
			lut.lo,
			lut.hi,
			TextureFormat::Rgba32Float,
		)
	}

	/// Run the display LUT pass: sample the 3D `lut_token` texture at every
	/// pixel of `src` and write the result into a new texture of `dst_format`.
	///
	/// `edge` is the LUT's grid size and `lo`/`hi` the input value ranges it was
	/// baked for; they are passed to the shader verbatim. The destination is
	/// sized from `src`, registered like any other texture and owned by the
	/// caller. The external lifecycle lock is held for the whole pass so an
	/// external producer cannot write the source concurrently.
	///
	/// # Errors
	///
	/// `Error::NotFound` when `src` or `lut_token` is unknown; texture and
	/// pipeline creation errors propagate unchanged.
	fn apply_lut_to(
		&self,
		src: u64,
		lut_token: u64,
		edge: u32,
		lo: [f32; 3],
		hi: [f32; 3],
		dst_format: wgpu::TextureFormat,
	) -> Result<u64> {
		let _external = self.external_lock.lock().unwrap_or_else(|e| e.into_inner());
		let src_tex = lock(&self.textures)
			.get(&src)
			.cloned()
			.ok_or(Error::NotFound)?;
		let lut_tex = lock(&self.textures)
			.get(&lut_token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let dst = self.create_texture_format(
			src_tex.width as i32,
			src_tex.height as i32,
			1,
			dst_format,
			TextureUsages::TEXTURE_BINDING
				| TextureUsages::RENDER_ATTACHMENT
				| TextureUsages::COPY_SRC,
		)?;
		let dst_tex = lock(&self.textures)
			.get(&dst)
			.cloned()
			.ok_or(Error::NotFound)?;

		let pipeline = self.present_pipeline(dst_format)?;
		let params: [f32; 12] = [
			edge as f32,
			0.0,
			0.0,
			0.0,
			lo[0],
			lo[1],
			lo[2],
			0.0,
			hi[0],
			hi[1],
			hi[2],
			0.0,
		];
		let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("quercusrender-present-params"),
			size: 48,
			usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
			mapped_at_creation: false,
		});
		self.queue
			.write_buffer(&uniform, 0, &f32_uniform_bytes(&params));
		let src_view = src_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let lut_view = lut_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let dst_view = dst_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("quercusrender-present-bg"),
			layout: &pipeline.layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: BindingResource::TextureView(&src_view),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: BindingResource::TextureView(&lut_view),
				},
				wgpu::BindGroupEntry {
					binding: 2,
					resource: uniform.as_entire_binding(),
				},
			],
		});
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-present"),
			});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-present-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&pipeline.pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(dst)
	}

	/// The display LUT pass, cached per destination format.
	///
	/// Samples the working-space `Rgba32Float` source and an `edge³` 3D LUT with
	/// manual trilinear interpolation, so no float-filtering device feature is
	/// required, and writes the result as `format`. Binds the source (0), the
	/// LUT as `texture_3d` (1) and a 48-byte edge/lo/hi uniform (2). Built
	/// inside a validation error scope; a rejected pipeline is never cached.
	///
	/// # Errors
	///
	/// `Error::Failed` when wgpu rejects the new render pipeline.
	fn present_pipeline(&self, format: wgpu::TextureFormat) -> Result<PresentPipeline> {
		let mut cache = lock(&self.present);
		if let Some((_, p)) = cache.iter().find(|(f, _)| *f == format) {
			return Ok(p.clone());
		}
		let layout = self
			.device
			.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some("quercusrender-present-layout"),
				entries: &[
					wgpu::BindGroupLayoutEntry {
						binding: 0,
						visibility: ShaderStages::FRAGMENT,
						ty: BindingType::Texture {
							sample_type: TextureSampleType::Float { filterable: false },
							view_dimension: TextureViewDimension::D2,
							multisampled: false,
						},
						count: None,
					},
					wgpu::BindGroupLayoutEntry {
						binding: 1,
						visibility: ShaderStages::FRAGMENT,
						ty: BindingType::Texture {
							sample_type: TextureSampleType::Float { filterable: false },
							view_dimension: TextureViewDimension::D3,
							multisampled: false,
						},
						count: None,
					},
					wgpu::BindGroupLayoutEntry {
						binding: 2,
						visibility: ShaderStages::FRAGMENT,
						ty: BindingType::Buffer {
							ty: BufferBindingType::Uniform,
							has_dynamic_offset: false,
							min_binding_size: None,
						},
						count: None,
					},
				],
			});
		let vs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-present-vs"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(EFFECT_VS_WGSL)),
			});
		let fs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-present-fs"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(PRESENT_WGSL)),
			});
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("quercusrender-present-pipeline-layout"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});
		let scope = self.device.push_error_scope(ErrorFilter::Validation);
		let pipeline = self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("quercusrender-present"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &vs,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &fs,
					entry_point: Some("main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			});
		if let Some(err) = pollster_block_on(scope.pop()) {
			return Err(Error::Failed(format!(
				"present pipeline validation failed: {err}"
			)));
		}
		let program = PresentPipeline { pipeline, layout };
		cache.push((format, program.clone()));
		Ok(program)
	}

	/// Run the YUV→RGB pass (M5 dependency): three `R16Float` plane
	/// textures (values normalized 0..1) → one `Rgba32Float` destination,
	/// with the BT.601/709/2020 matrix and full/limited range encoded in
	/// `transform`. This is the GPU replacement for the CPU swscale /
	/// `colormath::yuv444p16_to_rgb_f32` conversion; the hardware-decode
	/// import path (M5) feeds it imported planes.
	pub fn run_yuv_to_rgb(
		&self,
		y: u64,
		u: u64,
		v: u64,
		dst: u64,
		transform: &YuvTransform,
	) -> Result<()> {
		let y_tex = lock(&self.textures)
			.get(&y)
			.cloned()
			.ok_or(Error::NotFound)?;
		let u_tex = lock(&self.textures)
			.get(&u)
			.cloned()
			.ok_or(Error::NotFound)?;
		let v_tex = lock(&self.textures)
			.get(&v)
			.cloned()
			.ok_or(Error::NotFound)?;
		let dst_tex = lock(&self.textures)
			.get(&dst)
			.cloned()
			.ok_or(Error::NotFound)?;
		let pipeline = self.yuv_pipeline()?;
		let m = transform.matrix;
		let b = transform.offset;
		let params: [f32; 16] = [
			m[0][0],
			m[0][1],
			m[0][2],
			b[0],
			m[1][0],
			m[1][1],
			m[1][2],
			b[1],
			m[2][0],
			m[2][1],
			m[2][2],
			b[2],
			u_tex.width as f32 / y_tex.width as f32,
			u_tex.height as f32 / y_tex.height as f32,
			0.0,
			0.0,
		];
		let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("quercusrender-yuv-params"),
			size: 64,
			usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
			mapped_at_creation: false,
		});
		self.queue
			.write_buffer(&uniform, 0, &f32_uniform_bytes(&params));
		let y_view = y_tex.texture.create_view(&TextureViewDescriptor::default());
		let u_view = u_tex.texture.create_view(&TextureViewDescriptor::default());
		let v_view = v_tex.texture.create_view(&TextureViewDescriptor::default());
		let dst_view = dst_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("quercusrender-yuv-bg"),
			layout: &pipeline.layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: BindingResource::TextureView(&y_view),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: BindingResource::TextureView(&u_view),
				},
				wgpu::BindGroupEntry {
					binding: 2,
					resource: BindingResource::TextureView(&v_view),
				},
				wgpu::BindGroupEntry {
					binding: 3,
					resource: uniform.as_entire_binding(),
				},
			],
		});
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-yuv"),
			});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-yuv-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&pipeline.pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(())
	}

	/// The three-plane YUV→RGB pipeline, built once and then reused.
	///
	/// Binds three fragment-visible, non-filterable `texture_2d<f32>` planes
	/// (Y, U and V, sampled with `textureLoad`) plus the uniform
	/// [`YuvTransform`] at binding 3. Cached in the `yuv` slot and drawn by
	/// [`GpuContext::run_yuv_to_rgb`].
	fn yuv_pipeline(&self) -> Result<PresentPipeline> {
		let mut cache = lock(&self.yuv);
		if let Some(p) = cache.as_ref() {
			return Ok(p.clone());
		}
		let plane = |binding: u32| wgpu::BindGroupLayoutEntry {
			binding,
			visibility: ShaderStages::FRAGMENT,
			ty: BindingType::Texture {
				sample_type: TextureSampleType::Float { filterable: false },
				view_dimension: TextureViewDimension::D2,
				multisampled: false,
			},
			count: None,
		};
		let layout = self
			.device
			.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some("quercusrender-yuv-layout"),
				entries: &[
					plane(0),
					plane(1),
					plane(2),
					wgpu::BindGroupLayoutEntry {
						binding: 3,
						visibility: ShaderStages::FRAGMENT,
						ty: BindingType::Buffer {
							ty: BufferBindingType::Uniform,
							has_dynamic_offset: false,
							min_binding_size: None,
						},
						count: None,
					},
				],
			});
		let vs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-yuv-vs"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(EFFECT_VS_WGSL)),
			});
		let fs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-yuv-fs"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(YUV_WGSL)),
			});
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("quercusrender-yuv-pipeline-layout"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});
		let scope = self.device.push_error_scope(ErrorFilter::Validation);
		let pipeline = self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("quercusrender-yuv"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &vs,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &fs,
					entry_point: Some("main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format: TextureFormat::Rgba32Float,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			});
		if let Some(err) = pollster_block_on(scope.pop()) {
			return Err(Error::Failed(format!(
				"YUV pipeline validation failed: {err}"
			)));
		}
		let program = PresentPipeline { pipeline, layout };
		*cache = Some(program.clone());
		Ok(program)
	}

	/// Run the planar YUV→RGB pass (M5 zero-copy decode): the imported
	/// luma plane (R8/R16, bind 0) and interleaved chroma plane
	/// (R8G8/R16G16, bind 1) → one `Rgba32Float` destination. The
	/// matrix/range live in `transform`, exactly like
	/// [`GpuContext::run_yuv_to_rgb`].
	pub fn run_planar_yuv_to_rgb(
		&self,
		y: u64,
		uv: u64,
		dst: u64,
		transform: &YuvTransform,
	) -> Result<()> {
		let y_view = self.texture_view(y)?;
		let uv_view = self.texture_view(uv)?;
		let (y_size, uv_size) = {
			let textures = lock(&self.textures);
			let y = textures.get(&y).ok_or(Error::NotFound)?;
			let uv = textures.get(&uv).ok_or(Error::NotFound)?;
			((y.width, y.height), (uv.width, uv.height))
		};
		let pipeline = self.planar_yuv_pipeline()?;
		let m = transform.matrix;
		let b = transform.offset;
		let params: [f32; 16] = [
			m[0][0],
			m[0][1],
			m[0][2],
			b[0],
			m[1][0],
			m[1][1],
			m[1][2],
			b[1],
			m[2][0],
			m[2][1],
			m[2][2],
			b[2],
			uv_size.0 as f32 / y_size.0.max(1) as f32,
			uv_size.1 as f32 / y_size.1.max(1) as f32,
			0.0,
			0.0,
		];
		self.run_planar_views_pass(
			&[&y_view, &uv_view],
			dst,
			&pipeline,
			&params,
			"quercusrender-yuv-planar",
		)
	}

	/// Bind `views` (1–3 plane textures) plus the uniform `params` and
	/// draw the fullscreen planar pass into `dst`. Shared by the legacy
	/// 1:1 semi-planar pass and the descriptor-driven matrix pass; the
	/// pipeline's layout must have one sampled-texture binding per view
	/// (0..n) and the uniform block at binding n.
	fn run_planar_views_pass(
		&self,
		views: &[&wgpu::TextureView],
		dst: u64,
		pipeline: &PresentPipeline,
		params: &[f32],
		label: &str,
	) -> Result<()> {
		let _external = self.external_lock.lock().unwrap_or_else(|e| e.into_inner());
		if self.device_lost() {
			return Err(Error::Failed("wgpu device lost before planar pass".into()));
		}
		if views.is_empty() || views.len() > 3 {
			return Err(Error::Invalid);
		}
		let dst_tex = lock(&self.textures)
			.get(&dst)
			.cloned()
			.ok_or(Error::NotFound)?;
		let mut uniform_bytes = f32_uniform_bytes(params);
		let uniform_size = (uniform_bytes.len() as u64).next_multiple_of(16).max(16);
		uniform_bytes.resize(uniform_size as usize, 0);
		let params_label = format!("{label}-params");
		let uniform = match uniform_buffer_from_bytes(
			&self.device,
			&params_label,
			&uniform_bytes,
			uniform_size,
		) {
			Ok(buffer) => buffer,
			Err(error) => {
				self.mark_device_lost();
				return Err(error);
			}
		};
		let dst_view = dst_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let mut entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::with_capacity(views.len() + 1);
		for (binding, view) in views.iter().enumerate() {
			entries.push(wgpu::BindGroupEntry {
				binding: binding as u32,
				resource: BindingResource::TextureView(view),
			});
		}
		entries.push(wgpu::BindGroupEntry {
			binding: views.len() as u32,
			resource: uniform.as_entire_binding(),
		});
		let bg_label = format!("{label}-bg");
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some(&bg_label),
			layout: &pipeline.layout,
			entries: &entries,
		});
		let pass_label = format!("{label}-pass");
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some(&pass_label),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&pipeline.pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(())
	}

	/// The legacy semi-planar YUV→RGB pass pipeline (built once): two
	/// plane bindings (luma + interleaved chroma) and the uniform block.
	fn planar_yuv_pipeline(&self) -> Result<PresentPipeline> {
		let mut cache = lock(&self.yuv_planar);
		if let Some(p) = cache.as_ref() {
			return Ok(p.clone());
		}
		let program = self.build_planar_pipeline(YUV_PLANAR_WGSL, 2, "quercusrender-yuv-planar")?;
		*cache = Some(program.clone());
		Ok(program)
	}

	/// The descriptor-driven planar-matrix → Rgba32Float pipeline: three
	/// plane bindings (unused slots are bound to a duplicate view) and the
	/// uniform block.
	fn planar_matrix_pipeline(&self) -> Result<PresentPipeline> {
		let mut cache = lock(&self.planar_matrix);
		if let Some(p) = cache.as_ref() {
			return Ok(p.clone());
		}
		let program =
			self.build_planar_pipeline(YUV_PLANAR_MATRIX_WGSL, 3, "quercusrender-planar-matrix")?;
		*cache = Some(program.clone());
		Ok(program)
	}

	/// Build a planar pass pipeline: `texture_count` sampled plane
	/// bindings (0..n) plus the uniform block at binding n, the fixed
	/// fullscreen-triangle vertex stage and one `Rgba32Float` target.
	fn build_planar_pipeline(
		&self,
		shader: &str,
		texture_count: usize,
		label: &str,
	) -> Result<PresentPipeline> {
		let plane = |binding: u32| wgpu::BindGroupLayoutEntry {
			binding,
			visibility: ShaderStages::FRAGMENT,
			ty: BindingType::Texture {
				sample_type: TextureSampleType::Float { filterable: false },
				view_dimension: TextureViewDimension::D2,
				multisampled: false,
			},
			count: None,
		};
		let mut layout_entries = Vec::with_capacity(texture_count + 1);
		for binding in 0..texture_count {
			layout_entries.push(plane(binding as u32));
		}
		layout_entries.push(wgpu::BindGroupLayoutEntry {
			binding: texture_count as u32,
			visibility: ShaderStages::FRAGMENT,
			ty: BindingType::Buffer {
				ty: BufferBindingType::Uniform,
				has_dynamic_offset: false,
				min_binding_size: None,
			},
			count: None,
		});
		let layout_label = format!("{label}-layout");
		let layout = self
			.device
			.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some(&layout_label),
				entries: &layout_entries,
			});
		let vs_label = format!("{label}-vs");
		let vs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some(&vs_label),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(EFFECT_VS_WGSL)),
			});
		let fs_label = format!("{label}-fs");
		let fs = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some(&fs_label),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(shader)),
			});
		let pipeline_layout_label = format!("{label}-pipeline-layout");
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some(&pipeline_layout_label),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});
		let pipeline_label = format!("{label}-pipeline");
		let scope = self.device.push_error_scope(ErrorFilter::Validation);
		let pipeline = self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some(&pipeline_label),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &vs,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &fs,
					entry_point: Some("main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format: TextureFormat::Rgba32Float,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			});
		if let Some(err) = pollster_block_on(scope.pop()) {
			return Err(Error::Failed(format!(
				"{label} pipeline validation failed: {err}"
			)));
		}
		Ok(PresentPipeline { pipeline, layout })
	}

	/// Run the descriptor-driven planar-matrix → RGBA pass with bilinear
	/// target scaling. Handles 1–3 plane bindings: planar YUV (`[Y,U,V]`),
	/// semi-planar YUV (`[Y,UV]`) and interleaved RGB/RGBA; output is
	/// always `Rgba32Float`.
	///
	/// `src_size` is the frame's visible luma size (the first plane's
	/// texture may be larger when a hardware surface pads its allocation);
	/// every plane texture must be at least the descriptor's nominal size
	/// for that luma size (odd chroma dimensions round up). `dst_size` must
	/// equal the destination texture's dimensions. All
	/// sampling is normalized and bilinear (`textureLoad` 4-tap, edge
	/// clamped), so arbitrary source/destination sizes work. `transform`
	/// is applied to the YUV models and ignored for RGB/RGBA (direct
	/// passthrough; RGBA keeps alpha, RGB outputs alpha 1.0).
	pub fn run_planar_matrix_to_rgb(
		&self,
		planes: &[u64],
		dst: u64,
		descriptor: PlanarDescriptor,
		transform: &YuvTransform,
		src_size: (u32, u32),
		dst_size: (u32, u32),
	) -> Result<()> {
		let plane_count = descriptor.plane_count();
		if planes.len() != plane_count
			|| src_size.0 == 0
			|| src_size.1 == 0
			|| dst_size.0 == 0
			|| dst_size.1 == 0
		{
			return Err(Error::Invalid);
		}
		let mut views = Vec::with_capacity(plane_count);
		for (plane, token) in planes.iter().enumerate() {
			let view = self.texture_view(*token)?;
			let (size, format) = {
				let textures = lock(&self.textures);
				let entry = textures.get(token).ok_or(Error::NotFound)?;
				((entry.width, entry.height), entry.format)
			};
			let want_size = descriptor
				.plane_size(plane, src_size.0 as i32, src_size.1 as i32)
				.ok_or(Error::Invalid)?;
			// Hardware surfaces may pad plane allocations beyond the
			// visible chroma size (row/height alignment). The pass only
			// samples the visible region, so accept any texture at least as
			// large as the descriptor's nominal plane; a smaller plane is
			// an error.
			if size.0 < want_size.0 as u32
				|| size.1 < want_size.1 as u32
				|| Some(format) != descriptor.plane_texture_format(plane)
			{
				if env::var_os("OAK_DEBUG_HW").is_some() {
					eprintln!(
						"[planar] plane {plane} mismatch: size={size:?} format={format:?} want={want_size:?} descriptor={:?}",
						descriptor.plane_texture_format(plane),
					);
				}
				return Err(Error::Invalid);
			}
			views.push(view);
		}
		{
			let dst_tex = lock(&self.textures)
				.get(&dst)
				.cloned()
				.ok_or(Error::NotFound)?;
			if (dst_tex.width, dst_tex.height) != dst_size
				|| dst_tex.format != TextureFormat::Rgba32Float
			{
				return Err(Error::Invalid);
			}
		}
		// Bind a duplicate view for unused plane slots; the shader
		// references all three bindings, and distinct view objects avoid
		// any question of binding one view twice.
		let pad_view = if plane_count < 3 {
			Some(self.texture_view(planes[0])?)
		} else {
			None
		};
		let v0 = &views[0];
		let v1 = views.get(1).or(pad_view.as_ref()).unwrap_or(v0);
		let v2 = views.get(2).or(pad_view.as_ref()).unwrap_or(v0);
		let mode = match descriptor.model {
			crate::texture::ColorModel::YuvPlanar => 0.0,
			crate::texture::ColorModel::YuvSemiPlanar => 1.0,
			crate::texture::ColorModel::Rgb => 2.0,
			crate::texture::ColorModel::Rgba => 3.0,
		};
		let m = transform.matrix;
		let b = transform.offset;
		let uv_scale = descriptor.uv_scale();
		let chroma_size = descriptor
			.plane_size(1, src_size.0 as i32, src_size.1 as i32)
			.unwrap_or((src_size.0 as i32, src_size.1 as i32));
		let params: [f32; 24] = [
			m[0][0],
			m[0][1],
			m[0][2],
			b[0],
			m[1][0],
			m[1][1],
			m[1][2],
			b[1],
			m[2][0],
			m[2][1],
			m[2][2],
			b[2],
			uv_scale.0,
			uv_scale.1,
			mode,
			0.0,
			src_size.0 as f32 / dst_size.0 as f32,
			src_size.1 as f32 / dst_size.1 as f32,
			0.0,
			0.0,
			src_size.0 as f32,
			src_size.1 as f32,
			chroma_size.0 as f32,
			chroma_size.1 as f32,
		];
		let pipeline = self.planar_matrix_pipeline()?;
		self.run_planar_views_pass(
			&[v0, v1, v2],
			dst,
			&pipeline,
			&params,
			"quercusrender-planar-matrix",
		)
	}

	/// Native-size (1:1) convenience twin of
	/// [`GpuContext::run_planar_matrix_to_rgb`]: the source size is read
	/// from the first plane texture and the destination size from `dst`.
	///
	/// For hardware imports whose plane textures are padded beyond the
	/// visible frame, call [`GpuContext::run_planar_matrix_to_rgb`] with
	/// the frame's visible size instead (the descriptor twin then samples
	/// only the visible top-left region).
	pub fn run_planar_matrix_to_rgb_native(
		&self,
		planes: &[u64],
		dst: u64,
		descriptor: PlanarDescriptor,
		transform: &YuvTransform,
	) -> Result<()> {
		let first = planes.first().copied().ok_or(Error::Invalid)?;
		let (width, height) = self.texture_size(first).ok_or(Error::NotFound)?;
		let (dst_width, dst_height) = self.texture_size(dst).ok_or(Error::NotFound)?;
		self.run_planar_matrix_to_rgb(
			planes,
			dst,
			descriptor,
			transform,
			(width, height),
			(dst_width, dst_height),
		)
	}

	/// Legacy scaled adapter for the two-plane semi-planar layouts: the
	/// storage is inferred from the luma texture format and the
	/// subsampling from the actual UV/luma texture dimensions, so 4:2:2
	/// (`uv width = src width / 2`, `uv height = src height`) and 4:2:0
	/// both map without a chroma-location hint.
	pub fn run_planar_yuv_to_rgb_scaled(
		&self,
		y: u64,
		uv: u64,
		dst: u64,
		transform: &YuvTransform,
		src_size: (u32, u32),
		dst_size: (u32, u32),
	) -> Result<()> {
		if src_size.0 == 0 || src_size.1 == 0 || dst_size.0 == 0 || dst_size.1 == 0 {
			return Err(Error::Invalid);
		}
		let descriptor = self.infer_semi_planar_descriptor(y, uv, Some(src_size))?;
		self.run_planar_matrix_to_rgb(&[y, uv], dst, descriptor, transform, src_size, dst_size)
	}

	/// Infer a semi-planar descriptor from an imported luma/chroma pair
	/// (legacy adapters and raw uploads). `visible_size` is the luma size
	/// the pass will sample (the texture dimensions may be larger due to
	/// hardware plane padding); `None` means the luma texture's own size.
	///
	/// Candidate subsamplings are tried from least to most subsampled and
	/// the first whose nominal chroma plane fits inside the actual chroma
	/// texture wins, so aligned/padded allocations (and odd visible
	/// dimensions, which round chroma up) infer correctly.
	fn infer_semi_planar_descriptor(
		&self,
		y: u64,
		uv: u64,
		visible_size: Option<(u32, u32)>,
	) -> Result<PlanarDescriptor> {
		let (y_size, y_format, uv_size) = {
			let textures = lock(&self.textures);
			let y = textures.get(&y).ok_or(Error::NotFound)?;
			let uv = textures.get(&uv).ok_or(Error::NotFound)?;
			((y.width, y.height), y.format, (uv.width, uv.height))
		};
		let visible = visible_size.unwrap_or(y_size);
		if visible.0 == 0 || visible.1 == 0 || y_size.0 < visible.0 || y_size.1 < visible.1 {
			return Err(Error::Invalid);
		}
		let subsampling = [Subsampling::Cs444, Subsampling::Cs422, Subsampling::Cs420]
			.into_iter()
			.find(|candidate| {
				let (width, height) = candidate.chroma_size(visible.0 as i32, visible.1 as i32);
				width > 0 && height > 0 && uv_size.0 >= width as u32 && uv_size.1 >= height as u32
			})
			.ok_or(Error::Invalid)?;
		let storage = PlaneStorage::from_texture_format(y_format).ok_or(Error::Invalid)?;
		Ok(PlanarDescriptor::yuv_semi_planar(subsampling, storage))
	}

	/// Clear a texture to transparent black on the GPU (no CPU transfer):
	/// the graph compositor's starting accumulator and single-sided
	/// transition sides.
	pub fn clear_texture(&self, token: u64) -> Result<()> {
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let view = entry.texture.create_view(&TextureViewDescriptor::default());
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-clear"),
			});
		{
			let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-clear-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(())
	}

	/// Upload a tightly packed CPU [`Frame`] into a registry texture.
	///
	/// `frame` must be `PixelFormat::F32` and match the texture's width/height
	/// exactly; `frame.data` must hold at least `linesize_bytes() * height`
	/// bytes, and that prefix is what is uploaded (the frame's line size becomes
	/// the copy row pitch). Counted in [`gpu_transfer_counters`] as a CPU→GPU
	/// transfer.
	///
	/// # Errors
	///
	/// `Error::NotFound` for an unknown `token`; `Error::Invalid` when the frame
	/// is not F32, has the wrong size, or is shorter than its line size implies.
	pub fn upload(&self, token: u64, frame: &Frame) -> Result<()> {
		GPU_UPLOADS.fetch_add(1, Ordering::Relaxed);
		if frame.format != PixelFormat::F32 {
			return Err(Error::Invalid);
		}
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let w = entry.width as usize;
		let h = entry.height as usize;
		if frame.width != w as i32 || frame.height != h as i32 {
			return Err(Error::Invalid);
		}
		let linesize = frame.linesize_bytes();
		if frame.data.len() < linesize * h {
			return Err(Error::Invalid);
		}
		self.queue.write_texture(
			wgpu::TexelCopyTextureInfo {
				texture: &entry.texture,
				mip_level: 0,
				origin: Origin3d::ZERO,
				aspect: TextureAspect::All,
			},
			&frame.data[..linesize * h],
			wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(linesize as u32),
				rows_per_image: None,
			},
			wgpu::Extent3d {
				width: w as u32,
				height: h as u32,
				depth_or_array_layers: 1,
			},
		);
		Ok(())
	}

	/// Download a texture into a CPU frame. Counted as a GPU→CPU transfer
	/// ([`gpu_transfer_counters`]); the playback path must never take it.
	///
	/// Format-aware (M2): `Rgba32Float` copies raw, `Rgba16Float` (the
	/// present target) converts half→f32. Other formats are rejected —
	/// explicit readback boundaries only ever meet these two.
	pub fn download(&self, token: u64) -> Result<Frame> {
		if self.device_lost() {
			return Err(Error::Failed(
				"wgpu device lost; texture download unavailable".into(),
			));
		}
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let _external = self.external_lock.lock().unwrap_or_else(|e| e.into_inner());
		download_texture(
			&self.device,
			&self.queue,
			&entry.texture,
			entry.format,
			entry.width,
			entry.height,
		)
	}

	/// Download a texture this context already owns (not a registry token)
	/// into a CPU [`Frame`] for an explicit consumer.
	///
	/// `texture`, `format`, `width` and `height` must describe the same
	/// resource. Only `Rgba32Float` (copied raw) and `Rgba16Float` (converted
	/// half→f32) are supported, so the frame is always `PixelFormat::F32`. The
	/// external lifecycle lock is held for the whole copy, keeping an external
	/// producer (e.g. a CUDA writer) from touching the texture mid-read.
	///
	/// # Errors
	///
	/// `Error::Failed` on a lost device, an unsupported format, or a map
	/// timeout/failure.
	pub fn download_shared_texture(
		&self,
		texture: &wgpu::Texture,
		format: wgpu::TextureFormat,
		width: u32,
		height: u32,
	) -> Result<Frame> {
		if self.device_lost() {
			return Err(Error::Failed(
				"wgpu device lost; texture download unavailable".into(),
			));
		}
		let _external = self.external_lock.lock().unwrap_or_else(|e| e.into_inner());
		download_texture(&self.device, &self.queue, texture, format, width, height)
	}

	/// Asynchronous variant of [`GpuContext::download`]: the GPU wait/map
	/// runs on a dedicated readback thread, so the render thread can submit
	/// the next frame while this one is being mapped.
	pub fn download_async(&self, token: u64) -> Result<FrameReadback> {
		if self.device_lost() {
			return Err(Error::Failed(
				"wgpu device lost; texture download unavailable".into(),
			));
		}
		let entry = lock(&self.textures)
			.get(&token)
			.cloned()
			.ok_or(Error::NotFound)?;
		let guard = entry.keep_alive.clone();
		let (tx, rx) = std::sync::mpsc::channel();
		readback_worker()
			.send(ReadbackJob {
				device: self.device.clone(),
				queue: self.queue.clone(),
				texture: entry.texture,
				format: entry.format,
				width: entry.width,
				height: entry.height,
				external_lock: self.external_lock.clone(),
				tx,
			})
			.map_err(|_| Error::Failed("readback worker stopped".into()))?;
		Ok(FrameReadback { rx, _guard: guard })
	}

	/// Blit texture → texture. `processor` selects the color-managed path,
	/// which is **deferred**: the OCIO→WGSL shader generation is not part
	/// of this pass (see README §4), so a `Some` processor returns
	/// `Error::Failed` and the plain-copy WGSL pipeline is used for `None`.
	pub fn blit(
		&self,
		src: u64,
		dst: u64,
		processor: Option<&color::ColorProcessor>,
	) -> Result<()> {
		if processor.is_some() {
			return Err(Error::Failed(
				"color-managed GPU blit deferred: OCIO→WGSL generation not in this pass".into(),
			));
		}
		let (src_tex, dst_tex) = {
			let reg = lock(&self.textures);
			let s = reg.get(&src).ok_or(Error::NotFound)?;
			let d = reg.get(&dst).ok_or(Error::NotFound)?;
			(s.texture.clone(), d.texture.clone())
		};

		let pipeline = {
			let mut cache = lock(&self.blit);
			if cache.is_none() {
				*cache = Some(self.create_blit_pipeline()?);
			}
			cache.clone().unwrap()
		};

		let src_view = src_tex.create_view(&TextureViewDescriptor::default());
		let dst_view = dst_tex.create_view(&TextureViewDescriptor::default());
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("quercusrender-blit-bg"),
			layout: &self.blit_bind_group_layout(),
			entries: &[wgpu::BindGroupEntry {
				binding: 0,
				resource: BindingResource::TextureView(&src_view),
			}],
		});

		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-blit"),
			});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-blit-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(())
	}

	/// The bind-group layout shared by both copy blit pipelines.
	///
	/// One fragment-visible `texture_2d<f32>` at binding 0, declared
	/// non-filterable to match the shader's `textureLoad`. Recreated on each
	/// call (there is no cache); used by [`GpuContext::blit`] and
	/// [`GpuContext::convert_to_rgba16f`].
	fn blit_bind_group_layout(&self) -> wgpu::BindGroupLayout {
		self.device
			.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some("quercusrender-blit-layout"),
				entries: &[wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: ShaderStages::FRAGMENT,
					ty: BindingType::Texture {
						sample_type: TextureSampleType::Float { filterable: false },
						view_dimension: TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				}],
			})
	}

	/// GPU->GPU copy of `src` into a new `Rgba16Float` texture. The host
	/// presenter's surface pipeline samples with a filterable layout, which
	/// `Rgba32Float` does not satisfy on devices without FLOAT32_FILTERABLE;
	/// half-float is also the 10-bit display format. Returns the new token.
	pub fn convert_to_rgba16f(&self, src: u64) -> Result<u64> {
		let (src_tex, width, height) = {
			let reg = lock(&self.textures);
			let entry = reg.get(&src).ok_or(Error::NotFound)?;
			(entry.texture.clone(), entry.width, entry.height)
		};
		let dst = self.create_texture_format(
			width as i32,
			height as i32,
			1,
			TextureFormat::Rgba16Float,
			TextureUsages::TEXTURE_BINDING
				| TextureUsages::RENDER_ATTACHMENT
				| TextureUsages::COPY_DST
				| TextureUsages::COPY_SRC,
		)?;
		let dst_tex = {
			let reg = lock(&self.textures);
			reg.get(&dst)
				.map(|e| e.texture.clone())
				.ok_or(Error::NotFound)?
		};
		let pipeline = {
			let mut cache = lock(&self.blit16);
			if cache.is_none() {
				*cache = Some(self.create_blit_pipeline16()?);
			}
			cache.clone().unwrap()
		};
		let src_view = src_tex.create_view(&TextureViewDescriptor::default());
		let dst_view = dst_tex.create_view(&TextureViewDescriptor::default());
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("quercusrender-blit16-bg"),
			layout: &self.blit_bind_group_layout(),
			entries: &[wgpu::BindGroupEntry {
				binding: 0,
				resource: BindingResource::TextureView(&src_view),
			}],
		});
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-blit16"),
			});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-blit16-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(dst)
	}

	/// Build the copy blit pipeline whose color target is `Rgba16Float`.
	///
	/// Same shader, bind-group layout and vertex stage as
	/// [`GpuContext::create_blit_pipeline`]; only the render-target format
	/// differs. Used by [`GpuContext::convert_to_rgba16f`] for the host
	/// presenter, whose surface path needs a filterable format. Cached in the
	/// `blit16` slot of the context.
	fn create_blit_pipeline16(&self) -> Result<wgpu::RenderPipeline> {
		// Recreate with the half-float target; the layout/vertex/fragment
		// modules are identical to the Rgba32 blit.
		let shader = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-blit16-shader"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(BLIT_WGSL)),
			});
		let layout = self.blit_bind_group_layout();
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("quercusrender-blit16-layout"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});
		Ok(self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("quercusrender-blit16"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &shader,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &shader,
					entry_point: Some("fs_main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format: TextureFormat::Rgba16Float,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			}))
	}

	/// Build the `Rgba32Float` copy blit pipeline used by [`GpuContext::blit`].
	///
	/// The fragment shader samples the single bound source texture with
	/// `textureLoad` (nearest texel, no filtering) and writes it unchanged, so
	/// the pass is a straight GPU→GPU copy. The bind group comes from
	/// [`GpuContext::blit_bind_group_layout`]; [`GpuContext::blit`] caches the
	/// pipeline in the `blit` slot, so it is built once per context.
	fn create_blit_pipeline(&self) -> Result<wgpu::RenderPipeline> {
		let shader = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-blit-shader"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(BLIT_WGSL)),
			});
		let layout = self.blit_bind_group_layout();
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("quercusrender-blit-layout"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});
		let pipeline = self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("quercusrender-blit"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &shader,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &shader,
					entry_point: Some("fs_main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format: TextureFormat::Rgba32Float,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			});
		Ok(pipeline)
	}

	/// The process-wide shared context (lazy; `None` when no adapter is
	/// available — callers then take the CPU fallback). The effect/montage
	/// evaluation path renders through this context; the backend choice
	/// follows the user's `GraphicsBackend` config (`QUERCUS_RENDER_BACKEND`
	/// overrides). `DisplayRenderer::init` adopts it too, so a process
	/// owns exactly one wgpu device.
	///
	/// The host may install a context first ([`GpuContext::install_shared`],
	/// M2): the app hands over the gpui device so the render thread and
	/// the presenter share it. Once decided (installed or lazily created)
	/// the slot is fixed for the process.
	pub fn shared() -> Option<Arc<GpuContext>> {
		let mut slot = lock(shared_slot());
		if !slot.decided {
			slot.ctx = Self::create(BackendKind::from_user_config());
			slot.decided = true;
		}
		slot.ctx.clone()
	}

	/// Eagerly create the engine-owned shared context with an explicit
	/// backend preference, instead of the user config [`GpuContext::shared`]
	/// would read. This is the entry point for hosts that cannot hand over a
	/// wgpu device (a pure C ABI consumer): the engine creates and owns the
	/// device exactly as the lazy path would.
	///
	/// Only the first initialization chooses the backend: when the slot is
	/// already decided (an earlier init, render, or adopt), this is a no-op.
	/// Does not set the host-installed flag — the hardware-surface import
	/// gate keeps its lazy-path behavior; [`GpuContext::mark_host_context`]
	/// remains the way to declare the device as the host's. Returns whether
	/// a GPU context exists afterwards (`false` means rendering stays on the
	/// CPU path; requesting [`BackendKind::Cpu`] therefore always returns
	/// `false`).
	pub fn init_shared(kind: BackendKind) -> bool {
		let mut slot = lock(shared_slot());
		if !slot.decided {
			slot.ctx = Self::create(kind);
			slot.decided = true;
		}
		slot.ctx.is_some()
	}

	/// Install (or clear) the process-wide shared context (M2). Called by
	/// the app before the first render with the UI's adopted device, so
	/// the pipeline backend renders on the presenter's device.
	///
	/// **Timing guard**: an engine-created context that has not touched
	/// the GPU yet is *replaced* — an early [`GpuContext::shared`] call
	/// (a thumbnail, a task) must not silently cost the zero-copy present
	/// path. Once the incumbent has created any texture or pipeline the
	/// replacement is refused (`false`); the caller degrades to the
	/// single staging readback. Also `false` when an adopted context is
	/// already installed.
	pub fn install_shared(ctx: Option<Arc<GpuContext>>) -> bool {
		let mut slot = lock(shared_slot());
		if slot.decided {
			let replaceable = ctx.is_some()
				&& slot
					.ctx
					.as_ref()
					.is_some_and(|c| !c.is_adopted() && !c.is_used());
			if !replaceable {
				return false;
			}
		}
		slot.host_installed = ctx.is_some();
		slot.ctx = ctx;
		slot.decided = true;
		true
	}

	/// True when the host installed the shared device (the app's UI
	/// context). The M5 hardware-surface import requires this: a lazily
	/// created engine context belongs to a worker/CLI/test and must not
	/// pull decode onto the GPU.
	pub fn host_gpu_installed() -> bool {
		lock(shared_slot()).host_installed
	}

	/// Declare the process's shared context as the HOST's render device
	/// (M5), creating it on first use if the engine has not yet.
	///
	/// On Linux/FreeBSD the app adopts the window's wgpu device via
	/// [`GpuContext::install_shared`]; on macOS/Windows gpui does not
	/// expose that device, so the engine-created shared context *is* the
	/// app's render device and must be marked here — otherwise the decode
	/// import's host gate would make the D3D11VA/VideoToolbox rows dead
	/// code (M5 audit). Worker, CLI and test processes never call this,
	/// which is what keeps their decode on the CPU staging path.
	pub fn mark_host_context() -> bool {
		let mut slot = lock(shared_slot());
		if !slot.decided {
			slot.ctx = Self::create(BackendKind::from_user_config());
			slot.decided = true;
		}
		slot.host_installed = slot.ctx.is_some();
		slot.ctx.is_some()
	}

	/// True when the app installed the shared context (as opposed to the
	/// engine lazily creating one from user config). Tests/UI use this to
	/// tell "the presenter's device" from "a private device".
	pub fn shared_is_installed() -> bool {
		let slot = lock(shared_slot());
		slot.decided && slot.ctx.is_some()
	}

	/// True when the device can linear-sample Rgba32Float textures
	/// (FLOAT32_FILTERABLE). Effect shaders use a filtering sampler when
	/// true, nearest otherwise.
	pub fn is_filterable(&self) -> bool {
		self.filterable
	}

	/// The context's shared 1×1 transparent placeholder texture, created
	/// on first use: unconnected effect inputs bind it (C++ binds texture
	/// id 0 — an empty texture — the same way).
	pub fn placeholder_texture(&self) -> Result<u64> {
		let mut slot = lock(&self.placeholder);
		if let Some(token) = *slot {
			return Ok(token);
		}
		let token = self.create_texture(1, 1)?;
		let mut pod = VideoParamsPod::default();
		pod.width = 1;
		pod.height = 1;
		pod.format = PixelFormat::F32 as i32;
		let mut frame = Frame::new();
		frame.set_video_params(pod);
		frame.allocate();
		self.upload(token, &frame)?;
		*slot = Some(token);
		Ok(token)
	}

	/// Compile (or fetch from the cache) an effect pass: the translated
	/// fragment WGSL (`shaderfx::translate` output, entry point `main`)
	/// paired with the fixed fullscreen-triangle vertex stage. `key`
	/// identifies the shader program in the context cache (include the
	/// filtering mode when it varies for the same shader). Pipeline
	/// creation runs under a validation error scope so a bad shader is a
	/// fallible result, not a device loss.
	pub fn compile_shader_pass(
		&self,
		key: &str,
		wgsl: &str,
		texture_count: u32,
		has_uniforms: bool,
		filtering: bool,
	) -> Result<Arc<ShaderProgram>> {
		self.compile_shader_pass_format(
			key,
			wgsl,
			texture_count,
			has_uniforms,
			filtering,
			TextureFormat::Rgba32Float,
		)
	}

	/// [`GpuContext::compile_shader_pass`] with an explicit render-target
	/// format. Effect passes target `Rgba32Float`; the export pack pass targets
	/// `Rgba8Unorm` / `Rgba16Unorm` so the GPU can hand the encoder a packed
	/// buffer instead of float RGBA.
	pub fn compile_shader_pass_format(
		&self,
		key: &str,
		wgsl: &str,
		texture_count: u32,
		has_uniforms: bool,
		filtering: bool,
		target_format: wgpu::TextureFormat,
	) -> Result<Arc<ShaderProgram>> {
		self.used.store(true, Ordering::Release);
		let key = format!("{key}|{target_format:?}");
		if let Some(p) = lock(&self.programs).get(&key) {
			return Ok(p.clone());
		}

		// Bind group layout, mirroring shaderfx's binding assignment:
		// binding 0 = the uniform block (when present), then each input
		// texture as a (texture, sampler) pair.
		let mut entries = Vec::new();
		if has_uniforms {
			entries.push(wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: ShaderStages::FRAGMENT,
				ty: BindingType::Buffer {
					ty: BufferBindingType::Uniform,
					has_dynamic_offset: false,
					min_binding_size: None,
				},
				count: None,
			});
		}
		let sample_type = TextureSampleType::Float {
			filterable: self.filterable && filtering,
		};
		for i in 0..texture_count {
			entries.push(wgpu::BindGroupLayoutEntry {
				binding: 1 + 2 * i,
				visibility: ShaderStages::FRAGMENT,
				ty: BindingType::Texture {
					sample_type,
					view_dimension: TextureViewDimension::D2,
					multisampled: false,
				},
				count: None,
			});
			entries.push(wgpu::BindGroupLayoutEntry {
				binding: 2 + 2 * i,
				visibility: ShaderStages::FRAGMENT,
				ty: BindingType::Sampler(if self.filterable && filtering {
					SamplerBindingType::Filtering
				} else {
					SamplerBindingType::NonFiltering
				}),
				count: None,
			});
		}
		let layout = self
			.device
			.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
				label: Some("quercusrender-fx-layout"),
				entries: &entries,
			});

		let vs_module = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-fx-vs"),
				source: ShaderSource::Wgsl(borrow::Cow::Borrowed(EFFECT_VS_WGSL)),
			});
		let fs_module = self
			.device
			.create_shader_module(wgpu::ShaderModuleDescriptor {
				label: Some("quercusrender-fx-fs"),
				source: ShaderSource::Wgsl(borrow::Cow::Owned(wgsl.to_string())),
			});
		let pipeline_layout = self
			.device
			.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("quercusrender-fx-pipeline-layout"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			});

		let scope = self.device.push_error_scope(ErrorFilter::Validation);
		let pipeline = self
			.device
			.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("quercusrender-fx"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &vs_module,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: PrimitiveState::default(),
				depth_stencil: None,
				multisample: MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &fs_module,
					entry_point: Some("main"),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format: target_format,
						blend: None,
						write_mask: ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			});
		if let Some(err) = pollster_block_on(scope.pop()) {
			return Err(Error::Failed(format!(
				"effect pipeline validation failed: {err}"
			)));
		}

		let program = Arc::new(ShaderProgram {
			pipeline,
			layout,
			texture_count,
			has_uniforms,
			filtering,
		});
		lock(&self.programs).insert(key.to_string(), program.clone());
		Ok(program)
	}

	/// Run one effect pass: fragment-shade `dst` from `textures[0]` (the
	/// main input) plus any extra input textures, with `uniforms` as the
	/// packed std140 block (see `quercus_render::shaderfx::pack_uniforms`).
	pub fn run_shader_pass(
		&self,
		program: &ShaderProgram,
		uniforms: &[u8],
		textures: &[u64],
		dst: u64,
	) -> Result<()> {
		if textures.len() != program.texture_count as usize {
			return Err(Error::Invalid);
		}
		let _external = self.external_lock.lock().unwrap_or_else(|e| e.into_inner());
		let dst_tex = lock(&self.textures)
			.get(&dst)
			.cloned()
			.ok_or(Error::NotFound)?;

		let uniform_buffer = if program.has_uniforms {
			let size = uniforms.len().max(16) as u64;
			let buffer = uniform_buffer_from_bytes(
				&self.device,
				"quercusrender-fx-uniforms",
				uniforms,
				size,
			)?;
			Some(buffer)
		} else {
			None
		};

		let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("quercusrender-fx-sampler"),
			mag_filter: if self.filterable && program.filtering {
				FilterMode::Linear
			} else {
				FilterMode::Nearest
			},
			min_filter: if self.filterable && program.filtering {
				FilterMode::Linear
			} else {
				FilterMode::Nearest
			},
			address_mode_u: AddressMode::ClampToEdge,
			address_mode_v: AddressMode::ClampToEdge,
			..Default::default()
		});

		let mut bg_entries: Vec<wgpu::BindGroupEntry> = Vec::new();
		if let Some(buffer) = &uniform_buffer {
			bg_entries.push(wgpu::BindGroupEntry {
				binding: 0,
				resource: buffer.as_entire_binding(),
			});
		}
		// Texture views must outlive the bind group creation.
		let views: Vec<wgpu::TextureView> = textures
			.iter()
			.map(|t| {
				let reg = lock(&self.textures);
				let tex = reg.get(t).ok_or(Error::NotFound)?;
				Ok(tex.texture.create_view(&TextureViewDescriptor::default()))
			})
			.collect::<Result<Vec<_>>>()?;
		for (i, view) in views.iter().enumerate() {
			bg_entries.push(wgpu::BindGroupEntry {
				binding: 1 + 2 * i as u32,
				resource: BindingResource::TextureView(view),
			});
			bg_entries.push(wgpu::BindGroupEntry {
				binding: 2 + 2 * i as u32,
				resource: BindingResource::Sampler(&sampler),
			});
		}
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("quercusrender-fx-bg"),
			layout: &program.layout,
			entries: &bg_entries,
		});

		let dst_view = dst_tex
			.texture
			.create_view(&TextureViewDescriptor::default());
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("quercusrender-fx"),
			});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("quercusrender-fx-pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &dst_view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: LoadOp::Clear(Color::TRANSPARENT),
						store: StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(&program.pipeline);
			pass.set_bind_group(0, &bind_group, &[]);
			pass.draw(0..3, 0..1);
		}
		self.queue.submit(Some(encoder.finish()));
		Ok(())
	}

	/// Run the export output-pack pass: sample `src` (working-space F32
	/// RGBA), apply the working→delivery gamut matrix and the delivery
	/// transfer, and write packed RGBA to `dst`.
	///
	/// `dst` must be `Rgba8Unorm` (8-bit delivery) or `Rgba16Unorm`
	/// (10/12/16-bit delivery). The engine's internal pipeline stays F32 +
	/// ACEScg: this is the export boundary's GPU packing step, replacing the
	/// CPU row-wise color transform plus the F32→u8 conversion.
	pub fn pack_output(&self, src: u64, dst: u64, params: &PackParams) -> Result<()> {
		let format = {
			let reg = lock(&self.textures);
			reg.get(&dst).ok_or(Error::NotFound)?.format
		};
		if format != TextureFormat::Rgba8Unorm
			&& format != TextureFormat::Rgba16Unorm
			&& format != TextureFormat::Rgba16Float
			&& format != TextureFormat::Rgba32Float
		{
			return Err(Error::Failed(format!(
				"pack_output: unsupported destination format {format:?}"
			)));
		}
		let program = self.compile_shader_pass_format(
			"quercus-export-pack",
			PACK_WGSL,
			1,
			true,
			false,
			format,
		)?;
		let m = params.matrix;
		let mut uniforms: Vec<u8> = Vec::with_capacity(64);
		for row in &m {
			for col in row {
				uniforms.extend_from_slice(&col.to_le_bytes());
			}
			uniforms.extend_from_slice(&0.0f32.to_le_bytes());
		}
		uniforms.extend_from_slice(&(params.transfer as f32).to_le_bytes());
		uniforms.extend_from_slice(&[0u8; 12]);
		self.run_shader_pass(&program, &uniforms, &[src], dst)
	}

	/// Pack `src` (F32 working-space RGBA) into an 8-bit or 16-bit
	/// (10/12-bit delivery) RGBA frame and read it back.
	///
	/// The destination texture is transient: created, packed, downloaded and
	/// destroyed. `bit_depth` <= 8 selects `Rgba8Unorm`; anything above
	/// selects `Rgba16Unorm`, which carries 10-bit delivery values without
	/// the float readback.
	pub fn pack_output_cpu(
		&self,
		src: u64,
		width: i32,
		height: i32,
		bit_depth: u8,
		params: &PackParams,
	) -> Result<Frame> {
		// 8-bit delivery packs into `Rgba8Unorm` (4 bytes/px).
		//
		// Higher bit depths pack into `Rgba32Float`, NOT a half/normalized
		// 16-bit container: `Rgba16Unorm` is not a render target on backends
		// without TEXTURE_FORMAT_16BIT_NORM (Apple Metal) and `Rgba16Float`
		// only has ~11 mantissa bits — measured at 0.47 of a 10-bit code, and
		// outright lossy for 12/16-bit delivery. Float keeps the packed values
		// exact; the encoder converts them to 16-bit for swscale, so the only
		// quantization left is the final N-bit one.
		let format = if bit_depth > 8 {
			TextureFormat::Rgba32Float
		} else {
			TextureFormat::Rgba8Unorm
		};
		let dst = self.create_texture_format(
			width,
			height,
			1,
			format,
			TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
		)?;
		let result = self
			.pack_output(src, dst, params)
			.and_then(|()| self.download(dst));
		self.destroy_texture(dst);
		result
	}
}

/// Parameters of the export output-pack pass
/// ([`GpuContext::pack_output`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PackParams {
	/// Row-major 3×3 matrix: working-space primaries → delivery primaries.
	pub matrix: [[f32; 3]; 3],
	/// Delivery transfer id (see [`PACK_TRANSFER_SRGB`] & co).
	pub transfer: u32,
}

/// sRGB piecewise transfer (matches `colormath::OutputTransfer::Srgb`).
pub const PACK_TRANSFER_SRGB: u32 = 0;
/// Pure gamma 2.2 transfer (`colormath::OutputTransfer::Gamma22`).
pub const PACK_TRANSFER_GAMMA22: u32 = 1;
/// SMPTE ST 2084 PQ (`colormath::OutputTransfer::Pq`).
pub const PACK_TRANSFER_PQ: u32 = 2;
/// ARIB STD-B67 HLG (`colormath::OutputTransfer::Hlg`).
pub const PACK_TRANSFER_HLG: u32 = 3;

/// The export pack pass: F32 RGBA in, delivery-encoded packed RGBA out.
///
/// The transfer constants mirror `quercus_core::colormath` (PQ_M1..C3, HLG
/// constants); the output is clamped to [0, 1] and scaled by the unorm target,
/// exactly like the CPU path's normalized float output.
const PACK_WGSL: &str = r#"
struct Params {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // x = transfer id (0 sRGB, 1 gamma 2.2, 2 PQ, 3 HLG).
    info: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

fn srgb_oetf(x: f32) -> f32 {
    if (x <= 0.0031308) { return 12.92 * x; }
    return 1.055 * pow(x, 1.0 / 2.4) - 0.055;
}

fn pq_oetf(x: f32) -> f32 {
    let m1 = 2610.0 / 16384.0;
    let m2 = (2523.0 / 4096.0) * 128.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = (2413.0 / 4096.0) * 32.0;
    let c3 = (2392.0 / 4096.0) * 32.0;
    let y = pow(x, m1);
    return pow((c1 + c2 * y) / (1.0 + c3 * y), m2);
}

fn hlg_oetf(x: f32) -> f32 {
    let a = 0.17883277;
    let b = 1.0 - 4.0 * a;
    let c = 0.5 - a * log(4.0 * a);
    if (x <= 1.0 / 12.0) { return sqrt(3.0 * x); }
    return a * log(12.0 * x - b) + c;
}

fn encode(x: f32, id: i32) -> f32 {
    let v = clamp(x, 0.0, 1.0);
    if (id == 1) { return pow(v, 1.0 / 2.2); }
    if (id == 2) { return pq_oetf(v); }
    if (id == 3) { return hlg_oetf(v); }
    return srgb_oetf(v);
}

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(frag.xy), 0);
    let lin = vec3<f32>(
        u.m0.x * c.r + u.m0.y * c.g + u.m0.z * c.b,
        u.m1.x * c.r + u.m1.y * c.g + u.m1.z * c.b,
        u.m2.x * c.r + u.m2.y * c.g + u.m2.z * c.b
    );
    let id = i32(u.info.x + 0.5);
    return vec4<f32>(
        encode(lin.r, id),
        encode(lin.g, id),
        encode(lin.b, id),
        clamp(c.a, 0.0, 1.0)
    );
}
"#;

/// A compiled effect pass: the translated fragment WGSL paired with the
/// fixed fullscreen-triangle vertex stage, plus its bind group layout.
pub struct ShaderProgram {
	pipeline: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
	/// Input texture count (each binds a (texture, sampler) pair).
	pub texture_count: u32,
	/// Whether the shader declares the uniform block (binding 0).
	pub has_uniforms: bool,
	/// Whether the input samplers filter (subject to FLOAT32_FILTERABLE).
	pub filtering: bool,
}

/// The compiled display-LUT pass (M2).
#[derive(Clone)]
struct PresentPipeline {
	pipeline: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
}

/// The YUV→RGB matrix/offset for [`GpuContext::run_yuv_to_rgb`], derived
/// from the same (Kr, Kb) coefficients and range expansion as
/// [`crate::colormath::yuv444p16_to_rgb_f32`]. Inputs are R16Unorm
/// plane textures (normalized 0..1); `rgb = M·yuv + b`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvTransform {
	/// Row-major 3×3 matrix applied to `(y, u, v)`.
	pub matrix: [[f32; 3]; 3],
	/// Per-channel offset added after the matrix.
	pub offset: [f32; 3],
}

impl YuvTransform {
	/// Build the transform for a luma matrix and range. Inputs are
	/// normalized 16-bit code values (code / 65535).
	pub fn from_matrix(matrix: colormath::YuvMatrix, full_range: bool) -> Self {
		Self::from_matrix_depth(matrix, full_range, 16)
	}

	/// Build the transform for a luma matrix, range and source bit depth.
	/// Plane inputs are normalized code values (code / (2^bits - 1));
	/// the limited-range expansion uses the depth's own code limits
	/// (16..235 luma, 16..240 chroma scaled to the depth).
	pub fn from_matrix_depth(
		matrix: colormath::YuvMatrix,
		full_range: bool,
		bit_depth: u32,
	) -> Self {
		let (kr, kb) = matrix.kr_kb();
		let kg = 1.0 - kr - kb;
		let max = ((1u32 << bit_depth) - 1) as f32;
		// Code limits for this depth. `scale` is the 8-bit-code shift the
		// CPU reference uses (depth 16 = 8-bit codes << 8, so 16 stays
		// bit-identical with `colormath::yuv444p16_to_rgb_f32`).
		let scale = if bit_depth > 8 {
			1u32 << (bit_depth - 8)
		} else {
			1
		};
		let (y_black, y_white, c_mid, c_span) = if full_range {
			(0.0, max, (1u32 << (bit_depth - 1)) as f32, max)
		} else {
			(
				(16 * scale) as f32,
				(235 * scale) as f32,
				(128 * scale) as f32,
				(224 * scale) as f32,
			)
		};
		let ay = max / (y_white - y_black);
		let by = y_black / (y_white - y_black);
		let ac = max / c_span;
		let bc = c_mid / c_span;
		let rv = 2.0 * (1.0 - kr) * ac;
		let bu = 2.0 * (1.0 - kb) * ac;
		let gu = -(2.0 * kb * (1.0 - kb) / kg) * ac;
		let gv = -(2.0 * kr * (1.0 - kr) / kg) * ac;
		Self {
			matrix: [[ay, 0.0, rv], [ay, gu, gv], [ay, bu, 0.0]],
			offset: [
				-by - 2.0 * (1.0 - kr) * bc,
				-by + (2.0 * kr * (1.0 - kr) + 2.0 * kb * (1.0 - kb)) / kg * bc,
				-by - 2.0 * (1.0 - kb) * bc,
			],
		}
	}

	/// BT.601 luma matrix with limited/studio range.
	///
	/// Luma 16..235 and chroma 16..240 (8-bit equivalents) are expanded to
	/// 0..1 at the 16-bit code scale this constructor implies; the usual choice
	/// for broadcast SD. Counterpart of [`YuvTransform::bt601_full`].
	pub fn bt601_limited() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt601, false)
	}

	/// BT.601 luma matrix with full range.
	///
	/// The whole code range maps to 0..1 with no 16..235 expansion; use it for
	/// full-range SD sources such as computer-generated or JPEG-origin video.
	/// Counterpart of [`YuvTransform::bt601_limited`].
	pub fn bt601_full() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt601, true)
	}

	/// BT.709 luma matrix with limited/studio range.
	///
	/// Luma 16..235 and chroma 16..240 (8-bit equivalents) are expanded to
	/// 0..1 at the 16-bit code scale this constructor implies; the common case
	/// for broadcast HD. Counterpart of [`YuvTransform::bt709_full`].
	pub fn bt709_limited() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt709, false)
	}

	/// BT.709 luma matrix with full range.
	///
	/// The whole code range maps to 0..1 with no 16..235 expansion; use it for
	/// full-range HD sources. Counterpart of [`YuvTransform::bt709_limited`].
	pub fn bt709_full() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt709, true)
	}

	/// BT.2020 luma matrix with limited/studio range.
	///
	/// Luma 16..235 and chroma 16..240 (8-bit equivalents) are expanded to
	/// 0..1 at the 16-bit code scale this constructor implies; the usual choice
	/// for broadcast UHD. Counterpart of [`YuvTransform::bt2020_full`].
	pub fn bt2020_limited() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt2020, false)
	}

	/// BT.2020 luma matrix with full range.
	///
	/// The whole code range maps to 0..1 with no 16..235 expansion; plane
	/// inputs stay 16-bit normalized code values. Counterpart of
	/// [`YuvTransform::bt2020_limited`].
	pub fn bt2020_full() -> Self {
		Self::from_matrix(colormath::YuvMatrix::Bt2020, true)
	}
}

fn install_device_lost_logging(device: &wgpu::Device, device_lost: Arc<AtomicBool>) {
	device.set_device_lost_callback(move |reason, message| {
		eprintln!("[wgpu] device lost: reason={reason:?} message={message}");
		device_lost.store(true, Ordering::Release);
	});
}

/// Create a `UNIFORM | COPY_DST` buffer of `size` bytes filled from `data`.
///
/// The buffer is created `mapped_at_creation` and written through the
/// mapping instead of `queue.write_buffer` with
/// `wgpu::BufferUsages::COPY_DST`, because a queue write can block a render
/// thread behind a host/UI write (observed as a playback freeze on the
/// CUDA/OpenFX path). At most `min(data.len(), size)` bytes are copied; the
/// rest of the buffer stays zero. `label` is only a wgpu debug label.
///
/// # Errors
///
/// `Error::Failed` when wgpu reports a validation or out-of-memory error,
/// or when buffer creation unwinds (device lost).
fn uniform_buffer_from_bytes(
	device: &wgpu::Device,
	label: &str,
	data: &[u8],
	size: u64,
) -> Result<wgpu::Buffer> {
	let validation_scope = device.push_error_scope(ErrorFilter::Validation);
	let oom_scope = device.push_error_scope(ErrorFilter::OutOfMemory);
	let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
		let buffer = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some(label),
			size,
			usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
			mapped_at_creation: true,
		});
		{
			let mut view = buffer.slice(..).get_mapped_range_mut();
			let n = data.len().min(view.len());
			view.slice(..n).copy_from_slice(&data[..n]);
		}
		buffer.unmap();
		buffer
	}));
	let oom_error = pollster_block_on(oom_scope.pop());
	let validation_error = pollster_block_on(validation_scope.pop());
	if let Some(error) = oom_error.or(validation_error) {
		eprintln!("[wgpu] create buffer {label} failed: {error}");
		return Err(Error::Failed(format!(
			"create planar uniform failed: {error}"
		)));
	}
	result.map_err(|_| Error::Failed("wgpu device lost creating planar uniforms".into()))
}

/// Pack an f32 slice as little-endian bytes for `write_buffer`.
fn f32_uniform_bytes(values: &[f32]) -> Vec<u8> {
	let mut out = Vec::with_capacity(values.len() * 4);
	for v in values {
		out.extend_from_slice(&v.to_le_bytes());
	}
	out
}

/// The display pass: manual trilinear 3D-LUT sampling (R32Float, no
/// float-filtering feature needed) of the working-space pixel, alpha
/// passes through. The LUT is the CPU-baked output node + display ICC
/// chain, so the presentation transform runs entirely on the GPU.
const PRESENT_WGSL: &str = r#"
struct Params {
    edge: vec4<f32>,
    lo: vec4<f32>,
    hi: vec4<f32>,
};
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var lut_tex: texture_3d<f32>;
@group(0) @binding(2) var<uniform> p: Params;

	fn lut_at(x: i32, y: i32, z: i32) -> vec3<f32> {
    return textureLoad(lut_tex, vec3<i32>(x, y, z), 0).rgb;
}

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(src_tex);
    let coord = clamp(
        vec2<i32>(i32(frag.x), i32(frag.y)),
        vec2<i32>(0, 0),
        vec2<i32>(i32(dims.x), i32(dims.y)) - vec2<i32>(1, 1),
    );
    let c = textureLoad(src_tex, coord, 0);
    let n = i32(p.edge.x);
    let last = f32(n - 1);
    let span = p.hi.xyz - p.lo.xyz;
    let u = (c.xyz - p.lo.xyz) / max(span, vec3<f32>(1e-12));
    let t = clamp(u, vec3<f32>(0.0), vec3<f32>(1.0)) * last;
    let i0 = vec3<i32>(floor(t));
    let f = t - floor(t);
    let i1 = min(i0 + vec3<i32>(1, 1, 1), vec3<i32>(n - 1));
    let c000 = lut_at(i0.x, i0.y, i0.z);
    let c100 = lut_at(i1.x, i0.y, i0.z);
    let c010 = lut_at(i0.x, i1.y, i0.z);
    let c110 = lut_at(i1.x, i1.y, i0.z);
    let c001 = lut_at(i0.x, i0.y, i1.z);
    let c101 = lut_at(i1.x, i0.y, i1.z);
    let c011 = lut_at(i0.x, i1.y, i1.z);
    let c111 = lut_at(i1.x, i1.y, i1.z);
    let c00 = mix(c000, c100, f.x);
    let c10 = mix(c010, c110, f.x);
    let c01 = mix(c001, c101, f.x);
    let c11 = mix(c011, c111, f.x);
    let c0 = mix(c00, c10, f.y);
    let c1 = mix(c01, c11, f.y);
    let out = mix(c0, c1, f.z);
    return vec4<f32>(out, c.a);
}
"#;

/// The YUV→RGB pass (M5 dependency): three `R16Float` plane inputs
/// sampled 1:1 from the frame's Y/U/V planes, one `Rgba32Float` output.
const YUV_WGSL: &str = r#"
struct Params {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    uv_scale: vec4<f32>,
};
@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var u_tex: texture_2d<f32>;
@group(0) @binding(2) var v_tex: texture_2d<f32>;
@group(0) @binding(3) var<uniform> p: Params;

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let yd = textureDimensions(y_tex);
    let coord = clamp(
        vec2<i32>(i32(frag.x), i32(frag.y)),
        vec2<i32>(0, 0),
        vec2<i32>(i32(yd.x), i32(yd.y)) - vec2<i32>(1, 1),
    );
    let yv = textureLoad(y_tex, coord, 0).r;
    let ud = textureDimensions(u_tex);
    let uc = clamp(
        vec2<i32>(vec2<f32>(coord) * p.uv_scale.xy),
        vec2<i32>(0, 0),
        vec2<i32>(i32(ud.x), i32(ud.y)) - vec2<i32>(1, 1),
    );
    let uv = textureLoad(u_tex, uc, 0).r;
    let vv = textureLoad(v_tex, uc, 0).r;
    let yuv = vec3<f32>(yv, uv, vv);
    let rgb = vec3<f32>(
        dot(p.m0.xyz, yuv) + p.m0.w,
        dot(p.m1.xyz, yuv) + p.m1.w,
        dot(p.m2.xyz, yuv) + p.m2.w,
    );
    return vec4<f32>(rgb, 1.0);
}
"#;

/// The planar (hardware-import) YUV→RGB pass (M5): luma + interleaved
/// chroma plane inputs (R8/R8G8 or R16/R16G16, normalized 0..1), one
/// `Rgba32Float` output. Same matrix/range math as [`YUV_WGSL`].
const YUV_PLANAR_WGSL: &str = r#"
struct Params {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    uv_scale: vec4<f32>,
};
@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> p: Params;

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let yd = textureDimensions(y_tex);
    let coord = clamp(
        vec2<i32>(i32(frag.x), i32(frag.y)),
        vec2<i32>(0, 0),
        vec2<i32>(i32(yd.x), i32(yd.y)) - vec2<i32>(1, 1),
    );
    let yv = textureLoad(y_tex, coord, 0).r;
    let ud = textureDimensions(uv_tex);
    let uc = clamp(
        vec2<i32>(vec2<f32>(coord) * p.uv_scale.xy),
        vec2<i32>(0, 0),
        vec2<i32>(i32(ud.x), i32(ud.y)) - vec2<i32>(1, 1),
    );
    let uv = textureLoad(uv_tex, uc, 0);
    let yuv = vec3<f32>(yv, uv.r, uv.g);
    let rgb = vec3<f32>(
        dot(p.m0.xyz, yuv) + p.m0.w,
        dot(p.m1.xyz, yuv) + p.m1.w,
        dot(p.m2.xyz, yuv) + p.m2.w,
    );
    return vec4<f32>(rgb, 1.0);
}
"#;

/// The descriptor-driven planar-matrix pass: 1–3 plane textures sampled
/// with normalized bilinear mapping (four `textureLoad` taps, edge
/// clamped) and converted to RGB either through the YUV matrix (planar
/// and semi-planar models) or directly (interleaved RGB/RGBA). The mode
/// uniform selects the model, so one pipeline serves the whole matrix.
///
/// `pos = frag.xy * (src_size / dst_size) - 0.5` puts destination pixel
/// centers on source texel coordinates for any source/destination size;
/// chroma uses `src_size/dst_size * uv_scale`, so 4:2:0
/// (`uv_scale = 0.5,0.5`) and 4:2:2 (`0.5,1.0`) and 4:4:4 (`1,1`) all
/// resolve in their own plane. The visible luma/chroma sizes are
/// uniforms (odd dimensions round the nominal chroma up), and sampling
/// is clamped to them, so padded hardware plane allocations never leak
/// into the result.
const YUV_PLANAR_MATRIX_WGSL: &str = r#"
struct Params {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // x,y = chroma/luma dimension ratio; z = model (0 planar, 1
    // semi-planar, 2 rgb, 3 rgba); w unused.
    uv_scale_mode: vec4<f32>,
    // x,y = source luma size / destination size.
    src_scale: vec4<f32>,
    // x,y = visible luma size; z,w = nominal (ceil-subsampled) chroma
    // size. Sampling is clamped to these, not to the (possibly padded)
    // texture allocation.
    src_size: vec4<f32>,
};
@group(0) @binding(0) var plane0: texture_2d<f32>;
@group(0) @binding(1) var plane1: texture_2d<f32>;
@group(0) @binding(2) var plane2: texture_2d<f32>;
@group(0) @binding(3) var<uniform> p: Params;

fn sample_bilinear(tex: texture_2d<f32>, pos: vec2<f32>, limit: vec2<f32>) -> vec4<f32> {
    // Clamp to the visible plane extent, then to the allocation (hardware
    // planes may be padded beyond the visible region).
    let dims = textureDimensions(tex);
    let hi = min(limit, vec2<f32>(dims) - vec2<f32>(1.0));
    let clamped = clamp(pos, vec2<f32>(0.0), hi);
    let base = vec2<i32>(floor(clamped));
    let f = clamped - vec2<f32>(base);
    let x1 = min(base.x + 1, i32(hi.x));
    let y1 = min(base.y + 1, i32(hi.y));
    let c00 = textureLoad(tex, base, 0);
    let c10 = textureLoad(tex, vec2<i32>(x1, base.y), 0);
    let c01 = textureLoad(tex, vec2<i32>(base.x, y1), 0);
    let c11 = textureLoad(tex, vec2<i32>(x1, y1), 0);
    let top = mix(c00, c10, f.x);
    let bottom = mix(c01, c11, f.x);
    return mix(top, bottom, f.y);
}

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let mode = i32(p.uv_scale_mode.z + 0.5);
    // Destination pixel center -> source luma texel coordinate.
    let y_pos = frag.xy * p.src_scale.xy - vec2<f32>(0.5);
    let y_limit = vec2<f32>(p.src_size.x, p.src_size.y) - vec2<f32>(1.0);
    // At 1:1, use exact texel loads: bilinear on a subsampled UV plane
    // would blend neighbouring chroma samples that the CPU/native path
    // does not blend. Scaled output keeps the 4-tap bilinear.
    let native = p.src_scale.x == 1.0 && p.src_scale.y == 1.0;
    if (mode >= 2) {
        var c: vec4<f32>;
        if (native) {
            c = textureLoad(plane0, vec2<i32>(frag.xy), 0);
        } else {
            c = sample_bilinear(plane0, y_pos, y_limit);
        }
        if (mode == 3) {
            return vec4<f32>(c.rgb, c.a);
        }
        return vec4<f32>(c.rgb, 1.0);
    }
    var yv: f32;
    if (native) {
        yv = textureLoad(plane0, vec2<i32>(frag.xy), 0).r;
    } else {
        yv = sample_bilinear(plane0, y_pos, y_limit).r;
    }
    let c_pos = frag.xy * (p.src_scale.xy * p.uv_scale_mode.xy) - vec2<f32>(0.5);
    let c_limit = vec2<f32>(p.src_size.z, p.src_size.w) - vec2<f32>(1.0);
    let c_native = vec2<i32>(frag.xy * p.uv_scale_mode.xy);
    var u: f32;
    var v: f32;
    if (mode == 0) {
        if (native) {
            u = textureLoad(plane1, c_native, 0).r;
            v = textureLoad(plane2, c_native, 0).r;
        } else {
            u = sample_bilinear(plane1, c_pos, c_limit).r;
            v = sample_bilinear(plane2, c_pos, c_limit).r;
        }
    } else {
        var uv: vec4<f32>;
        if (native) {
            uv = textureLoad(plane1, c_native, 0);
        } else {
            uv = sample_bilinear(plane1, c_pos, c_limit);
        }
        u = uv.r;
        v = uv.g;
    }
    let yuv = vec3<f32>(yv, u, v);
    let rgb = vec3<f32>(
        dot(p.m0.xyz, yuv) + p.m0.w,
        dot(p.m1.xyz, yuv) + p.m1.w,
        dot(p.m2.xyz, yuv) + p.m2.w,
    );
    return vec4<f32>(rgb, 1.0);
}
"#;

/// The fixed vertex stage for effect passes: a fullscreen triangle
/// emitting `ove_texcoord`-convention UVs at location 0. UV v=0 is the
/// first texture data row (the upload/download row order), so effect
/// passes are pixel-identity with the CPU pipeline — no vertical flip
/// anywhere in the chain.
const EFFECT_VS_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = pos[vi];
    return VsOut(vec4<f32>(p, 0.0, 1.0), vec2<f32>(0.5 + 0.5 * p.x, 0.5 - 0.5 * p.y));
}
"#;

impl GpuContextLike for GpuContext {
	fn kind(&self) -> BackendKind {
		self.kind()
	}

	fn destroy_texture(&self, token: u64) {
		self.destroy_texture(token);
	}

	fn upload(&self, token: u64, frame: &Frame) -> Result<()> {
		self.upload(token, frame)
	}

	fn download(&self, token: u64) -> Result<Frame> {
		self.download(token)
	}

	fn download_async(&self, token: u64) -> Result<FrameReadback> {
		self.download_async(token)
	}

	fn blit(&self, src: u64, dst: u64, processor: Option<&color::ColorProcessor>) -> Result<()> {
		self.blit(src, dst, processor)
	}

	fn as_any(&self) -> Option<&dyn any::Any> {
		Some(self)
	}

	fn texture_handle(&self, token: u64) -> Option<Arc<wgpu::Texture>> {
		self.texture_handle(token)
	}
}

/// Plain-copy blit shader: fullscreen triangle, `textureLoad` (no
/// filtering — Rgba32Float is not filterable), 1:1 pixel mapping with
/// edge clamping.
const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var src_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(pos[vi], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(src_tex);
    let coord = clamp(
        vec2<u32>(u32(i32(frag.x)), u32(i32(frag.y))),
        vec2<u32>(0u, 0u),
        dims - vec2<u32>(1u, 1u),
    );
    return textureLoad(src_tex, coord, 0);
}
"#;

/// Run a wgpu future to completion on the current thread. wgpu's adapter/
/// device futures complete immediately for the native backends (no async
/// runtime needed); this is the same block_on the examples use.
fn pollster_block_on<F: future::Future>(future: F) -> F::Output {
	// Local minimal block_on: native wgpu futures are already complete
	// after creation; polling to readiness is enough.
	futures_executor::block_on(future)
}

// Minimal futures executor (wgpu brings futures-core transitively; a tiny
// block_on is enough for the immediately-ready adapter/device futures).
mod futures_executor {
	use std::future::Future;
	use std::pin::pin;
	use std::ptr;
	use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
	use std::thread;

	fn noop_raw_waker() -> RawWaker {
		fn no_op(_: *const ()) {}
		fn clone(_: *const ()) -> RawWaker {
			noop_raw_waker()
		}
		static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
		RawWaker::new(ptr::null(), &VTABLE)
	}

	fn noop_waker() -> Waker {
		// SAFETY: the no-op vtable is valid for any data pointer.
		unsafe { Waker::from_raw(noop_raw_waker()) }
	}

	/// Drive `future` until completion (panics on a genuinely pending
	/// future — native wgpu futures never are).
	pub fn block_on<F: Future>(future: F) -> F::Output {
		let mut future = pin!(future);
		let waker = noop_waker();
		let mut cx = Context::from_waker(&waker);
		loop {
			match future.as_mut().poll(&mut cx) {
				Poll::Ready(out) => return out,
				Poll::Pending => thread::yield_now(),
			}
		}
	}
}

/// The display renderer (C++ `olive::Renderer`): a wgpu context (when an
/// adapter exists) or the CPU path. Owns nothing else — textures hold
/// their own `Arc<GpuContext>`.
pub struct DisplayRenderer {
	ctx: Option<Arc<GpuContext>>,
	backend: BackendKind,
}

impl DisplayRenderer {
	/// A renderer for the given backend kind (not yet initialized).
	pub fn new(backend: BackendKind) -> Self {
		Self { ctx: None, backend }
	}

	/// Initialize: create the GPU context for the configured backend.
	/// `gl_context` must be null — a foreign OpenGL context cannot be
	/// adopted by wgpu (documented limitation).
	///
	/// When the requested backend matches the user's configured choice
	/// (the common case), the process-wide shared context
	/// ([`GpuContext::shared`]) is adopted so the process owns exactly
	/// one wgpu device; an explicit different backend gets its own
	/// context.
	pub fn init(&mut self, gl_context: *mut ffi::c_void) -> Result<()> {
		if !gl_context.is_null() {
			return Err(Error::Invalid);
		}
		self.ctx = if self.backend == BackendKind::from_user_config() {
			GpuContext::shared()
		} else {
			GpuContext::create(self.backend)
		};
		if self.ctx.is_none() {
			return Err(Error::Failed("no GPU adapter available".into()));
		}
		Ok(())
	}

	/// The backend this renderer was built for.
	///
	/// This is the requested [`BackendKind`], which need not be the kind its
	/// live context ended up on — [`DisplayRenderer::init`] may adopt the
	/// process-wide shared context; compare with [`GpuContext::kind`].
	pub fn backend(&self) -> BackendKind {
		self.backend
	}

	/// The live GPU context (None on the CPU path / before init).
	pub fn context(&self) -> Option<&Arc<GpuContext>> {
		self.ctx.as_ref()
	}

	/// True for an OpenGL-backed renderer.
	pub fn is_open_gl(&self) -> bool {
		self.backend == BackendKind::Gl
	}

	/// True for a Vulkan-backed renderer.
	pub fn is_vulkan(&self) -> bool {
		self.backend == BackendKind::Vulkan
	}

	/// True after successful init.
	pub fn is_initialized(&self) -> bool {
		self.ctx.is_some()
	}

	/// Create a texture (GPU when initialized, else a CPU frame). `pixels`
	/// (with `linesize` bytes per row) initializes the data.
	pub fn create_texture(
		&self,
		params: &VideoParamsPod,
		pixels: Option<(*const u8, usize)>,
	) -> Result<Texture> {
		let width = params.effective_width();
		let height = params.effective_height();
		if width <= 0 || height <= 0 {
			return Err(Error::Invalid);
		}
		if let Some(ctx) = &self.ctx {
			let token = ctx.create_texture(width, height)?;
			if let Some((ptr, linesize)) = pixels {
				let mut frame = Frame::new();
				let mut pod = *params;
				pod.width = width;
				pod.height = height;
				pod.format = PixelFormat::F32 as i32;
				frame.set_video_params(pod);
				let frame_linesize = frame.linesize_bytes();
				if linesize != frame_linesize {
					ctx.destroy_texture(token);
					return Err(Error::Invalid);
				}
				frame.data = unsafe {
					slice::from_raw_parts(ptr, frame_linesize * height as usize).to_vec()
				};
				ctx.upload(token, &frame)?;
			}
			Ok(Texture::gpu(
				ctx.clone(),
				token,
				width,
				height,
				PixelFormat::F32,
			))
		} else {
			let mut frame = Frame::new();
			let mut pod = *params;
			pod.width = width;
			pod.height = height;
			pod.format = PixelFormat::F32 as i32;
			frame.set_video_params(pod);
			frame.allocate();
			if let Some((ptr, linesize)) = pixels {
				let frame_linesize = frame.linesize_bytes();
				if linesize != frame_linesize {
					return Err(Error::Invalid);
				}
				frame.data = unsafe {
					slice::from_raw_parts(ptr, frame_linesize * height as usize).to_vec()
				};
			}
			Ok(Texture::wrap_frame(frame))
		}
	}

	/// Upload pixels into a texture (GPU: backend upload; CPU: buffer copy).
	///
	/// # Safety
	///
	/// `pixels` must point to at least `linesize * height` readable bytes
	/// of the texture's pixel format, and `linesize` must equal the
	/// frame's line size.
	pub unsafe fn upload_texture(
		&self,
		texture: &mut Texture,
		pixels: *const u8,
		linesize: usize,
	) -> Result<()> {
		let size = texture.size();
		match texture {
			Texture::Gpu { token, ctx, .. } => {
				let frame = unsafe { frame_from_pixels_for_upload(size, pixels, linesize) }?;
				ctx.upload(*token, &frame)
			}
			// Imported hardware planes are immutable views of decoder
			// memory (M5); uploading into them is never valid.
			Texture::Planar(_) => Err(Error::State),
			Texture::Cpu(frame) => {
				let stride = frame.linesize_bytes();
				if linesize != stride {
					return Err(Error::Invalid);
				}
				let h = frame.height as usize;
				frame
					.data
					.copy_from_slice(unsafe { slice::from_raw_parts(pixels, stride * h) });
				Ok(())
			}
		}
	}

	/// Download a texture's pixels into `dst` (with `linesize` stride).
	///
	/// # Safety
	///
	/// `dst` must point to at least `linesize * height` writable bytes,
	/// and `linesize` must equal the frame's line size.
	pub unsafe fn download_texture(
		&self,
		texture: &Texture,
		dst: *mut u8,
		linesize: usize,
	) -> Result<()> {
		let frame = texture.to_frame()?;
		let stride = frame.linesize_bytes();
		if linesize != stride {
			return Err(Error::Invalid);
		}
		let h = frame.height as usize;
		unsafe {
			ptr::copy_nonoverlapping(frame.data.as_ptr(), dst, stride * h);
		}
		Ok(())
	}

	/// Blit `src` into `dst`, optionally through a color processor.
	///
	/// Both textures must live on the same backend: a `Texture::Gpu` pair owned
	/// by one [`GpuContext`] blits on the GPU ([`GpuContext::blit`]), a
	/// `Texture::Cpu` pair copies the source bytes into `dst` and then runs
	/// `processor.convert_frame` in place. A CPU/GPU mix, two different GPU
	/// contexts, or `src == None` returns `Error::Failed`; a CPU pair with
	/// differing sizes returns `Error::Invalid`.
	///
	/// A `Some` processor on the GPU path is deferred in this pass and fails
	/// with `Error::Failed`, so color management currently only happens on the
	/// CPU path. `dst` is fully overwritten.
	pub fn blit_color_managed(
		&self,
		src: Option<&Texture>,
		dst: &mut Texture,
		processor: Option<&color::ColorProcessor>,
	) -> Result<()> {
		match (src, dst) {
			(
				Some(Texture::Gpu { token: s, ctx, .. }),
				Texture::Gpu {
					token: d,
					ctx: dctx,
					..
				},
			) if Arc::ptr_eq(ctx, dctx) => ctx.blit(*s, *d, processor),
			(Some(Texture::Cpu(sf)), Texture::Cpu(df)) => {
				if sf.width != df.width || sf.height != df.height {
					return Err(Error::Invalid);
				}
				df.data.clear();
				df.data.extend_from_slice(&sf.data);
				if let Some(p) = processor {
					p.convert_frame(df)?;
				}
				Ok(())
			}
			_ => Err(Error::Failed(
				"mixed CPU/GPU texture blit unsupported".into(),
			)),
		}
	}

	/// Cross-backend texture download by id (GPU registry only; CPU
	/// textures have no id registry — documented).
	///
	/// # Safety
	///
	/// `dst` must point to at least `linesize * height` writable bytes,
	/// and `linesize` must equal the F32 line size.
	pub unsafe fn download_from_texture(
		&self,
		texture_id: i32,
		params: &VideoParamsPod,
		dst: *mut u8,
		linesize: usize,
	) -> Result<()> {
		let ctx = self
			.ctx
			.as_ref()
			.ok_or(Error::Failed("no GPU context".into()))?;
		let frame = ctx.download(texture_id as u64)?;
		let want_linesize = params.effective_width() as usize * 4 * 4;
		if linesize != want_linesize {
			return Err(Error::Invalid);
		}
		let h = frame.height as usize;
		unsafe {
			ptr::copy_nonoverlapping(frame.data.as_ptr(), dst, want_linesize * h);
		}
		Ok(())
	}
}

/// The GPU texture id of a texture (0 for CPU textures; the ffi
/// `quercusrender_display_texture_id` export).
pub fn texture_id_of(t: &Texture) -> i32 {
	match t {
		Texture::Gpu { token, .. } => *token as i32,
		// Imported planar frames have no single displayable texture id;
		// the footage path resolves them before presentation.
		Texture::Planar(_) | Texture::Cpu(_) => 0,
	}
}

/// Build an F32 frame from raw pixels (for GPU upload).
///
/// # Safety
///
/// `pixels` must point to `linesize * height` readable bytes of F32 RGBA
/// data (a null pointer is rejected with `Error::Invalid`).
pub unsafe fn frame_from_pixels_for_upload(
	size: (i32, i32),
	pixels: *const u8,
	linesize: usize,
) -> Result<Frame> {
	let (w, h) = size;
	if w <= 0 || h <= 0 || pixels.is_null() {
		return Err(Error::Invalid);
	}
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	pod.format = PixelFormat::F32 as i32;
	frame.set_video_params(pod);
	let stride = frame.linesize_bytes();
	if linesize != stride {
		return Err(Error::Invalid);
	}
	frame.data = unsafe { slice::from_raw_parts(pixels, stride * h as usize).to_vec() };
	Ok(frame)
}

#[cfg(test)]
mod tests;
