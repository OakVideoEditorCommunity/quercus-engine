use super::*;

const EPS: f64 = 1e-9;

fn assert_close(a: f64, b: f64) {
	assert!((a - b).abs() < EPS, "expected {a} close to {b} (eps {EPS})");
}

#[test]
fn default_has_expected_values() {
	let p = EncodingParams::default();
	assert_eq!(p.filename, [0; 1024]);
	assert_eq!(p.format, -1);
	assert_eq!(p.video_enabled, 0);
	assert_eq!(p.video_codec, -1);
	assert_eq!(p.video_width, 0);
	assert_eq!(p.video_height, 0);
	assert_eq!(p.video_time_base_num, 1);
	assert_eq!(p.video_time_base_den, 1);
	assert_eq!(p.video_pixel_format, PixelFormat::Invalid);
	assert_eq!(p.video_interlacing, 0);
	assert_eq!(p.video_pixel_aspect_num, 0);
	assert_eq!(p.video_pixel_aspect_den, 0);
	assert_eq!(p.video_bit_rate, 0);
	assert_eq!(p.video_min_bit_rate, 0);
	assert_eq!(p.video_max_bit_rate, 0);
	assert_eq!(p.video_buffer_size, 0);
	assert_eq!(p.video_threads, 0);
	assert_eq!(p.video_pix_fmt, [0; 64]);
	assert_eq!(p.video_is_image_sequence, 0);
	assert_eq!(p.video_scaling_method, VideoScalingMethod::Stretch);
	assert_eq!(p.audio_enabled, 0);
	assert_eq!(p.audio_codec, -1);
	assert_eq!(p.audio_sample_rate, 0);
	assert_eq!(p.audio_channel_layout, 0);
	assert_eq!(p.audio_sample_format, SampleFormat::Invalid);
	assert_eq!(p.audio_bit_rate, 0);
	assert_eq!(p.subtitles_enabled, 0);
	assert_eq!(p.subtitles_codec, -1);
	assert_eq!(p.subtitles_are_sidecar, 0);
	assert_eq!(p.subtitles_sidecar_format, -1);
	assert_eq!(p.color_transform_output, [0; 256]);
	assert_eq!(p.export_length_num, 0);
	assert_eq!(p.export_length_den, 0);
	assert_eq!(p.has_custom_range, 0);
	assert_eq!(p.custom_range_in_num, 0);
	assert_eq!(p.custom_range_in_den, 0);
	assert_eq!(p.custom_range_out_num, 0);
	assert_eq!(p.custom_range_out_den, 0);
}

#[test]
fn extension_mapping() {
	let mut p = EncodingParams::default();
	let cases: [(i32, &str); 15] = [
		(0, "mxf"),
		(1, "mkv"),
		(2, "mp4"),
		(3, "exr"),
		(4, "mov"),
		(5, "png"),
		(6, "tiff"),
		(7, "wav"),
		(8, "aiff"),
		(9, "mp3"),
		(10, "flac"),
		(11, "ogg"),
		(12, "webm"),
		(13, "srt"),
		(14, "m4a"),
	];
	for (fmt, ext) in cases {
		p.format = fmt;
		assert_eq!(p.extension(), ext, "format {fmt}");
	}
	// Unknown / invalid formats map to the empty string.
	p.format = 99;
	assert_eq!(p.extension(), "");
	p.format = -3;
	assert_eq!(p.extension(), "");
}

#[test]
fn generate_matrix_stretch_is_identity() {
	let mut out = [0.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Stretch, 1920, 1080, 1280, 720, &mut out);
	assert_eq!(out, identity());
}

#[test]
fn generate_matrix_equal_aspect_is_identity() {
	// Same 16:9 aspect: export_ar == source_ar -> fuzzy-equal -> identity.
	let mut out = [0.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Fit, 1920, 1080, 1280, 720, &mut out);
	assert_eq!(out, identity());
}

#[test]
fn generate_matrix_fit_scales_x() {
	// src square (ar 1.0) -> dst 2:1 (ar 2.0). Fit: source wider/narrower
	// relative to export -> the x axis is squeezed to source_ar/export_ar.
	let mut out = [0.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Fit, 1000, 1000, 2000, 1000, &mut out);
	// export_ar(2.0) > source_ar(1.0); Fit => scale(source_ar/export_ar, 1).
	assert_close(out[0], 0.5);
	assert_close(out[5], 1.0);
}

#[test]
fn generate_matrix_crop_scales_y() {
	// src square -> dst 2:1. Crop: fit inside, so we zoom the y axis by
	// export_ar/source_ar and keep x unscaled.
	let mut out = [0.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Crop, 1000, 1000, 2000, 1000, &mut out);
	// export_ar(2.0) > source_ar(1.0); not Fit => scale(1, export_ar/source_ar).
	assert_close(out[0], 1.0);
	assert_close(out[5], 2.0);
}

#[test]
fn generate_matrix_crop_scales_x() {
	// src square -> dst 0.5:1 (ar 0.5). Crop: export narrower than source,
	// zoom the x axis by source_ar/export_ar.
	let mut out = [0.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Crop, 1000, 1000, 500, 1000, &mut out);
	// export_ar(0.5) < source_ar(1.0); (ar>source) == false, (method==Fit)
	// false => false == false true => scale(source_ar/export_ar, 1) = 2.0.
	assert_close(out[0], 2.0);
	assert_close(out[5], 1.0);
}

#[test]
fn generate_matrix_degenerate_is_identity() {
	// Zero / negative source sizes must not produce inf/NaN.
	let mut out = [9.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Fit, 0, 1080, 1280, 720, &mut out);
	assert_eq!(out, identity());

	let mut out = [9.0; 16];
	EncodingParams::generate_matrix(VideoScalingMethod::Crop, -1, 1080, 1280, 720, &mut out);
	assert_eq!(out, identity());
}

#[test]
fn save_load_roundtrip() {
	let mut p = EncodingParams::default();
	p.filename = [0; 1024];
	let name = b"out/video.mp4";
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2;
	p.video_enabled = 1;
	p.video_codec = 3;
	p.video_width = 1280;
	p.video_height = 720;
	p.video_time_base_num = 1;
	p.video_time_base_den = 30;
	p.video_pixel_format = PixelFormat::U8;
	p.video_interlacing = 1;
	p.video_pixel_aspect_num = 1;
	p.video_pixel_aspect_den = 1;
	p.video_bit_rate = 8_000_000;
	p.video_min_bit_rate = 4_000_000;
	p.video_max_bit_rate = 16_000_000;
	p.video_buffer_size = 4_000_000;
	p.video_threads = 4;
	let pix = b"yuv420p";
	p.video_pix_fmt[..pix.len()].copy_from_slice(pix);
	p.video_is_image_sequence = 1;
	p.video_scaling_method = VideoScalingMethod::Crop;

	p.audio_enabled = 1;
	p.audio_codec = 1;
	p.audio_sample_rate = 48000;
	p.audio_channel_layout = 3; // stereo mask
	p.audio_sample_format = SampleFormat::F32Planar;
	p.audio_bit_rate = 320_000;

	p.subtitles_enabled = 1;
	p.subtitles_codec = 1;
	p.subtitles_are_sidecar = 1;
	p.subtitles_sidecar_format = 13; // srt

	let xml = p.save_to_string();
	assert!(xml.starts_with("<export version=\"1\">"), "xml: {xml}");

	let mut q = EncodingParams::default();
	q.load(&xml).expect("load round-trip");

	assert_eq!(q.filename[..name.len()], name[..]);
	assert_eq!(q.format, 2);
	assert_eq!(q.video_enabled, 1);
	assert_eq!(q.video_codec, 3);
	assert_eq!(q.video_width, 1280);
	assert_eq!(q.video_height, 720);
	assert_eq!(q.video_time_base_num, 1);
	assert_eq!(q.video_time_base_den, 30);
	assert_eq!(q.video_pixel_format, PixelFormat::U8);
	assert_eq!(q.video_interlacing, 1);
	assert_eq!(q.video_pixel_aspect_num, 1);
	assert_eq!(q.video_pixel_aspect_den, 1);
	assert_eq!(q.video_bit_rate, 8_000_000);
	assert_eq!(q.video_min_bit_rate, 4_000_000);
	assert_eq!(q.video_max_bit_rate, 16_000_000);
	assert_eq!(q.video_buffer_size, 4_000_000);
	assert_eq!(q.video_threads, 4);
	assert_eq!(q.video_pix_fmt[..pix.len()], pix[..]);
	assert_eq!(q.video_is_image_sequence, 1);
	assert_eq!(q.video_scaling_method, VideoScalingMethod::Crop);

	assert_eq!(q.audio_enabled, 1);
	assert_eq!(q.audio_codec, 1);
	assert_eq!(q.audio_sample_rate, 48000);
	assert_eq!(q.audio_channel_layout, 3);
	assert_eq!(q.audio_sample_format, SampleFormat::F32Planar);
	assert_eq!(q.audio_bit_rate, 320_000);

	assert_eq!(q.subtitles_enabled, 1);
	assert_eq!(q.subtitles_codec, 1);
	assert_eq!(q.subtitles_are_sidecar, 1);
	assert_eq!(q.subtitles_sidecar_format, 13);
}

#[test]
fn load_malformed_returns_err() {
	let mut p = EncodingParams::default();
	assert!(p.load("not xml at all").is_err());
	assert!(p.load("<unclosed").is_err());
	assert!(p.load("<export><video enabled=\"1\"></export>").is_err());
}

#[test]
fn load_non_export_returns_err() {
	let mut p = EncodingParams::default();
	assert!(p.load("<foo/>").is_err());
}

#[test]
fn custom_range_not_assigned_on_load() {
	// The C++ load_v1 bug: custom range is parsed but never assigned.
	let mut p = EncodingParams::default();
	p.has_custom_range = 1;
	p.custom_range_in_num = 5;
	p.custom_range_in_den = 1;
	p.custom_range_out_num = 10;
	p.custom_range_out_den = 1;
	let xml = p.save_to_string();

	let mut q = EncodingParams::default();
	q.load(&xml).expect("load");
	// Fields stay at their defaults despite being serialized.
	assert_eq!(q.has_custom_range, 0);
	assert_eq!(q.custom_range_in_num, 0);
	assert_eq!(q.custom_range_in_den, 0);
	assert_eq!(q.custom_range_out_num, 0);
	assert_eq!(q.custom_range_out_den, 0);
}

#[test]
fn cstr_set_cstr_roundtrip_and_truncate() {
	let mut arr = [0u8; 8];
	set_cstr(&mut arr, "hello");
	assert_eq!(cstr(&arr), "hello");
	assert_eq!(arr[5], 0);

	// Longer than the buffer -> truncated to the first 8 bytes (no
	// terminator required when the buffer is exactly full).
	set_cstr(&mut arr, "this string is far too long");
	assert_eq!(cstr(&arr), "this str");
	assert_eq!(arr.len(), 8);
	assert_eq!(&arr[..], b"this str");

	// Empty -> all NUL.
	set_cstr(&mut arr, "");
	assert_eq!(arr, [0u8; 8]);
	assert_eq!(cstr(&arr), "");
}

#[test]
fn xml_escaping_roundtrips() {
	let s = "a&b<c>d";
	assert_eq!(unescape_xml(&escape_xml(s)), s);
	assert_eq!(escape_xml("plain"), "plain");
}

fn identity() -> [f64; 16] {
	[
		1.0, 0.0, 0.0, 0.0, //
		0.0, 1.0, 0.0, 0.0, //
		0.0, 0.0, 1.0, 0.0, //
		0.0, 0.0, 0.0, 1.0,
	]
}

/// `quercuscodec_encoding_params` byte-level layout lock. The original C
/// ABI (`include/codec/encoder.h`) has been retired, but the offsets of
/// the pre-existing fields stay frozen so any on-disk/IPC copy of the
/// POD still reads in place; the delivery color-metadata fields are
/// appended at the end.
#[test]
fn encoding_params_c_abi_layout() {
	use std::mem::{offset_of, size_of};

	assert_eq!(size_of::<EncodingParams>(), 1552);
	assert_eq!(offset_of!(EncodingParams, filename), 0);
	assert_eq!(offset_of!(EncodingParams, format), 1024);
	assert_eq!(offset_of!(EncodingParams, video_enabled), 1028);
	assert_eq!(offset_of!(EncodingParams, video_pixel_aspect_den), 1064);
	assert_eq!(offset_of!(EncodingParams, video_bit_rate), 1072);
	assert_eq!(offset_of!(EncodingParams, video_pix_fmt), 1108);
	assert_eq!(offset_of!(EncodingParams, video_is_image_sequence), 1172);
	assert_eq!(offset_of!(EncodingParams, audio_channel_layout), 1192);
	assert_eq!(offset_of!(EncodingParams, audio_bit_rate), 1208);
	assert_eq!(offset_of!(EncodingParams, color_transform_output), 1232);
	assert_eq!(offset_of!(EncodingParams, has_custom_range), 1496);
	assert_eq!(offset_of!(EncodingParams, custom_range_in_num), 1504);
	assert_eq!(offset_of!(EncodingParams, custom_range_out_den), 1528);
	// Appended delivery color metadata (H.273 code points).
	assert_eq!(offset_of!(EncodingParams, color_primaries), 1536);
	assert_eq!(offset_of!(EncodingParams, color_trc), 1540);
	assert_eq!(offset_of!(EncodingParams, color_space), 1544);
	assert_eq!(offset_of!(EncodingParams, color_range), 1548);
}
