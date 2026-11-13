//! Owned video frames with explicit source and pixel-format metadata.

pub(crate) mod montage;

use quercus_core::{PixelFormat, Rational};
use quercus_node::footage::FootageBehavior;
use quercus_node::id::NodeId;
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use quercus_render::manager::RenderManager;
use quercus_render::ticket::{TicketPayload, VideoTicketParams};

use std::sync::atomic::{AtomicBool, Ordering};

/// True when the host applies a display/output transform on the CPU. The
/// engine then must not hand back a working-space texture as the final
/// presentation surface; it stages the pixels so the host's LUT runs.
static HOST_DISPLAY_TRANSFORM_REQUIRED: AtomicBool = AtomicBool::new(true);

/// Tell the engine whether the host will apply a display/output transform.
/// When true (the safe default), GPU textures are staged to CPU bytes so the
/// host's color-managed path remains authoritative. When false, the engine
/// keeps the final texture zero-copy.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_set_display_transform_required(required: bool) {
	HOST_DISPLAY_TRANSFORM_REQUIRED.store(required, Ordering::Release);
}

use crate::handle::NativeMirror;
use montage::{
	clamp_render_size, preview_media, single_track_video_montage, video_montage_with_adjustments,
};

/// Original producer of an owned frame's bytes.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOrigin {
	Cpu = 0,
	Gpu = 1,
	SharedMemory = 2,
}

/// Wire format of an owned frame's byte buffer.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPixelFormat {
	F32Rgba = 0,
	Bgra8 = 1,
}

/// Owned bytes and geometry for one video frame.
#[derive(Clone)]
pub struct VideoBufferData {
	pub width: i32,
	pub height: i32,
	pub linesize: i32,
	pub origin: FrameOrigin,
	pub pixel_format: VideoPixelFormat,
	pub bytes: Vec<u8>,
	/// Optional zero-copy presentation texture. When present the bytes are
	/// empty and the host should sample this texture directly instead of
	/// copying pixels.
	pub gpu_texture: Option<std::sync::Arc<quercus_core::wgpu::Texture>>,
	gpu_context: Option<std::sync::Arc<quercus_core::backend::GpuContext>>,
	cpu_bytes: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
}

impl std::fmt::Debug for VideoBufferData {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("VideoBufferData")
			.field("width", &self.width)
			.field("height", &self.height)
			.field("linesize", &self.linesize)
			.field("origin", &self.origin)
			.field("pixel_format", &self.pixel_format)
			.field("byte_count", &self.cpu_byte_count())
			.field("has_gpu_texture", &self.gpu_texture.is_some())
			.finish()
	}
}

/// Video metadata without the variable-length byte buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoBufferMetadata {
	/// Frame width in pixels.
	pub width: i32,
	/// Frame height in pixels.
	pub height: i32,
	/// Bytes per scanline in the owned buffer.
	pub linesize: i32,
	/// Producer that supplied the original pixels.
	pub origin: FrameOrigin,
	/// Byte layout of the owned pixels.
	pub pixel_format: VideoPixelFormat,
	/// Number of bytes in the owned buffer.
	pub byte_count: usize,
}

impl VideoBufferData {
	fn metadata(&self) -> VideoBufferMetadata {
		VideoBufferMetadata {
			width: self.width,
			height: self.height,
			linesize: self.linesize,
			origin: self.origin,
			pixel_format: self.pixel_format,
			byte_count: self.cpu_byte_count(),
		}
	}

	fn cpu_byte_count(&self) -> usize {
		if self.bytes.is_empty() && self.gpu_texture.is_some() {
			return self.width.max(0) as usize * self.height.max(0) as usize * 16;
		}
		self.bytes.len()
	}

	/// Run `f` on the frame's CPU bytes, materialising them if needed.
	///
	/// Returns `Some` with `f`'s result when CPU bytes are available: either the
	/// owned `bytes`, or — for a GPU-only frame — a download of `gpu_texture`
	/// through
	/// [`quercus_core::backend::GpuContext::download_shared_texture`], cached in
	/// `cpu_bytes` so later calls do not read the GPU again. Returns `None` when
	/// the frame has neither bytes nor both a GPU texture and its context, or when
	/// the download fails.
	fn with_cpu_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
		if !self.bytes.is_empty() {
			return Some(f(&self.bytes));
		}
		let gpu = self.gpu_context.as_ref()?;
		let texture = self.gpu_texture.as_ref()?;
		let mut cache = self.cpu_bytes.lock().unwrap_or_else(|e| e.into_inner());
		if cache.is_none() {
			let frame = gpu
				.download_shared_texture(
					texture,
					texture.format(),
					self.width as u32,
					self.height as u32,
				)
				.ok()?;
			*cache = Some(frame.data);
		}
		cache.as_deref().map(f)
	}
}

/// Opaque reference-counted video result.
pub struct VideoBufferHandle {
	pub(crate) data: VideoBufferData,
}

crate::export_handle!(
	VideoBufferHandle,
	quercus_video_buffer_add_ref,
	quercus_video_buffer_release
);

impl VideoBufferHandle {
	/// Wrap an owned [`VideoBufferData`] in a handle and return it as a raw
	/// pointer for the C ABI.
	///
	/// The reference count starts at one; the caller releases it with
	/// `quercus_video_buffer_release` (generated by `export_handle!`) and may add
	/// references with `quercus_video_buffer_add_ref`.
	pub(crate) fn from_data(data: VideoBufferData) -> *const Self {
		crate::handle::into_ffi(Self { data })
	}
}

/// Geometry, source, pixel format and byte count of a video frame.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusVideoBufferInfo {
	/// Frame width in pixels.
	pub width: i32,
	/// Frame height in pixels.
	pub height: i32,
	/// Bytes per scanline in the owned buffer.
	pub linesize: i32,
	/// `FrameOrigin` discriminant.
	pub origin: i32,
	/// `VideoPixelFormat` discriminant.
	pub pixel_format: i32,
	/// Number of bytes in the owned buffer.
	pub byte_count: usize,
}

impl NativeMirror for QuercusVideoBufferInfo {
	type Native = VideoBufferMetadata;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			width: native.width,
			height: native.height,
			linesize: native.linesize,
			origin: native.origin as i32,
			pixel_format: native.pixel_format as i32,
			byte_count: native.byte_count,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			width: self.width,
			height: self.height,
			linesize: self.linesize,
			origin: match self.origin {
				1 => FrameOrigin::Gpu,
				2 => FrameOrigin::SharedMemory,
				_ => FrameOrigin::Cpu,
			},
			pixel_format: match self.pixel_format {
				1 => VideoPixelFormat::Bgra8,
				_ => VideoPixelFormat::F32Rgba,
			},
			byte_count: self.byte_count,
		}
	}
}

/// Fill `out` with the frame's metadata; `false` when either pointer is null.
///
/// `out` receives width/height, line size, origin and pixel-format
/// discriminants plus the byte count (see [`QuercusVideoBufferInfo`]); the
/// pixel buffer itself is not touched.
///
/// # Safety
///
/// `frame` must be a live handle or null, and `out` must be null or writable
/// for one [`QuercusVideoBufferInfo`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_video_buffer_info(
	frame: *const VideoBufferHandle,
	out: *mut QuercusVideoBufferInfo,
) -> bool {
	let (Some(frame), Some(out)) = (unsafe { frame.as_ref() }, unsafe { out.as_mut() }) else {
		return false;
	};
	*out = QuercusVideoBufferInfo::from_native(&frame.data.metadata());
	true
}

/// Copy the frame's packed pixels into caller-provided storage.
///
/// Returns the number of bytes copied, or `0` (leaving `dst` untouched) when
/// `frame` is null, `dst` is null, or `dst_capacity` is smaller than the
/// frame's byte count. A GPU-backed frame is downloaded and cached on first
/// call.
///
/// # Safety
///
/// `frame` must be a live handle from this ABI; `dst` must be null or point
/// to at least `dst_capacity` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_video_buffer_copy_bytes(
	frame: *const VideoBufferHandle,
	dst: *mut u8,
	dst_capacity: usize,
) -> usize {
	let Some(frame) = (unsafe { frame.as_ref() }) else {
		return 0;
	};
	let required = frame.data.cpu_byte_count();
	if dst.is_null() || dst_capacity < required {
		return 0;
	}
	frame
		.data
		.with_cpu_bytes(|bytes| unsafe {
			std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
			bytes.len()
		})
		.unwrap_or(0)
}

/// Clone the frame's optional zero-copy presentation texture and return it as
/// an `Arc<wgpu::Texture>` raw pointer.
///
/// The host takes ownership and must rebuild the `Arc` with `Arc::from_raw`;
/// dropping the pointer without doing so leaks the texture. When `width`/
/// `height` are non-null they receive the frame dimensions. Returns null when
/// `frame` is null or the frame has no GPU texture (CPU or shared-memory
/// frame). The host must have adopted the engine device
/// (`quercus_render_gpu_install`), otherwise the texture cannot be sampled.
///
/// # Safety
///
/// `frame` must be a live handle from this ABI, `width`/`height` must be null
/// or writable for one `i32`, and the caller must consume the returned pointer
/// exactly once with `Arc::from_raw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_video_buffer_take_texture(
	frame: *mut VideoBufferHandle,
	width: *mut i32,
	height: *mut i32,
) -> *const std::ffi::c_void {
	let Some(frame) = (unsafe { frame.as_mut() }) else {
		return std::ptr::null();
	};
	let Some(texture) = frame.data.gpu_texture.clone() else {
		return std::ptr::null();
	};
	if !width.is_null() {
		unsafe { *width = frame.data.width };
	}
	if !height.is_null() {
		unsafe { *height = frame.data.height };
	}
	std::sync::Arc::into_raw(texture) as *const std::ffi::c_void
}

/// Convert packed F32 RGBA samples to 8-bit BGRA bytes.
///
/// Every four floats become four bytes in `B`, `G`, `R`, `A` order; each
/// channel is clamped to `0.0..=1.0`, scaled by 255 and rounded. A trailing
/// partial pixel is ignored.
fn f32_to_bgra8(samples: &[f32]) -> Vec<u8> {
	samples
		.chunks_exact(4)
		.flat_map(|pixel| {
			let convert = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
			[
				convert(pixel[2]),
				convert(pixel[1]),
				convert(pixel[0]),
				convert(pixel[3]),
			]
		})
		.collect()
}

/// Repack F32 RGBA samples from a strided CPU buffer into an owned video
/// buffer of the requested layout.
///
/// `bytes` holds `height` rows of `linesize` bytes each, and a row never needs
/// more than `width * 16` bytes: the source stride is `max(linesize, width*16)`
/// and only the first `width * 16` bytes of each row are read. Samples are
/// decoded as native-endian f32; `pixel_format` then decides the output —
/// `F32Rgba` keeps them packed (`linesize = width * 16`), `Bgra8` converts
/// through [`f32_to_bgra8`] (`linesize = width * 4`).
///
/// # Errors
///
/// Returns a message when `width`/`height` is not positive or `bytes` does not
/// cover the last row.
fn pack_f32_bytes(
	width: i32,
	height: i32,
	linesize: i32,
	bytes: &[u8],
	origin: FrameOrigin,
	pixel_format: VideoPixelFormat,
) -> Result<VideoBufferData, String> {
	let row_bytes = width.max(0) as usize * 16;
	let source_stride = (linesize.max(0) as usize).max(row_bytes);
	if width <= 0 || height <= 0 || bytes.len() < source_stride * (height as usize - 1) + row_bytes
	{
		return Err("video frame has invalid geometry or truncated pixels".into());
	}
	let mut samples = Vec::with_capacity(width as usize * height as usize * 4);
	for y in 0..height as usize {
		let row = &bytes[y * source_stride..y * source_stride + row_bytes];
		for value in row.chunks_exact(4) {
			samples.push(f32::from_ne_bytes(
				value.try_into().expect("four-byte sample"),
			));
		}
	}
	let bytes = match pixel_format {
		VideoPixelFormat::F32Rgba => samples
			.iter()
			.flat_map(|sample| sample.to_ne_bytes())
			.collect(),
		VideoPixelFormat::Bgra8 => f32_to_bgra8(&samples),
	};
	Ok(VideoBufferData {
		width,
		height,
		linesize: width
			* match pixel_format {
				VideoPixelFormat::F32Rgba => 16,
				VideoPixelFormat::Bgra8 => 4,
			},
		origin,
		pixel_format,
		bytes,
		gpu_texture: None,
		gpu_context: None,
		cpu_bytes: std::sync::Arc::new(std::sync::Mutex::new(None)),
	})
}

/// Convert an owned CPU [`quercus_core::texture::Frame`] into a video buffer.
///
/// Reads the frame's `video_params()` (width/height) plus its real line size
/// and repacks through [`pack_f32_bytes`]; `origin` is reported as the
/// provenance and `pixel_format` is the requested output layout.
///
/// # Errors
///
/// Propagates [`pack_f32_bytes`]'s message for invalid geometry or truncated
/// data.
fn frame_from_cpu(
	frame: quercus_core::texture::Frame,
	origin: FrameOrigin,
	pixel_format: VideoPixelFormat,
) -> Result<VideoBufferData, String> {
	let params = frame.video_params();
	pack_f32_bytes(
		params.width,
		params.height,
		frame.linesize_bytes() as i32,
		&frame.data,
		origin,
		pixel_format,
	)
}

/// Present `token` and hand out its texture when this device is the host's.
///
/// A texture from the engine's own device cannot be sampled by the Oak viewer
/// — handing it over produces invalid surface bind groups instead of a frame
/// — so this returns `None` unless `ctx` is a concrete
/// [`quercus_core::backend::GpuContext`] that reports `is_adopted()` and is
/// also the process-wide shared context. On success the display LUT is
/// applied ([`quercus_core::backend::GpuContext::present_texture`]) and the
/// resulting texture is removed from the registry
/// ([`quercus_core::backend::GpuContext::take_texture_handle`]), so the caller
/// owns the only reference along with the shared context.
fn adopted_texture(
	ctx: &std::sync::Arc<dyn quercus_core::backend::GpuContextLike>,
	token: u64,
) -> Option<(
	std::sync::Arc<quercus_core::backend::GpuContext>,
	std::sync::Arc<quercus_core::wgpu::Texture>,
)> {
	let gpu = ctx
		.as_any()
		.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>())?;
	if !gpu.is_adopted() {
		return None;
	}
	let converted = gpu.present_texture(token).ok()?;
	let texture = gpu.take_texture_handle(converted)?;
	let shared = quercus_core::backend::GpuContext::shared()?;
	(std::sync::Arc::as_ptr(&shared) == gpu).then_some((shared, texture))
}

/// Wrap an already-presented GPU texture as a zero-copy video frame.
///
/// `texture` must be the `Rgba32Float`/`Rgba16Float` texture that
/// `gpu_context` owns; `width`/`height` are the frame size in pixels and the
/// reported `linesize` is `width * 16`. No pixels are copied: `bytes` stays
/// empty and `cpu_byte_count` reports the equivalent F32 size.
///
/// # Errors
///
/// Returns a message when `width` or `height` is not positive.
fn frame_from_gpu_texture(
	width: i32,
	height: i32,
	texture: std::sync::Arc<quercus_core::wgpu::Texture>,
	gpu_context: std::sync::Arc<quercus_core::backend::GpuContext>,
) -> Result<VideoBufferData, String> {
	if width <= 0 || height <= 0 {
		return Err("GPU frame has invalid geometry".into());
	}
	Ok(VideoBufferData {
		width,
		height,
		linesize: width * 16,
		origin: FrameOrigin::Gpu,
		pixel_format: VideoPixelFormat::F32Rgba,
		bytes: Vec::new(),
		gpu_texture: Some(texture),
		gpu_context: Some(gpu_context),
		cpu_bytes: std::sync::Arc::new(std::sync::Mutex::new(None)),
	})
}

/// Convert a shared-memory pool frame into an owned [`VideoBufferData`].
///
/// Copies the frame's slot out of the shared region and truncates it to the
/// producer's `data_size`, so the pool slot is released before returning and
/// the result no longer aliases shared memory. Three source layouts are
/// handled: F32 pixels are repacked through [`pack_f32_bytes`], BGRA8 pixels
/// are kept as bytes when `pixel_format` is `Bgra8`, and BGRA8 pixels are
/// expanded to F32 (channels reordered to RGBA, alpha preserved) for an F32
/// request.
///
/// `pixel_format` is the requested output layout, not the producer's; the
/// returned `origin` is [`FrameOrigin::SharedMemory`].
///
/// # Errors
///
/// Returns a message when the slot is shorter than `data_size` or the pixels
/// are truncated relative to `width`/`height`/`linesize`.
fn frame_from_shared(
	frame: &quercus_render::procpool::ShmFrameRef,
	pixel_format: VideoPixelFormat,
) -> Result<VideoBufferData, String> {
	let meta = &frame.meta;
	let bytes = frame.shm.slot_to_vec(frame.slot);
	let bytes = bytes
		.get(..meta.data_size.max(0) as usize)
		.ok_or("shared-memory frame is truncated")?;
	if meta.format == PixelFormat::F32 as i32 {
		pack_f32_bytes(
			meta.width,
			meta.height,
			meta.linesize,
			bytes,
			FrameOrigin::SharedMemory,
			pixel_format,
		)
	} else if pixel_format == VideoPixelFormat::Bgra8 {
		let row_bytes = meta.width.max(0) as usize * 4;
		let source_stride = (meta.linesize.max(0) as usize).max(row_bytes);
		if meta.width <= 0
			|| meta.height <= 0
			|| bytes.len() < source_stride * (meta.height as usize - 1) + row_bytes
		{
			return Err("shared-memory BGRA frame is truncated".into());
		}
		let mut packed = Vec::with_capacity(row_bytes * meta.height as usize);
		for y in 0..meta.height as usize {
			packed.extend_from_slice(&bytes[y * source_stride..y * source_stride + row_bytes]);
		}
		Ok(VideoBufferData {
			width: meta.width,
			height: meta.height,
			linesize: meta.width * 4,
			origin: FrameOrigin::SharedMemory,
			pixel_format,
			bytes: packed,
			gpu_texture: None,
			gpu_context: None,
			cpu_bytes: std::sync::Arc::new(std::sync::Mutex::new(None)),
		})
	} else {
		let mut converted = Vec::with_capacity(meta.width as usize * meta.height as usize * 16);
		for pixel in bytes.chunks_exact(4) {
			for index in [2, 1, 0, 3] {
				converted.extend_from_slice(&(f32::from(pixel[index]) / 255.0).to_ne_bytes());
			}
		}
		pack_f32_bytes(
			meta.width,
			meta.height,
			meta.width * 16,
			&converted,
			FrameOrigin::SharedMemory,
			pixel_format,
		)
	}
}

/// Renders through an existing manager or the inline evaluator and returns an
/// owned frame. Shared-memory slots are copied and released before returning.
pub(crate) fn render_video(
	project: std::sync::Arc<std::sync::Mutex<Project>>,
	viewer: NodeId,
	track: Option<NodeId>,
	playback: Option<(i64, i64, u64)>,
	time: Rational,
	width: i32,
	height: i32,
	pixel_format: VideoPixelFormat,
) -> Result<VideoBufferData, String> {
	if width <= 0 || height <= 0 {
		return Err("invalid video dimensions".into());
	}
	let (width, height) = {
		let guard = project.lock().unwrap_or_else(|e| e.into_inner());
		if guard
			.graph
			.get(viewer)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
			.is_some()
		{
			clamp_render_size(&guard, viewer, time, width, height)
		} else {
			(width, height)
		}
	};
	if let Some(manager) = RenderManager::global() {
		let (montage, adjustments, footage) = {
			let guard = project.lock().unwrap_or_else(|e| e.into_inner());
			if let Some(track) = track {
				(
					single_track_video_montage(&guard, viewer, track, time),
					Vec::new(),
					None,
				)
			} else {
				let is_sequence = guard
					.graph
					.get(viewer)
					.and_then(|entry| entry.behavior.as_any())
					.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
					.is_some();
				if is_sequence {
					let (montage, adjustments) =
						video_montage_with_adjustments(&guard, viewer, time);
					(montage, adjustments, None)
				} else {
					let footage = guard
						.graph
						.get(viewer)
						.and_then(|entry| entry.behavior.as_any())
						.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>())
						.and_then(preview_media);
					(Vec::new(), Vec::new(), footage)
				}
			}
		};
		let params = VideoTicketParams {
			viewer: if track.is_some() {
				0
			} else {
				viewer.identity()
			},
			// P5C exercises the semantic montage path explicitly. A project UUID
			// would select graph-snapshot mode and bypass adjustments/proxy limits.
			project: String::new(),
			time,
			force_size: Some((width, height)),
			force_format: (pixel_format == VideoPixelFormat::F32Rgba).then_some(PixelFormat::F32),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage,
			montage,
			adjustments,
		};
		let id = manager.tickets.next_id();
		if let Some((frame, distance, generation)) = playback {
			manager.tickets.submit_playback_with_id(
				id,
				params,
				frame,
				distance,
				generation,
				Box::new(|_| {}),
			);
		} else {
			manager
				.tickets
				.submit_video_with_id(id, params, Box::new(|_| {}));
		}
		manager.tickets.wait(id).map_err(|e| e.to_string())?;
		let result = manager
			.tickets
			.result(id)
			.ok_or("video ticket produced no result")?;
		return match result {
			Ok(TicketPayload::Video(quercus_core::texture::Texture::Cpu(frame))) => {
				frame_from_cpu(frame, FrameOrigin::Cpu, pixel_format)
			}
			Ok(TicketPayload::Video(texture)) => {
				if !HOST_DISPLAY_TRANSFORM_REQUIRED.load(Ordering::Acquire) {
					if let quercus_core::texture::Texture::Gpu {
						token, width, height, ctx, ..
					} = &texture
					{
						if let Some((gpu, handle)) = adopted_texture(ctx, *token) {
							if let Ok(frame) = frame_from_gpu_texture(*width, *height, handle, gpu) {
								return Ok(frame);
							}
						}
					}
				}
				// Async readback is opt-in until the shared-queue/CUDA
				// serialization is validated for project-open/OFX clips.
				let frame = if std::env::var_os("QUERCUS_ASYNC_READBACK").is_some() {
					match &texture {
						quercus_core::texture::Texture::Gpu { .. } => texture
							.to_frame_async()
							.map_err(|e| e.to_string())?
							.wait()
							.map_err(|e| e.to_string())?,
						_ => texture.to_frame().map_err(|e| e.to_string())?,
					}
				} else {
					texture.to_frame().map_err(|e| e.to_string())?
				};
				frame_from_cpu(frame, FrameOrigin::Gpu, pixel_format)
			}
			Ok(TicketPayload::ShmFrame(frame)) => {
				let converted = frame_from_shared(&frame, pixel_format);
				manager.release_frame(&frame);
				converted
			}
			_ => Err("video render produced no frame".into()),
		};
	}
	if track.is_some() {
		let montage = {
			let guard = project.lock().unwrap_or_else(|e| e.into_inner());
			single_track_video_montage(&guard, viewer, track.expect("checked track"), time)
		};
		let params = VideoTicketParams {
			viewer: 0,
			project: String::new(),
			time,
			force_size: Some((width, height)),
			force_format: Some(PixelFormat::F32),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage,
			adjustments: Vec::new(),
		};
		// GPU montage compositor: the normal F32 path keeps decode, scale
		// and composition on the GPU; the owned-buffer ABI below is a CPU
		// boundary, so only this final handoff reads back.
		let rendered = quercus_render::eval::render_montage_frame(
			time,
			&params,
			(width, height),
			PixelFormat::F32,
		)
		.map_err(|error| error.to_string())?;
		if !HOST_DISPLAY_TRANSFORM_REQUIRED.load(Ordering::Acquire) {
			if let quercus_core::texture::Texture::Gpu {
				token, width: tw, height: th, ctx, ..
			} = &rendered
			{
				if let Some((gpu, handle)) = adopted_texture(ctx, *token) {
					if let Ok(frame) = frame_from_gpu_texture(*tw, *th, handle, gpu) {
						return Ok(frame);
					}
				}
			}
		}
		let frame = match rendered {
			quercus_core::texture::Texture::Cpu(frame) => frame,
			texture @ quercus_core::texture::Texture::Gpu { .. } => {
				if std::env::var_os("QUERCUS_ASYNC_READBACK").is_some() {
					match &texture {
						quercus_core::texture::Texture::Gpu { .. } => texture
							.to_frame_async()
							.map_err(|error| error.to_string())?
							.wait()
							.map_err(|error| error.to_string())?,
						_ => texture.to_frame().map_err(|error| error.to_string())?,
					}
				} else {
					texture.to_frame().map_err(|error| error.to_string())?
				}
			}
			quercus_core::texture::Texture::Planar(_) => {
				return Err("unresolved planar montage texture".to_string())
			}
		};
		return frame_from_cpu(frame, FrameOrigin::Gpu, pixel_format);
	}
	let samples = crate::render::render_frame_f32_data(project, viewer, time, width, height)?;
	let bytes: Vec<u8> = samples
		.iter()
		.flat_map(|sample| sample.to_ne_bytes())
		.collect();
	pack_f32_bytes(
		width,
		height,
		width * 16,
		&bytes,
		FrameOrigin::Cpu,
		pixel_format,
	)
}
