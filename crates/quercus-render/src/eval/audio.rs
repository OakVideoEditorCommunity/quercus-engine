use super::*;

/// Render the audio montage over `params.range` (M12 P1): every clip
/// overlapping the range is decoded (interleaved f32 at the output rate
/// and layout) and mixed with its gain; uncovered parts stay silent. The
/// mixed output is clamped to [-1, 1].
pub fn render_audio_samples(
	params: &crate::ticket::AudioTicketParams,
) -> Result<crate::ticket::TicketPayload> {
	let (rate, layout, channels, total_frames) = audio_layout(params)?;
	let mut acc = vec![0.0f32; total_frames.saturating_mul(channels as usize)];
	mix_audio_montage(params, rate, channels, total_frames, &mut acc)?;
	Ok(crate::ticket::TicketPayload::Audio(
		crate::ticket::AudioSamples {
			samples: acc,
			sample_rate: rate,
			channel_layout: layout,
			channel_count: channels,
		},
	))
}

/// The output layout an audio render produces: `(sample_rate,
/// channel_layout, channel_count, total_sample_frames)`.
fn audio_layout(params: &crate::ticket::AudioTicketParams) -> Result<(i32, u64, i32, usize)> {
	let rate = params.sample_rate.max(1);
	let channels = params.channel_layout.count_ones().max(1) as i32;
	let duration = params.range.out() - params.range.in_();
	let seconds = if duration.denominator() == 0 {
		0.0
	} else {
		duration.numerator() as f64 / duration.denominator() as f64
	};
	if seconds <= 0.0 || seconds > 3600.0 {
		return Err(Error::Invalid);
	}

	// Anchor each chunk to the absolute sample grid (`round(out·rate) -
	// round(in·rate)`) instead of rounding the duration: at fractional
	// frame rates (29.97 fps → 1601.6 samples/frame) a duration-round
	// would emit 1602 samples for every chunk and accumulate ~12 extra
	// samples per second, slowly desyncing audio from video. Per-chunk
	// anchoring keeps the total exact and matches the decode side
	// (`FFmpegDecoder::retrieve_audio_to` fills `round(out·rate) -
	// round(in·rate)` samples).
	let total_frames = ((params.range.out().to_f64() * rate as f64).round()
		- (params.range.in_().to_f64() * rate as f64).round())
	.max(0.0) as usize;
	Ok((rate, params.channel_layout, channels, total_frames))
}

/// The byte length (interleaved f32, little-endian) an audio render of
/// `params` writes into a shm slot — the worker's slot-geometry check
/// (M15 S3). Mirrors [`render_audio_samples_into`]'s layout math.
/// Return the required little-endian f32 byte length for an audio render.
pub fn audio_samples_byte_len(params: &crate::ticket::AudioTicketParams) -> Result<usize> {
	let (_rate, _layout, channels, total_frames) = audio_layout(params)?;
	Ok(total_frames
		.saturating_mul(channels as usize)
		.saturating_mul(4))
}

/// Apply one clip's audio parameters to a decoded interleaved chunk.
///
/// Order (audio-post Phase 1): per-frame gain, stereo balance pan
/// (-3 dB at centre), linear fade-in/fade-out anchored at the clip's
/// timeline in/out, then the clip's audio effect chain in source-first
/// order. `first_time` is the absolute sequence time (seconds) of frame 0;
/// `channels` is the output channel count. Pan affects the first two
/// channels only (mono output is unpanned; extra channels only get the
/// gain/fade/effects).
///
/// `sidechain` is the already-mixed sequence accumulator over the same
/// frame window (the previous clips in montage order, interleaved exactly
/// like `buf`). Ducking effects read it; all other effects ignore it.
/// Future clips never contribute because they have not been mixed yet.
pub(crate) fn process_clip_audio(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	clip: &crate::ticket::MontageClip,
	first_time: f64,
	rate: i32,
	sidechain: Option<&[f32]>,
) {
	let channels = channels.max(1);
	let rate = rate.max(1) as f64;
	let gain = if clip.gain.is_finite() {
		clip.gain as f64
	} else {
		1.0
	};
	let pan = if clip.pan.is_finite() {
		clip.pan.clamp(-1.0, 1.0) as f64
	} else {
		0.0
	};
	// Stereo balance law: centre (0) is unity on both channels (so the
	// default clip is not quieter than the old gain-only path); panning
	// attenuates the opposite channel with a cosine taper and isolates it
	// completely at hard left/right. This is intentionally not the
	// -3 dB-centre constant-power law, which would make every default
	// stereo clip 3 dB quieter.
	let (left_pan, right_pan) = if pan > 0.0 {
		((pan * std::f64::consts::FRAC_PI_2).cos(), 1.0)
	} else if pan < 0.0 {
		(1.0, (-pan * std::f64::consts::FRAC_PI_2).cos())
	} else {
		(1.0, 1.0)
	};
	let in_time = clip.in_time.to_f64();
	let out_time = clip.out_time.to_f64();
	let fade_in = clip.fade_in.to_f64();
	let fade_out = clip.fade_out.to_f64();
	let fade_in = if fade_in.is_finite() && fade_in > 0.0 {
		fade_in
	} else {
		0.0
	};
	let fade_out = if fade_out.is_finite() && fade_out > 0.0 {
		fade_out
	} else {
		0.0
	};

	for frame in 0..frames {
		let t = first_time + frame as f64 / rate;
		let mut factor = gain;
		if fade_in > 0.0 {
			factor *= ((t - in_time) / fade_in).clamp(0.0, 1.0);
		}
		if fade_out > 0.0 {
			factor *= ((out_time - t) / fade_out).clamp(0.0, 1.0);
		}
		let base = frame * channels;
		if channels >= 2 {
			buf[base] *= (factor * left_pan) as f32;
			buf[base + 1] *= (factor * right_pan) as f32;
			for channel in 2..channels {
				buf[base + channel] *= factor as f32;
			}
		} else {
			buf[base] *= factor as f32;
		}
	}

	for effect in &clip.audio_effects {
		apply_audio_effect(buf, frames, channels, effect, rate, sidechain);
	}
}

/// Read a scalar effect parameter with a finite fallback.
///
/// `input` is the parameter input id looked up on `effect`; the value is
/// returned as an `f64` only when present and finite, otherwise `default`.
/// Used by the audio effect kernels so a missing or NaN/inf parameter
/// never poisons the mix.
fn effect_param(effect: &crate::ticket::MontageAudioEffect, input: &str, default: f64) -> f64 {
	effect
		.param(input)
		.map(|value| value.to_double())
		.filter(|value| value.is_finite())
		.unwrap_or(default)
}

/// Three-band EQ: one-pole low/high split with per-band dB gains.
///
/// `buf` holds `frames * channels` interleaved F32 samples and is modified
/// in place; `rate` is the sample rate in Hz (clamped to at least 1). The
/// low band is a one-pole low-pass at 200 Hz, the high band is the residual
/// above a 4 kHz low-pass, and the mid band is what remains. `effect`
/// supplies the low/mid/high gains in dB, each clamped to `[-24, 24]` and
/// defaulting to 0.
fn apply_eq(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
) {
	let rate = rate.max(1.0);
	let gain =
		|input: &str| 10f64.powf(effect_param(effect, input, 0.0).clamp(-24.0, 24.0) / 20.0) as f32;
	let low_gain = gain(quercus_node::block::AUDIO_EQ_LOW_GAIN_INPUT);
	let mid_gain = gain(quercus_node::block::AUDIO_EQ_MID_GAIN_INPUT);
	let high_gain = gain(quercus_node::block::AUDIO_EQ_HIGH_GAIN_INPUT);
	let low_alpha = (1.0 - (-std::f64::consts::TAU * 200.0 / rate).exp()) as f32;
	let high_alpha = (1.0 - (-std::f64::consts::TAU * 4_000.0 / rate).exp()) as f32;
	let mut low = vec![0.0f32; channels];
	let mut high_lp = vec![0.0f32; channels];
	for frame in 0..frames {
		for channel in 0..channels {
			let x = buf[frame * channels + channel];
			low[channel] += low_alpha * (x - low[channel]);
			high_lp[channel] += high_alpha * (x - high_lp[channel]);
			let low_band = low[channel];
			let high_band = x - high_lp[channel];
			let mid_band = x - low_band - high_band;
			buf[frame * channels + channel] =
				low_band * low_gain + mid_band * mid_gain + high_band * high_gain;
		}
	}
}

/// Feed-forward compressor with a sample-peak detector and exponential
/// attack/release smoothing.
///
/// `buf` holds `frames * channels` interleaved F32 samples and is modified
/// in place; `rate` is the sample rate in Hz (clamped to at least 1); the
/// detector peak is linked across channels. `effect` supplies threshold
/// (dB, clamped to `[-80, 0]`, default -24), ratio (clamped to `[1, 40]`,
/// default 4), attack (ms, clamped to `[0.1, 1000]`, default 10), release
/// (ms, clamped to `[1, 5000]`, default 100) and makeup gain (dB, clamped
/// to `[-24, 24]`, default 0), applied after the envelope.
fn apply_compressor(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
) {
	let rate = rate.max(1.0);
	let threshold = effect_param(
		effect,
		quercus_node::block::AUDIO_COMP_THRESHOLD_INPUT,
		-24.0,
	)
	.clamp(-80.0, 0.0);
	let ratio =
		effect_param(effect, quercus_node::block::AUDIO_COMP_RATIO_INPUT, 4.0).clamp(1.0, 40.0);
	let attack_ms = effect_param(effect, quercus_node::block::AUDIO_COMP_ATTACK_INPUT, 10.0)
		.clamp(0.1, 1_000.0);
	let release_ms = effect_param(effect, quercus_node::block::AUDIO_COMP_RELEASE_INPUT, 100.0)
		.clamp(1.0, 5_000.0);
	let makeup = 10f64.powf(
		effect_param(effect, quercus_node::block::AUDIO_COMP_MAKEUP_INPUT, 0.0).clamp(-24.0, 24.0)
			/ 20.0,
	) as f32;
	let attack = (-1.0f64 / (rate * attack_ms / 1_000.0)).exp() as f32;
	let release = (-1.0f64 / (rate * release_ms / 1_000.0)).exp() as f32;
	let mut envelope = 1.0f32;
	for frame in 0..frames {
		let mut peak = 0.0f32;
		for channel in 0..channels {
			peak = peak.max(buf[frame * channels + channel].abs());
		}
		let peak_db = 20.0 * (peak.max(1e-9) as f64).log10();
		let target_db = if peak_db > threshold {
			threshold + (peak_db - threshold) / ratio
		} else {
			peak_db
		};
		let target = 10f32.powf(((target_db - peak_db) / 20.0) as f32);
		let coeff = if target < envelope { attack } else { release };
		envelope = coeff * envelope + (1.0 - coeff) * target;
		let gain = envelope * makeup;
		for channel in 0..channels {
			buf[frame * channels + channel] *= gain;
		}
	}
}

/// Brick-wall peak limiter with an instantaneous attack and an
/// exponential release. The envelope gain is the minimum of the gain
/// needed to put the current sample frame under the ceiling and the
/// released previous gain, so the output never exceeds `ceiling_db`
/// (a final sample clamp guarantees the bound even after f32 rounding).
///
/// `buf` holds `frames * channels` interleaved F32 samples and is modified
/// in place; `rate` is the sample rate in Hz (clamped to at least 1). The
/// detector is linked across channels. `effect` supplies `ceiling` (dB,
/// clamped to `[-60, 0]`, default -1) and `release` (ms, clamped to
/// `[1, 5000]`, default 50).
fn apply_limiter(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
) {
	let rate = rate.max(1.0);
	let ceiling_db = effect_param(
		effect,
		quercus_node::block::AUDIO_LIMITER_CEILING_INPUT,
		-1.0,
	)
	.clamp(-60.0, 0.0);
	let release_ms = effect_param(
		effect,
		quercus_node::block::AUDIO_LIMITER_RELEASE_INPUT,
		50.0,
	)
	.clamp(1.0, 5_000.0);
	let ceiling = 10f64.powf(ceiling_db / 20.0) as f32;
	let release = (-1.0f64 / (rate * release_ms / 1_000.0)).exp() as f32;
	let mut envelope = 1.0f32;
	for frame in 0..frames {
		let mut peak = 0.0f32;
		for channel in 0..channels {
			peak = peak.max(buf[frame * channels + channel].abs());
		}
		let required = if peak > ceiling { ceiling / peak } else { 1.0 };
		if required < envelope {
			// Instant attack: reduce the gain on this sample.
			envelope = required;
		} else {
			envelope = release * envelope + (1.0 - release) * 1.0;
		}
		for channel in 0..channels {
			let index = frame * channels + channel;
			buf[index] = (buf[index] * envelope).clamp(-ceiling, ceiling);
		}
	}
}

/// Sample-peak noise gate with attack/release smoothing and a hold time.
///
/// The detector is linked across channels (the frame peak). A frame at or
/// above `threshold_db` opens the gate and resets the hold counter; frames
/// below threshold stay open until the hold expires, then the gain
/// releases toward silence. The gate starts open so the first transient is
/// not swallowed.
fn apply_gate(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
) {
	let rate = rate.max(1.0);
	let threshold_db = effect_param(
		effect,
		quercus_node::block::AUDIO_GATE_THRESHOLD_INPUT,
		-40.0,
	)
	.clamp(-100.0, 0.0);
	let attack_ms =
		effect_param(effect, quercus_node::block::AUDIO_GATE_ATTACK_INPUT, 5.0).clamp(0.1, 1_000.0);
	let release_ms = effect_param(effect, quercus_node::block::AUDIO_GATE_RELEASE_INPUT, 100.0)
		.clamp(1.0, 5_000.0);
	let hold_ms =
		effect_param(effect, quercus_node::block::AUDIO_GATE_HOLD_INPUT, 20.0).clamp(0.0, 5_000.0);
	let threshold = 10f64.powf(threshold_db / 20.0) as f32;
	let attack = (-1.0f64 / (rate * attack_ms / 1_000.0)).exp() as f32;
	let release = (-1.0f64 / (rate * release_ms / 1_000.0)).exp() as f32;
	let hold_frames = (rate * hold_ms / 1_000.0).round() as usize;
	let mut hold = 0usize;
	let mut gain = 1.0f32;
	for frame in 0..frames {
		let mut peak = 0.0f32;
		for channel in 0..channels {
			peak = peak.max(buf[frame * channels + channel].abs());
		}
		let target = if peak >= threshold {
			hold = hold_frames;
			1.0
		} else if hold > 0 {
			hold -= 1;
			1.0
		} else {
			0.0
		};
		let coeff = if target > gain { attack } else { release };
		gain = coeff * gain + (1.0 - coeff) * target;
		for channel in 0..channels {
			buf[frame * channels + channel] *= gain;
		}
	}
}

/// Sidechain ducker: when the detector (the already-mixed sequence
/// accumulator) is at or above `threshold_db`, the clip is attenuated by
/// `amount_db` with attack/release smoothing.
///
/// The sidechain is the montage accumulator at the clip's position, not
/// the clip's own signal and not a future clip. It is therefore only
/// available during montage mixing; on the master bus (which has no
/// sidechain) the effect is bypassed with a one-shot warning.
fn apply_ducking(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
	sidechain: Option<&[f32]>,
) {
	let Some(sidechain) = sidechain else {
		warn_unsupported_once(
			&effect.type_id,
			"needs a clip-position accumulator sidechain (not available on the master bus)",
		);
		return;
	};
	let rate = rate.max(1.0);
	let threshold_db = effect_param(
		effect,
		quercus_node::block::AUDIO_DUCKING_THRESHOLD_INPUT,
		-30.0,
	)
	.clamp(-100.0, 0.0);
	let amount_db = effect_param(
		effect,
		quercus_node::block::AUDIO_DUCKING_AMOUNT_INPUT,
		-12.0,
	)
	.clamp(-60.0, 0.0);
	let attack_ms = effect_param(
		effect,
		quercus_node::block::AUDIO_DUCKING_ATTACK_INPUT,
		50.0,
	)
	.clamp(0.1, 5_000.0);
	let release_ms = effect_param(
		effect,
		quercus_node::block::AUDIO_DUCKING_RELEASE_INPUT,
		300.0,
	)
	.clamp(1.0, 10_000.0);
	let threshold = 10f64.powf(threshold_db / 20.0) as f32;
	let amount = 10f64.powf(amount_db / 20.0) as f32;
	let attack = (-1.0f64 / (rate * attack_ms / 1_000.0)).exp() as f32;
	let release = (-1.0f64 / (rate * release_ms / 1_000.0)).exp() as f32;
	let mut envelope = 1.0f32;
	for frame in 0..frames {
		let mut detector = 0.0f32;
		for channel in 0..channels {
			if let Some(value) = sidechain.get(frame * channels + channel) {
				detector = detector.max(value.abs());
			}
		}
		let target = if detector >= threshold { amount } else { 1.0 };
		let coeff = if target < envelope { attack } else { release };
		envelope = coeff * envelope + (1.0 - coeff) * target;
		for channel in 0..channels {
			buf[frame * channels + channel] *= envelope;
		}
	}
}

/// Apply one built-in audio effect to an interleaved chunk in place.
///
/// Dispatch is by `effect.type_id`: `EQ`, `Compressor`, `Limiter`, `Gate`
/// and `Ducking` have dedicated kernels; `Volume`/`Pan` drive the existing
/// node behavior through the factory, so the mixer reuses the node graph's
/// `process_samples` kernels instead of duplicating them. A disabled
/// effect is bypassed, and an unknown type id passes the chunk through
/// with a one-shot warning ([`warn_unsupported_once`]).
///
/// `buf` holds `frames * channels` interleaved F32 samples and is modified
/// in place; `frames` is a frame count (not a sample count); `rate` is the
/// sample rate in Hz (the kernels clamp it to at least 1); `sidechain` is
/// the montage accumulator over the clip window and is required only by
/// `Ducking` — the master bus passes `None`.
pub(crate) fn apply_audio_effect(
	buf: &mut [f32],
	frames: usize,
	channels: usize,
	effect: &crate::ticket::MontageAudioEffect,
	rate: f64,
	sidechain: Option<&[f32]>,
) {
	if !effect.enabled {
		return;
	}
	if effect.type_id == quercus_node::block::AUDIO_EFFECT_EQ_TYPE_ID {
		apply_eq(buf, frames, channels, effect, rate);
		return;
	}
	if effect.type_id == quercus_node::block::AUDIO_EFFECT_COMPRESSOR_TYPE_ID {
		apply_compressor(buf, frames, channels, effect, rate);
		return;
	}
	if effect.type_id == quercus_node::block::AUDIO_EFFECT_LIMITER_TYPE_ID {
		apply_limiter(buf, frames, channels, effect, rate);
		return;
	}
	if effect.type_id == quercus_node::block::AUDIO_EFFECT_GATE_TYPE_ID {
		apply_gate(buf, frames, channels, effect, rate);
		return;
	}
	if effect.type_id == quercus_node::block::AUDIO_EFFECT_DUCKING_TYPE_ID {
		apply_ducking(buf, frames, channels, effect, rate, sidechain);
		return;
	}
	let (param_input, default) = match effect.type_id.as_str() {
		quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID => {
			(quercus_node::block::AUDIO_VOLUME_INPUT, 1.0)
		}
		quercus_node::block::AUDIO_EFFECT_PAN_TYPE_ID => {
			(quercus_node::block::AUDIO_PAN_INPUT, 0.0)
		}
		other => {
			warn_unsupported_once(other, "not a built-in float-parameter audio effect");
			return;
		}
	};
	let Some((core, behavior)) =
		quercus_node::factory::Factory::global().create_any(&effect.type_id)
	else {
		warn_unsupported_once(&effect.type_id, "no node factory entry");
		return;
	};
	let value = effect
		.param(param_input)
		.map(|value| value.to_double())
		.filter(|value| value.is_finite())
		.unwrap_or(default);

	// Interleaved f32 -> planar F32 SampleBuffer for the node kernel.
	let mut input = quercus_node::value::SampleBuffer {
		format: quercus_core::SampleFormat::F32Planar,
		channels,
		sample_count: frames,
		data: vec![0u8; channels * frames * 4],
	};
	for channel in 0..channels {
		for frame in 0..frames {
			input.set_sample_value(channel, frame, buf[frame * channels + channel] as f64);
		}
	}
	let mut output = input.clone();
	let mut inputs = NodeValueRow::new();
	inputs.insert("samples_in".to_string(), NodeValue::Samples(input));
	inputs.insert(param_input.to_string(), NodeValue::Float(value));
	let range = TimeRange::new(
		Rational::new(0, 1),
		quercus_core::Rational::from_double(frames as f64 / rate.max(1.0)),
	);
	behavior.process_samples(&core, &inputs, range, &mut output);
	for channel in 0..channels {
		for frame in 0..frames {
			buf[frame * channels + channel] = output.sample_value(channel, frame) as f32;
		}
	}
}

/// Mix the audio montage into `acc` (`total_frames * channels` samples,
/// zero-initialized by the caller). Shared by the heap
/// [`render_audio_samples`] and the shm-slot [`render_audio_samples_into`]
/// paths so the decode/mix logic exists once.
pub(crate) fn mix_audio_montage(
	params: &crate::ticket::AudioTicketParams,
	rate: i32,
	channels: i32,
	total_frames: usize,
	acc: &mut [f32],
) -> Result<()> {
	for clip in &params.montage {
		// Overlap of the clip with the requested range.
		let in_time = params.range.in_().max(clip.in_time);
		let out_time = params.range.out().min(clip.out_time);
		if out_time <= in_time {
			continue;
		}
		// Round to the absolute sample grid like the decode side (which
		// anchors at `round(media_start·rate)`): truncation would shift a
		// clip's mix by up to one sample per chunk.
		let start_frame = ((in_time - params.range.in_()).to_f64() * rate as f64).round() as usize;
		let end_frame = ((out_time - params.range.in_()).to_f64() * rate as f64).round() as usize;
		if start_frame >= total_frames {
			continue;
		}
		let frames = (end_frame - start_frame).min(total_frames - start_frame);
		if frames == 0 {
			continue;
		}

		// Media time of the overlap start; the media-out is
		// media_start + (overlap duration).
		let media_start = clip.media_in + (in_time - clip.in_time);
		let media_end = media_start + (out_time - in_time);
		let mut buf = vec![0.0f32; frames * channels as usize];
		let decoder = open_decoder(&clip.filename, clip.stream_index)?;
		let range = TimeRange::new(media_start, media_end);
		let status = decoder
			.retrieve_audio(&mut buf, &range, rate, params.channel_layout)
			.map_err(|e| Error::Failed(format!("footage audio decode: {e:?}")))?;
		let written = match status {
			RetrieveAudioStatus::Success => frames,
			_ => 0,
		};
		// Gain, constant-power pan, clip fades and the audio effect chain
		// all run through the shared per-clip processor. The first decoded
		// frame sits at the rounded absolute sequence time. Ducking reads
		// the accumulator over this clip's window: it holds exactly the
		// clips already mixed (montage order), never future clips.
		let first_time = params.range.in_().to_f64() + start_frame as f64 / rate as f64;
		let start_sample = start_frame * channels as usize;
		let sample_count = written * channels as usize;
		let sidechain = &acc[start_sample..start_sample + sample_count];
		process_clip_audio(
			&mut buf,
			written,
			channels as usize,
			clip,
			first_time,
			rate,
			Some(sidechain),
		);
		// Mix into the accumulator.
		for i in 0..sample_count {
			acc[start_sample + i] += buf[i];
		}
	}
	// Master bus: the sequence-level chain runs on the fully mixed
	// accumulator, then the playback/delivery full-scale clamp.
	apply_bus_effects(acc, channels as usize, &params.bus_effects, rate as f64);
	// Clamp to [-1, 1]: overlapping clips sum linearly and can exceed
	// full scale, and the playback sink (cpal) forwards samples without
	// any clamping of its own.
	for sample in acc.iter_mut() {
		*sample = sample.clamp(-1.0, 1.0);
	}
	Ok(())
}

/// Apply a master-bus chain to the mixed interleaved accumulator.
///
/// The chain supports the built-in `Volume`, `Pan`, `EQ`, `Compressor`,
/// `Limiter` and `Gate` kernels. `Ducking` is a clip-position sidechain
/// effect and is bypassed on the bus (there is no detector signal after
/// the final mix).
///
/// `acc` holds `frames * channels` interleaved F32 samples and is modified
/// in place; `channels` is floored to 1 so the frame count cannot divide by
/// zero; `effects` run in chain order with each stage feeding the next;
/// `rate` is the sample rate in Hz. Called by [`mix_audio_montage`] after
/// the per-clip mixing and before the full-scale clamp.
pub(crate) fn apply_bus_effects(
	acc: &mut [f32],
	channels: usize,
	effects: &[crate::ticket::MontageAudioEffect],
	rate: f64,
) {
	let channels = channels.max(1);
	let frames = acc.len() / channels;
	for effect in effects {
		apply_audio_effect(acc, frames, channels, effect, rate, None);
	}
}

/// Render the audio montage over `params.range` directly into `dst` as
/// little-endian f32 bytes (M15 S3 worker seam): the render worker passes
/// a shared-memory slot slice as `dst`, so the samples land in the slot
/// with no staging allocation. `dst.len()` must hold
/// `frame_count * channels * 4` bytes.
/// Render the audio montage over `params.range` directly into `dst` as
/// little-endian f32 bytes (M15 S3 worker seam): the render worker passes
/// a shared-memory slot slice as `dst`, so the samples land in the slot
/// with no staging allocation. `dst.len()` must hold
/// `frame_count * channels * 4` bytes.
pub fn render_audio_samples_into(
	params: &crate::ticket::AudioTicketParams,
	dst: &mut [u8],
) -> Result<()> {
	let (rate, _layout, channels, total_frames) = audio_layout(params)?;
	let need = total_frames
		.saturating_mul(channels as usize)
		.saturating_mul(4);
	if dst.len() < need {
		return Err(Error::NoMem);
	}
	let mut acc = vec![0.0f32; total_frames.saturating_mul(channels as usize)];
	mix_audio_montage(params, rate, channels, total_frames, &mut acc)?;
	// Interleaved f32 -> little-endian bytes in the slot.
	for (out, sample) in dst[..need].chunks_exact_mut(4).zip(&acc) {
		out.copy_from_slice(&sample.to_le_bytes());
	}
	Ok(())
}
