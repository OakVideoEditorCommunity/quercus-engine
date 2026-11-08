//! Owned audio-analysis results and their C ABI.
//!
//! Waveform extraction itself lives in quercus-audio. This module only gives the
//! result a stable opaque handle and POD queries, allowing SDK consumers to use
//! decoded peaks without depending on quercus-audio or codec stream types.

use crate::handle::NativeMirror;
use quercus_audio::waveform::SamplePerChannel;

/// One channel's amplitude range within one waveform window.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusWaveformPoint {
	/// Minimum sample amplitude in this window.
	pub min: f32,
	/// Maximum sample amplitude in this window.
	pub max: f32,
}

impl NativeMirror for QuercusWaveformPoint {
	type Native = SamplePerChannel;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			min: native.min,
			max: native.max,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			min: self.min,
			max: self.max,
		}
	}
}

/// Immutable decoded waveform owned by a reference-counted handle.
#[derive(Clone, Debug)]
pub struct WaveformData {
	pub channels: i32,
	pub sample_rate: i32,
	pub samples_per_point: i32,
	/// Channel-interleaved points: `point * channels + channel`.
	pub points: Vec<SamplePerChannel>,
}

/// Waveform layout metadata without the variable-length point buffer.
/// This is the lossless native counterpart of [`QuercusWaveformInfo`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaveformMetadata {
	/// Number of interleaved audio channels.
	pub channels: i32,
	/// Actual source sample rate in Hz.
	pub sample_rate: i32,
	/// Source frames summarized by each point.
	pub samples_per_point: i32,
	/// Number of windows per channel.
	pub point_count: usize,
}

impl WaveformData {
	fn metadata(&self) -> WaveformMetadata {
		WaveformMetadata {
			channels: self.channels,
			sample_rate: self.sample_rate,
			samples_per_point: self.samples_per_point,
			point_count: self.points.len() / self.channels.max(1) as usize,
		}
	}
}

/// Opaque C handle containing one immutable [`WaveformData`].
pub struct WaveformHandle {
	pub(crate) data: WaveformData,
}

crate::export_handle!(WaveformHandle, quercus_waveform_add_ref, quercus_waveform_release);

impl WaveformHandle {
	/// Move `data` into a new reference-counted C handle and return its raw
	/// pointer.
	///
	/// The pointer owns one strong reference (an [`std::sync::Arc`] created by
	/// [`crate::handle::into_ffi`]); the C caller must pass it to
	/// `quercus_waveform_release` exactly once. No copy is made — the peak buffer
	/// moves into the handle.
	pub(crate) fn from_data(data: WaveformData) -> *const Self {
		crate::handle::into_ffi(Self { data })
	}
}

/// Decodes a complete audio stream into channel-interleaved min/max peaks.
///
/// This is synchronous by design; the task wrapper in `task.rs` calls it on an
/// engine worker. Invalid paths, missing streams and decode failures include
/// the underlying error instead of being converted into an empty waveform.
pub(crate) fn extract_waveform(
	filename: &str,
	stream_index: i32,
	samples_per_point: i32,
) -> Result<WaveformData, String> {
	if filename.is_empty() || stream_index < 0 || samples_per_point <= 0 {
		return Err("invalid waveform extraction arguments".into());
	}
	let filename_c = std::ffi::CString::new(filename)
		.map_err(|_| "media filename contains an interior NUL".to_string())?;
	let outcome = quercus_audio::waveform::extract(&filename_c, stream_index, samples_per_point)
		.map_err(|e| e.to_string())?;
	if outcome.channels <= 0 || outcome.sample_rate <= 0 || outcome.points.is_empty() {
		return Err("the audio stream produced no waveform peaks".into());
	}
	Ok(WaveformData {
		channels: outcome.channels,
		sample_rate: outcome.sample_rate,
		samples_per_point,
		points: outcome.points,
	})
}

/// Decode a whole audio stream into an owned waveform handle, synchronously.
///
/// `filename`/`filename_len` are a UTF-8 path pair; `stream_index` selects
/// the audio stream (0-based; negative values fail) and `samples_per_point`
/// is the number of source frames summarized by each peak (must be > 0).
/// Returns a handle owning the channel-interleaved peaks (query it with
/// `quercus_waveform_info` / `quercus_waveform_point_at`) or null when the
/// path is null, empty or not UTF-8, the arguments are out of range, or
/// decoding produces no peaks. The caller owns one reference and must call
/// `quercus_waveform_release`; UI code should use
/// `quercus_task_waveform_extract` instead of blocking here.
///
/// # Safety
///
/// `filename` must point to `filename_len` readable bytes that stay valid for
/// the call and contain valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_waveform_extract(
	filename: *const u8,
	filename_len: usize,
	stream_index: i32,
	samples_per_point: i32,
) -> *const WaveformHandle {
	let Some(filename) = (unsafe { crate::codec::str_arg(filename, filename_len) }) else {
		return std::ptr::null();
	};
	extract_waveform(filename, stream_index, samples_per_point)
		.ok()
		.map(WaveformHandle::from_data)
		.unwrap_or(std::ptr::null())
}

/// Writes `(channels, sample_rate, samples_per_point, point_count)` metadata.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusWaveformInfo {
	/// Number of interleaved audio channels.
	pub channels: i32,
	/// Actual source sample rate in Hz.
	pub sample_rate: i32,
	/// Source frames summarized by each point.
	pub samples_per_point: i32,
	/// Number of windows per channel.
	pub point_count: usize,
}

impl NativeMirror for QuercusWaveformInfo {
	type Native = WaveformMetadata;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			channels: native.channels,
			sample_rate: native.sample_rate,
			samples_per_point: native.samples_per_point,
			point_count: native.point_count,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			channels: self.channels,
			sample_rate: self.sample_rate,
			samples_per_point: self.samples_per_point,
			point_count: self.point_count,
		}
	}
}

/// Write a waveform handle's `(channels, sample_rate, samples_per_point,
/// point_count)` metadata into `out`.
///
/// `point_count` is the number of windows per channel. Returns `false` and
/// leaves `out` untouched when either pointer is null; otherwise fills the
/// struct and returns `true`. The handle's data is immutable, so the query is
/// safe to repeat from multiple threads.
///
/// # Safety
///
/// `waveform` must be a live handle from
/// [`quercus_audio_waveform_extract`] (or an extra reference) and `out` must
/// point to writable storage for one [`QuercusWaveformInfo`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_waveform_info(
	waveform: *const WaveformHandle,
	out: *mut QuercusWaveformInfo,
) -> bool {
	let (Some(waveform), Some(out)) = (unsafe { waveform.as_ref() }, unsafe { out.as_mut() })
	else {
		return false;
	};
	*out = QuercusWaveformInfo::from_native(&waveform.data.metadata());
	true
}

/// Read one channel's min/max peak without exposing the native point storage.
///
/// `point` is the 0-based window index and `channel` the 0-based interleaved
/// channel (validated against the waveform's channel count); `out` receives
/// the [`QuercusWaveformPoint`]. Returns `false`, leaving `out` untouched,
/// when either pointer is null, `channel` is out of range, or
/// `point * channels + channel` is past the peak buffer (including on
/// multiply overflow).
///
/// # Safety
///
/// `waveform` must be a live handle from
/// [`quercus_audio_waveform_extract`] (or an extra reference) and `out` must
/// point to writable storage for one [`QuercusWaveformPoint`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_waveform_point_at(
	waveform: *const WaveformHandle,
	point: usize,
	channel: i32,
	out: *mut QuercusWaveformPoint,
) -> bool {
	let (Some(waveform), Some(out)) = (unsafe { waveform.as_ref() }, unsafe { out.as_mut() })
	else {
		return false;
	};
	if channel < 0 || channel >= waveform.data.channels {
		return false;
	}
	let index = point
		.checked_mul(waveform.data.channels as usize)
		.and_then(|base| base.checked_add(channel as usize));
	let Some(point) = index.and_then(|index| waveform.data.points.get(index)) else {
		return false;
	};
	*out = QuercusWaveformPoint::from_native(point);
	true
}

/// Plain waveform-correlation result shared by offset and stretch queries.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusAudioOffsetEstimate {
	/// Candidate delay relative to the reference, in source samples.
	pub offset_samples: i64,
	/// Normalized correlation confidence in `[0, 1]`.
	pub confidence: f64,
	/// Whether correlation found a usable offset.
	pub valid: bool,
}

impl NativeMirror for QuercusAudioOffsetEstimate {
	type Native = quercus_audio::waveformsync::OffsetResult;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			offset_samples: native.offset_samples,
			confidence: native.confidence,
			valid: native.valid,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			offset_samples: self.offset_samples,
			confidence: self.confidence,
			valid: self.valid,
		}
	}
}

/// Waveform-correlation result with a candidate playback-rate correction.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusAudioStretchEstimate {
	/// Candidate playback-rate correction.
	pub rate: f64,
	/// Candidate delay after applying the rate correction.
	pub offset_samples: i64,
	/// Normalized correlation confidence in `[0, 1]`.
	pub confidence: f64,
	/// Whether the rate/offset search found a usable result.
	pub valid: bool,
}

impl NativeMirror for QuercusAudioStretchEstimate {
	type Native = quercus_audio::waveformsync::StretchOffsetResult;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			rate: native.rate,
			offset_samples: native.offset_samples,
			confidence: native.confidence,
			valid: native.valid,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			rate: self.rate,
			offset_samples: self.offset_samples,
			confidence: self.confidence,
			valid: self.valid,
		}
	}
}

/// Borrow a C `(ptr, len)` `f64` buffer as a slice.
///
/// Returns `Some(&[])` for `len == 0` (even with a null pointer); for a
/// non-zero `len` returns `None` when `ptr` is null, letting the C entry
/// points fail closed instead of dereferencing bad input.
///
/// # Safety
///
/// For non-zero `len`, `ptr` must point to `len` initialized `f64` values
/// that stay valid for `'a` and are not mutated concurrently.
unsafe fn f64_slice<'a>(ptr: *const f64, len: usize) -> Option<&'a [f64]> {
	if len == 0 {
		Some(&[])
	} else {
		(!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
	}
}

/// Borrow a C `(ptr, len)` `bool` buffer as a slice.
///
/// Returns `Some(&[])` for `len == 0` (even with a null pointer); for a
/// non-zero `len` returns `None` when `ptr` is null, letting the C entry
/// points fail closed instead of dereferencing bad input.
///
/// # Safety
///
/// For non-zero `len`, `ptr` must point to `len` readable bytes that are each
/// `0` or `1` (any other byte is an invalid `bool` and undefined behaviour),
/// valid for `'a`, and not mutated concurrently.
unsafe fn bool_slice<'a>(ptr: *const bool, len: usize) -> Option<&'a [bool]> {
	if len == 0 {
		Some(&[])
	} else {
		(!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
	}
}

/// Estimate the candidate's offset from precomputed RMS/peak envelopes.
///
/// `reference`/`candidate` hold one value per envelope window and
/// `window_samples` is the number of source samples each window summarizes
/// (0 fails). `max_offset_windows` bounds the lag search to
/// `[-max_offset_windows, +max_offset_windows]` windows, and the returned
/// `offset_samples` is the best lag multiplied back by `window_samples`.
/// `reference_valid`/`candidate_valid` are optional per-window masks: an
/// empty slice means "all windows valid", and a mask whose length does not
/// match its envelope is ignored entirely. `valid = false` (offset 0) when
/// nothing correlates; `confidence` is `max(0, best_score)`.
///
/// # Safety
///
/// Each `(ptr, len)` pair must point to `len` readable elements of its stated
/// type (or have `len == 0`), and `bool` bytes must be `0` or `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_estimate_envelope_offset(
	reference: *const f64,
	reference_len: usize,
	candidate: *const f64,
	candidate_len: usize,
	reference_valid: *const bool,
	reference_valid_len: usize,
	candidate_valid: *const bool,
	candidate_valid_len: usize,
	window_samples: usize,
	max_offset_windows: i64,
) -> QuercusAudioOffsetEstimate {
	let (Some(reference), Some(candidate), Some(reference_valid), Some(candidate_valid)) = (
		unsafe { f64_slice(reference, reference_len) },
		unsafe { f64_slice(candidate, candidate_len) },
		unsafe { bool_slice(reference_valid, reference_valid_len) },
		unsafe { bool_slice(candidate_valid, candidate_valid_len) },
	) else {
		return QuercusAudioOffsetEstimate::default();
	};
	if window_samples == 0 {
		return QuercusAudioOffsetEstimate::default();
	}
	let result = quercus_audio::waveformsync::estimate_envelope_offset_valid(
		reference,
		candidate,
		reference_valid,
		candidate_valid,
		window_samples,
		max_offset_windows,
	);
	QuercusAudioOffsetEstimate::from_native(&result)
}

/// Search playback rate and offset together, for cases where correlation at
/// rate 1 is weak.
///
/// Like [`quercus_audio_estimate_envelope_offset`], but the candidate is
/// resampled for each rate in `[min_rate, max_rate]` stepping by `rate_step`
/// (all three must be finite, `min_rate > 0`, `max_rate >= min_rate`,
/// `rate_step > 0`; otherwise the invalid default is returned).
/// `window_samples` is the source samples per envelope window and
/// `max_offset_windows` bounds the lag search to `±max_offset_windows`
/// windows. Returns the best `(rate, offset_samples, confidence)`;
/// `valid = false` (and `rate = 1.0`) when no rate/lag produced a usable
/// correlation.
///
/// # Safety
///
/// The four `(ptr, len)` pairs must be readable for their lengths (or have
/// `len == 0`), and `bool` bytes must be `0` or `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_estimate_stretch_offset(
	reference: *const f64,
	reference_len: usize,
	candidate: *const f64,
	candidate_len: usize,
	reference_valid: *const bool,
	reference_valid_len: usize,
	candidate_valid: *const bool,
	candidate_valid_len: usize,
	window_samples: usize,
	max_offset_windows: i64,
	min_rate: f64,
	max_rate: f64,
	rate_step: f64,
) -> QuercusAudioStretchEstimate {
	let (Some(reference), Some(candidate), Some(reference_valid), Some(candidate_valid)) = (
		unsafe { f64_slice(reference, reference_len) },
		unsafe { f64_slice(candidate, candidate_len) },
		unsafe { bool_slice(reference_valid, reference_valid_len) },
		unsafe { bool_slice(candidate_valid, candidate_valid_len) },
	) else {
		return QuercusAudioStretchEstimate::default();
	};
	if window_samples == 0
		|| !min_rate.is_finite()
		|| !max_rate.is_finite()
		|| !rate_step.is_finite()
		|| min_rate <= 0.0
		|| max_rate < min_rate
		|| rate_step <= 0.0
	{
		return QuercusAudioStretchEstimate::default();
	}
	let result = quercus_audio::waveformsync::estimate_stretch_and_offset(
		reference,
		candidate,
		reference_valid,
		candidate_valid,
		window_samples,
		max_offset_windows,
		min_rate,
		max_rate,
		rate_step,
	);
	QuercusAudioStretchEstimate::from_native(&result)
}

/// Source-time metadata used by source-timecode placement.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusAudioSourceClip {
	pub source_start_time: crate::coretypes::QuercusRational,
	pub media_in: crate::coretypes::QuercusRational,
	pub has_source_start_time: bool,
}

impl NativeMirror for QuercusAudioSourceClip {
	type Native = quercus_audio::synchronizer::SourceClip;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			source_start_time: crate::coretypes::QuercusRational::from_native(
				&native.source_start_time,
			),
			media_in: crate::coretypes::QuercusRational::from_native(&native.media_in),
			has_source_start_time: native.has_source_start_time,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			source_start_time: self.source_start_time.to_native(),
			media_in: self.media_in.to_native(),
			has_source_start_time: self.has_source_start_time,
		}
	}
}

/// Validated timeline placement produced by either synchronization policy.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusAudioSyncPlacement {
	pub timeline_in: crate::coretypes::QuercusRational,
	pub valid: bool,
}

impl NativeMirror for QuercusAudioSyncPlacement {
	type Native = quercus_audio::synchronizer::Placement;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			timeline_in: crate::coretypes::QuercusRational::from_native(&native.timeline_in),
			valid: native.valid,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			timeline_in: self.timeline_in.to_native(),
			valid: self.valid,
		}
	}
}

/// Place the candidate on the timeline by aligning its source timecode with
/// the reference.
///
/// `reference`/`candidate` carry source start time, media in point and a
/// "source start time is set" flag. The result's `timeline_in` is
/// `reference_timeline_in + (candidate.source_start_time +
/// candidate.media_in) - (reference.source_start_time + reference.media_in)`
/// in seconds. Returns the invalid default (`valid = false`, null rational)
/// when either pointer is null, either clip lacks a source start time, a
/// rational is NaN, or the computed time is NaN.
///
/// # Safety
///
/// `reference` and `candidate` must point to readable
/// [`QuercusAudioSourceClip`] values for the duration of the call; null is
/// allowed and maps to the invalid default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_place_by_source_time(
	reference: *const QuercusAudioSourceClip,
	candidate: *const QuercusAudioSourceClip,
	reference_timeline_in: crate::coretypes::QuercusRational,
) -> QuercusAudioSyncPlacement {
	let (Some(reference), Some(candidate)) =
		(unsafe { reference.as_ref() }, unsafe { candidate.as_ref() })
	else {
		return QuercusAudioSyncPlacement::default();
	};
	QuercusAudioSyncPlacement::from_native(&quercus_audio::synchronizer::place_by_source_time(
		&reference.to_native(),
		&candidate.to_native(),
		reference_timeline_in.to_native(),
	))
}

/// Convert a measured waveform offset into a placement relative to the
/// reference clip.
///
/// `candidate_offset_samples` is the delay measured in source samples
/// (negative = candidate earlier than the reference) and `sample_rate` is the
/// source rate in Hz; the returned `timeline_in` is `reference_timeline_in +
/// offset_samples / sample_rate` seconds. Returns the invalid default
/// (`valid = false`, null rational) when `sample_rate <= 0` or the computed
/// time is NaN.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_place_by_waveform_offset(
	reference_timeline_in: crate::coretypes::QuercusRational,
	candidate_offset_samples: i64,
	sample_rate: i32,
) -> QuercusAudioSyncPlacement {
	QuercusAudioSyncPlacement::from_native(&quercus_audio::synchronizer::place_by_waveform_offset(
		reference_timeline_in.to_native(),
		candidate_offset_samples,
		sample_rate,
	))
}
