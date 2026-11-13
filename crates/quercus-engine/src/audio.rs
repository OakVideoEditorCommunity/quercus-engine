use quercus_audio::manager::*;
use quercus_audio::params::AudioParams;
use quercus_core::SampleFormat;
use crate::handle::NativeMirror;
use crate::vecs::{IntoFfiVec, QuercusVecString};
/// Audio stream parameters. Bridge to C value handle.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuercusAudioParams{
    /// Sample rate in Hz.
    pub sample_rate: i32,
    /// ffmpeg-style channel layout mask (0 = unknown/unspecified).
    pub channel_layout: u64,
    /// Sample format (see [`SampleFormat`]).
    pub format: i32,
}

impl NativeMirror for QuercusAudioParams {
    type Native = AudioParams;

    fn from_native(params: &AudioParams) -> Self {
        Self {
            sample_rate: params.sample_rate,
            channel_layout: params.channel_layout,
            format: params.format as i32,
        }
    }

    fn to_native(&self) -> AudioParams {
        AudioParams {
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
            format: match self.format {
                // Unsigned 8-bit planar.
                0 => SampleFormat::U8Planar,
                // Signed 16-bit planar.
                1 => SampleFormat::S16Planar,
                // Signed 32-bit planar.
                2 => SampleFormat::S32Planar,
                // Signed 64-bit planar.
                3 => SampleFormat::S64Planar,
                // 32-bit float planar.
                4 => SampleFormat::F32Planar,
                // 64-bit float planar.
                5 => SampleFormat::F64Planar,
                // Unsigned 8-bit packed.
                6 => SampleFormat::U8,
                // Signed 16-bit packed.
                7 => SampleFormat::S16,
                // Signed 32-bit packed.
                8 => SampleFormat::S32,
                // Signed 64-bit packed.
                9 => SampleFormat::S64,
                // 32-bit float packed.
                10 => SampleFormat::F32,
                // 64-bit float packed.
                11 => SampleFormat::F64,
                // Invalid/unspecified.
                _ => SampleFormat::Invalid
            } ,
        }
    }
}

/// Creates the process-wide audio manager instance.
///
/// Call this before any other `quercus_audio_manager_*` function; those return
/// their failure value (`false` / `-1`) while no live instance exists. A no-op
/// when the instance is already created, and re-arms an instance previously torn
/// down by [`quercus_audio_manager_destroy_instance`]. Returns `true` on success
/// — the Rust allocation cannot fail, so `false` is not produced in practice.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_create_instance() -> bool{
    ManagerInner::create_instance().is_ok()
}
/// Tears down the process-wide audio manager instance.
///
/// Clears the playback state (output params, queue and running flag) so a later
/// create starts clean; the device stream itself is not closed here. Every
/// `quercus_audio_manager_*` call then takes its failure path until
/// [`quercus_audio_manager_create_instance`] resurrects the instance. Safe to
/// call when no instance exists (no-op). Not reference-counted: all handles refer
/// to the same process-wide singleton.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_destroy_instance(){
    ManagerInner::destroy_instance()
}

/// Returns the host's output device names in enumeration order.
///
/// The result is a ref-counted `QuercusVecString` handle (initial ref count 1):
/// index `i` is the `device` index accepted by
/// [`quercus_audio_manager_set_output_device`], and the caller owns the handle
/// and must release it with `quercus_vec_string_release`. The default device is
/// not marked — callers prepend their own "system default" entry (index `-1`).
/// The vector is empty when the host reports no output devices.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_output_device_names() -> *mut QuercusVecString{
    output_device_names().into_ffi_vec()
}
/// Returns the host's input device names in enumeration order.
///
/// The result is a ref-counted `QuercusVecString` handle (initial ref count 1):
/// index `i` is the `device` index accepted by
/// [`quercus_audio_manager_set_input_device`], and the caller owns the handle
/// and must release it with `quercus_vec_string_release`. The default device is
/// not marked — callers prepend their own "system default" entry (index `-1`).
/// The vector is empty when the host reports no input devices.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_input_device_names() -> *mut QuercusVecString{
    input_device_names().into_ffi_vec()
}

/// Looks up a device's enumeration index by its exact name.
///
/// `name` is a borrowed, non-NUL-terminated UTF-8 buffer of `len` bytes (the
/// bytes are not copied and ownership stays with the caller); `is_output`
/// selects the output list (`true`) or the input list (`false`), in the same
/// order as [`quercus_audio_manager_output_device_names`] and
/// [`quercus_audio_manager_input_device_names`]. Returns the index, or `-1`
/// when `name` is null, is not valid UTF-8, or matches no device.
///
/// # Safety
///
/// `name` must be valid for `len` byte reads when non-null; it need not be
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_manager_device_index_by_name(name: *const u8, len: usize, is_output: bool) -> i32{
    if name.is_null() {
        return -1;
    }
    // The buffer is borrowed from C: build a &str view over it (no copy,
    // no ownership transfer), validating UTF-8 at the boundary.
    let bytes = unsafe { std::slice::from_raw_parts(name, len) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return -1;
    };
    device_index_by_name(name, is_output).unwrap_or(-1)
}

/// Switches the output device to enumeration index `device`.
///
/// `device` indexes [`quercus_audio_manager_output_device_names`]; `-1` selects
/// the system default. The current stream is closed, queued audio is dropped and
/// the cached params are cleared so the next
/// [`quercus_audio_manager_push_to_output`] re-opens the stream on the new
/// device. Returns `false` when no manager instance exists; the index itself is
/// not validated here.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_set_output_device(device: i32) -> bool{
    let Some(mut inst) = instance() else { return false; };
    inst.set_output_device(device).is_ok()
}

/// Selects the input (capture) device used by the next recording.
///
/// `device` is an index into [`quercus_audio_manager_input_device_names`]; `-1`
/// is PortAudio's `paNoDevice` sentinel (no device), which makes the next
/// recording fail rather than selecting a default. The index is stored without
/// validation and without calling the audio backend, so an out-of-range value
/// only surfaces when the device is opened. Returns `false` when no manager
/// instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_set_input_device(device: i32) -> bool{
    let Some(mut inst) = instance() else { return false; };
    inst.set_input_device(device).is_ok()
}

/// Reads per-channel peak levels of the buffered, not-yet-consumed output.
///
/// `peaks` receives up to `capacity` linear peak values (`0.0..=1.0` and above)
/// in channel order; pass null with `capacity == 0` to only query the channel
/// count. Returns the channel count (independent of how many entries fit), `0`
/// when no output is configured or the layout is unknown, or `-1` when no
/// manager instance exists or `peaks` is null with a non-zero `capacity`. Only
/// packed or planar F32 queues are analyzed (other formats report zeros); the
/// window is the most recent 8192 frames.
///
/// # Safety
///
/// `peaks` must be valid for `capacity` f32 writes when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_manager_output_levels(peaks: *mut f32, capacity: usize) -> i32{
    if peaks.is_null() && capacity > 0 {
        return -1;
    }
    let Some(inst) = instance() else { return -1; };
    let peaks = if peaks.is_null() {
        // from_raw_parts_mut(null, 0) is UB; an empty slice is not.
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(peaks, capacity) }
    };
    inst.output_levels(peaks).unwrap_or(-1)
}

/// Reads the seconds of audio consumed by the output device since the last
/// clock reset.
///
/// On success writes the value through `seconds` and returns `true`; the value
/// is `consumed_frames / sample_rate` clamped to `>= 0`, or `-1.0` when no
/// output stream is running or the sample rate is unknown. Returns `false`
/// without writing when `seconds` is null, when no manager instance exists, or
/// if reading the clock fails.
///
/// # Safety
///
/// `seconds` must be a valid `*mut f64`, or null (which is rejected).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_manager_seconds(seconds: *mut f64) -> bool{
    if seconds.is_null() {
        return false;
    }
    let Some(inst) = instance() else { return false; };
    let mut seconds_got:f64 = 0.0;
    if !inst.seconds(&mut seconds_got).is_ok(){
        return false;
    }
    unsafe { *seconds = seconds_got };
    true
}

/// Discards all audio currently buffered for output without stopping playback.
///
/// Unlike [`quercus_audio_manager_stop_output`], the output clock keeps running
/// and only the queued bytes are dropped. Returns `false` when no manager
/// instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_clear_buffered_output() -> bool{
    let Some(inst) = instance() else { return false; };
    inst.clear_buffered_output().is_ok()
}

/// Restarts the output playback clock at zero.
///
/// Only the consumed-frame counter is reset; queued audio is untouched. Pairs
/// with [`quercus_audio_manager_seconds`], which reads that clock. Returns
/// `false` when no manager instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_reset_output_clock() -> bool{
    let Some(inst) = instance() else { return false; };
    inst.reset_output_clock().is_ok()
}

/// Stops playback and discards everything still queued for the output device.
///
/// Clears the running flag and the queues, so
/// [`quercus_audio_manager_seconds`] reports `-1.0` until the next
/// [`quercus_audio_manager_push_to_output`] restarts playback; the device stream
/// is not explicitly closed. Returns `false` when no manager instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_stop_output() -> bool{
    let Some(mut inst) = instance() else { return false; };
    inst.stop_output().is_ok()
}

/// Reads and resets the output underrun counter.
///
/// The counter holds the frames the output callback had to zero-fill because the
/// queue was empty; reading it resets it to `0`. The playback engine polls this
/// every tick and resyncs afterwards, since the zero-fill advanced the output
/// clock past the queued content. Returns `-1` when no manager instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_take_output_underrun_frames() -> i64{
    let Some(inst) = instance() else { return -1; };
    inst.take_output_underrun_frames()
}

/// Returns the number of frames currently queued for the output callback.
///
/// Diagnostic counter over the whole queue, not just the analysis window of
/// [`quercus_audio_manager_output_levels`]. Returns `-1` when no manager
/// instance exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_output_queued_frames() -> i64{
    let Some(inst) = instance() else { return -1; };
    inst.output_queued_frames()
}

/// Drops up to `frame` frames from the front of the output queue.
///
/// Used to resync the queue after an underrun without a full clear; the output
/// clock is not adjusted. Returns the number of frames actually dropped (fewer
/// than `frame` when the queue is shorter), or `-1` when no manager instance
/// exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_manager_drop_output_frames(frame: i64) -> i64{
    let Some(inst) = instance() else { return -1; };
    inst.drop_output_frames(frame)
}

/// Buffers one block of raw sample bytes for the audio output device.
///
/// `params` describes the block (sample rate in Hz, channel layout, sample
/// format) and is compared with the previous block — a change re-opens the
/// output stream (device index `< 0` selects the system default). `sample_len`
/// is the byte length of `sample` (bytes per frame times the frame count, not a
/// frame count). `error_buf`/`error_buf_len` are a caller-owned UTF-8 buffer
/// reserved for an error message; the Rust path does not currently write it.
///
/// Returns `true` once the bytes are queued, including when the device fails to
/// open (the samples then stay buffered and playback is silent). Returns `false`
/// when no manager instance exists or when a null pointer is paired with a
/// non-zero length.
///
/// # Safety
///
/// `sample` must be valid for `sample_len` byte reads and `error_buf` valid for
/// `error_buf_len` byte writes; either may be null only when its length is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_manager_push_to_output(params: QuercusAudioParams,
                                                          sample: *const u8, sample_len: usize,
                                                          error_buf: *mut u8, error_buf_len: usize
) -> bool{
    // from_raw_parts on null is UB even with len 0; empty slices are not.
    if sample.is_null() && sample_len > 0 {
        return false;
    }
    if error_buf.is_null() && error_buf_len > 0 {
        return false;
    }
    let sample_buf = if sample.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(sample, sample_len) }
    };
    let error_buf = if error_buf.is_null() {
        &mut []
    } else {
        unsafe { std::slice::from_raw_parts_mut(error_buf, error_buf_len) }
    };
    let Some(mut inst) = instance() else { return false; };
    inst.push_to_output(params.to_native(), sample_buf, error_buf).is_ok()
}

// ---------------------------------------------------------------------------
// Audio params conversions (pure functions)
// ---------------------------------------------------------------------------

/// Counts the channels encoded in an ffmpeg-style channel layout mask.
///
/// Returns the population count of `channel_layout` (`0x3`, i.e. stereo, gives
/// `2`); a mask of `0` returns `0` rather than falling back to stereo — the
/// stereo fallback lives in the audio processor, not here. The other
/// `AudioParams` fields are filled with dummies by this wrapper; only the layout
/// matters.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_channel_count(channel_layout: u64) -> i32 {
	quercus_audio::params::AudioParams {
		sample_rate: 0,
		channel_layout,
		format: quercus_core::SampleFormat::Invalid,
	}
	.channel_count()
}

/// Bytes per sample per channel for a SampleFormat discriminant.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_bytes_per_sample_per_channel(format: i32) -> i64 {
	quercus_audio::params::AudioParams {
		sample_rate: 0,
		channel_layout: 0,
		format: quercus_audio::params::sample_format_from_i32(format),
	}
	.bytes_per_sample_per_channel()
}

/// Converts a frame count to a byte count for the given audio parameters.
///
/// `sample_rate` is in Hz and does not affect the arithmetic; `channel_layout`
/// is an ffmpeg-style mask whose popcount is the channel count; `format` is the
/// `SampleFormat` discriminant (`0..=11`; out of range maps to `Invalid`, i.e.
/// 0 bytes per sample); `samples` is the frame count (may be negative). Returns
/// `samples * bytes_per_sample_per_channel * channel_count`, so an unknown or
/// empty layout, or an unknown format, yields `0`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_samples_to_bytes(
	sample_rate: i32,
	channel_layout: u64,
	format: i32,
	samples: i64,
) -> i64 {
	quercus_audio::params::AudioParams {
		sample_rate,
		channel_layout,
		format: quercus_audio::params::sample_format_from_i32(format),
	}
	.samples_to_bytes(samples)
}

/// Converts a frame count to rational seconds at `sample_rate`.
///
/// `frames` is a per-channel frame count and `sample_rate` the rate in Hz; the
/// result is the exact reduced fraction `frames / sample_rate` (a
/// `QuercusRational`) with no rounding. `sample_rate == 0` produces the null
/// sentinel (`0/0`, NaN), which
/// [`quercus_audio_params_rational_to_samples`] maps back to `0` samples.
/// Counterpart of [`quercus_audio_params_rational_to_samples`].
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_frames_to_rational(
	frames: i64,
	sample_rate: i32,
) -> crate::coretypes::QuercusRational {
	crate::handle::NativeMirror::from_native(&quercus_audio::params::frames_to_rational(
		frames,
		sample_rate,
	))
}

/// Converts a rational time to a sample-frame index at `sample_rate`.
///
/// `time` is a duration in seconds (a `QuercusRational`, passed by value);
/// `sample_rate` is in Hz. The result is `round(time_seconds * sample_rate)` —
/// rounded to nearest, ties away from zero, not truncated. Returns `0` when
/// `time` is the null/NaN sentinel (its `to_f64` is NaN) or when the product
/// rounds to zero.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_rational_to_samples(
	time: crate::coretypes::QuercusRational,
	sample_rate: i32,
) -> i64 {
	quercus_audio::params::rational_to_samples(
		crate::handle::NativeMirror::to_native(&time),
		sample_rate,
	)
}

/// Converts a double-precision second count to a rational time.
///
/// `value` is a duration in seconds (a finite `f64`). The conversion is the
/// continued-fraction approximation of `Rational::from_double`: NaN or a
/// magnitude above `i32::MAX + 3` yields the null sentinel (`0/0`), and values
/// too small to represent are retried at maximum (`i64::MAX`) precision. Used
/// wherever the C API needs a `double` second count as a `QuercusRational`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_audio_params_rational_from_double(
	value: f64,
) -> crate::coretypes::QuercusRational {
	crate::handle::NativeMirror::from_native(&quercus_audio::params::rational_from_double(value))
}

// ---------------------------------------------------------------------------
// Level meter
// ---------------------------------------------------------------------------

/// Per-channel level statistics.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QuercusChannelStats {
	/// Peak amplitude, linear scale (0.0 when silent).
	pub peak_linear: f64,
	/// Peak amplitude, decibel scale (-200.0 when silent).
	pub peak_db: f64,
	/// RMS level, linear scale.
	pub rms_linear: f64,
	/// RMS level, decibel scale (-200.0 when silent).
	pub rms_db: f64,
	/// VU-meter ballistics reading, decibel scale.
	pub vu_db: f64,
}

impl NativeMirror for QuercusChannelStats {
	type Native = quercus_audio::levelmeter::ChannelStats;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			peak_linear: native.peak_linear,
			peak_db: native.peak_db,
			rms_linear: native.rms_linear,
			rms_db: native.rms_db,
			vu_db: native.vu_db,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			peak_linear: self.peak_linear,
			peak_db: self.peak_db,
			rms_linear: self.rms_linear,
			rms_db: self.rms_db,
			vu_db: self.vu_db,
		}
	}
}

/// Aggregate level metadata without the separately returned channel vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelSummary {
	/// Maximum peak across all channels, linear scale.
	pub max_peak_linear: f64,
	/// Integrated loudness in LUFS.
	pub integrated_lufs: f64,
	/// Whether every channel is below the silence gate.
	pub silence: bool,
}

impl LevelSummary {
	fn from_stats(stats: &quercus_audio::levelmeter::Stats) -> Self {
		Self {
			max_peak_linear: stats.max_peak_linear,
			integrated_lufs: stats.integrated_lufs,
			silence: stats.silence,
		}
	}
}

/// Whole-buffer level statistics.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QuercusLevelStats {
	/// Maximum peak across all channels, linear scale.
	pub max_peak_linear: f64,
	/// Integrated loudness (EBU R128 LUFS; -200.0 for silence).
	pub integrated_lufs: f64,
	/// Every channel below the noise gate.
	pub silence: i32,
}

impl NativeMirror for QuercusLevelStats {
	type Native = LevelSummary;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			max_peak_linear: native.max_peak_linear,
			integrated_lufs: native.integrated_lufs,
			silence: i32::from(native.silence),
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			max_peak_linear: self.max_peak_linear,
			integrated_lufs: self.integrated_lufs,
			silence: self.silence != 0,
		}
	}
}

/// Analyzes a planar f32 sample buffer and writes per-channel plus whole-buffer
/// level statistics.
///
/// `planar` points to `channels` channel pointers, each addressing
/// `samples_per_channel` f32 samples; `channel_stats` receives one entry per
/// channel, in channel order; `out` receives the aggregate summary (max peak,
/// EBU R128 integrated loudness, silence flag). The analysis itself is
/// `quercus_audio::levelmeter::analyze_sample_buffer`: VU equals RMS dB, and the
/// silence gate is `|peak| < 1e-12`.
///
/// Returns `false` without writing anything when `out` is null, `planar` or
/// `channel_stats` is null while `channels > 0`, or a channel pointer is null
/// while `samples_per_channel > 0`. Passing `channels == 0` with a null `planar`
/// is allowed and yields empty statistics.
///
/// # Safety
///
/// `planar` must be valid for `channels` pointer reads; every non-null channel
/// pointer must be valid for `samples_per_channel` f32 reads; `channel_stats`
/// must be valid for `channels` writes when non-null; `out` must be valid for one
/// write when non-null. The output buffers must not overlap the input samples.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_audio_levelmeter_analyze(
	planar: *const *const f32,
	samples_per_channel: usize,
	channels: usize,
	channel_stats: *mut QuercusChannelStats,
	out: *mut QuercusLevelStats,
) -> bool {
	if out.is_null() || (planar.is_null() && channels > 0) {
		return false;
	}
	if channels > 0 && channel_stats.is_null() {
		return false;
	}
	let channels_ptrs =
		unsafe { std::slice::from_raw_parts(planar, channels) };
	let mut lanes: Vec<&[f32]> = Vec::with_capacity(channels);
	for &ptr in channels_ptrs {
		if ptr.is_null() && samples_per_channel > 0 {
			return false;
		}
		lanes.push(if ptr.is_null() {
			&[]
		} else {
			unsafe { std::slice::from_raw_parts(ptr, samples_per_channel) }
		});
	}
	let stats = quercus_audio::levelmeter::analyze_sample_buffer(&lanes);
	for (i, c) in stats.channels.iter().enumerate() {
		unsafe {
			*channel_stats.add(i) = QuercusChannelStats::from_native(c);
		}
	}
	unsafe {
		*out = QuercusLevelStats::from_native(&LevelSummary::from_stats(&stats));
	}
	true
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn params_conversions() {
		// Stereo (layout 0x3) F32 (discriminant 10): 4 bytes/sample/channel.
		assert_eq!(quercus_audio_params_channel_count(0x3), 2);
		assert_eq!(quercus_audio_params_bytes_per_sample_per_channel(10), 4);
		assert_eq!(quercus_audio_params_samples_to_bytes(48000, 0x3, 10, 100), 800);

		let r = quercus_audio_params_frames_to_rational(48, 48_000);
		assert_eq!((r.num, r.den), (1, 1000));
		assert_eq!(quercus_audio_params_rational_to_samples(r, 48_000), 48);
		let d = quercus_audio_params_rational_from_double(0.5);
		assert_eq!((d.num, d.den), (1, 2));
	}

	#[test]
	fn levelmeter_analyze() {
		unsafe {
			let left = [0.5f32, -0.5, 0.25];
			let right = [0.25f32, 0.25, -0.25];
			let planar = [left.as_ptr(), right.as_ptr()];
			let mut channels = [QuercusChannelStats {
				peak_linear: 0.0,
				peak_db: 0.0,
				rms_linear: 0.0,
				rms_db: 0.0,
				vu_db: 0.0,
			}; 2];
			let mut stats = QuercusLevelStats {
				max_peak_linear: 0.0,
				integrated_lufs: 0.0,
				silence: 1,
			};
			assert!(quercus_audio_levelmeter_analyze(
				planar.as_ptr(),
				3,
				2,
				channels.as_mut_ptr(),
				&mut stats,
			));
			assert_eq!(stats.max_peak_linear, 0.5);
			assert_eq!(stats.silence, 0);
			assert_eq!(channels[0].peak_linear, 0.5);
			assert_eq!(channels[1].peak_linear, 0.25);

			// Null safety.
			assert!(!quercus_audio_levelmeter_analyze(
				std::ptr::null(),
				3,
				1,
				channels.as_mut_ptr(),
				&mut stats,
			));
			assert!(!quercus_audio_levelmeter_analyze(
				planar.as_ptr(),
				3,
				2,
				std::ptr::null_mut(),
				&mut stats,
			));
		}
	}
}
