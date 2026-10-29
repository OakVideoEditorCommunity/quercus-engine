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

/// Synchronously extracts a waveform and returns an owned handle.
///
/// Null indicates invalid input or decode failure. UI consumers should normally
/// use `quercus_task_waveform_extract`; this entry point exists for hosts that
/// already own a media-analysis worker thread.
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

/// Reads waveform metadata; false for null handles/output pointers.
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

/// Reads one channel-interleaved peak without exposing native point storage.
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

unsafe fn f64_slice<'a>(ptr: *const f64, len: usize) -> Option<&'a [f64]> {
	if len == 0 {
		Some(&[])
	} else {
		(!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
	}
}

unsafe fn bool_slice<'a>(ptr: *const bool, len: usize) -> Option<&'a [bool]> {
	if len == 0 {
		Some(&[])
	} else {
		(!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
	}
}

/// Estimates candidate offset from precomputed RMS/peak envelopes.
///
/// Empty validity masks mean all windows valid, matching quercus-audio. A mask with
/// a nonmatching length is also ignored by the underlying algorithm.
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

/// Searches playback rate and offset together when plain correlation is weak.
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

/// Aligns candidate source timecode to the reference at a timeline anchor.
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

/// Converts a measured sample offset into a timeline placement.
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
