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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
/// Hardware decode errors observed on a session (frame-level; the L1 policy's
/// input). Distinct from [`HW_OPEN_FAILURES`] (device/session creation).
pub static HW_DECODE_FAILURES: std::sync::atomic::AtomicU64 =
	std::sync::atomic::AtomicU64::new(0);
/// Hardware session restarts performed by the L1 retry policy.
pub static HW_SESSION_RESTARTS: std::sync::atomic::AtomicU64 =
	std::sync::atomic::AtomicU64::new(0);
/// Frames decoded through the per-frame software fallback while the hardware
/// session stayed alive (L1).
pub static HW_FRAME_FALLBACKS: std::sync::atomic::AtomicU64 =
	std::sync::atomic::AtomicU64::new(0);
/// Streams pinned to software by the L2 policy.
pub static HW_STREAM_PINS: std::sync::atomic::AtomicU64 =
	std::sync::atomic::AtomicU64::new(0);
/// Formats degraded by the L3 ratio breaker.
pub static HW_FORMAT_DEGRADES: std::sync::atomic::AtomicU64 =
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
///
/// This is the *device* half of the negative cache: it must only be
/// written when `av_hwdevice_ctx_create` fails (no driver / no hardware).
/// A frame-level decode error is a property of the (device, format) pair —
/// see [`HwFormatKey`] and [`note_format_attempt`].
pub fn mark_device_unavailable(device_type: sys::AVHWDeviceType) {
    if let Some(f) = UNAVAILABLE_DEVICES.get(device_type as usize) {
        f.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Decode-failure policy (L0 / L1 / L2 / L3)
// ---------------------------------------------------------------------------
//
// One bad frame must not disable hardware decoding for the whole process, and
// a file that genuinely cannot be hardware-decoded must not be retried on
// every frame. The policy is layered:
//
// * **L0** — two negative caches with different scopes: [`device_unavailable`]
//   (device-context creation failed; process-wide, sticky) and
//   [`format_degraded`] (a device+format combination whose decode attempts
//   mostly fail; process-wide, TTL-bounded).
// * **L1** — per stream: a hardware decode error restarts the same device and
//   retries the request. The request path re-seeks to the preceding keyframe,
//   so the retry is inherently keyframe-aligned. The restart budget bounds
//   how often that may happen.
// * **L2** — per stream: consecutive failures or an exhausted restart budget
//   pin the stream to software for its remaining frames.
// * **L3** — process-wide: a sliding window of attempts per [`HwFormatKey`]
//   degrades the format when its hardware success rate stays below
//   [`FORMAT_DEGRADE_SUCCESS_PERCENT`]; the entry expires after
//   [`FORMAT_DEGRADED_TTL`] and is probed again.

/// L1: hardware session restarts allowed inside one rolling window of
/// [`HW_RESTART_WINDOW_FRAMES`] decoded frames before the stream is pinned.
///
/// A 4K 4:2:2 10-bit H.264 stream that VideoToolbox can decode needs a
/// session restart on roughly 4 % of its frames (scene changes / reconfigs),
/// so a clip-level budget of 32 restarts is enough; the window keeps a long
/// healthy session from accumulating restarts forever.
pub const MAX_HW_SESSION_RESTARTS: u32 = 32;

/// L1: decoded frames after which the restart counter is reset (the rolling
/// window for [`MAX_HW_SESSION_RESTARTS`]).
pub const HW_RESTART_WINDOW_FRAMES: u32 = 300;

/// L2: consecutive hardware decode failures after which the stream stops
/// being retried (a frame fallback is still allowed until its own budget is
/// spent).
///
/// Measured on the 4K 4:2:2 10-bit H.264 sample: failures arrive in bursts of
/// 3-4 at specific timestamps, and restarting the session re-seeks to the same
/// frame, which fails again. The frame-level software fallback therefore does
/// the real recovery; this budget only bounds a stream that fails *every*
/// frame (a truly broken configuration).
pub const MAX_CONSECUTIVE_DECODE_FAILURES: u32 = 8;

/// L1: per-frame software fallbacks allowed per stream before the stream is
/// pinned. Reserved for the frame-level path (decode one frame in software and
/// keep hardware for the rest); the current implementation pins the stream
/// instead of carrying two sessions.
pub const MAX_FRAME_FALLBACKS_PER_STREAM: u32 = 10;

/// L3: attempts in the sliding window before the success ratio is judged.
pub const FORMAT_WINDOW_ATTEMPTS: u32 = 20;

/// L3: hardware success rate (percent) below which the format is degraded.
pub const FORMAT_DEGRADE_SUCCESS_PERCENT: u32 = 20;

/// L3: how long a degraded format stays degraded before it is probed again.
pub const FORMAT_DEGRADED_TTL: Duration = Duration::from_secs(600);

/// Whether the bounded hardware-retry policy is active. `QUERCUS_HW_RETRY=0`
/// restores the historical behaviour (the first decode error pins the stream
/// to software) — the field escape hatch for a machine where retrying costs
/// more than falling back.
pub fn hw_retry_enabled() -> bool {
	std::env::var("QUERCUS_HW_RETRY").map(|value| value != "0").unwrap_or(true)
}

/// Identity of one hardware decode configuration: the device plus the codec
/// profile and source pixel format it is asked to decode.
///
/// The format-level negative cache is keyed on this rather than on the device
/// alone: a device can reject one profile (e.g. H.264 High 4:2:2 10-bit) and
/// still decode another (High 4:2:0 8-bit) perfectly well.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HwFormatKey {
	/// `AVHWDeviceType` as its raw integer value.
	pub device: i32,
	/// `AVCodecID` of the source stream.
	pub codec_id: i32,
	/// `AVProfile` of the source stream.
	pub profile: i32,
	/// `AVPixelFormat` of the source stream.
	pub pixel_format: i32,
}

impl HwFormatKey {
	/// Build the key for `device` decoding `codec_id` / `profile` from
	/// `pixel_format`.
	pub fn new(
		device: sys::AVHWDeviceType,
		codec_id: i32,
		profile: i32,
		pixel_format: i32,
	) -> Self {
		Self { device: device as i32, codec_id, profile, pixel_format }
	}
}

/// Per-format hardware health: a sliding success window plus the degradation
/// deadline.
#[derive(Debug)]
struct FormatHealth {
	attempts: u32,
	successes: u32,
	degraded_until: Option<Instant>,
}

impl Default for FormatHealth {
	fn default() -> Self {
		Self { attempts: 0, successes: 0, degraded_until: None }
	}
}

/// The process-wide format-level negative cache (L0/L3).
static FORMAT_HEALTH: OnceLock<Mutex<HashMap<HwFormatKey, FormatHealth>>> = OnceLock::new();

fn format_health() -> &'static Mutex<HashMap<HwFormatKey, FormatHealth>> {
	FORMAT_HEALTH.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `key`'s hardware decode is currently degraded — the candidate scan
/// skips it. An expired entry clears itself and is probed again.
pub fn format_degraded(key: &HwFormatKey) -> bool {
	let mut table = format_health().lock().unwrap_or_else(|e| e.into_inner());
	match table.get_mut(key) {
		Some(entry) => match entry.degraded_until {
			Some(until) if Instant::now() < until => true,
			Some(_) => {
				entry.degraded_until = None;
				entry.attempts = 0;
				entry.successes = 0;
				false
			}
			None => false,
		},
		None => false,
	}
}

/// Record one hardware decode attempt (`success` = a hardware frame was
/// produced) for `key`. Once the window is full, a success rate below
/// [`FORMAT_DEGRADE_SUCCESS_PERCENT`] degrades the format for
/// [`FORMAT_DEGRADED_TTL`] and resets the window.
pub fn note_format_attempt(key: &HwFormatKey, success: bool) {
	let mut table = format_health().lock().unwrap_or_else(|e| e.into_inner());
	let entry = table.entry(*key).or_default();
	entry.attempts = entry.attempts.saturating_add(1);
	if success {
		entry.successes = entry.successes.saturating_add(1);
	}
	if entry.attempts < FORMAT_WINDOW_ATTEMPTS {
		return;
	}
	let percent = entry.successes.saturating_mul(100) / entry.attempts;
	if percent < FORMAT_DEGRADE_SUCCESS_PERCENT {
		entry.degraded_until = Some(Instant::now() + FORMAT_DEGRADED_TTL);
		HW_FORMAT_DEGRADES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		eprintln!(
			"[hwdecode] format {:?} degraded for {}s (hardware success {}% over {} attempts)",
			key,
			FORMAT_DEGRADED_TTL.as_secs(),
			percent,
			entry.attempts
		);
	}
	entry.attempts = 0;
	entry.successes = 0;
}

/// Hardware formats whose decoded frames cannot be imported into the render
/// GPU. Every frame then pays an `av_hwframe_transfer_data` download, which
/// on 4K 4:2:2 10-bit costs more than software decoding the stream (measured
/// on the 4K 4:2:2 sample: hardware was ~5 % slower end to end). The
/// heuristic below flips those formats to software after a couple of failed
/// imports.
fn hw_frame_download_is_pessimization(pixel_format: i32) -> bool {
	pixel_format == sys::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32
		|| pixel_format == sys::AVPixelFormat::AV_PIX_FMT_YUV422P10BE as i32
}

/// Consecutive import failures after which the format prefers software.
pub const IMPORT_UNSUPPORTED_LIMIT: u32 = 2;

/// Per-format consecutive unsupported-import counts (L0 heuristic input).
static IMPORT_UNSUPPORTED: OnceLock<Mutex<HashMap<HwFormatKey, u32>>> = OnceLock::new();
/// Formats currently preferring software decode, with the deadline at which
/// the heuristic is re-probed.
static PREFER_SOFTWARE: OnceLock<Mutex<HashMap<HwFormatKey, Instant>>> = OnceLock::new();

fn import_unsupported_counts() -> &'static Mutex<HashMap<HwFormatKey, u32>> {
	IMPORT_UNSUPPORTED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prefer_software_table() -> &'static Mutex<HashMap<HwFormatKey, Instant>> {
	PREFER_SOFTWARE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that a hardware frame for `key` could not be imported into the GPU
/// (zero-copy unavailable). Formats with a known-expensive download switch to
/// software after [`IMPORT_UNSUPPORTED_LIMIT`] consecutive failures.
pub fn note_hw_import_unsupported(key: &HwFormatKey) {
	if !hw_frame_download_is_pessimization(key.pixel_format) {
		return;
	}
	let mut counts = import_unsupported_counts()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let count = counts.entry(*key).or_insert(0);
	*count = count.saturating_add(1);
	if *count < IMPORT_UNSUPPORTED_LIMIT {
		return;
	}
	prefer_software_table()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(*key, Instant::now() + FORMAT_DEGRADED_TTL);
	eprintln!(
		"[hwdecode] format {:?} prefers software decode: hardware frames cannot be imported \
		 (4:2:2 10-bit download costs more than decoding)",
		key
	);
}

/// Whether `key` currently prefers software decode because its hardware
/// frames cannot be imported (auto-expires, then hardware is probed again).
pub fn prefer_software_for(key: &HwFormatKey) -> bool {
	let mut table = prefer_software_table()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	match table.get(key).copied() {
		Some(until) if Instant::now() < until => true,
		Some(_) => {
			table.remove(key);
			false
		}
		None => false,
	}
}

/// Per-stream hardware decode health (L1/L2).
///
/// Owned by one `(filename, stream)` decode session and preserved across
/// hardware session restarts, so reopening the same device does not reset the
/// retry budget.
#[derive(Clone, Debug, Default)]
pub struct HwStreamHealth {
	consecutive_failures: u32,
	session_restarts: u32,
	frame_fallbacks: u32,
	frames_since_restart_window: u32,
	pinned: bool,
}

impl HwStreamHealth {
	/// A fresh, unpinned stream.
	pub fn new() -> Self {
		Self::default()
	}

	/// Whether hardware decoding may still be attempted for this stream.
	pub fn hardware_allowed(&self) -> bool {
		!self.pinned
	}

	/// Whether the stream has been pinned to software.
	pub fn pinned(&self) -> bool {
		self.pinned
	}

	/// Consecutive hardware decode failures (resets on the next success).
	pub fn consecutive_failures(&self) -> u32 {
		self.consecutive_failures
	}

	/// Session restarts spent in the current rolling window.
	pub fn session_restarts(&self) -> u32 {
		self.session_restarts
	}

	/// Frame-level software fallbacks spent on this stream (reserved, L1).
	pub fn frame_fallbacks(&self) -> u32 {
		self.frame_fallbacks
	}

	/// A hardware frame decoded: the failure streak is over.
	pub fn on_decode_success(&mut self) {
		self.consecutive_failures = 0;
		self.frames_since_restart_window = self.frames_since_restart_window.saturating_add(1);
		if self.frames_since_restart_window >= HW_RESTART_WINDOW_FRAMES {
			self.frames_since_restart_window = 0;
			self.session_restarts = 0;
		}
	}

	/// Record a hardware decode failure (the failure streak grows).
	pub fn note_decode_failure(&mut self) {
		self.consecutive_failures = self.consecutive_failures.saturating_add(1);
	}

	/// Ask to restart the hardware session for the failed request. Returns
	/// `false` when the L1 retry budgets are exhausted — the caller then tries
	/// a per-frame software decode, and finally pins the stream.
	pub fn admit_restart(&mut self) -> bool {
		if self.pinned
			|| !hw_retry_enabled()
			|| self.consecutive_failures >= MAX_CONSECUTIVE_DECODE_FAILURES
			|| self.session_restarts >= MAX_HW_SESSION_RESTARTS
		{
			return false;
		}
		self.session_restarts = self.session_restarts.saturating_add(1);
		true
	}

	/// Ask to decode one frame in software while the hardware session stays
	/// alive for the following frames (the L1 frame-level fallback). Returns
	/// `false` when the per-stream budget is exhausted.
	pub fn admit_frame_fallback(&mut self) -> bool {
		if self.pinned || self.frame_fallbacks >= MAX_FRAME_FALLBACKS_PER_STREAM {
			return false;
		}
		self.frame_fallbacks = self.frame_fallbacks.saturating_add(1);
		true
	}

	/// Pin this stream to software without a retry.
	pub fn pin(&mut self) {
		self.pinned = true;
	}
}

/// Snapshot of the decode-failure counters (observability: field reports,
/// `--check`-style diagnostics and the benchmark harness).
#[derive(Clone, Copy, Debug, Default)]
pub struct HwDecodeStats {
	/// Hardware decode errors observed on a session (frame-level).
	pub decode_failures: u64,
	/// Hardware session restarts performed by the L1 policy.
	pub session_restarts: u64,
	/// Frames decoded through the L1 per-frame software fallback.
	pub frame_fallbacks: u64,
	/// Streams pinned to software by the L2 policy.
	pub stream_pins: u64,
	/// Formats degraded by the L3 ratio breaker.
	pub format_degrades: u64,
	/// Hardware sessions abandoned for non-hardware output frames.
	pub hw_fallbacks: u64,
}

/// Read the decode-failure counters.
pub fn stats_snapshot() -> HwDecodeStats {
	use std::sync::atomic::Ordering;
	HwDecodeStats {
		decode_failures: HW_DECODE_FAILURES.load(Ordering::Relaxed),
		session_restarts: HW_SESSION_RESTARTS.load(Ordering::Relaxed),
		frame_fallbacks: HW_FRAME_FALLBACKS.load(Ordering::Relaxed),
		stream_pins: HW_STREAM_PINS.load(Ordering::Relaxed),
		format_degrades: HW_FORMAT_DEGRADES.load(Ordering::Relaxed),
		hw_fallbacks: HW_FALLBACKS.load(Ordering::Relaxed),
	}
}

#[cfg(test)]
fn reset_format_health() {
	format_health().lock().unwrap_or_else(|e| e.into_inner()).clear();
	import_unsupported_counts()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
	prefer_software_table()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
}

/// Test helper: move `key`'s degradation deadline into the past so the TTL
/// expiry path can be exercised without sleeping.
#[cfg(test)]
fn expire_format(key: &HwFormatKey) {
	if let Some(entry) =
		format_health().lock().unwrap_or_else(|e| e.into_inner()).get_mut(key)
	{
		let past = Instant::now()
			.checked_sub(Duration::from_secs(1))
			.unwrap_or_else(Instant::now);
		entry.degraded_until = Some(past);
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
pub(crate) fn hw_pixel_format(device_type: sys::AVHWDeviceType) -> Option<sys::AVPixelFormat> {
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
	opts.set("threads", "1");
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
		let Some((opened, _device)) = opened else {
			// Headless runners and FFmpeg builds without VideoToolbox support
			// use the mandated software fallback; there is no device to assert.
			return;
		};
		assert_eq!(
			unsafe { (*opened.as_ptr()).thread_count },
			1,
			"hardware decoding stays single-threaded"
		);
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

	/// L1: restarts are admitted inside the failure-streak budget; once the
	/// streak is exhausted the per-frame fallback takes over instead of
	/// pinning the whole stream.
	#[test]
	fn restart_budget_then_frame_fallback() {
		let mut health = HwStreamHealth::new();
		for _ in 0..(MAX_CONSECUTIVE_DECODE_FAILURES - 1) {
			health.note_decode_failure();
			assert!(health.admit_restart(), "a restart is admitted inside the budget");
		}
		health.note_decode_failure();
		assert!(!health.admit_restart(), "the failure streak stops restarts");
		assert!(health.admit_frame_fallback(), "the frame fallback still applies");
		assert!(!health.pinned(), "a burst of failures alone must not pin the stream");
	}

	/// L1: the per-frame fallback budget is bounded; once spent, the caller
	/// pins the stream.
	#[test]
	fn frame_fallback_budget_is_bounded() {
		let mut health = HwStreamHealth::new();
		for _ in 0..MAX_FRAME_FALLBACKS_PER_STREAM {
			assert!(health.admit_frame_fallback());
		}
		assert_eq!(health.frame_fallbacks(), MAX_FRAME_FALLBACKS_PER_STREAM);
		assert!(!health.admit_frame_fallback());
		health.pin();
		assert!(!health.admit_frame_fallback());
		assert!(!health.admit_restart());
		assert!(!health.hardware_allowed());
	}

	/// A decoded hardware frame clears the failure streak, so an isolated
	/// reconfig (the VideoToolbox 4:2:2 10-bit case) does not exhaust the
	/// retry budget.
	#[test]
	fn stream_health_success_resets_the_streak() {
		let mut health = HwStreamHealth::new();
		health.note_decode_failure();
		health.on_decode_success();
		assert_eq!(health.consecutive_failures(), 0);
		health.note_decode_failure();
		assert!(health.admit_restart());
		assert!(!health.pinned());
	}

	/// The restart budget rolls over after a healthy window, so a long healthy
	/// session cannot accumulate restarts until it stops retrying.
	#[test]
	fn stream_health_restart_budget_rolls_over() {
		let mut health = HwStreamHealth::new();
		health.note_decode_failure();
		assert!(health.admit_restart());
		assert_eq!(health.session_restarts(), 1);
		for _ in 0..HW_RESTART_WINDOW_FRAMES {
			health.on_decode_success();
		}
		assert_eq!(health.session_restarts(), 0, "the window resets the restart counter");
		health.note_decode_failure();
		assert!(health.admit_restart());
	}

	/// L3: a format whose hardware attempts mostly fail is degraded, and the
	/// degradation expires so the format is probed again.
	#[test]
	fn format_health_degrades_and_expires() {
		let _lock = crate::lock_tests();
		reset_format_health();
		// VDPAU is not a platform candidate, so this key cannot disturb the
		// real decode tests in this process.
		let key =
			HwFormatKey::new(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU, 27, 122, 0);
		for _ in 0..FORMAT_WINDOW_ATTEMPTS {
			note_format_attempt(&key, false);
		}
		assert!(format_degraded(&key));
		expire_format(&key);
		assert!(!format_degraded(&key), "an expired degradation must be probed again");
		reset_format_health();
	}

	/// 4:2:2 10-bit heuristic: a stream whose hardware frames keep failing
	/// the GPU import switches to software; other formats are unaffected.
	#[test]
	fn unimportable_hw_frames_prefer_software() {
		let _lock = crate::lock_tests();
		reset_format_health();
		let key = HwFormatKey::new(
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
			27,
			122,
			sys::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32,
		);
		assert!(!prefer_software_for(&key));
		note_hw_import_unsupported(&key);
		assert!(!prefer_software_for(&key), "one failure is not enough");
		note_hw_import_unsupported(&key);
		assert!(prefer_software_for(&key), "repeated import failures flip the format");

		// 4:2:0 8-bit hardware frames are not flipped by the heuristic.
		let other = HwFormatKey::new(
			sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
			27,
			100,
			sys::AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
		);
		note_hw_import_unsupported(&other);
		note_hw_import_unsupported(&other);
		assert!(!prefer_software_for(&other));
		reset_format_health();
	}

	/// L3: a format whose hardware attempts mostly succeed is never degraded.
	#[test]
	fn format_health_keeps_healthy_format() {
		let _lock = crate::lock_tests();
		reset_format_health();
		let key = HwFormatKey::new(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2, 27, 100, 0);
		let half = FORMAT_WINDOW_ATTEMPTS / 2;
		for _ in 0..half {
			note_format_attempt(&key, false);
		}
		for _ in 0..(FORMAT_WINDOW_ATTEMPTS - half) {
			note_format_attempt(&key, true);
		}
		assert!(!format_degraded(&key));
		reset_format_health();
	}
}
