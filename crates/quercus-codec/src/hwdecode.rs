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

//! Hardware video decoding (user-mandated default): the platform's
//! hardware acceleration is preferred over pure software decoding, with
//! a config switch and an automatic software fallback.
//!
//! **FFmpeg 8 removed the standalone hardware decoders** (`h264_videotoolbox`,
//! `h264_vaapi`, `h264_nvdec`, `h264_d3d11va` are all gone from its
//! configure): hardware decode now only exists as a *hwaccel* attached
//! to the software decoder. The model here is therefore uniform across
//! platforms: create the platform's hardware device context
//! (`av_hwdevice_ctx_create`), set it as `hw_device_ctx` on the codec
//! context of the regular decoder, and FFmpeg automatically engages the
//! matching hwaccel (`h264_videotoolbox_hwaccel` & co) on open. Codecs
//! without a matching hwaccel silently stay software — the pipeline
//! below only transfers frames whose pixel format is actually a
//! hardware surface.
//!
//! - **macOS**: `AV_HWDEVICE_TYPE_VIDEOTOOLBOX`
//! - **Linux**: `VAAPI` (DMA-BUF, the M5 import path), then `CUDA` (NVDEC)
//! - **Windows**: `D3D11VA`, then `CUDA` (NVDEC)
//!
//! Device creation can fail on machines without the device/driver (a
//! headless Linux box, no NVIDIA GPU) — the candidate is skipped and
//! the next one (or the software decoder) is used. Hardware frames
//! (`AV_PIX_FMT_VIDEOTOOLBOX` / `VAAPI` / `CUDA` / `D3D11VA_VLD` /
//! `D3D11`) either go through the M5 zero-copy import
//! (`crate::gpuinterop`) or are transferred to system memory with
//! `av_hwframe_transfer_data` before the swscale conversion.

use ffmpeg::ffi as sys;
use ffmpeg::Dictionary;
use ffmpeg_next as ffmpeg;

use crate::error::{Error, Result};

/// Number of hardware frames transferred to system memory so far
/// (process-wide). An observability counter: a hardware decode that
/// never produces a hardware surface stays at zero, so tests can prove
/// the hwaccel really engaged rather than silently staying software.
pub static HW_TRANSFERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Decoder sessions opened with a hardware device attached.
pub static HW_DECODER_OPENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Decoder sessions opened in software mode.
pub static SW_DECODER_OPENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Hardware device/decoder creation attempts that failed.
pub static HW_OPEN_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Hardware sessions that failed at the first frame and were reopened as software.
pub static HW_FALLBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Video decoder sessions opened with a hardware device.
pub static HW_VIDEO_OPENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Video decoder sessions opened in software.
pub static SW_VIDEO_OPENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Audio decoder sessions (always software).
pub static SW_AUDIO_OPENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Frames delivered as hardware surfaces (before transfer/import).
pub static HW_SURFACES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Hardware frames successfully imported without a CPU transfer.
pub static HW_IMPORTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// `get_format` chose a software format even though a hardware device was attached.
pub static HW_FORMAT_FALLBACKS: std::sync::atomic::AtomicU64 =
	std::sync::atomic::AtomicU64::new(0);

/// The config key of the hardware-decode switch (1 = prefer hardware,
/// 0 = force software). Default ON by user mandate.
pub const CONFIG_KEY_HARDWARE_DECODING: &str = "HardwareDecoding";

/// Process-wide negative cache of failed device types (a `static`, not a
/// `const` — a const array is inlined per use site, so writes from
/// `mark_device_unavailable` would never be visible to `device_unavailable`).
/// Creating a hardware device context (`av_hwdevice_ctx_create`) is
/// expensive when the driver is missing and emits FFmpeg's
/// `[VAAPI @ ...] Failed to initialise VAAPI connection` spam every time —
/// on a box without libva that is once per decoder open, per process, with
/// the full log line. The cache marks a device type unavailable after its
/// first failed creation, so every later open skips it (and its log noise)
/// entirely. The slot count is generous: `AVHWDeviceType` values are small
/// non-negative enumerants (< 32), 64 covers them all.
static UNAVAILABLE_DEVICES: [std::sync::atomic::AtomicBool; 64] =
    [const { std::sync::atomic::AtomicBool::new(false) }; 64];

#[cfg(test)]
thread_local! {
	/// Test-only counter of `open_hw_accel` device-context creation
	/// attempts on the calling thread (incremented at the top of the
	/// function, before any FFmpeg call). Lets tests prove the negative
	/// cache short-circuits before FFmpeg is involved.
	///
	/// Thread-local on purpose: the real-media tests decode on other
	/// threads *without* the shared test lock, so a process-wide counter
	/// would let their attempts perturb the negative-cache assertion on
	/// this thread.
	static CREATE_ATTEMPTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The calling thread's creation-attempt count (tests only).
#[cfg(test)]
fn creation_attempts() -> u64 {
	CREATE_ATTEMPTS.with(std::cell::Cell::get)
}

/// Count one creation attempt on the calling thread (tests only).
#[cfg(test)]
fn note_creation_attempt() {
	CREATE_ATTEMPTS.with(|count| count.set(count.get() + 1));
}

/// Reset the calling thread's creation-attempt count (tests only).
#[cfg(test)]
fn reset_creation_attempts() {
	CREATE_ATTEMPTS.with(|count| count.set(0));
}

/// Whether `device_type` is known unavailable — a device-context
/// creation failed once earlier in this process.
pub fn device_unavailable(device_type: sys::AVHWDeviceType) -> bool {
    UNAVAILABLE_DEVICES
        .get(device_type as usize)
        .map(|f| f.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(false)
}

/// Record that `device_type`'s device context failed to create (sticky
/// for the process; the caller falls back to the next candidate).
pub fn mark_device_unavailable(device_type: sys::AVHWDeviceType) {
    if let Some(f) = UNAVAILABLE_DEVICES.get(device_type as usize) {
        f.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether hardware decoding is preferred (the config switch). Default
/// ON by user mandate; only an explicit `"false"` turns it off (the
/// string accessor, same convention as the app's config helpers — the
/// store's typed `get_bool` only parses pre-typed Bool entries). The
/// `QUERCUS_HWACCEL` environment variable overrides both: `0` force-disables
/// hardware decode entirely (a diagnostic escape hatch on machines where
/// probing the device is slow or noisy).
pub fn hardware_decoding_enabled() -> bool {
    if let Ok(v) = std::env::var("QUERCUS_HWACCEL") {
        return v != "0";
    }
    match quercus_core::configstore::ConfigStore::instance()
        .get(None, CONFIG_KEY_HARDWARE_DECODING)
    {
        Ok(value) => value != "false",
        Err(_) => true,
    }
}

/// The hardware device types to try, most preferred first. On a machine
/// without the device/driver the candidate fails creation and the next
/// one is tried; the software decoder is the final fallback.
/// Hardware candidates in preference order for this stream. VAAPI on the
/// NVIDIA driver does not implement 10-bit 4:2:2 H.264; trying it first makes
/// libavcodec decode corrupted software frames (green bars, DC/AC concealment
/// errors) before the next candidate is reached. Prefer NVDEC/CUDA for that
/// profile.
pub fn candidates_for(params: &ffmpeg::codec::Parameters) -> Vec<sys::AVHWDeviceType> {
	let mut candidates = device_type_candidates().to_vec();
	let format = unsafe { (*params.as_ptr()).format };
	let ten_bit_422 = format == sys::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32
		|| format == sys::AVPixelFormat::AV_PIX_FMT_YUV422P10BE as i32;
	if ten_bit_422 {
		candidates.sort_by_key(|candidate| {
			(*candidate != sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA) as u8
		});
	}
	candidates
}

/// Platform hardware decoder candidates, in default preference order.
pub fn device_type_candidates() -> &'static [sys::AVHWDeviceType] {
	#[cfg(target_os = "macos")]
	{
		&[sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX]
	}
	#[cfg(target_os = "windows")]
	{
		&[
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA,
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
		]
	}
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		// CUDA/NVDEC first: the VMM export path (`cuMemExportToShareableHandle`
		// -> Vulkan OPAQUE_FD) is the import that is continuously verified on
		// NVIDIA. VAAPI import is opt-in because some drivers block inside
		// `vkCreateImage`/`vkAllocateMemory` for DMA-BUF; it stays as the
		// fallback on machines without libcuda.
		&[
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
		]
	}
}

/// A display name for a device type (status reporting and tests).
pub fn device_type_name(device_type: sys::AVHWDeviceType) -> &'static str {
	match device_type {
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX => "videotoolbox",
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => "vaapi",
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => "cuda/nvdec",
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA => "d3d11va",
		_ => "unknown",
	}
}

/// Try to open the software codec with a hardware device context of
/// `device_type` attached — FFmpeg then engages the matching hwaccel
/// (e.g. `h264_videotoolbox_hwaccel`) on open. Returns the opened codec
/// The hardware pixel format paired with `device_type`.
fn hw_pixel_format(device_type: sys::AVHWDeviceType) -> Option<sys::AVPixelFormat> {
	use sys::AVHWDeviceType;
	use sys::AVPixelFormat;
	match device_type {
		AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX => Some(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX),
		AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => Some(AVPixelFormat::AV_PIX_FMT_VAAPI),
		AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => Some(AVPixelFormat::AV_PIX_FMT_CUDA),
		AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA => Some(AVPixelFormat::AV_PIX_FMT_D3D11),
		_ => None,
	}
}

/// First non-hardware format in an offered list; `AV_PIX_FMT_NONE` when the
/// list contains only hardware formats.
fn first_software_format(formats: *const sys::AVPixelFormat) -> sys::AVPixelFormat {
	use sys::AVPixelFormat;
	unsafe {
		let mut cursor = formats;
		while !cursor.is_null() && *cursor != AVPixelFormat::AV_PIX_FMT_NONE {
			if matches!(
				*cursor,
				AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
					| AVPixelFormat::AV_PIX_FMT_VAAPI
					| AVPixelFormat::AV_PIX_FMT_CUDA
					| AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD
					| AVPixelFormat::AV_PIX_FMT_D3D11
					| AVPixelFormat::AV_PIX_FMT_VDPAU
			) {
				cursor = cursor.add(1);
				continue;
			}
			return *cursor;
		}
		AVPixelFormat::AV_PIX_FMT_NONE
	}
}

/// `get_format` callback for a decoder opened with `hw_device_ctx`.
///
/// `avctx` is the decoder context (non-null; its `hw_device_ctx` may be null);
/// `formats` is FFmpeg's `AV_PIX_FMT_NONE`-terminated list of offered pixel
/// formats, borrowed for the call. Returns the hardware pixel format when the
/// device type has one, it is offered, and its frames context initializes;
/// otherwise returns `AV_PIX_FMT_NONE` to force FFmpeg's first-frame hardware
/// fallback. The first software format is returned only when no hardware device
/// is attached or the device type maps to no known hardware pixel format.
/// Setting `hw_device_ctx` alone is not enough on all
/// FFmpeg/libavcodec combinations: the default callback may pick a software
/// format and decode on the CPU.
///
/// # Safety
///
/// `avctx` must be null or point to a valid `AVCodecContext`; `formats` must be
/// null or point to a valid `AV_PIX_FMT_NONE`-terminated array of
/// `AVPixelFormat`. Both are only read during the call.
unsafe extern "C" fn select_hw_format(
	avctx: *mut sys::AVCodecContext,
	formats: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
	unsafe {
	use sys::AVPixelFormat;
	if avctx.is_null() || formats.is_null() {
		return AVPixelFormat::AV_PIX_FMT_NONE;
	}
	let mut first = AVPixelFormat::AV_PIX_FMT_NONE;
	let mut cursor = formats;
	while *cursor != AVPixelFormat::AV_PIX_FMT_NONE {
		if first == AVPixelFormat::AV_PIX_FMT_NONE {
			first = *cursor;
		}
		cursor = cursor.add(1);
	}
	let device = (*avctx).hw_device_ctx;
	if device.is_null() || (*device).data.is_null() {
		return first_software_format(formats);
	}
	let device_ctx = (*device).data as *mut sys::AVHWDeviceContext;
	let ctx_type = (*device_ctx).type_;
	let Some(hw_fmt) = hw_pixel_format(ctx_type) else {
		return first_software_format(formats);
	};
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!("[hwfmt] ctx_type={ctx_type:?} hw_fmt={hw_fmt:?} first={first:?}");
	}
	let mut offered = formats;
	while *offered != AVPixelFormat::AV_PIX_FMT_NONE {
		if *offered == hw_fmt {
			let mut frames: *mut sys::AVBufferRef = std::ptr::null_mut();
			let rc = sys::avcodec_get_hw_frames_parameters(avctx, device, hw_fmt, &mut frames);
			if rc >= 0 && !frames.is_null() {
				let frames_started = std::time::Instant::now();
				let init = sys::av_hwframe_ctx_init(frames);
				if std::env::var_os("OAK_DEBUG_HW").is_some() {
					eprintln!(
						"[hwfmt] frames init {hw_fmt:?} took {}ms rc={init}",
						frames_started.elapsed().as_millis()
					);
				}
				if init >= 0 {
					// Ownership transfers to the codec context; libavcodec
					// unrefs it after decoding / on close.
					(*avctx).hw_frames_ctx = frames;
					if std::env::var_os("OAK_DEBUG_HW").is_some() {
						eprintln!("[hwfmt] selected {hw_fmt:?}");
					}
					return hw_fmt;
				}
				if std::env::var_os("OAK_DEBUG_HW").is_some() {
					eprintln!("[hwfmt] frames init failed rc={init} for {hw_fmt:?}");
				}
				sys::av_buffer_unref(&mut frames);
			}
			HW_FORMAT_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			// Do NOT return `first`: it may be another hardware format
			// (commonly CUDA offered before VAAPI) while `hw_device_ctx`
			// is VAAPI. Returning it makes libavcodec abort with
			// "format cuda does not match the device context". Returning
			// NONE forces the first-frame hardware fallback, which can
			// retry the next device type before settling on software.
			return AVPixelFormat::AV_PIX_FMT_NONE;
		}
		offered = offered.add(1);
	}
	// The device is attached but the decoder does not offer its hardware
	// pixel format for this stream (e.g. VAAPI has no 10-bit 4:2:2 H.264
	// config. Returning software here would silently keep the hardware
	// session but decode on the CPU. Force the first-frame fallback instead;
	// it marks this device unavailable and retries the next candidate
	// (NVDEC/CUDA), finally settling on software only if none works.
	HW_FORMAT_FALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	AVPixelFormat::AV_PIX_FMT_NONE
	}

}

fn attach_hw_get_format(avctx: *mut sys::AVCodecContext) {
	unsafe {
		(*avctx).get_format = Some(select_hw_format);
	}
}

/// Try to open `codec` with a hardware device of `device_type`.
///
/// Returns the opened decoder context plus the device type in use, or `None`
/// when the device is unavailable (the caller tries the next candidate, then
/// pure software).
pub fn open_hw_accel(
	params: &ffmpeg::codec::Parameters,
	codec: ffmpeg::Codec,
	device_type: sys::AVHWDeviceType,
) -> Option<(ffmpeg::codec::decoder::Opened, sys::AVHWDeviceType)> {
	// Negative cache: a device type whose context creation failed once
	// (no driver, headless box) is never tried again — creation is slow
	// and logs `[VAAPI @ ...] Failed to initialise VAAPI connection` per
	// attempt. Checked before any FFmpeg call so marked types cost
	// nothing.
	if device_unavailable(device_type) {
		return None;
	}
	#[cfg(test)]
	note_creation_attempt();
	let mut context = ffmpeg::codec::Context::from_parameters(params.clone()).ok()?;
	let mut device: *mut sys::AVBufferRef = std::ptr::null_mut();
	let device_started = std::time::Instant::now();
	let rc = if device_type == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA {
		device = shared_cuda_device().unwrap_or(std::ptr::null_mut());
		if device.is_null() { -1 } else { 0 }
	} else {
		// SAFETY: `device` is a valid out-pointer; on success it owns the
		// device reference, which is handed to the codec context below.
		unsafe {
			sys::av_hwdevice_ctx_create(
				&mut device,
				device_type,
				std::ptr::null(),
				std::ptr::null_mut(),
				0,
			)
		}
	};
	if rc < 0 || device.is_null() {
		// The device is unavailable (missing driver / no hardware): mark
		// it so later opens skip the attempt and its log noise.
		HW_OPEN_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		mark_device_unavailable(device_type);
		return None;
	}
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!(
			"[hwfmt] device create {device_type:?} took {}ms",
			device_started.elapsed().as_millis()
		);
	}
	// SAFETY: `hw_device_ctx` takes ownership of the reference; the codec
	// context frees it with the context.
	unsafe { (*context.as_mut_ptr()).hw_device_ctx = device };
	unsafe { attach_hw_get_format(context.as_mut_ptr()); }
	let mut opts = Dictionary::new();
	opts.set("threads", &crate::ffmpeg::decoder_threads());
	let decoder_started = std::time::Instant::now();
	let opened = match context.decoder().open_as_with(codec, opts) {
		Ok(opened) => opened,
		Err(_) => {
			HW_OPEN_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			// Decoder creation is codec-specific. Do not poison the whole
			// device type: VideoToolbox may reject MPEG-2 and still decode
			// H.264 perfectly well on the next session.
			return None;
		}
	};
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!(
			"[hwfmt] decoder open {device_type:?} took {}ms",
			decoder_started.elapsed().as_millis()
		);
	}
	HW_DECODER_OPENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	Some((opened, device_type))
}

fn shared_cuda_device() -> Option<*mut sys::AVBufferRef> {
	static DEVICE: std::sync::OnceLock<std::sync::Mutex<Option<usize>>> =
		std::sync::OnceLock::new();
	let mut cached = DEVICE
		.get_or_init(|| std::sync::Mutex::new(None))
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	if cached.is_none() {
		let mut device: *mut sys::AVBufferRef = std::ptr::null_mut();
		let rc = unsafe {
			sys::av_hwdevice_ctx_create(
				&mut device,
				sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
				std::ptr::null(),
				std::ptr::null_mut(),
				0,
			)
		};
		if rc < 0 || device.is_null() {
			return None;
		}
		*cached = Some(device as usize);
	}
	let device = cached.unwrap() as *mut sys::AVBufferRef;
	let reference = unsafe { sys::av_buffer_ref(device) };
	(!reference.is_null()).then_some(reference)
}

/// Whether a decoded frame's pixel format is a hardware surface that
/// must be transferred to system memory before swscale can consume it.
pub fn is_hw_format(format: sys::AVPixelFormat) -> bool {
	matches!(
		format,
		sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
			| sys::AVPixelFormat::AV_PIX_FMT_VAAPI
			| sys::AVPixelFormat::AV_PIX_FMT_CUDA
			| sys::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD
			| sys::AVPixelFormat::AV_PIX_FMT_D3D11
	)
}

/// Transfer a hardware frame to system memory (`av_hwframe_transfer_data`
/// picks the software format: NV12 for 8-bit, P010LE for 10-bit sources;
/// swscale consumes both). Presentation metadata the frame cache relies
/// on is carried over.
pub fn transfer_to_cpu(frame: &ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video> {
	let mut cpu = ffmpeg::frame::Video::empty();
	// SAFETY: `cpu` and `frame` are valid AVFrames; flags 0 = default
	// transfer direction (hardware -> system memory).
	let rc = unsafe { sys::av_hwframe_transfer_data(cpu.as_mut_ptr(), frame.as_ptr(), 0) };
	if rc < 0 {
		return Err(Error::Failed(format!(
			"hwframe transfer to CPU failed (av error {rc})"
		)));
	}
	HW_TRANSFERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	cpu.set_pts(frame.pts());
	// SAFETY: plain field copies between valid AVFrames.
	unsafe {
		(*cpu.as_mut_ptr()).pkt_dts = (*frame.as_ptr()).pkt_dts;
		(*cpu.as_mut_ptr()).duration = (*frame.as_ptr()).duration;
		(*cpu.as_mut_ptr()).time_base = (*frame.as_ptr()).time_base;
	}
	Ok(cpu)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The current platform has at least one hardware device candidate
	/// (hardware decode is the mandated default on every supported
	/// platform).
	#[test]
	fn platform_has_a_hardware_candidate() {
		assert!(
			!device_type_candidates().is_empty(),
			"no hardware decode candidate on this platform"
		);
	}

	/// Device types round-trip through their display names.
	#[test]
	fn device_type_names_are_stable() {
		assert_eq!(
			device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX),
			"videotoolbox"
		);
		assert_eq!(device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI), "vaapi");
		assert_eq!(device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA), "cuda/nvdec");
		assert_eq!(
			device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA),
			"d3d11va"
		);
	}

	/// Software and hardware pixel formats are classified correctly.
	#[test]
	fn hw_format_classification() {
		assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX));
		assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_VAAPI));
		assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_CUDA));
		assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD));
		assert!(!is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_YUV420P));
		assert!(!is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_NV12));
	}

	/// On macOS the VideoToolbox device context must be creatable (the
	/// mandated default decode path) and the H.264 demo must open with
	/// the hwaccel attached.
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_device_opens_for_h264() {
		let mut input = ffmpeg::format::input(&"../../testdata/demo.mp4".to_string())
			.expect("open demo.mp4");
		let fstream = input.stream(0).expect("stream 0");
		let params = fstream.parameters();
		let codec = ffmpeg::decoder::find(params.id()).expect("software h264 codec");
		let opened = open_hw_accel(
			&params,
			codec,
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
		);
		assert!(opened.is_some(), "VideoToolbox hwaccel must open for H.264");
	}

	/// A device type marked unavailable is short-circuited before any
	/// FFmpeg call: `open_hw_accel` returns None without attempting device
	/// creation (which is what used to log
	/// `[VAAPI @ ...] Failed to initialise VAAPI connection` every time on
	/// boxes without the driver).
	#[test]
	fn negative_cache_skips_marked_device() {
		// Serialize with the media tests that hold the shared test lock
		// (the counter itself is thread-local, but the negative cache is
		// process-wide).
		let _lock = crate::lock_tests();
		// VDPAU is not a candidate on any supported platform, so marking
		// it cannot disturb the platform tests in this process (e.g. the
		// macOS VideoToolbox test above).
		let dev = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU;
		mark_device_unavailable(dev);
		assert!(device_unavailable(dev));

		let input = ffmpeg::format::input(&"../../testdata/demo.mp4".to_string())
			.expect("open demo.mp4");
		let fstream = input.stream(0).expect("stream 0");
		let params = fstream.parameters();
		let codec = ffmpeg::decoder::find(params.id()).expect("software h264 codec");

		reset_creation_attempts();
		let opened = open_hw_accel(&params, codec, dev);
		assert!(opened.is_none(), "marked device must not open");
		assert_eq!(
			creation_attempts(),
			0,
			"marked device must be skipped before any creation attempt"
		);

		// Restore so the (non-candidate) type stays clean for other tests.
		UNAVAILABLE_DEVICES[dev as usize].store(false, std::sync::atomic::Ordering::Relaxed);
	}

	/// A device type never tried before is not cached as unavailable
	/// (only a failed creation marks it). DXVA2 is not a candidate on any
	/// supported platform (Windows uses D3D11VA/CUDA), so it is never
	/// touched by other tests in this process.
	#[test]
	fn unmarked_device_type_is_not_unavailable() {
		let dev = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2;
		assert!(!device_unavailable(dev));
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn cuda_device_context_is_shared_by_reference() {
		let Some(mut first) = shared_cuda_device() else {
			eprintln!("no CUDA device; skipping shared-context test");
			return;
		};
		let Some(mut second) = shared_cuda_device() else {
			panic!("second CUDA device reference failed");
		};
		unsafe {
			assert_eq!((*first).buffer, (*second).buffer);
		}
		unsafe {
			sys::av_buffer_unref(&mut first);
			sys::av_buffer_unref(&mut second);
		}
	}
}
