use super::*;

#[test]
fn compatible_bridge_pixel_format_no_limit_prefers_rgba_then_rgb_a64() {
	// `maximum == invalid (-1)`: candidate list is [rgba, rgb_a64_le, none].
	let list = compatible_bridge_pixel_format_list(PIX_FMT_INVALID);
	assert_eq!(list[0], FB_PIX_FMT_RGBA);
	assert_eq!(list[1], FB_PIX_FMT_RGB_A64_LE);
	assert_eq!(list[2], FB_PIX_FMT_NONE);
}

#[test]
fn compatible_bridge_pixel_format_u8_clamp_allows_only_rgba() {
	// `maximum == u8`: candidate list is [rgba, none].
	let list = compatible_bridge_pixel_format_list(PIX_FMT_U8);
	assert_eq!(list[0], FB_PIX_FMT_RGBA);
	assert_eq!(list[1], FB_PIX_FMT_NONE);
}

#[test]
fn compatible_bridge_pixel_format_f32_clamp_adds_rgba_f32() {
	// `maximum == f32`: candidate list is [rgba, rgb_a64_le, rgba_f32_le, none].
	let list = compatible_bridge_pixel_format_list(PIX_FMT_F32);
	assert_eq!(list[0], FB_PIX_FMT_RGBA);
	assert_eq!(list[1], FB_PIX_FMT_RGB_A64_LE);
	assert_eq!(list[2], FB_PIX_FMT_RGBA_F32_LE);
	assert_eq!(list[3], FB_PIX_FMT_NONE);
}

#[test]
fn compatible_bridge_pixel_format_u16_clamp_matches_default() {
	// `maximum == u16` is neither u8 nor f32, so it matches the no-limit
	// candidate list.
	assert_eq!(
		compatible_bridge_pixel_format_list(PIX_FMT_U16),
		compatible_bridge_pixel_format_list(PIX_FMT_INVALID)
	);
}

#[test]
fn compatible_bridge_pixel_format_picks_exact_candidate() {
	// With the test stub an exact match in the candidate list is preferred.
	assert_eq!(
		get_compatible_bridge_pixel_format(FB_PIX_FMT_RGBA, PIX_FMT_INVALID),
		FB_PIX_FMT_RGBA
	);
	assert_eq!(
		get_compatible_bridge_pixel_format(FB_PIX_FMT_RGB_A64_LE, PIX_FMT_F32),
		FB_PIX_FMT_RGB_A64_LE
	);
	assert_eq!(
		get_compatible_bridge_pixel_format(FB_PIX_FMT_RGBA_F32_LE, PIX_FMT_F32),
		FB_PIX_FMT_RGBA_F32_LE
	);
}

#[test]
fn compatible_bridge_pixel_format_unknown_falls_back_to_first() {
	// A source format that is not among the candidates falls back to the
	// first (most desirable) candidate.
	assert_eq!(
		get_compatible_bridge_pixel_format(FB_PIX_FMT_YU_V420_P, PIX_FMT_INVALID),
		FB_PIX_FMT_RGBA
	);
}

#[test]
fn compatible_pixel_format_maps_native_to_least_lossy() {
	assert_eq!(get_compatible_pixel_format(PIX_FMT_U8), PIX_FMT_U8);
	assert_eq!(get_compatible_pixel_format(PIX_FMT_U10), PIX_FMT_U8);
	assert_eq!(get_compatible_pixel_format(PIX_FMT_U16), PIX_FMT_U16);
	assert_eq!(get_compatible_pixel_format(PIX_FMT_F16), PIX_FMT_U16);
	assert_eq!(get_compatible_pixel_format(PIX_FMT_F32), PIX_FMT_U16);
}

#[test]
fn compatible_pixel_format_invalid_and_count_map_to_invalid() {
	assert_eq!(
		get_compatible_pixel_format(PIX_FMT_INVALID),
		PIX_FMT_INVALID
	);
	assert_eq!(get_compatible_pixel_format(PIX_FMT_COUNT), PIX_FMT_INVALID);
}

#[test]
fn ffmpeg_pixel_format_rgb_channel_layout() {
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U8, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_RG_B24
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U10, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_NONE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U16, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_RG_B48_LE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_F16, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_RGB_F16_LE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_F32, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_RGB_F32_LE
	);
}

#[test]
fn ffmpeg_pixel_format_rgba_channel_layout() {
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U8, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_RGBA
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U10, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_NONE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_U16, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_RGB_A64_LE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_F16, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_RGBA_F16_LE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_F32, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_RGBA_F32_LE
	);
}

#[test]
fn ffmpeg_pixel_format_other_channel_layout_returns_none() {
	assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 0), FB_PIX_FMT_NONE);
	assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 2), FB_PIX_FMT_NONE);
	assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 5), FB_PIX_FMT_NONE);
}

#[test]
fn ffmpeg_pixel_format_invalid_and_count_return_none() {
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_INVALID, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_NONE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_COUNT, RGB_CHANNEL_COUNT),
		FB_PIX_FMT_NONE
	);
	assert_eq!(
		get_ffmpeg_pixel_format(PIX_FMT_INVALID, RGBA_CHANNEL_COUNT),
		FB_PIX_FMT_NONE
	);
}

#[test]
fn native_sample_format_maps_bridge_to_native() {
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_U8), SMP_FMT_U8);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S16), SMP_FMT_S16);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S32), SMP_FMT_S32);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S64), SMP_FMT_S64);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_FLT), SMP_FMT_F32);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_DBL), SMP_FMT_F64);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_U8_P), SMP_FMT_U8_P);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S16_P), SMP_FMT_S16_P);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S32_P), SMP_FMT_S32_P);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S64_P), SMP_FMT_S64_P);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_FLTP), SMP_FMT_F32_P);
	assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_DBLP), SMP_FMT_F64_P);
}

#[test]
fn native_sample_format_unknown_maps_to_invalid() {
	assert_eq!(
		get_native_sample_format(FB_SAMPLE_FMT_NONE),
		SMP_FMT_INVALID
	);
	// A bogus code that does not match any bridge sample format.
	assert_eq!(get_native_sample_format(12345), SMP_FMT_INVALID);
}

#[test]
fn ffmpeg_sample_format_maps_native_to_bridge() {
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_U8), FB_SAMPLE_FMT_U8);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S16), FB_SAMPLE_FMT_S16);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S32), FB_SAMPLE_FMT_S32);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S64), FB_SAMPLE_FMT_S64);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F32), FB_SAMPLE_FMT_FLT);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F64), FB_SAMPLE_FMT_DBL);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_U8_P), FB_SAMPLE_FMT_U8_P);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S16_P), FB_SAMPLE_FMT_S16_P);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S32_P), FB_SAMPLE_FMT_S32_P);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S64_P), FB_SAMPLE_FMT_S64_P);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F32_P), FB_SAMPLE_FMT_FLTP);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F64_P), FB_SAMPLE_FMT_DBLP);
}

#[test]
fn ffmpeg_sample_format_invalid_and_count_return_none() {
	assert_eq!(
		get_ffmpeg_sample_format(SMP_FMT_INVALID),
		FB_SAMPLE_FMT_NONE
	);
	assert_eq!(get_ffmpeg_sample_format(SMP_FMT_COUNT), FB_SAMPLE_FMT_NONE);
}

#[test]
fn sample_format_mappings_are_inverse() {
	for native in [
		SMP_FMT_U8,
		SMP_FMT_S16,
		SMP_FMT_S32,
		SMP_FMT_S64,
		SMP_FMT_F32,
		SMP_FMT_F64,
		SMP_FMT_U8_P,
		SMP_FMT_S16_P,
		SMP_FMT_S32_P,
		SMP_FMT_S64_P,
		SMP_FMT_F32_P,
		SMP_FMT_F64_P,
	] {
		let bridge = get_ffmpeg_sample_format(native);
		assert_eq!(get_native_sample_format(bridge), native);
	}
}

#[test]
fn jpeg_space_converts_to_regular_space() {
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J420_P),
		FB_PIX_FMT_YU_V420_P
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J422_P),
		FB_PIX_FMT_YU_V422_P
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J444_P),
		FB_PIX_FMT_YU_V444_P
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J440_P),
		FB_PIX_FMT_YU_V440_P
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J411_P),
		FB_PIX_FMT_YU_V411_P
	);
}

#[test]
fn jpeg_space_leaves_non_jpeg_formats_unchanged() {
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_RGBA),
		FB_PIX_FMT_RGBA
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_NONE),
		FB_PIX_FMT_NONE
	);
	assert_eq!(
		convert_jpeg_space_to_regular_space(FB_PIX_FMT_YU_V420_P),
		FB_PIX_FMT_YU_V420_P
	);
}
