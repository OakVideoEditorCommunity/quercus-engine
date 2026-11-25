use super::*;
use crate::decoder::RetrieveVideoParams;
use quercus_core::Rational;

fn video_params() -> RetrieveVideoParams {
	RetrieveVideoParams {
		stream: CodecStream::new(),
		time: Rational::new(1, 30),
		length: TimeRange::default(),
		force_range: crate::decoder::K_COLOR_RANGE_DEFAULT,
		is_image_sequence: false,
		image_sequence_digits: 0,
		image_sequence_number: 0,
		mode: crate::decoder::RenderMode::Offline,
		alpha_is_premultiplied: false,
		target_size: None,
	}
}

#[test]
fn ffmpeg_decoder_identity_and_capabilities() {
	let d = FFmpegDecoder::new();
	assert_eq!(d.id(), "ffmpeg");
	assert!(d.supports_video());
	assert!(d.supports_audio());
	assert!(d.probe("any.mp4", None).is_none());
}

#[test]
fn ffmpeg_decoder_closed_state_errors() {
	let d = FFmpegDecoder::new();
	// Open on a missing file fails and leaves the decoder closed
	// (C++ parity: the stream is unset on failure).
	let s = CodecStream::with_block("in.mp4".to_string(), 0, None);
	assert!(d.open(&s).is_err());
	assert_eq!(d.stream().filename(), "");
	assert!(d.close().is_ok());

	// All media operations on a closed decoder fail.
	assert!(d.retrieve_video_frame(&video_params()).is_err());
	assert!(d.retrieve_video(&video_params()).is_err());
	let mut dest = [0f32; 8];
	assert!(d
		.retrieve_audio(
			&mut dest,
			&TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
			48000,
			0x3
		)
		.is_err());
	assert!(d
		.conform_audio(&["a.pcm".to_string()], 48000, 0x3, 10, None)
		.is_err());
	assert_eq!(d.get_audio_start_offset(), Rational::new(0, 1));
}

#[test]
fn ffmpeg_encoder_identity_and_config_validation() {
	let mut params = EncodingParams::default();
	let name = b"out/ffmpeg.mp4";
	params.filename[..name.len()].copy_from_slice(name);
	params.format = 2; // mp4
	params.video_enabled = 1;
	params.video_codec = 1; // H264
	params.video_width = 64;
	params.video_height = 64;
	params.video_time_base_num = 1;
	params.video_time_base_den = 10;
	let e = FFmpegEncoder::with_params(params);

	assert_eq!(e.id(), "ffmpeg");
	assert!(e.supports_video());
	assert!(e.supports_audio());
	assert!(e.supports_subtitles());
	assert!(!e.supports_image_sequences());
	assert!(e.is_configurable());
	assert_eq!(e.filename(), "out/ffmpeg.mp4");
	assert_eq!(e.desired_pixel_format(), Some(PixelFormat::F32));
	assert_eq!(e.desired_sample_format(), Some(SampleFormat::F32));
	assert_eq!(e.get_error(), "");

	// configure rejects unknown export formats.
	let mut bad = EncodingParams::default();
	bad.format = 99;
	assert!(e.configure(&bad).is_err());

	// A valid configures succeeds but nothing is open yet.
	let mut good = EncodingParams::default();
	good.format = 2;
	good.video_enabled = 1;
	good.video_codec = 1;
	good.video_width = 64;
	good.video_height = 64;
	good.video_time_base_num = 1;
	good.video_time_base_den = 10;
	assert!(e.configure(&good).is_ok());

	// Operations before open fail cleanly; close/flush are safe.
	let frame = Frame::new();
	assert!(e.write_video(&frame).is_err());
	assert!(e.write_audio(&[0f32; 4], 1).is_err());
	assert!(e.write_subtitle("hi", 0.0, 1.0).is_err());
	assert!(e.close().is_ok());
	assert!(e.flush().is_ok());
}

#[test]
fn ffmpeg_encoder_open_rejects_invalid_config() {
	// No filename.
	let e = FFmpegEncoder::with_params(EncodingParams::default());
	assert!(e.open().is_err());

	// No enabled tracks.
	let mut p = EncodingParams::default();
	let name = b"out/x.mp4";
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2;
	let e = FFmpegEncoder::with_params(p);
	assert!(e.open().is_err());
}

#[test]
fn pix_fmt_depth_and_yuv_detects_depth_and_kind() {
	// YUV luma depths (on the YUVJ→regular-normalized format).
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV420P), (8, true));
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV420P10LE), (10, true));
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV444P16LE), (16, true));
	// RGB formats never take the high-bit-depth YUV fallback.
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::RGBA), (8, false));
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::RGB48LE), (16, false));
}

#[test]
fn yuv_matrix_mapping_is_strict() {
	use quercus_core::colormath::YuvMatrix;
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_BT709, 1920, 1080),
		YuvMatrix::Bt709
	);
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_BT470BG, 640, 480),
		YuvMatrix::Bt601
	);
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_SMPTE170M, 1920, 1080),
		YuvMatrix::Bt601
	);
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_BT2020_NCL, 1920, 1080),
		YuvMatrix::Bt2020
	);
	// SMPTE 240M / BT.2020 CL / unknown tags are NOT mapped directly —
	// they fall back by size (HD → BT.709, SD → BT.601).
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_SMPTE240M, 1920, 1080),
		YuvMatrix::Bt709
	);
	assert_eq!(
		yuv_matrix_for(AVCOL_SPC_BT2020_CL, 640, 480),
		YuvMatrix::Bt601
	);
	assert_eq!(yuv_matrix_for(0, 1920, 1080), YuvMatrix::Bt709);
	assert_eq!(yuv_matrix_for(0, 640, 480), YuvMatrix::Bt601);
	assert_eq!(yuv_matrix_for(0, 1000, 600), YuvMatrix::Bt709); // h > 576
	assert_eq!(yuv_matrix_for(0, 720, 576), YuvMatrix::Bt601); // 576 is SD
}

#[test]
fn sws_colorspace_mapping_keeps_legacy_behavior() {
	assert_eq!(sws_colorspace_for(AVCOL_SPC_BT709, 0, 0), SWS_CS_ITU709);
	assert_eq!(sws_colorspace_for(AVCOL_SPC_BT470BG, 0, 0), SWS_CS_ITU601);
	assert_eq!(sws_colorspace_for(AVCOL_SPC_SMPTE170M, 0, 0), SWS_CS_ITU601);
	assert_eq!(
		sws_colorspace_for(AVCOL_SPC_SMPTE240M, 0, 0),
		SWS_CS_SMPTE240M
	);
	assert_eq!(
		sws_colorspace_for(AVCOL_SPC_BT2020_NCL, 0, 0),
		SWS_CS_BT2020
	);
	assert_eq!(sws_colorspace_for(AVCOL_SPC_BT2020_CL, 0, 0), SWS_CS_BT2020);
	assert_eq!(sws_colorspace_for(0, 1920, 1080), SWS_CS_ITU709);
	assert_eq!(sws_colorspace_for(0, 640, 480), SWS_CS_ITU601);
}

/// A 2×1 YUV444P16LE frame: full-range white (Y=65535, neutral C) left,
/// full-range black (Y=0, neutral C) right. Each sample is a u16.
fn synthetic_yuv444p16_frame() -> ffmpeg::frame::Video {
	let mut f = ffmpeg::frame::Video::new(Pixel::YUV444P16LE, 2, 1);
	for plane in 0..3 {
		let data = f.data_mut(plane);
		for (px, v) in data.chunks_exact_mut(2).take(2).enumerate() {
			let code = match plane {
				0 => [65535u16, 0u16][px], // luma: white, black
				_ => 32768u16,             // chroma: neutral
			};
			v[..2].copy_from_slice(&code.to_le_bytes());
		}
	}
	f
}

#[test]
fn yuv444p16_fallback_round_trips_full_range_white_and_black() {
	let f = synthetic_yuv444p16_frame();
	let bytes = convert_yuv444p16_to_rgba_f32(&f, 2, 1, YuvMatrix::Bt709, true);
	let px = |i: usize| -> [f32; 4] {
		let b = &bytes[i * 16..i * 16 + 16];
		[
			f32::from_le_bytes(b[0..4].try_into().unwrap()),
			f32::from_le_bytes(b[4..8].try_into().unwrap()),
			f32::from_le_bytes(b[8..12].try_into().unwrap()),
			f32::from_le_bytes(b[12..16].try_into().unwrap()),
		]
	};
	let white = px(0);
	let black = px(1);
	for c in 0..3 {
		assert!((white[c] - 1.0).abs() < 1e-6, "white[{c}] = {}", white[c]);
		assert!(black[c].abs() < 1e-6, "black[{c}] = {}", black[c]);
	}
	assert_eq!(white[3], 1.0);
	assert_eq!(black[3], 1.0);
}

// ---- helpers and decoder state (M5 audit batch) ----------------------

/// `demo.mp4` at the repository root, opened as `stream`.
fn demo_stream(stream: i32) -> CodecStream {
	CodecStream::with_block(
		std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("../../testdata/demo.mp4")
			.to_string_lossy()
			.into_owned(),
		stream,
		None,
	)
}

#[test]
fn jpeg_space_conversion_maps_every_variant_and_passes_others_through() {
	for (jpeg, regular) in [
		(Pixel::YUVJ420P, Pixel::YUV420P),
		(Pixel::YUVJ422P, Pixel::YUV422P),
		(Pixel::YUVJ444P, Pixel::YUV444P),
		(Pixel::YUVJ440P, Pixel::YUV440P),
		(Pixel::YUVJ411P, Pixel::YUV411P),
	] {
		assert_eq!(convert_jpeg_space_to_regular_space(jpeg), regular);
	}
	assert_eq!(
		convert_jpeg_space_to_regular_space(Pixel::YUV420P),
		Pixel::YUV420P
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(Pixel::VAAPI),
		Pixel::VAAPI
	);
}

#[test]
fn native_pixel_format_classifies_depth() {
	assert_eq!(native_pixel_format(Pixel::RGBAF32LE), PixelFormat::F32);
	assert_eq!(native_pixel_format(Pixel::GBRPF32LE), PixelFormat::F32);
	assert_eq!(native_pixel_format(Pixel::RGB48LE), PixelFormat::U16);
	assert_eq!(native_pixel_format(Pixel::GBRP10LE), PixelFormat::U16);
	assert_eq!(native_pixel_format(Pixel::GBRP16BE), PixelFormat::U16);
	assert_eq!(native_pixel_format(Pixel::YUV420P), PixelFormat::U8);
	assert_eq!(native_pixel_format(Pixel::RGBA), PixelFormat::U8);
}

#[test]
fn channel_layout_from_mask_zero_and_multichannel() {
	let stereo = channel_layout_from_mask(0);
	assert_eq!(stereo.channels(), 2, "zero mask falls back to stereo");
	let surround = channel_layout_from_mask(0b111);
	assert_eq!(surround.channels(), 3);
	assert_eq!(surround.bits(), 0b111);
}

#[test]
fn error_and_cancel_helpers_are_informative() {
	let err = ffmpeg_err(FfmpegError::Eof);
	assert!(format!("{err:?}").contains("ffmpeg error"));
	assert!(is_eof_or_eagain(&FfmpegError::Eof));
	assert!(is_eof_or_eagain(&FfmpegError::Other {
		errno: ffmpeg::error::EAGAIN
	}));
	assert!(!is_eof_or_eagain(&FfmpegError::InvalidData));

	assert!(!cancel_atom_is_cancelled(None));
	let atom = CancelAtom::new();
	assert!(!cancel_atom_is_cancelled(Some(&atom)));
	atom.cancel();
	assert!(cancel_atom_is_cancelled(Some(&atom)));
}

#[test]
fn ref_frame_shares_buffers_and_reports_geometry() {
	let mut video = ffmpeg::frame::Video::new(Pixel::YUV420P, 8, 4);
	video.set_pts(Some(42));
	let reference = RefFrame::from_video(&video).expect("reference");
	assert_eq!(reference.width(), 8);
	assert_eq!(reference.height(), 4);
	assert_eq!(reference.pts(), Some(42));
	assert!(!format!("{reference:?}").is_empty());

	// A clone keeps the buffers alive after the original reference drops.
	let clone = reference.clone();
	drop(reference);
	let back = clone.to_video().expect("owned video");
	assert_eq!((back.width(), back.height()), (8, 4));
	assert_eq!(back.pts(), Some(42));

	// `clone_raw` adopts a raw frame with the same semantics.
	let raw = RefFrame::clone_raw(unsafe { video.as_ptr() }).expect("clone_raw");
	assert_eq!(raw.width(), 8);
	assert_eq!(raw.height(), 4);
}

#[test]
fn decoder_open_state_machine_and_hardware_report() {
	let d = FFmpegDecoder::new();
	// Unopened sessions report state instead of panicking.
	assert!(d.retrieve_video_frame_gpu(&video_params()).is_err());
	assert!(!d.hardware_decoding());

	// A stream without a filename is rejected before any IO.
	assert!(d.open(&CodecStream::new()).is_err());

	// Real media: the same stream re-opens as a no-op, a different one
	// is rejected, and close is idempotent.
	let video = demo_stream(0);
	d.open(&video).expect("open video");
	let _ = d.hardware_decoding();
	d.open(&video).expect("same stream re-open");
	let audio = demo_stream(1);
	assert!(matches!(d.open(&audio), Err(crate::error::Error::State)));
	assert!(d.close().is_ok());
	assert!(d.close().is_ok());
}

#[test]
fn cross_stream_retrieval_reports_unsupported() {
	let d = FFmpegDecoder::new();
	let mut dest = [0f32; 8];
	let range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 10));

	// A video session has no audio to retrieve.
	let video = demo_stream(0);
	d.open(&video).expect("open video");
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, 48000, 0x3).unwrap(),
		RetrieveAudioStatus::Unsupported
	);
	// No host GPU context is installed in tests: the import declines
	// cleanly (Ok(None)) instead of erroring.
	assert!(d.retrieve_video_frame_gpu(&video_params()).is_ok());
	d.close().expect("close");

	// An audio session cannot produce video, on either path.
	let audio = demo_stream(1);
	d.open(&audio).expect("open audio");
	assert!(d.retrieve_video_frame(&video_params()).is_err());
	assert!(d.retrieve_video(&video_params()).is_err());
	assert!(d.retrieve_video_frame_gpu(&video_params()).is_err());
	// Invalid audio arguments are unsupported, not a crash.
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, 0, 0x3).unwrap(),
		RetrieveAudioStatus::Unsupported
	);
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, 48000, 0).unwrap(),
		RetrieveAudioStatus::Unsupported
	);
	d.close().expect("close");
}

// ---- extended coverage: pure helpers ---------------------------------

fn small_frame(format: Pixel, pts: Option<i64>) -> ffmpeg::frame::Video {
	let mut f = ffmpeg::frame::Video::new(format, 2, 2);
	f.set_pts(pts);
	f
}

fn cached_frame(pts: Option<i64>) -> RefFrame {
	RefFrame::from_video(&small_frame(Pixel::YUV420P, pts)).expect("reference frame")
}

fn video_state(cache: Vec<RefFrame>, cache_at_zero: bool, cache_at_eof: bool) -> VideoDecodeState {
	VideoDecodeState {
		scaler: None,
		cache: cache.into(),
		cache_at_zero,
		cache_at_eof,
		second_ts: 10,
		eof_fallback_warned: false,
	}
}

#[test]
fn decoder_default_state_accessors() {
	let d = FFmpegDecoder::default();
	assert!(d.hw_decoder_name().is_none());
	assert_eq!(d.audio_seek_count(), 0);
	assert!(!d.hardware_decoding());
	assert_eq!(d.stream().filename(), "");
}

#[test]
fn ref_frame_unset_pts_and_debug() {
	let no_pts = cached_frame(None);
	assert_eq!(no_pts.pts(), None);
	assert!(format!("{no_pts:?}").contains("pts"));
	assert_eq!((no_pts.width(), no_pts.height()), (2, 2));

	let with_pts = cached_frame(Some(7));
	assert_eq!(with_pts.pts(), Some(7));

	// `clone_raw` shares the buffers of a raw frame and reports the pts.
	let raw = RefFrame::clone_raw(with_pts.as_ptr()).expect("clone_raw");
	assert_eq!(raw.pts(), Some(7));
	assert!(raw.to_video().is_some());
}

#[test]
fn get_frame_from_cache_covers_boundaries_and_eof() {
	let cache = vec![
		cached_frame(Some(0)),
		cached_frame(Some(10)),
		cached_frame(Some(20)),
	];

	// Before the front: the front frame only when the cache came from
	// a zero seek; otherwise a miss.
	let s = video_state(cache.clone(), true, false);
	assert_eq!(get_frame_from_cache(&s, -5).unwrap().pts(), Some(0));
	let s = video_state(cache.clone(), false, false);
	assert!(get_frame_from_cache(&s, -5).is_none());

	// Inside the cache: exact hits and the previous frame for gaps.
	let s = video_state(cache.clone(), false, false);
	assert_eq!(get_frame_from_cache(&s, 0).unwrap().pts(), Some(0));
	assert_eq!(get_frame_from_cache(&s, 5).unwrap().pts(), Some(0));
	assert_eq!(get_frame_from_cache(&s, 15).unwrap().pts(), Some(10));
	assert_eq!(get_frame_from_cache(&s, 20).unwrap().pts(), Some(20));
	assert!(get_frame_from_cache(&s, 21).is_none());

	// Past the back: the last frame only at EOF.
	let s = video_state(cache.clone(), false, true);
	assert_eq!(get_frame_from_cache(&s, 25).unwrap().pts(), Some(20));
	let s = video_state(cache.clone(), false, false);
	assert!(get_frame_from_cache(&s, 25).is_none());

	// An empty cache never yields a frame.
	let s = video_state(Vec::new(), true, true);
	assert!(get_frame_from_cache(&s, 0).is_none());
}

#[test]
fn frame_interval_ts_edges() {
	let two = video_state(
		vec![cached_frame(Some(0)), cached_frame(Some(10))],
		false,
		false,
	);
	assert_eq!(DecoderState::frame_interval_ts(&two), Some(10));

	let irregular = video_state(
		vec![
			cached_frame(Some(0)),
			cached_frame(Some(30)),
			cached_frame(Some(40)),
		],
		false,
		false,
	);
	assert_eq!(DecoderState::frame_interval_ts(&irregular), Some(10));

	let same = video_state(
		vec![cached_frame(Some(5)), cached_frame(Some(5))],
		false,
		false,
	);
	assert_eq!(DecoderState::frame_interval_ts(&same), None);

	let single = video_state(vec![cached_frame(Some(5))], false, false);
	assert_eq!(DecoderState::frame_interval_ts(&single), None);

	let no_pts = video_state(vec![cached_frame(None), cached_frame(None)], false, false);
	assert_eq!(DecoderState::frame_interval_ts(&no_pts), None);
}

#[test]
fn frame_colorimetry_resolves_force_and_jpeg_ranges() {
	let plain = small_frame(Pixel::YUV420P, Some(0));
	assert!(
		!unsafe { frame_colorimetry(plain.as_ptr(), 99).3 },
		"untagged is limited"
	);
	assert!(unsafe { frame_colorimetry(plain.as_ptr(), quercus_core_COLOR_RANGE_FULL).3 });
	assert!(!unsafe { frame_colorimetry(plain.as_ptr(), quercus_core_COLOR_RANGE_LIMITED).3 });

	let mut full = small_frame(Pixel::YUV420P, Some(0));
	full.set_color_range(ffmpeg::color::Range::JPEG);
	assert!(
		unsafe { frame_colorimetry(full.as_ptr(), 99).3 },
		"JPEG is full range"
	);

	// A YUVJ source is full range by definition.
	let jpeg = small_frame(Pixel::YUVJ420P, Some(0));
	assert!(unsafe { frame_colorimetry(jpeg.as_ptr(), 99).3 });
}

#[test]
fn pix_fmt_depth_handles_missing_descriptor() {
	assert_eq!(pix_fmt_depth_and_yuv(Pixel::None), (8, false));
}

#[test]
fn convert_rgba_f32_le_honours_stride_and_forces_opaque_alpha() {
	let (w, h) = (2u32, 2u32);
	let stride = 2 * PIXEL_F32_BYTES + 8; // padded rows
	let mut data = vec![0u8; stride * h as usize];
	for y in 0..h as usize {
		for x in 0..w as usize {
			let off = y * stride + x * PIXEL_F32_BYTES;
			for c in 0..4 {
				data[off + c * 4..off + c * 4 + 4].copy_from_slice(&(c as f32 + 0.5).to_le_bytes());
			}
		}
	}
	let out = convert_rgba_f32_le(&data, w, h, stride);
	assert_eq!(out.len(), (w * h) as usize * PIXEL_F32_BYTES);
	for px in out.as_chunks::<PIXEL_F32_BYTES>().0 {
		assert_eq!(f32::from_le_bytes(px[0..4].try_into().unwrap()), 0.5);
		assert_eq!(f32::from_le_bytes(px[4..8].try_into().unwrap()), 1.5);
		assert_eq!(
			f32::from_le_bytes(px[12..16].try_into().unwrap()),
			1.0,
			"alpha is forced opaque"
		);
	}
}

#[test]
fn convert_rgba8_to_f32_maps_unorm_channels() {
	let data = [
		0u8, 255, 128, 255, // pixel 0
		255, 0, 0, 0, // pixel 1
	];
	let out = convert_rgba8_to_f32(&data, 2, 1, 8);
	assert_eq!(out.len(), 2 * PIXEL_F32_BYTES);
	let px = |i: usize| f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
	assert_eq!(px(0), 0.0);
	assert_eq!(px(1), 1.0);
	assert!((px(2) - 128.0 / 255.0).abs() < 1e-6);
	assert_eq!(px(3), 1.0);
	assert_eq!(px(4), 1.0);
	assert_eq!(px(7), 0.0);
}

#[test]
fn export_codec_mapping_covers_every_code() {
	use ffmpeg::codec::Id;
	let cases: [(i32, Id); 15] = [
		(0, Id::DNXHD),
		(1, Id::H264),
		(2, Id::H264),
		(3, Id::HEVC),
		(6, Id::PRORES),
		(7, Id::CFHD),
		(10, Id::MPEG2VIDEO),
		(11, Id::MP3),
		(12, Id::AAC),
		(13, Id::PCM_S16LE),
		(14, Id::OPUS),
		(15, Id::VORBIS),
		(16, Id::FLAC),
		(17, Id::SUBRIP),
		(18, Id::AV1),
	];
	for (code, id) in cases {
		assert_eq!(export_codec_to_id(code), Some(id), "code {code}");
	}
	for code in [4, 5, 8, 9, 19, -1, 99] {
		assert_eq!(export_codec_to_id(code), None, "unknown code {code}");
	}
}

#[test]
fn default_encoder_formats_per_codec() {
	use ffmpeg::codec::Id;
	assert_eq!(
		default_pixel_format_for_codec(Id::PRORES),
		Pixel::YUV422P10LE
	);
	assert_eq!(
		default_pixel_format_for_codec(Id::DNXHD),
		Pixel::YUV422P10LE
	);
	assert_eq!(default_pixel_format_for_codec(Id::H264), Pixel::YUV420P);
	assert!(matches!(
		default_sample_format_for_codec(Id::PCM_S16LE),
		Sample::I16(SampleType::Packed)
	));
	assert!(matches!(
		default_sample_format_for_codec(Id::FLAC),
		Sample::I16(SampleType::Planar)
	));
	assert!(matches!(
		default_sample_format_for_codec(Id::MP3),
		Sample::F32(SampleType::Packed)
	));
	assert!(matches!(
		default_sample_format_for_codec(Id::AAC),
		Sample::F32(SampleType::Planar)
	));
}

#[test]
fn hardware_encoder_priority_matches_platform() {
	#[cfg(target_os = "macos")]
	{
		assert_eq!(hardware_video_encoder_names(1), &["h264_videotoolbox"][..]);
		assert_eq!(hardware_video_encoder_names(3), &["hevc_videotoolbox"][..]);
		assert!(hardware_video_encoder_names(18).is_empty());
	}
	#[cfg(any(target_os = "linux", target_os = "windows"))]
	{
		assert_eq!(
			hardware_video_encoder_names(1),
			&["h264_nvenc", "h264_qsv", "h264_amf"][..]
		);
		assert_eq!(
			hardware_video_encoder_names(3),
			&["hevc_nvenc", "hevc_qsv", "hevc_amf"][..]
		);
		assert_eq!(
			hardware_video_encoder_names(18),
			&["av1_nvenc", "av1_qsv", "av1_amf"][..]
		);
	}
	assert!(hardware_video_encoder_names(2).is_empty());
	assert!(hardware_video_encoder_names(6).is_empty());
}

#[test]
fn software_encoder_is_the_final_candidate() {
	let Some(software) = ffmpeg::encoder::find(ffmpeg::codec::Id::H264) else {
		return;
	};
	let candidates = video_encoder_candidates(1, ffmpeg::codec::Id::H264);
	assert_eq!(
		candidates.last().map(|codec| codec.name()),
		Some(software.name())
	);
}

#[test]
fn hardware_encoder_pixel_format_supports_cpu_input() {
	let params = EncodingParams::default();
	assert_eq!(
		pixel_format_for_video_encoder(&params, ffmpeg::codec::Id::H264, "h264_qsv"),
		Pixel::NV12
	);
	assert_eq!(
		pixel_format_for_video_encoder(&params, ffmpeg::codec::Id::H264, "h264_nvenc"),
		Pixel::YUV420P
	);

	let mut explicit = EncodingParams::default();
	explicit.video_pix_fmt[..7].copy_from_slice(b"yuv420p");
	assert_eq!(
		pixel_format_for_video_encoder(&explicit, ffmpeg::codec::Id::H264, "h264_qsv"),
		Pixel::NV12
	);
}

#[test]
fn pixel_format_from_name_parses_and_rejects() {
	let empty = [0u8; 64];
	assert_eq!(pixel_format_from_name(&empty), None);

	let mut named = [0u8; 64];
	named[..7].copy_from_slice(b"yuv420p");
	assert_eq!(pixel_format_from_name(&named), Some(Pixel::YUV420P));

	// No NUL terminator, unparseable name.
	let full = [b'x'; 64];
	assert_eq!(pixel_format_from_name(&full), None);

	// Invalid UTF-8 before the terminator.
	let mut invalid = [0u8; 64];
	invalid[0] = 0xff;
	assert_eq!(pixel_format_from_name(&invalid), None);
}

#[test]
fn sample_format_to_ffmpeg_covers_every_variant() {
	for format in [
		SampleFormat::U8Planar,
		SampleFormat::S16Planar,
		SampleFormat::S32Planar,
		SampleFormat::S64Planar,
		SampleFormat::F32Planar,
		SampleFormat::F64Planar,
		SampleFormat::U8,
		SampleFormat::S16,
		SampleFormat::S32,
		SampleFormat::S64,
		SampleFormat::F32,
		SampleFormat::F64,
	] {
		assert!(
			sample_format_to_ffmpeg(format).is_some(),
			"{format:?} must map"
		);
	}
	assert!(sample_format_to_ffmpeg(SampleFormat::Invalid).is_none());
	assert!(matches!(
		sample_format_to_ffmpeg(SampleFormat::S16Planar),
		Some(Sample::I16(SampleType::Planar))
	));
	assert!(matches!(
		sample_format_to_ffmpeg(SampleFormat::F64),
		Some(Sample::F64(SampleType::Packed))
	));
}

#[test]
fn apply_sws_output_colorspace_covers_matrix_arms() {
	fn rgba_to_yuv() -> scaling::Context {
		scaling::Context::get(
			Pixel::RGBA,
			2,
			2,
			Pixel::YUV420P,
			2,
			2,
			scaling::Flags::BILINEAR,
		)
		.expect("scaler")
	}
	// 1 = BT.709, 9/10 = BT.2020, everything else = BT.601; both
	// limited (0/1) and full (2) ranges.
	for (space, range) in [(0, 0), (1, 0), (9, 2), (10, 2), (6, 1), (99, 2)] {
		let mut scaler = rgba_to_yuv();
		let p = EncodingParams {
			color_space: space,
			color_range: range,
			..EncodingParams::default()
		};
		apply_sws_output_colorspace(&mut scaler, &p);
	}
}

#[test]
fn extract_source_start_time_reads_timecode_and_bwf() {
	let timecode = [("timecode".to_string(), "00:00:00:10".to_string())];
	let parsed = extract_source_start_time(&timecode, FfRational(1, 25), 0);
	assert!(parsed.valid);
	assert_eq!(parsed.time, Rational::new(2, 5));

	// A valid timecode wins even if a time_reference follows.
	let both = [
		("timecode".to_string(), "00:00:00:10".to_string()),
		("time_reference".to_string(), "48000".to_string()),
	];
	let parsed = extract_source_start_time(&both, FfRational(1, 25), 48000);
	assert!(parsed.valid);
	assert_eq!(parsed.time, Rational::new(2, 5));

	let bwf = [("time_reference".to_string(), "48000".to_string())];
	let parsed = extract_source_start_time(&bwf, FfRational(1, 1), 48000);
	assert!(parsed.valid);
	assert_eq!(parsed.time, Rational::new(1, 1));

	// Invalid values leave the result invalid.
	let junk = [
		("timecode".to_string(), "junk".to_string()),
		(
			"time_reference".to_string(),
			"also-not-a-number".to_string(),
		),
		("unrelated".to_string(), "x".to_string()),
	];
	let parsed = extract_source_start_time(&junk, FfRational(1, 25), 48000);
	assert!(!parsed.valid);
	assert!(parsed.time.is_null());
}

#[test]
fn stream_time_base_after_header_reports_unset_values() {
	let path = std::env::temp_dir().join(format!("quercuscodec_tb_{}.mp4", std::process::id()));
	let mut output = ffmpeg::format::output(&path).expect("output context");
	let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MPEG2VIDEO).expect("mpeg2 encoder");
	let stream = output.add_stream(codec).expect("add stream");
	let index = stream.index();
	// Before `write_header` the stream time base is still 0/0.
	assert_eq!(stream_time_base_after_header(&output, index), None);
	// Out-of-range indices are a clean None.
	assert_eq!(stream_time_base_after_header(&output, 12345), None);
	drop(output);
	let _ = std::fs::remove_file(&path);
}

// ---- extended coverage: real media -----------------------------------

/// Encode a 64x64, 10 fps, 1 s clip with a stereo PCM track.
fn test_clip(tag: &str) -> std::path::PathBuf {
	let path = std::env::temp_dir().join(format!(
		"quercuscodec_ffmpeg_{tag}_{}.mp4",
		std::process::id()
	));
	crate::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip");
	path
}

fn clip_stream(path: &std::path::Path, index: i32) -> CodecStream {
	CodecStream::with_block(path.to_string_lossy().into_owned(), index, None)
}

fn temp_file(tag: &str, extension: &str) -> std::path::PathBuf {
	std::env::temp_dir().join(format!(
		"quercuscodec_ffmpeg_{tag}_{}.{extension}",
		std::process::id()
	))
}

#[test]
fn decode_video_cache_seek_and_eof_fallback() {
	let _guard = crate::lock_tests();
	let path = test_clip("video");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 0)).expect("open video");
	let _ = d.hardware_decoding();

	let mut p = video_params();
	p.time = Rational::new(0, 1);
	p.target_size = Some((32, 32));
	p.force_range = quercus_core_COLOR_RANGE_FULL;
	let frame = d.retrieve_video_frame(&p).expect("frame at 0");
	assert_eq!((frame.width(), frame.height()), (32, 32));
	assert_eq!(frame.format(), PixelFormat::F32);
	assert!(!frame.data().expect("pixels").is_empty());

	// Same request: served from the frame cache.
	let cached = d.retrieve_video_frame(&p).expect("cached frame");
	assert_eq!(cached.timestamp, Rational::new(0, 1));

	// Before the first frame: the zero-seek cache flag makes the first
	// decoded frame come back even though its PTS is after the target.
	p.time = Rational::new(-1, 1);
	let before = d.retrieve_video_frame(&p).expect("negative time");
	assert_eq!(before.timestamp, Rational::new(-1, 1));

	// A later time forces a seek/decode; the limited-range metadata path.
	p.time = Rational::new(1, 2);
	p.target_size = None;
	p.force_range = quercus_core_COLOR_RANGE_LIMITED;
	let mid = d.retrieve_video_frame(&p).expect("frame at 1/2");
	assert_eq!(mid.width(), 64);

	// A degenerate (zero) target size falls back to the native size.
	p.target_size = Some((0, 0));
	let native = d.retrieve_video_frame(&p).expect("zero target falls back");
	assert_eq!(native.width(), 64);
	p.target_size = None;

	// `retrieve_video` reports decode success without a texture.
	let _token = d.retrieve_video(&p).expect("retrieve_video token");

	// No host GPU context is installed in tests: the import path
	// declines cleanly; on a GPU-equipped host it may import, but it
	// must never fail.
	assert!(d.retrieve_video_frame_gpu(&p).is_ok());

	// Audio operations on a video session fail with the medium error.
	let error = d
		.conform_audio(&["unused.pcm".to_string()], 48000, 0x3, 1, None)
		.unwrap_err();
	assert!(
		error.to_string().contains("audio stream"),
		"the medium error is named: {error}"
	);

	// Format start offset is always a value (stream start or default).
	let _offset = d.get_audio_start_offset();

	// Far beyond the 1 s clip: exercises the EOF fallback (last cached
	// frame + once-per-session warning) or a clean decode error.
	p.time = Rational::new(30, 1);
	let beyond = d.retrieve_video_frame(&p);
	if let Ok(frame) = &beyond {
		assert_eq!(frame.width(), 64);
	}

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_audio_continuity_and_edge_ranges() {
	let _guard = crate::lock_tests();
	let path = test_clip("audio");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 1)).expect("open audio");
	let rate = 48000;
	let layout = 0x3;

	let mut dest = vec![0f32; 4800 * 2];
	let range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 10));
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, rate, layout).unwrap(),
		RetrieveAudioStatus::Success
	);
	assert!(
		dest.iter().any(|v| v.abs() > 1e-6),
		"the sine tone decodes to non-silence"
	);
	let seeks = d.audio_seek_count();
	assert!(seeks >= 1, "the first chunk seeks");

	// The immediately following chunk continues without a seek.
	let mut next = vec![0f32; 4800 * 2];
	let next_range = TimeRange::new(Rational::new(1, 10), Rational::new(1, 5));
	assert_eq!(
		d.retrieve_audio(&mut next, &next_range, rate, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);
	assert_eq!(
		d.audio_seek_count(),
		seeks,
		"a contiguous chunk must not re-seek"
	);

	// A non-contiguous chunk seeks again.
	let mut jump = vec![0f32; 4800 * 2];
	let jump_range = TimeRange::new(Rational::new(1, 2), Rational::new(3, 5));
	assert_eq!(
		d.retrieve_audio(&mut jump, &jump_range, rate, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);
	assert!(d.audio_seek_count() > seeks, "a jump re-seeks");

	// An empty range succeeds without decoding (start == end).
	let mut empty_dest = [0f32; 8];
	let empty = TimeRange::new(Rational::new(1, 5), Rational::new(1, 5));
	assert_eq!(
		d.retrieve_audio(&mut empty_dest, &empty, rate, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);

	// A destination shorter than the range: clipped fill, still success.
	let mut short_dest = vec![0f32; 480];
	assert_eq!(
		d.retrieve_audio(&mut short_dest, &next_range, rate, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);

	// A zero-length destination still decodes (the overflow is kept as
	// the next chunk's carry).
	let mut zero_dest: [f32; 0] = [];
	assert_eq!(
		d.retrieve_audio(&mut zero_dest, &range, rate, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);

	// A mono destination exercises a one-channel layout conversion.
	let mut mono = vec![0f32; 4410];
	let mono_range = TimeRange::new(Rational::new(3, 10), Rational::new(2, 5));
	assert_eq!(
		d.retrieve_audio(&mut mono, &mono_range, 44100, 0x4)
			.unwrap(),
		RetrieveAudioStatus::Success
	);

	// A rate conversion leaves samples buffered inside the resampler;
	// the flush tail is appended after the decoded frames.
	let mut resampled = vec![0f32; 4410 * 2];
	let resample_range = TimeRange::new(Rational::new(1, 5), Rational::new(3, 10));
	assert_eq!(
		d.retrieve_audio(&mut resampled, &resample_range, 44100, layout)
			.unwrap(),
		RetrieveAudioStatus::Success
	);
	assert!(
		resampled.iter().any(|v| v.abs() > 1e-6),
		"the resampled range is not silent"
	);

	// Invalid output parameters are Unsupported, not errors.
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, 0, layout).unwrap(),
		RetrieveAudioStatus::Unsupported
	);
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, rate, 0).unwrap(),
		RetrieveAudioStatus::Unsupported
	);

	let _ = d.get_audio_start_offset();
	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn retrieve_frame_honours_cancel_atom_and_any_timecode() {
	let _guard = crate::lock_tests();
	let path = test_clip("frame_cancel");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 0)).expect("open video");

	let atom = CancelAtom::new();
	atom.cancel();
	{
		let mut state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().expect("open state");
		// A pre-cancelled retrieve decodes nothing and is not an error.
		let frame = state
			.retrieve_frame(&Rational::new(0, 1), false, Some(&atom))
			.expect("cancelled retrieve");
		assert!(frame.is_none(), "no frame after the cancel");

		// `any_timecode` skips the seek/cache block and returns the
		// next decoded frame regardless of its PTS.
		let frame = state
			.retrieve_frame(&Rational::new(0, 1), true, None)
			.expect("any-timecode retrieve");
		assert_eq!(frame.expect("a frame").pts(), Some(0));
	}

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn retrieve_audio_failure_clears_continuity_state() {
	let _guard = crate::lock_tests();
	let path = test_clip("audio_fail");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 1)).expect("open audio");

	// Decode one good chunk to seed the continuity state.
	let mut dest = vec![0f32; 4800 * 2];
	let range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 10));
	assert_eq!(
		d.retrieve_audio(&mut dest, &range, 48000, 0x3).unwrap(),
		RetrieveAudioStatus::Success
	);

	// Break the input definition: the next (contiguous) chunk's
	// resampler creation fails, and the failure must clear the
	// continuity/carry state so the following chunk re-seeks fresh.
	{
		let mut state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().expect("open state");
		state.input_sample_rate = 0;
		state.audio.as_mut().expect("audio session").resampler = None;
	}
	let next = TimeRange::new(Rational::new(1, 10), Rational::new(1, 5));
	let error = d.retrieve_audio(&mut dest, &next, 48000, 0x3);
	assert!(error.is_err(), "a broken resampler definition fails");
	{
		let state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		let audio = state.as_ref().unwrap().audio.as_ref().unwrap();
		assert!(
			audio.contiguous_end_sample.is_none(),
			"the failure clears continuity"
		);
		assert!(audio.carry.is_empty(), "the failure clears the carry");
	}

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn conform_audio_planar_packed_and_error_paths() {
	let _guard = crate::lock_tests();
	let path = test_clip("conform");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 1)).expect("open audio");

	// S16 planar (code 1): one output file per channel.
	let left = temp_file("conform_l", "pcm");
	let right = temp_file("conform_r", "pcm");
	d.conform_audio(
		&[
			left.to_string_lossy().into_owned(),
			right.to_string_lossy().into_owned(),
		],
		48000,
		0x3,
		1,
		None,
	)
	.expect("planar conform");
	assert!(std::fs::metadata(&left).unwrap().len() > 0);
	assert!(std::fs::metadata(&right).unwrap().len() > 0);

	// F32 packed (code 10): a single interleaved output file.
	let packed = temp_file("conform_p", "pcm");
	d.conform_audio(
		&[packed.to_string_lossy().into_owned()],
		48000,
		0x3,
		10,
		None,
	)
	.expect("packed conform");
	assert!(std::fs::metadata(&packed).unwrap().len() > 0);

	// A missing output directory fails cleanly.
	let missing = std::env::temp_dir()
		.join("quercuscodec_missing_dir_for_conform")
		.join("out.pcm");
	assert!(d
		.conform_audio(
			&[missing.to_string_lossy().into_owned()],
			48000,
			0x3,
			1,
			None
		)
		.is_err());

	// A session without an input layout cannot conform.
	{
		let mut state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		state
			.as_mut()
			.expect("open state")
			.input_channel_layout_mask = 0;
	}
	let error = d
		.conform_audio(&[left.to_string_lossy().into_owned()], 48000, 0x3, 1, None)
		.unwrap_err();
	assert!(
		error.to_string().contains("channel layout"),
		"the layout error is named: {error}"
	);

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&left);
	let _ = std::fs::remove_file(&right);
	let _ = std::fs::remove_file(&packed);
}

#[test]
fn conform_audio_cancel_reports_cancelled() {
	let _guard = crate::lock_tests();
	let path = test_clip("conform_cancel");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 1)).expect("open audio");

	let out = temp_file("conform_cancel_out", "pcm");
	let atom = CancelAtom::new();
	atom.cancel();
	let error = d
		.conform_audio(
			&[out.to_string_lossy().into_owned()],
			48000,
			0x3,
			1,
			Some(&atom),
		)
		.unwrap_err();
	assert!(
		matches!(error, crate::error::Error::Cancelled),
		"a cancelled conform reports Cancelled, got {error:?}"
	);

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&out);
}

#[test]
fn probe_reports_streams_subtitles_and_cancel() {
	let _guard = crate::lock_tests();
	let path = test_clip("probe");
	let d = FFmpegDecoder::new();

	let desc = d.probe(&path.to_string_lossy(), None).expect("probe clip");
	assert_eq!(desc.decoder(), "ffmpeg");
	assert_eq!(desc.total_stream_count, 2);
	assert_eq!(desc.video_stream_count(), 1);
	assert_eq!(desc.audio_stream_count(), 1);
	let video = desc.get_video_stream(0).expect("video stream");
	assert_eq!((video.width, video.height), (64, 64));
	let audio = desc.get_audio_stream(0).expect("audio stream");
	assert_eq!(audio.sample_rate, 48000);

	// A pre-cancelled probe stops before reading any stream.
	let atom = CancelAtom::new();
	atom.cancel();
	assert!(d.probe(&path.to_string_lossy(), Some(&atom)).is_none());

	// A subtitle-only file: the stream is counted, but no subtitle
	// entry is added (the probe's fall-through arm).
	let srt = temp_file("subs", "srt");
	std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").expect("write srt");
	let desc = d.probe(&srt.to_string_lossy(), None).expect("probe srt");
	assert_eq!(desc.total_stream_count, 1);
	assert_eq!(desc.subtitle_stream_count(), 0);
	assert_eq!(desc.video_stream_count(), 0);

	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&srt);
}

#[test]
fn decoder_open_rejects_subtitle_stream() {
	let _guard = crate::lock_tests();
	let srt = temp_file("open_subs", "srt");
	std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").expect("write srt");
	let d = FFmpegDecoder::new();
	let error = d
		.open(&CodecStream::with_block(
			srt.to_string_lossy().into_owned(),
			0,
			None,
		))
		.expect_err("subtitle streams are not video/audio");
	assert!(
		error.to_string().contains("expected video or audio"),
		"the error names the medium: {error}"
	);
	let _ = std::fs::remove_file(&srt);
}

/// 64x64 F32-RGBA frame helper for the encoder tests.
fn f32_frame(width: i32, height: i32, timestamp: Rational) -> Frame {
	let mut vp = VideoParams::new_basic(
		width,
		height,
		QuercusPixelFormat::from_code(0),
		4,
		1,
		1,
		0,
		1,
	);
	vp.format = QuercusPixelFormat::from_code(PixelFormat::F32 as i32);
	let mut frame = Frame::with_params(vp);
	frame.timestamp = timestamp;
	frame.allocate().expect("frame allocation");
	frame
}

fn encoder_params(path: &std::path::Path, audio: bool) -> EncodingParams {
	let mut p = EncodingParams::default();
	let name = path.as_os_str().as_encoded_bytes();
	assert!(name.len() < p.filename.len(), "temp path too long");
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2; // MPEG-4 video
	p.video_enabled = 1;
	p.video_codec = 10; // MPEG-2 (B-frame-free)
	p.video_width = 64;
	p.video_height = 64;
	p.video_time_base_num = 1;
	p.video_time_base_den = 10;
	p.video_bit_rate = 2_000_000;
	p.video_max_bit_rate = 4_000_000;
	p.video_pixel_format = PixelFormat::F32;
	p.video_pix_fmt[..7].copy_from_slice(b"yuv420p");
	p.video_pixel_aspect_num = 1;
	p.video_pixel_aspect_den = 1;
	p.color_primaries = 1; // BT.709
	p.color_trc = 1;
	p.color_space = 9; // BT.2020 (exercises the matrix arm)
	p.color_range = 2; // full
	if audio {
		p.audio_enabled = 1;
		// PCM (not AAC): the AAC encoder rejects the resampler's
		// non-1024-sample middle frames in this FFmpeg pairing (the
		// same reason `testmedia` uses PCM).
		p.audio_codec = 13; // PCM S16LE
		p.audio_sample_rate = 48000;
		p.audio_channel_layout = 0x3;
		p.audio_sample_format = SampleFormat::F32;
		p.audio_bit_rate = 0;
	}
	p
}

#[test]
fn encoder_roundtrip_covers_guard_and_error_paths() {
	let _guard = crate::lock_tests();
	let path = temp_file("enc_roundtrip", "mp4");
	let e = FFmpegEncoder::with_params(encoder_params(&path, true));
	e.open().expect("open video+audio");
	e.open().expect("second open is a no-op");

	let first = f32_frame(64, 64, Rational::new(0, 1));
	e.write_video(&first).expect("first frame");
	let second = f32_frame(64, 64, Rational::new(1, 10));
	e.write_video(&second).expect("second frame");

	// A frame of the wrong size is rejected.
	let wrong = f32_frame(32, 32, Rational::new(2, 10));
	let error = e.write_video(&wrong).unwrap_err();
	assert!(
		error.to_string().contains("does not match"),
		"the size error is named: {error}"
	);

	// Audio: chunked writes plus an empty (no-op) write.
	let tone = vec![0.1f32; 960 * 2];
	e.write_audio(&tone, 960).expect("audio chunk");
	e.write_audio(&tone, 960).expect("second audio chunk");
	e.write_audio(&[], 0).expect("empty audio is a no-op");

	// The already-flushed guard branches (defensive: `close` clears the
	// output, so mark the state directly).
	{
		let mut state = e.state.lock().unwrap_or_else(|x| x.into_inner());
		state.output.as_mut().expect("open output").flushed = true;
	}
	assert!(e.write_video(&first).is_err());
	assert!(e.write_audio(&tone, 4800).is_err());
	{
		let mut state = e.state.lock().unwrap_or_else(|x| x.into_inner());
		state.output.as_mut().expect("open output").flushed = false;
	}

	e.close().expect("close flushes");
	assert!(e.close().is_ok(), "close is idempotent");
	e.flush().expect("flush after close is safe");
	assert!(std::fs::metadata(&path).expect("output exists").len() > 0);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn encoder_h264_roundtrip_uses_priority_candidates() {
	let _guard = crate::lock_tests();
	let path = temp_file("enc_h264", "mp4");
	let mut params = encoder_params(&path, false);
	params.video_codec = 1;
	params.video_width = 256;
	params.video_height = 256;
	let candidates = video_encoder_candidates(1, ffmpeg::codec::Id::H264);
	let e = FFmpegEncoder::with_params(params);
	e.open().expect("open H.264");

	let (selected, thread_count) = {
		let state = e.state.lock().unwrap_or_else(|x| x.into_inner());
		let output = state.output.as_ref().expect("open output");
		output
			.video
			.as_ref()
			.map(|video| {
				(
					video.encoder.codec().map(|codec| codec.name().to_string()),
					unsafe { (*video.encoder.as_ptr()).thread_count },
				)
			})
			.unwrap_or((None, 0))
	};
	assert!(
		candidates
			.iter()
			.any(|codec| Some(codec.name()) == selected.as_deref()),
		"selected encoder {selected:?} is not in the priority list"
	);
	let hardware = hardware_video_encoder_names(1)
		.iter()
		.any(|name| Some(*name) == selected.as_deref());
	let expected_threads = if hardware {
		1
	} else {
		software_codec_threads().parse::<i32>().unwrap()
	};
	assert_eq!(
		thread_count,
		expected_threads,
		"{} encoder thread policy",
		if hardware { "hardware" } else { "software" }
	);

	for frame in 0..3 {
		e.write_video(&f32_frame(256, 256, Rational::new(frame, 10)))
			.expect("H.264 frame");
	}
	e.close().expect("close H.264");
	assert!(std::fs::metadata(&path).expect("output exists").len() > 0);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn encoder_open_rejects_unknown_codecs() {
	let path = temp_file("enc_bad_codec", "mp4");

	let mut p = EncodingParams::default();
	let name = path.as_os_str().as_encoded_bytes();
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2;
	p.video_enabled = 1;
	p.video_codec = 99;
	p.video_width = 64;
	p.video_height = 64;
	p.video_time_base_num = 1;
	p.video_time_base_den = 10;
	let error = FFmpegEncoder::with_params(p).open().unwrap_err();
	assert!(
		error.to_string().contains("unknown video codec"),
		"unknown video codec is named: {error}"
	);

	let mut p = EncodingParams::default();
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2;
	p.audio_enabled = 1;
	p.audio_codec = 99;
	p.audio_sample_rate = 48000;
	p.audio_channel_layout = 0x3;
	let error = FFmpegEncoder::with_params(p).open().unwrap_err();
	assert!(
		error.to_string().contains("unknown audio codec"),
		"unknown audio codec is named: {error}"
	);

	let _ = std::fs::remove_file(&path);
}

// ---- extended coverage: session internals -----------------------------

/// `DecoderState::reopen_software` swaps the open session for a fresh
/// pure-software one (the hardware fallback's core); the swapped
/// session must still decode.
#[test]
fn reopen_software_swaps_in_a_software_session() {
	let _guard = crate::lock_tests();
	let path = test_clip("reopen_sw");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 0)).expect("open video");
	let hardware_before = d.hardware_decoding();
	{
		let mut state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().expect("open state");
		state.reopen_software().expect("software reopen");
		assert!(
			state.hw_device.is_none(),
			"the reopened session is software"
		);
		let DecoderInner::Video(decoder) = &state.inner else {
			panic!("video stream reopened as audio");
		};
		assert_eq!(
			unsafe { (*decoder.as_ptr()).thread_count }.to_string(),
			software_codec_threads(),
			"software fallback enables the CPU thread budget"
		);
	}
	if hardware_before {
		assert!(
			!d.hardware_decoding(),
			"the software swap dropped the hardware device"
		);
	}
	let frame = d
		.retrieve_video_frame(&video_params())
		.expect("decode after the reopen");
	assert!(frame.width() > 0 && frame.height() > 0);
	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

/// Draining a video session through `any_timecode` ends in the codec-EOF
/// branch: the last cached frame is returned (with the once-per-session
/// mismatch warning), not an error.
#[test]
fn any_timecode_drain_reaches_the_eof_fallback() {
	let _guard = crate::lock_tests();
	let path = test_clip("eof_fallback");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 0)).expect("open video");
	{
		let mut state = d.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().expect("open state");
		let mut last_pts = None;
		for _ in 0..10 {
			let frame = state
				.retrieve_frame(&Rational::new(0, 1), true, None)
				.expect("any-timecode retrieve")
				.expect("a decoded frame");
			last_pts = frame.pts();
		}
		assert!(last_pts.is_some(), "the last frame carries its PTS");

		// The decoder is drained: both extra retrieves fall back to the
		// last cached frame instead of failing.
		for _ in 0..2 {
			let frame = state
				.retrieve_frame(&Rational::new(0, 1), true, None)
				.expect("EOF fallback retrieve")
				.expect("the last cached frame");
			assert_eq!(frame.pts(), last_pts, "EOF fallback returns the tail");
		}
	}
	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
}

/// `c_string_1024` reads a NUL-terminated name and falls back to the
/// full buffer when no terminator exists.
#[test]
fn encoder_filename_reads_full_unterminated_buffer() {
	let params = EncodingParams {
		filename: [b'f'; 1024],
		..Default::default()
	};
	let encoder = FFmpegEncoder::with_params(params);
	assert_eq!(encoder.filename().len(), 1024);
	assert!(encoder.filename().bytes().all(|b| b == b'f'));

	// The default params buffer is all-NUL and reads as empty.
	let encoder = FFmpegEncoder::with_params(EncodingParams::default());
	assert_eq!(encoder.filename(), "");
}

/// The resampler's plane/frame conversion and its draining flush
/// (the pieces the chunked decode/encode paths call into).
#[test]
fn audio_resampler_planes_frames_and_flush_tail() {
	let mut resampler = AudioResampler::get(
		Sample::F32(SampleType::Packed),
		ChannelLayout::default(2),
		48000,
		Sample::F32(SampleType::Packed),
		ChannelLayout::default(2),
		44100,
	)
	.expect("resampler");
	let mut input = ffmpeg::frame::Audio::new(
		Sample::F32(SampleType::Packed),
		1000,
		ChannelLayout::default(2),
	);
	for (i, chunk) in input
		.data_mut(0)
		.as_chunks_mut::<4>()
		.0
		.iter_mut()
		.enumerate()
	{
		let v = ((i / 2) as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
		chunk.copy_from_slice(&v.to_le_bytes());
	}

	let planes = resampler.convert_to_planes(&input).expect("convert planes");
	assert!(!planes.is_empty());
	assert!(planes.iter().all(|p| !p.is_empty()));

	let frame = resampler.convert_to_frame(&input).expect("convert frame");
	assert!(frame.samples() > 0, "the frame carries resampled samples");

	// A rate conversion leaves a tail buffered inside swr; draining it
	// exercises the flush loop's read/write body.
	let tail = resampler.flush_planes().expect("flush");
	let written: usize = tail.iter().map(|p| p.first().map_or(0, Vec::len)).sum();
	assert!(written > 0, "the flush tail carries samples");
	// A second flush finds nothing buffered.
	assert!(resampler.flush_planes().expect("flush again").is_empty());
}

/// `conform_audio` at a different rate exercises the resampler-tail
/// flush write arm; a zero output rate fails resampler creation.
#[test]
fn conform_audio_rate_conversion_flushes_the_tail() {
	let _guard = crate::lock_tests();
	let path = test_clip("conform_rate");
	let d = FFmpegDecoder::new();
	d.open(&clip_stream(&path, 1)).expect("open audio");
	let out = temp_file("conform_rate_out", "pcm");
	d.conform_audio(&[out.to_string_lossy().into_owned()], 44100, 0x3, 1, None)
		.expect("conform at 44100");
	assert!(std::fs::metadata(&out).unwrap().len() > 0);

	// A zero output rate cannot create the resampler.
	assert!(d
		.conform_audio(&[out.to_string_lossy().into_owned()], 0, 0x3, 1, None)
		.is_err());

	d.close().expect("close");
	let _ = std::fs::remove_file(&path);
	let _ = std::fs::remove_file(&out);
}

#[test]
fn video_rate_control_plan_covers_software_hardware_and_fallback() {
	// Bitrate mode always applies bitrate and sets no quality options.
	for enc in [
		"libx264",
		"libx265",
		"libsvtav1",
		"h264_nvenc",
		"h264_qsv",
		"h264_amf",
		"h264_videotoolbox",
		"prores_ks",
	] {
		let plan = video_rate_control_plan(enc, VideoRateControl::Bitrate, 23);
		assert!(plan.apply_bitrate, "{enc} bitrate mode");
		assert!(plan.opts.is_empty(), "{enc} bitrate opts");
		assert_eq!(plan.qscale_quality, None, "{enc} bitrate qscale");
	}

	// Software x264 / x264rgb / x265 (0..=51 clamp).
	assert_eq!(
		video_rate_control_plan("libx264", VideoRateControl::Crf, 18),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("crf", "18".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("libx264rgb", VideoRateControl::Cqp, -5),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("qp", "0".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("libx265", VideoRateControl::Crf, 99),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("crf", "51".into())],
			qscale_quality: None,
		}
	);

	// Software SVT-AV1 / libaom-av1 (0..=63 clamp).
	assert_eq!(
		video_rate_control_plan("libsvtav1", VideoRateControl::Crf, 30),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("crf", "30".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("libsvtav1", VideoRateControl::Cqp, 80),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("qp", "63".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("libaom-av1", VideoRateControl::Crf, 32),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("crf", "32".into())],
			qscale_quality: None,
		}
	);

	// NVENC: rc=vbr + cq (1..=max_q) for CRF, rc=constqp + qp for CQP
	// (av1_nvenc qp maps 0..=63 onto AV1's 0..=255 qindex scale).
	assert_eq!(
		video_rate_control_plan("h264_nvenc", VideoRateControl::Crf, 0),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("rc", "vbr".into()), ("cq", "1".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("h264_nvenc", VideoRateControl::Crf, 23),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("rc", "vbr".into()), ("cq", "23".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("av1_nvenc", VideoRateControl::Cqp, 63),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("rc", "constqp".into()), ("qp", "255".into())],
			qscale_quality: None,
		}
	);

	// QSV: global_quality (1..=51) for CRF, qscale for CQP.
	assert_eq!(
		video_rate_control_plan("hevc_qsv", VideoRateControl::Crf, 0),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![("global_quality", "1".into())],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("h264_qsv", VideoRateControl::Cqp, 22),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: Vec::new(),
			qscale_quality: Some(22),
		}
	);

	// AMF: h264_amf and av1_amf include qp_b, hevc_amf omits qp_b;
	// av1_amf maps 0..=63 onto AV1's 0..=255 qindex scale.
	assert_eq!(
		video_rate_control_plan("h264_amf", VideoRateControl::Crf, 24),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![
				("rc", "cqp".into()),
				("qp_i", "24".into()),
				("qp_p", "24".into()),
				("qp_b", "24".into()),
			],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("hevc_amf", VideoRateControl::Cqp, 26),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![
				("rc", "cqp".into()),
				("qp_i", "26".into()),
				("qp_p", "26".into()),
			],
			qscale_quality: None,
		}
	);
	assert_eq!(
		video_rate_control_plan("av1_amf", VideoRateControl::Crf, 63),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: vec![
				("rc", "cqp".into()),
				("qp_i", "255".into()),
				("qp_p", "255".into()),
				("qp_b", "255".into()),
			],
			qscale_quality: None,
		}
	);

	// VideoToolbox: inverted 1..=100 qscale (0 -> 100, 51 -> 1).
	assert_eq!(
		video_rate_control_plan("h264_videotoolbox", VideoRateControl::Crf, 0),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: Vec::new(),
			qscale_quality: Some(100),
		}
	);
	assert_eq!(
		video_rate_control_plan("hevc_videotoolbox", VideoRateControl::Crf, 51),
		VideoRateControlPlan {
			apply_bitrate: false,
			opts: Vec::new(),
			qscale_quality: Some(1),
		}
	);

	// Unsupported codecs safely fall back to bitrate mode.
	for enc in ["prores", "prores_ks", "dnxhd", "cfhd", "mpeg2video"] {
		let plan = video_rate_control_plan(enc, VideoRateControl::Crf, 18);
		assert!(plan.apply_bitrate, "{enc} must fall back to bitrate");
		assert!(plan.opts.is_empty(), "{enc} must not emit crf/qp opts");
		assert_eq!(plan.qscale_quality, None);
	}
}

/// Encodes a short synthetic H.264 clip with the given rate control and
/// quality, returning the output file size in bytes.
fn encode_video_with_rate_control(
	tag: &str,
	format: i32,
	ext: &str,
	video_codec: i32,
	rate_control: VideoRateControl,
	quality: i32,
	frame_count: usize,
) -> u64 {
	let path = temp_file(tag, ext);
	let mut filename = [0u8; 1024];
	let path_str = path.to_string_lossy();
	filename[..path_str.len()].copy_from_slice(path_str.as_bytes());
	let p = EncodingParams {
		filename,
		format,
		video_enabled: 1,
		video_codec,
		video_width: 256,
		video_height: 256,
		video_time_base_num: 1,
		video_time_base_den: 24,
		video_pixel_aspect_num: 1,
		video_pixel_aspect_den: 1,
		video_bit_rate: 500_000, // Must be ignored in CRF/CQP mode.
		video_rate_control: rate_control.into(),
		video_quality: quality,
		..Default::default()
	};

	let enc = FFmpegEncoder::with_params(p);
	enc.open().expect("open video encoder");

	let vp = quercus_core::videoparams::VideoParams {
		width: 256,
		height: 256,
		time_base: (1, 24),
		format: quercus_core::ocioutils::PixelFormat::U8,
		channel_count: 4,
		..Default::default()
	};
	let mut frame = Frame::with_params(vp);
	frame.allocate().expect("allocate frame");
	let stride = frame.linesize_bytes() as usize;
	for f in 0..frame_count {
		frame.timestamp = Rational::new(f as i64, 24);
		{
			let buf = frame.data_mut().expect("allocated frame buffer");
			for y in 0..256usize {
				let row = &mut buf[y * stride..y * stride + 256 * 4];
				for x in 0..256usize {
					// High-frequency moving pattern so quality changes
					// materially affect encoded bitrate/file size.
					let v = ((x.wrapping_mul(17) ^ y.wrapping_mul(31)).wrapping_add(f * 29)) as u8;
					row[x * 4] = v;
					row[x * 4 + 1] = v.wrapping_mul(3);
					row[x * 4 + 2] = v.wrapping_add((x as u8) ^ (y as u8));
					row[x * 4 + 3] = 255;
				}
			}
		}
		enc.write_video(&frame).expect("write video frame");
	}
	enc.flush().expect("flush");
	enc.close().expect("close");

	let len = std::fs::metadata(&path).expect("output exists").len();
	assert!(len > 0, "encoded file must be non-empty");
	let _ = std::fs::remove_file(&path);
	len
}

#[test]
fn h264_crf_and_cqp_quality_controls_file_size() {
	let _guard = crate::lock_tests();

	// Hardware-preferred H.264 path (video_codec = 1).
	let crf_high =
		encode_video_with_rate_control("crf_18", 2, "mp4", 1, VideoRateControl::Crf, 18, 15);
	let crf_low =
		encode_video_with_rate_control("crf_42", 2, "mp4", 1, VideoRateControl::Crf, 42, 15);
	assert!(
		crf_high > crf_low,
		"H.264 CRF 18 ({crf_high} B) must be larger than CRF 42 ({crf_low} B)"
	);

	let cqp_high =
		encode_video_with_rate_control("cqp_20", 2, "mp4", 1, VideoRateControl::Cqp, 20, 15);
	let cqp_low =
		encode_video_with_rate_control("cqp_45", 2, "mp4", 1, VideoRateControl::Cqp, 45, 15);
	assert!(
		cqp_high > cqp_low,
		"H.264 CQP 20 ({cqp_high} B) must be larger than CQP 45 ({cqp_low} B)"
	);

	// Software-only H.264 RGB path (video_codec = 2 -> libx264, no HW candidates).
	let sw_crf_high =
		encode_video_with_rate_control("sw_crf_18", 2, "mp4", 2, VideoRateControl::Crf, 18, 15);
	let sw_crf_low =
		encode_video_with_rate_control("sw_crf_42", 2, "mp4", 2, VideoRateControl::Crf, 42, 15);
	assert!(
		sw_crf_high > sw_crf_low,
		"Software libx264 CRF 18 ({sw_crf_high} B) must be larger than CRF 42 ({sw_crf_low} B)"
	);

	let sw_cqp_high =
		encode_video_with_rate_control("sw_cqp_20", 2, "mp4", 2, VideoRateControl::Cqp, 20, 15);
	let sw_cqp_low =
		encode_video_with_rate_control("sw_cqp_45", 2, "mp4", 2, VideoRateControl::Cqp, 45, 15);
	assert!(
		sw_cqp_high > sw_cqp_low,
		"Software libx264 CQP 20 ({sw_cqp_high} B) must be larger than CQP 45 ({sw_cqp_low} B)"
	);

	// HEVC (video_codec = 3 -> hardware or libx265 software fallback).
	let hevc_crf_high =
		encode_video_with_rate_control("hevc_crf_18", 2, "mp4", 3, VideoRateControl::Crf, 18, 15);
	let hevc_crf_low =
		encode_video_with_rate_control("hevc_crf_42", 2, "mp4", 3, VideoRateControl::Crf, 42, 15);
	assert!(
		hevc_crf_high > hevc_crf_low,
		"HEVC CRF 18 ({hevc_crf_high} B) must be larger than CRF 42 ({hevc_crf_low} B)"
	);
	let hevc_cqp_high =
		encode_video_with_rate_control("hevc_cqp_20", 2, "mp4", 3, VideoRateControl::Cqp, 20, 15);
	let hevc_cqp_low =
		encode_video_with_rate_control("hevc_cqp_45", 2, "mp4", 3, VideoRateControl::Cqp, 45, 15);
	assert!(
		hevc_cqp_high > hevc_cqp_low,
		"HEVC CQP 20 ({hevc_cqp_high} B) must be larger than CQP 45 ({hevc_cqp_low} B)"
	);

	// AV1 (video_codec = 18 -> hardware or libaom-av1/libsvtav1 software fallback).
	// Some FFmpeg builds (e.g. macOS CI's minimal build) omit AV1 encoders.
	let _ = ffmpeg_init();
	if !video_encoder_candidates(18, ffmpeg::codec::Id::AV1).is_empty() {
		let av1_crf_high = encode_video_with_rate_control(
			"av1_crf_20",
			2,
			"mp4",
			18,
			VideoRateControl::Crf,
			20,
			4,
		);
		let av1_crf_low = encode_video_with_rate_control(
			"av1_crf_55",
			2,
			"mp4",
			18,
			VideoRateControl::Crf,
			55,
			4,
		);
		assert!(
			av1_crf_high > av1_crf_low,
			"AV1 CRF 20 ({av1_crf_high} B) must be larger than CRF 55 ({av1_crf_low} B)"
		);
		let av1_cqp_high = encode_video_with_rate_control(
			"av1_cqp_20",
			2,
			"mp4",
			18,
			VideoRateControl::Cqp,
			20,
			4,
		);
		let av1_cqp_low = encode_video_with_rate_control(
			"av1_cqp_55",
			2,
			"mp4",
			18,
			VideoRateControl::Cqp,
			55,
			4,
		);
		assert!(
			av1_cqp_high > av1_cqp_low,
			"AV1 CQP 20 ({av1_cqp_high} B) must be larger than CQP 55 ({av1_cqp_low} B)"
		);
	}

	// Unsupported codec (ProRes, video_codec = 6 in QuickTime format = 4)
	// must safely ignore CRF and still encode without error.
	let prores_len = encode_video_with_rate_control(
		"prores_fallback",
		4,
		"mov",
		6,
		VideoRateControl::Crf,
		18,
		15,
	);
	assert!(prores_len > 0);
}
