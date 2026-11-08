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

//! Waveform-based audio synchronization (`olive::AudioWaveformSync`). Pure
//! static helpers that correlate RMS envelopes to estimate sample offsets and
//! playback-rate corrections. No shared state.

/// A candidate offset and its correlation confidence.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.h`
/// (`AudioWaveformSync::OffsetResult`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OffsetResult {
	/// Offset of the candidate relative to the reference, in samples.
	pub offset_samples: i64,
	/// Normalized correlation confidence in `[0, 1]`.
	pub confidence: f64,
	/// Whether an offset could be determined.
	pub valid: bool,
}

/// A playback-rate change plus offset aligning candidate to reference.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.h`
/// (`AudioWaveformSync::StretchOffsetResult`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StretchOffsetResult {
	/// Rate the candidate must play at to align (`2.0` = candidate runs at
	/// half speed and must be sped up 2x).
	pub rate: f64,
	/// Offset in samples.
	pub offset_samples: i64,
	/// Normalized correlation confidence in `[0, 1]`.
	pub confidence: f64,
	/// Whether a rate+offset could be determined.
	pub valid: bool,
}

/// Extract a windowed RMS envelope from a planar sample buffer.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.cpp:28`
/// (`AudioWaveformSync::extract_rms_envelope`): the trailing partial window
/// is kept; the mean is over ALL channels' samples in the window.
pub fn extract_rms_envelope(planar: &[&[f32]], window_samples: usize) -> Vec<f64> {
	let mut envelope = Vec::new();

	let channel_count = planar.len();
	let sample_count = if channel_count > 0 {
		planar[0].len()
	} else {
		0
	};
	if channel_count == 0 || sample_count == 0 || window_samples == 0 {
		return envelope;
	}

	let window_count = sample_count.div_ceil(window_samples);
	envelope.resize(window_count, 0.0);

	for window in 0..window_count {
		let start = window * window_samples;
		let end = (start + window_samples).min(sample_count);
		let mut square_sum = 0.0f64;
		let mut total = 0usize;

		for data in planar.iter() {
			for &s in &data[start..end] {
				let value = f64::from(s);
				square_sum += value * value;
				total += 1;
			}
		}

		envelope[window] = if total > 0 {
			(square_sum / total as f64).sqrt()
		} else {
			0.0
		};
	}

	envelope
}

/// Estimates a plain sample offset (no rate change) between two planar buffers.
///
/// `reference` and `candidate` are planar f32 buffers (`[channel][sample]`); each
/// is reduced to one RMS envelope with [`extract_rms_envelope`] using
/// `window_samples` samples per window. The window count comes from the first
/// channel's length, and every channel must be at least that long because each
/// window pools all channels' samples. `max_offset_samples` caps the search after
/// truncating to whole windows (`max_offset_samples / window_samples`), so a
/// value below one window searches lag 0 only.
///
/// Returns the offset quantized to windows (`lag * window_samples`) with its
/// confidence; a zero `window_samples` yields the invalid result (offset 0,
/// confidence 0, `valid == false`). Both envelopes are treated as fully valid;
/// use [`estimate_envelope_offset_valid`] for masked synchronization.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.cpp:65`
/// (`AudioWaveformSync::estimate_offset`).
pub fn estimate_offset(
	reference: &[&[f32]],
	candidate: &[&[f32]],
	window_samples: usize,
	max_offset_samples: i64,
) -> OffsetResult {
	if window_samples == 0 {
		return OffsetResult {
			offset_samples: 0,
			confidence: 0.0,
			valid: false,
		};
	}

	let reference_envelope = extract_rms_envelope(reference, window_samples);
	let candidate_envelope = extract_rms_envelope(candidate, window_samples);
	let max_offset_windows = max_offset_samples / window_samples as i64;

	estimate_envelope_offset(
		&reference_envelope,
		&candidate_envelope,
		window_samples,
		max_offset_windows,
	)
}

/// Estimates an offset from two RMS envelopes, treating every window as valid.
///
/// `reference` and `candidate` hold one f64 per envelope window (as returned by
/// [`extract_rms_envelope`]); `window_samples` is the number of original samples
/// per window and converts the winning lag back to samples; `max_offset_windows`
/// bounds the lag search symmetrically to
/// `-max_offset_windows..=max_offset_windows`.
///
/// Equivalent to [`estimate_envelope_offset_valid`] with empty validity masks,
/// i.e. every overlapping window participates in the normalized
/// cross-correlation.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.cpp:84`
/// (`AudioWaveformSync::estimate_envelope_offset`, unmasked overload).
pub fn estimate_envelope_offset(
	reference: &[f64],
	candidate: &[f64],
	window_samples: usize,
	max_offset_windows: i64,
) -> OffsetResult {
	estimate_envelope_offset_valid(
		reference,
		candidate,
		&[],
		&[],
		window_samples,
		max_offset_windows,
	)
}

/// Estimates an offset from two RMS envelopes, excluding windows flagged invalid.
///
/// `reference` and `candidate` hold one f64 per envelope window;
/// `reference_valid`/`candidate_valid` are per-window masks where `true` means the
/// window may contribute. An empty mask means "all windows valid"; a mask whose
/// length differs from its envelope is ignored entirely instead of rejecting
/// windows. `window_samples` converts the winning lag to samples and
/// `max_offset_windows` bounds the lag search to
/// `-max_offset_windows..=max_offset_windows`.
///
/// For each lag the overlapping windows are compared by normalized correlation
/// after subtracting the mean over the valid windows; a lag is skipped when fewer
/// than two valid windows overlap or when either correlation energy is below
/// `1e-12`. The best-scoring lag then yields `offset_samples = lag *
/// window_samples`, `confidence = max(0, score)` and `valid == true`; if no lag
/// qualifies the result stays invalid (offset 0, confidence 0). This is the
/// variant the frozen C ABI exposes.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.cpp:95`
/// (`AudioWaveformSync::estimate_envelope_offset`, masked overload): a mask
/// whose size does NOT match its envelope is ignored entirely
/// (`mask.size() != size || mask.at(index)` — load-bearing); lags with
/// fewer than 2 valid overlap windows are skipped; windows whose
/// correlation energy is zero (`qFuzzyIsNull`, < 1e-12) are skipped;
/// confidence is `max(0, best_score)`.
pub fn estimate_envelope_offset_valid(
	reference: &[f64],
	candidate: &[f64],
	reference_valid: &[bool],
	candidate_valid: &[bool],
	window_samples: usize,
	max_offset_windows: i64,
) -> OffsetResult {
	let mut result = OffsetResult {
		offset_samples: 0,
		confidence: 0.0,
		valid: false,
	};
	if reference.is_empty() || candidate.is_empty() || window_samples == 0 {
		return result;
	}

	let is_valid =
		|mask: &[bool], size: usize, index: usize| -> bool { mask.len() != size || mask[index] };

	let mut best_score = -2.0f64;
	let mut best_lag = 0i64;

	let reference_size = reference.len() as i64;
	let candidate_size = candidate.len() as i64;

	for lag in -max_offset_windows..=max_offset_windows {
		let reference_start = 0i64.max(-lag);
		let candidate_start = 0i64.max(lag);
		let overlap = (reference_size - reference_start).min(candidate_size - candidate_start);

		if overlap < 2 {
			continue;
		}

		// Only windows marked valid on both sides participate in the score
		let mut reference_mean = 0.0f64;
		let mut candidate_mean = 0.0f64;
		let mut valid_count = 0i64;
		for i in 0..overlap {
			let reference_index = (reference_start + i) as usize;
			let candidate_index = (candidate_start + i) as usize;
			if !is_valid(reference_valid, reference.len(), reference_index)
				|| !is_valid(candidate_valid, candidate.len(), candidate_index)
			{
				continue;
			}
			reference_mean += reference[reference_index];
			candidate_mean += candidate[candidate_index];
			valid_count += 1;
		}

		if valid_count < 2 {
			continue;
		}

		reference_mean /= valid_count as f64;
		candidate_mean /= valid_count as f64;

		let mut numerator = 0.0f64;
		let mut reference_energy = 0.0f64;
		let mut candidate_energy = 0.0f64;
		for i in 0..overlap {
			let reference_index = (reference_start + i) as usize;
			let candidate_index = (candidate_start + i) as usize;
			if !is_valid(reference_valid, reference.len(), reference_index)
				|| !is_valid(candidate_valid, candidate.len(), candidate_index)
			{
				continue;
			}
			let reference_value = reference[reference_index] - reference_mean;
			let candidate_value = candidate[candidate_index] - candidate_mean;
			numerator += reference_value * candidate_value;
			reference_energy += reference_value * reference_value;
			candidate_energy += candidate_value * candidate_value;
		}

		// qFuzzyIsNull(double): |x| < 1e-12
		if reference_energy.abs() < 1e-12 || candidate_energy.abs() < 1e-12 {
			continue;
		}

		let score = numerator / (reference_energy * candidate_energy).sqrt();
		if score > best_score {
			best_score = score;
			best_lag = lag;
		}
	}

	if best_score > -2.0 {
		result.valid = true;
		result.confidence = best_score.max(0.0);
		result.offset_samples = best_lag * window_samples as i64;
	}

	result
}

/// Estimate a playback-rate change plus offset aligning the candidate to the
/// reference, resampling the candidate at each rate in `[min_rate, max_rate]`.
///
/// `// CPP-PARITY: src/audio/src/audiowaveformsync.cpp:202`
/// (`AudioWaveformSync::estimate_stretch_and_offset`): the rate loop upper
/// bound is `max_rate + rate_step * 0.5` (a half-step tolerance, so
/// floating-point step accumulation still reaches max_rate); a resampled
/// window is valid only when BOTH source windows are valid; a wrong-sized
/// candidate mask means all-valid.
pub fn estimate_stretch_and_offset(
	reference: &[f64],
	candidate: &[f64],
	reference_valid: &[bool],
	candidate_valid: &[bool],
	window_samples: usize,
	max_offset_windows: i64,
	min_rate: f64,
	max_rate: f64,
	rate_step: f64,
) -> StretchOffsetResult {
	let mut result = StretchOffsetResult {
		rate: 1.0,
		offset_samples: 0,
		confidence: 0.0,
		valid: false,
	};
	if reference.is_empty()
		|| candidate.is_empty()
		|| window_samples == 0
		|| min_rate <= 0.0
		|| max_rate < min_rate
		|| rate_step <= 0.0
	{
		return result;
	}

	let mut best_confidence = -2.0f64;

	let mut rate = min_rate;
	while rate <= max_rate + rate_step * 0.5 {
		// Resample the candidate envelope so that window i of the resampled
		// envelope corresponds to window i*rate of the original
		let resampled_size = (candidate.len() as f64 / rate) as i64;
		if resampled_size < 2 {
			rate += rate_step;
			continue;
		}

		let resampled_len = resampled_size as usize;
		let mut resampled = vec![0.0f64; resampled_len];
		let mut resampled_valid = vec![false; resampled_len];
		for i in 0..resampled_size as usize {
			let position = i as f64 * rate;
			let lower = position as usize;
			let upper = (lower + 1).min(candidate.len() - 1);
			let fraction = position - lower as f64;

			resampled[i] = candidate[lower] * (1.0 - fraction) + candidate[upper] * fraction;

			resampled_valid[i] = candidate_valid.len() != candidate.len()
				|| (candidate_valid[lower] && candidate_valid[upper]);
		}

		let offset = estimate_envelope_offset_valid(
			reference,
			&resampled,
			reference_valid,
			&resampled_valid,
			window_samples,
			max_offset_windows,
		);

		if offset.valid && offset.confidence > best_confidence {
			best_confidence = offset.confidence;
			result.valid = true;
			result.rate = rate;
			result.confidence = offset.confidence;
			result.offset_samples = offset.offset_samples;
		}

		rate += rate_step;
	}

	result
}
