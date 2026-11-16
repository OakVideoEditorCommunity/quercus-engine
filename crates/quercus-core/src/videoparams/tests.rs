use super::*;

fn default_vp() -> VideoParams {
	VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 1)
}

#[test]
fn default_constructor() {
	let vp = VideoParams::new();
	assert_eq!(vp.width(), 0);
	assert_eq!(vp.height(), 0);
	assert_eq!(vp.depth(), 0);
	assert_eq!(vp.time_base(), (0, 1));
	assert_eq!(vp.frame_rate(), (0, 1));
	assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
	assert_eq!(vp.format(), PixelFormat::Invalid);
	assert_eq!(vp.channel_count(), 0);
	assert_eq!(vp.interlacing(), Interlacing::None);
	assert_eq!(vp.divider(), 1);
	assert!(vp.enabled());
	assert_eq!(vp.stream_index(), 0);
	assert_eq!(vp.video_type(), VideoType::Video);
	assert_eq!(vp.start_time(), 0);
	assert_eq!(vp.duration(), 0);
	assert!(!vp.premultiplied_alpha());
	assert_eq!(vp.color_range(), ColorRange::Limited);
	assert_eq!(vp.color_primaries(), 0);
	assert_eq!(vp.color_transfer(), 0);
	assert_eq!(vp.colorspace(), "");
	assert!(!vp.is_3d());
	assert!(!vp.is_valid());
}

#[test]
fn new_basic_fields() {
	let vp = default_vp();
	assert_eq!(vp.width(), 1920);
	assert_eq!(vp.height(), 1080);
	assert_eq!(vp.depth(), 1);
	assert_eq!(vp.format(), PixelFormat::U8);
	assert_eq!(vp.channel_count(), 4);
	assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
	assert_eq!(vp.interlacing(), Interlacing::None);
	assert_eq!(vp.divider(), 1);
	assert_eq!(vp.time_base(), (0, 1));
	assert!(vp.is_valid());
}

#[test]
fn new_basic_reduces_par() {
	// Rational(16, 15) is stored as-is; Rational(0, 1) is null -> 1/1.
	let vp = VideoParams::new_basic(100, 100, PixelFormat::U8, 3, 16, 15, 0, 1);
	assert_eq!(vp.pixel_aspect_ratio(), (16, 15));
	let vp = VideoParams::new_basic(100, 100, PixelFormat::U8, 3, 0, 5, 0, 1);
	assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
}

#[test]
fn new_with_time_base() {
	let vp =
		VideoParams::new_with_time_base(1920, 1080, 1001, 30000, PixelFormat::U8, 4, 1, 1, 0, 1);
	assert_eq!(vp.time_base(), (1001, 30000));
	// frame rate is the flipped time base.
	assert_eq!(vp.frame_rate(), (30000, 1001));
	assert_eq!(vp.frame_rate_as_time_base(), (1001, 30000));
}

#[test]
fn setters_and_getters() {
	let mut vp = default_vp();
	vp.set_width(640);
	vp.set_height(480);
	vp.set_depth(2);
	vp.set_format(PixelFormat::F32);
	vp.set_channel_count(3);
	vp.set_interlacing(Interlacing::TopFirst);
	vp.set_divider(2);
	vp.set_enabled(false);
	vp.set_x(1.5);
	vp.set_y(-2.5);
	vp.set_stream_index(7);
	vp.set_video_type(VideoType::Still);
	vp.set_start_time(100);
	vp.set_duration(50);
	vp.set_premultiplied_alpha(true);
	vp.set_color_range(ColorRange::Full);
	vp.set_color_primaries(9);
	vp.set_color_transfer(18);
	vp.set_colorspace("sRGB");

	assert_eq!(vp.width(), 640);
	assert_eq!(vp.height(), 480);
	assert_eq!(vp.depth(), 2);
	assert_eq!(vp.format(), PixelFormat::F32);
	assert_eq!(vp.channel_count(), 3);
	assert_eq!(vp.interlacing(), Interlacing::TopFirst);
	assert_eq!(vp.divider(), 2);
	assert!(!vp.enabled());
	assert_eq!(vp.x(), 1.5);
	assert_eq!(vp.y(), -2.5);
	assert_eq!(vp.stream_index(), 7);
	assert_eq!(vp.video_type(), VideoType::Still);
	assert_eq!(vp.start_time(), 100);
	assert_eq!(vp.duration(), 50);
	assert!(vp.premultiplied_alpha());
	assert_eq!(vp.color_range(), ColorRange::Full);
	assert_eq!(vp.color_primaries(), 9);
	assert_eq!(vp.color_transfer(), 18);
	assert_eq!(vp.colorspace(), "sRGB");
}

#[test]
fn is_3d_derived_from_depth() {
	let mut vp = default_vp();
	assert!(!vp.is_3d());
	vp.set_depth(2);
	assert!(vp.is_3d());
	assert!(vp.depth() > 1);
	vp.set_depth(1);
	assert!(!vp.is_3d());
}

#[test]
fn effective_sizes() {
	let vp = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 2);
	assert_eq!(vp.effective_width(), 960);
	assert_eq!(vp.effective_height(), 540);
	assert_eq!(vp.effective_depth(), 1); // depth == 1 is not scaled
	assert_eq!(vp.square_pixel_width(), 1920);
}

#[test]
fn square_pixel_width_uses_par() {
	let vp = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 16, 15, 0, 1);
	assert_eq!(vp.square_pixel_width(), 2048); // lround(1920 * 16/15)
}

#[test]
fn is_valid_conditions() {
	assert!(default_vp().is_valid());
	assert!(!VideoParams::new().is_valid());
	let mut vp = default_vp();
	vp.set_channel_count(0);
	assert!(!vp.is_valid());
	let mut vp = default_vp();
	vp.set_format(PixelFormat::Invalid);
	assert!(!vp.is_valid());
	let mut vp = default_vp();
	vp.set_pixel_aspect_ratio(0, 1); // validated to 1/1, still valid
	assert!(vp.is_valid());
}

#[test]
fn bytes_per_channel_for_format() {
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::U8),
		1
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::U10),
		0
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::U16),
		2
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::F16),
		2
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::F32),
		4
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::Invalid),
		0
	);
	assert_eq!(
		VideoParams::bytes_per_channel_for_format(PixelFormat::Count),
		0
	);
}

#[test]
fn bytes_per_pixel_for_format() {
	assert_eq!(
		VideoParams::bytes_per_pixel_for_format(PixelFormat::U8, 4),
		4
	);
	assert_eq!(
		VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, 4),
		4
	);
	assert_eq!(
		VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, 3),
		0
	);
	assert_eq!(
		VideoParams::bytes_per_pixel_for_format(PixelFormat::F32, 4),
		16
	);
}

#[test]
fn buffer_size() {
	assert_eq!(
		VideoParams::calculate_buffer_size(1920, 1080, PixelFormat::U8, 4),
		8294400
	);
	let vp = default_vp();
	assert_eq!(vp.buffer_size(), 8294400);
	assert_eq!(vp.bytes_per_channel(), 1);
	assert_eq!(vp.bytes_per_pixel(), 4);
}

#[test]
fn format_is_float() {
	assert!(VideoParams::format_is_float(PixelFormat::F16));
	assert!(VideoParams::format_is_float(PixelFormat::F32));
	assert!(!VideoParams::format_is_float(PixelFormat::U8));
	assert!(!VideoParams::format_is_float(PixelFormat::U10));
	assert!(!VideoParams::format_is_float(PixelFormat::U16));
}

#[test]
fn generate_auto_divider_values() {
	assert_eq!(VideoParams::generate_auto_divider(100, 100), 1);
	assert_eq!(VideoParams::generate_auto_divider(1920, 1080), 2);
	assert_eq!(VideoParams::generate_auto_divider(3840, 2160), 3);
	assert_eq!(VideoParams::generate_auto_divider(10000, 10000), 12);
	assert_eq!(VideoParams::generate_auto_divider(20000, 20000), 16);
}

#[test]
fn scaled_dimension_and_target() {
	assert_eq!(VideoParams::get_scaled_dimension(1920, 2), 960);
	assert_eq!(
		VideoParams::get_divider_for_target_resolution(3840, 2160, 1920, 1080),
		2
	);
	assert_eq!(
		VideoParams::get_divider_for_target_resolution(1920, 1080, 1920, 1080),
		1
	);
}

#[test]
fn name_for_divider() {
	assert_eq!(VideoParams::name_for_divider(1).unwrap(), "Full");
	assert_eq!(VideoParams::name_for_divider(2).unwrap(), "1/2");
	assert_eq!(VideoParams::name_for_divider(8).unwrap(), "1/8");
}

#[test]
fn format_name() {
	assert_eq!(VideoParams::format_name(PixelFormat::U8).unwrap(), "8-bit");
	assert_eq!(
		VideoParams::format_name(PixelFormat::U10).unwrap(),
		"10-bit Packed"
	);
	assert_eq!(
		VideoParams::format_name(PixelFormat::U16).unwrap(),
		"16-bit Integer"
	);
	assert_eq!(
		VideoParams::format_name(PixelFormat::F16).unwrap(),
		"Half-Float (16-bit)"
	);
	assert_eq!(
		VideoParams::format_name(PixelFormat::F32).unwrap(),
		"Full-Float (32-bit)"
	);
	assert_eq!(
		VideoParams::format_name(PixelFormat::Invalid).unwrap(),
		"Unknown (0xFFFFFFFF)"
	);
	assert_eq!(
		VideoParams::format_name(PixelFormat::Count).unwrap(),
		"Unknown (0x5)"
	);
}

#[test]
fn frame_rate_to_string_values() {
	assert_eq!(
		VideoParams::frame_rate_to_string(24000, 1001).unwrap(),
		"23.976 FPS"
	);
	assert_eq!(VideoParams::frame_rate_to_string(24, 1).unwrap(), "24 FPS");
	assert_eq!(VideoParams::frame_rate_to_string(25, 1).unwrap(), "25 FPS");
	assert_eq!(
		VideoParams::frame_rate_to_string(30000, 1001).unwrap(),
		"29.97 FPS"
	);
	assert_eq!(
		VideoParams::frame_rate_to_string(60000, 1001).unwrap(),
		"59.9401 FPS"
	);
	assert_eq!(
		VideoParams::frame_rate_to_string(48000, 1001).unwrap(),
		"47.952 FPS"
	);
	assert_eq!(VideoParams::frame_rate_to_string(1, 1).unwrap(), "1 FPS");
	assert_eq!(VideoParams::frame_rate_to_string(0, 1).unwrap(), "0 FPS");
}

#[test]
fn time_in_timebase_units() {
	let mut vp = default_vp();
	// No time base set -> None.
	assert_eq!(vp.time_in_timebase_units(1, 1), None);

	vp.set_time_base(1, 1);
	assert_eq!(vp.time_in_timebase_units(1, 1), Some(1));

	vp.set_time_base(1001, 30000);
	vp.set_start_time(5);
	// 1 second at 30000/1001 fps -> 30 frames, + start_time 5.
	assert_eq!(vp.time_in_timebase_units(1, 1), Some(35));
}

#[test]
fn equals_compares_domain_fields() {
	let a = default_vp();
	let b = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 1);
	assert!(a.equals(&b));
	let mut c = a.clone();
	c.set_width(1919);
	assert!(!a.equals(&c));
	let mut d = a.clone();
	d.set_start_time(999); // not part of the C++ operator==
	assert!(a.equals(&d));
}

#[test]
fn xml_round_trip() {
	let mut vp =
		VideoParams::new_with_time_base(1920, 1080, 1001, 30000, PixelFormat::U8, 4, 16, 15, 1, 2);
	vp.set_enabled(true);
	vp.set_x(1.5);
	vp.set_y(-2.5);
	vp.set_stream_index(3);
	vp.set_video_type(VideoType::Still);
	vp.set_start_time(100);
	vp.set_duration(50);
	vp.set_premultiplied_alpha(true);
	vp.set_color_range(ColorRange::Full);
	vp.set_color_primaries(9);
	vp.set_color_transfer(18);
	vp.set_colorspace("sRGB");

	let xml = vp.save_xml().unwrap();
	assert!(xml.starts_with("<videoparams>"));
	assert!(xml.ends_with("</videoparams>"));
	assert!(xml.contains("<width>1920</width>"));

	let mut loaded = VideoParams::new();
	loaded.load_xml(&xml).unwrap();
	assert_eq!(loaded.width(), vp.width());
	assert_eq!(loaded.height(), vp.height());
	assert_eq!(loaded.depth(), vp.depth());
	assert_eq!(loaded.time_base(), vp.time_base());
	assert_eq!(loaded.frame_rate(), vp.frame_rate());
	assert_eq!(loaded.format(), vp.format());
	assert_eq!(loaded.channel_count(), vp.channel_count());
	assert_eq!(loaded.pixel_aspect_ratio(), vp.pixel_aspect_ratio());
	assert_eq!(loaded.interlacing(), vp.interlacing());
	assert_eq!(loaded.divider(), vp.divider());
	assert_eq!(loaded.enabled(), vp.enabled());
	assert_eq!(loaded.x(), vp.x());
	assert_eq!(loaded.y(), vp.y());
	assert_eq!(loaded.stream_index(), vp.stream_index());
	assert_eq!(loaded.video_type(), vp.video_type());
	assert_eq!(loaded.start_time(), vp.start_time());
	assert_eq!(loaded.duration(), vp.duration());
	assert_eq!(loaded.premultiplied_alpha(), vp.premultiplied_alpha());
	assert_eq!(loaded.color_range(), vp.color_range());
	assert_eq!(loaded.color_primaries(), vp.color_primaries());
	assert_eq!(loaded.color_transfer(), vp.color_transfer());
	assert_eq!(loaded.colorspace(), vp.colorspace());
	// Re-saving after a load must be byte-identical.
	assert_eq!(loaded.save_xml().unwrap(), xml);
}

#[test]
fn xml_skips_unknown_elements() {
	let xml = "<videoparams><width>640</width><bogus><nested>1</nested></bogus><height>480</height></videoparams>";
	let mut vp = VideoParams::new();
	vp.load_xml(xml).unwrap();
	assert_eq!(vp.width(), 640);
	assert_eq!(vp.height(), 480);
}

#[test]
fn xml_error_paths() {
	let mut vp = VideoParams::new();
	assert!(vp.load_xml("").is_err());
	assert!(vp.load_xml("not xml").is_err());
	assert!(vp.load_xml("<width>").is_err());
	assert!(vp.load_xml("<width>5</height>").is_err());
	// A non-numeric value inside a properly-rooted doc must error (stoi).
	assert!(vp
		.load_xml("<videoparams><width>notanumber</width></videoparams>")
		.is_err());
	// Self-closing element is not malformed; the empty value is accepted
	// for a text field (colorspace) but rejected for an integer field.
	assert!(vp
		.load_xml("<videoparams><width>640</width><colorspace/><depth>2</depth></videoparams>")
		.is_ok());
	assert_eq!(vp.width(), 640);
	assert_eq!(vp.depth(), 2);
	assert_eq!(vp.colorspace(), "");
	assert!(vp
		.load_xml("<videoparams><width/><colorspace>a</colorspace></videoparams>")
		.is_err());
}

// ---- Extended coverage --------------------------------------------------

#[test]
fn bytes_per_pixel_matrix() {
	// Full format × channel matrix, mirroring get_bytes_per_pixel():
	// packed u10 only supports RGBA (4); everything else is bpc*channels.
	for ch in [0, 1, 3, 4] {
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U8, ch),
			ch
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U16, ch),
			2 * ch
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::F16, ch),
			2 * ch
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::F32, ch),
			4 * ch
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::Invalid, ch),
			0
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::Count, ch),
			0
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, ch),
			if ch == 4 { 4 } else { 0 }
		);
	}
}

#[test]
fn bytes_per_channel_instance_and_buffer_size_f32() {
	let vp = VideoParams::new_basic(4, 2, PixelFormat::F32, 4, 1, 1, 0, 1);
	assert_eq!(vp.bytes_per_channel(), 4);
	assert_eq!(vp.bytes_per_pixel(), 16);
	assert_eq!(vp.buffer_size(), 4 * 2 * 16);
}

#[test]
fn buffer_size_wraps_like_cpp_int() {
	// C++ computes int w*h*bpp (wraps on overflow); the port must use
	// wrapping arithmetic so debug builds don't panic.
	let _ = VideoParams::calculate_buffer_size(i32::MAX, i32::MAX, PixelFormat::F32, 4);
}

#[test]
fn is_valid_matrix() {
	// Every real pixel format is valid (C++: format > invalid && < count).
	for f in [
		PixelFormat::U8,
		PixelFormat::U10,
		PixelFormat::U16,
		PixelFormat::F16,
		PixelFormat::F32,
	] {
		assert!(VideoParams::new_basic(2, 2, f, 4, 1, 1, 0, 1).is_valid());
	}
	// Count is out of range.
	assert!(!VideoParams::new_basic(2, 2, PixelFormat::Count, 4, 1, 1, 0, 1).is_valid());
	// Zero/negative dimensions are invalid.
	assert!(!VideoParams::new_basic(0, 2, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
	assert!(!VideoParams::new_basic(2, 0, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
	assert!(!VideoParams::new_basic(-2, 2, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
	// Zero/negative channel counts are invalid.
	assert!(!VideoParams::new_basic(2, 2, PixelFormat::U8, 0, 1, 1, 0, 1).is_valid());
	assert!(!VideoParams::new_basic(2, 2, PixelFormat::U8, -1, 1, 1, 0, 1).is_valid());
}

#[test]
fn effective_size_edges() {
	// Odd dimensions truncate like C++ integer division.
	let vp = VideoParams::new_basic(1921, 1081, PixelFormat::U8, 4, 1, 1, 0, 2);
	assert_eq!(vp.effective_width(), 960);
	assert_eq!(vp.effective_height(), 540);
	// depth > 1 is scaled by the divider; depth == 1 is not.
	let mut vp = VideoParams::new_basic(16, 16, PixelFormat::U8, 4, 1, 1, 0, 2);
	vp.set_depth(4);
	assert_eq!(vp.effective_depth(), 2);
	vp.set_depth(1);
	assert_eq!(vp.effective_depth(), 1);
}

#[test]
fn square_pixel_width_rounding() {
	// std::lround rounds half away from zero: 5 * 3/2 = 7.5 -> 8.
	let vp = VideoParams::new_basic(5, 5, PixelFormat::U8, 4, 3, 2, 0, 1);
	assert_eq!(vp.square_pixel_width(), 8);
}

#[test]
fn generate_auto_divider_boundaries() {
	// sqrt(1.0) == 1 -> smallest supported divider.
	assert_eq!(VideoParams::generate_auto_divider(1280, 720), 1);
	// Exactly at the top end (sqrt == 16) -> 16.
	assert_eq!(VideoParams::generate_auto_divider(20480, 11520), 16);
	// Tie between 2 and 3 (divider == 2.5 exactly): C++ picks `next`
	// because prev_diff < next_diff is false on a tie.
	assert_eq!(VideoParams::generate_auto_divider(2400, 2400), 3);
}

#[test]
fn divider_for_target_resolution_edges() {
	// Source already fits -> 1.
	assert_eq!(
		VideoParams::get_divider_for_target_resolution(100, 100, 1920, 1080),
		1
	);
	// 1921/2 = 960 fits 960, 1081/2 = 540 fits 540 -> 2.
	assert_eq!(
		VideoParams::get_divider_for_target_resolution(1921, 1081, 960, 540),
		2
	);
	assert_eq!(
		VideoParams::get_divider_for_target_resolution(1921, 1081, 959, 540),
		3
	);
}

#[test]
fn name_for_divider_unusual() {
	// C++ never validates; "1/" + std::to_string(div).
	assert_eq!(VideoParams::name_for_divider(0).unwrap(), "1/0");
	assert_eq!(VideoParams::name_for_divider(-2).unwrap(), "1/-2");
}

#[test]
fn frame_rate_to_string_edge_values() {
	assert_eq!(
		VideoParams::frame_rate_to_string(-24, 1).unwrap(),
		"-24 FPS"
	);
	assert_eq!(
		VideoParams::frame_rate_to_string(1, 1000000).unwrap(),
		"1e-06 FPS"
	);
	assert_eq!(
		VideoParams::frame_rate_to_string(1000000, 1).unwrap(),
		"1e+06 FPS"
	);
	assert_eq!(VideoParams::frame_rate_to_string(0, 0).unwrap(), "nan FPS");
}

#[test]
fn format_g_helper() {
	assert_eq!(format_g(23.976023976), "23.976");
	assert_eq!(format_g(0.0), "0");
	assert_eq!(format_g(-2.5), "-2.5");
	assert_eq!(format_g(f64::NAN), "nan");
	assert_eq!(format_g(f64::INFINITY), "inf");
	assert_eq!(format_g(f64::NEG_INFINITY), "-inf");
	assert_eq!(format_g(123456.0), "123456");
	assert_eq!(format_g(1234567.0), "1.23457e+06");
	assert_eq!(format_g(0.0001), "0.0001");
	assert_eq!(format_g(0.00001), "1e-05");
}

#[test]
fn time_base_normalization() {
	let mut vp = default_vp();
	vp.set_time_base(2, 4);
	assert_eq!(vp.time_base(), (1, 2));
	vp.set_time_base(1, -2);
	assert_eq!(vp.time_base(), (-1, 2));
	vp.set_time_base(0, 5);
	assert_eq!(vp.time_base(), (0, 1));
	// Zero denominator -> NaN rational -> treated as "no time base".
	vp.set_time_base(1, 0);
	assert_eq!(vp.time_base(), (0, 0));
	assert_eq!(vp.time_in_timebase_units(1, 1), None);
}

#[test]
fn pixel_aspect_ratio_sign_fix() {
	let mut vp = default_vp();
	vp.set_pixel_aspect_ratio(4, -3);
	assert_eq!(vp.pixel_aspect_ratio(), (-4, 3));
	vp.set_pixel_aspect_ratio(8, 8);
	assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
}

#[test]
fn frame_rate_as_time_base_null() {
	// A null frame rate flips to itself (C++ Rational::flipped no-op).
	let vp = VideoParams::new();
	assert_eq!(vp.frame_rate_as_time_base(), (0, 1));
}

#[test]
fn time_in_timebase_units_edges() {
	let mut vp = default_vp();
	vp.set_time_base(1, 2);
	// Half-unit rounds away from zero (llround): 0.25s * 2 = 0.5 -> 1.
	assert_eq!(vp.time_in_timebase_units(1, 4), Some(1));
	// Negative times round away from zero too: -0.5 -> -1.
	assert_eq!(vp.time_in_timebase_units(-1, 4), Some(-1));
	// NaN time (0/0) converts to 0 + start_time, matching
	// Timecode::time_to_timestamp.
	vp.set_start_time(7);
	assert_eq!(vp.time_in_timebase_units(0, 0), Some(7));
}

#[test]
fn equals_field_by_field() {
	let a = default_vp();
	let mut b = a.clone();
	b.set_height(1079);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_depth(2);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_interlacing(Interlacing::BottomFirst);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_time_base(1, 25);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_format(PixelFormat::U16);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_pixel_aspect_ratio(4, 3);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_divider(4);
	assert!(!a.equals(&b));
	let mut b = a.clone();
	b.set_channel_count(3);
	assert!(!a.equals(&b));
	// Fields outside C++ operator== don't participate.
	let mut b = a.clone();
	b.set_enabled(false);
	b.set_colorspace("rec709");
	b.set_frame_rate(24, 1);
	b.set_duration(10);
	assert!(a.equals(&b));
}

#[test]
fn save_xml_exact() {
	let vp = VideoParams::new_basic(640, 480, PixelFormat::U8, 3, 1, 1, 0, 1);
	let expected = "<videoparams>\
			<width>640</width><height>480</height><depth>1</depth>\
			<timebase>0/1</timebase><format>0</format><channelcount>3</channelcount>\
			<pixelaspectratio>1/1</pixelaspectratio><interlacing>0</interlacing>\
			<divider>1</divider><enabled>1</enabled>\
			<x>0.000000</x><y>0.000000</y>\
			<streamindex>0</streamindex><videotype>0</videotype>\
			<framerate>0/1</framerate><starttime>0</starttime><duration>0</duration>\
			<premultipliedalpha>0</premultipliedalpha><colorspace></colorspace>\
			<colorrange>0</colorrange><colorprimaries>0</colorprimaries>\
			<colortransfer>0</colortransfer>\
			</videoparams>";
	assert_eq!(vp.save_xml().unwrap(), expected);
}

#[test]
fn xml_round_trip_default() {
	let vp = VideoParams::new();
	let xml = vp.save_xml().unwrap();
	let mut loaded = VideoParams::new();
	loaded.load_xml(&xml).unwrap();
	assert!(loaded.equals(&vp));
	assert_eq!(loaded.save_xml().unwrap(), xml);
}

#[test]
fn load_xml_with_declaration_comments_and_cdata() {
	let xml = "<?xml version=\"1.0\"?><!-- comment --><videoparams>\
			<width>640</width><colorspace><![CDATA[a <b> & c]]></colorspace></videoparams>";
	let mut vp = VideoParams::new();
	vp.load_xml(xml).unwrap();
	assert_eq!(vp.width(), 640);
	assert_eq!(vp.colorspace(), "a <b> & c");
}

#[test]
fn load_xml_entities_and_boolean_coercion() {
	let xml = "<videoparams><enabled>2</enabled><premultipliedalpha>-1</premultipliedalpha>\
			<colorspace>a&amp;b</colorspace></videoparams>";
	let mut vp = VideoParams::new();
	vp.load_xml(xml).unwrap();
	assert!(vp.enabled()); // std::stoi("2") -> bool true
	assert!(vp.premultiplied_alpha()); // std::stoi("-1") -> bool true
	assert_eq!(vp.colorspace(), "a&b");
}

#[test]
fn load_xml_partial_update_keeps_existing() {
	// C++ load() only assigns fields present in the document.
	let mut vp = default_vp();
	vp.load_xml("<videoparams><width>320</width></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 320);
	assert_eq!(vp.height(), 1080); // untouched
	assert_eq!(vp.format(), PixelFormat::U8);
}

#[test]
fn load_xml_enum_fallbacks() {
	let xml = "<videoparams><interlacing>7</interlacing><videotype>9</videotype>\
			<colorrange>5</colorrange><format>99</format></videoparams>";
	let mut vp = default_vp();
	vp.load_xml(xml).unwrap();
	assert_eq!(vp.interlacing(), Interlacing::None);
	assert_eq!(vp.video_type(), VideoType::Video);
	assert_eq!(vp.color_range(), ColorRange::Limited);
	// Unknown format codes can't be represented in Rust; they land on
	// Invalid, which (like any out-of-range code) fails is_valid().
	assert_eq!(vp.format(), PixelFormat::Invalid);
	assert!(!vp.is_valid());
}

#[test]
fn load_xml_numeric_prefix_parsing() {
	let mut vp = VideoParams::new();
	// std::stoi/std::stof consume the longest valid prefix, ignore junk.
	vp.load_xml("<videoparams><width>640abc</width><x>1.5garbage</x><y> -2.5 </y></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 640);
	assert_eq!(vp.x(), 1.5);
	assert_eq!(vp.y(), -2.5);
}

#[test]
fn parse_f32_field_prefixes() {
	assert_eq!(parse_f32_field("1.5").unwrap(), 1.5);
	assert_eq!(parse_f32_field("  -2.5  ").unwrap(), -2.5);
	assert_eq!(parse_f32_field("1.5junk").unwrap(), 1.5);
	assert_eq!(parse_f32_field(".5").unwrap(), 0.5);
	assert_eq!(parse_f32_field("2.").unwrap(), 2.0);
	assert_eq!(parse_f32_field("1e3").unwrap(), 1000.0);
	assert_eq!(parse_f32_field("1.25e-2x").unwrap(), 0.0125);
	// An exponent with no digits is not part of the prefix.
	assert_eq!(parse_f32_field("1.5e").unwrap(), 1.5);
	assert!(parse_f32_field("").is_err());
	assert!(parse_f32_field("abc").is_err());
	assert!(parse_f32_field(".e5").is_err());
	assert!(parse_f32_field("-").is_err());
}

#[test]
fn load_xml_malformed_entities_fail() {
	let mut vp = VideoParams::new();
	// Unknown entity in character data is a parse error in this reader
	// (stricter than the subtitle reader, which preserves it verbatim).
	assert!(vp
		.load_xml("<videoparams><colorspace>a &bogus; b</colorspace></videoparams>")
		.is_err());
	// Unterminated comment.
	assert!(vp.load_xml("<videoparams><!-- never ends").is_err());
	// Mismatched nesting.
	assert!(vp
		.load_xml("<videoparams><width>1</videoparams></width>")
		.is_err());
}

#[test]
fn rational_helpers_match_quercuscore() {
	// The hand-rolled tuple rationals must agree with quercus_core::Rational,
	// the canonical port of the C++ Rational.
	for (n, d) in [
		(2i32, 4i32),
		(0, 5),
		(5, 0),
		(-3, -1),
		(1, -2),
		(7, 3),
		(100, 10),
	] {
		let r = crate::Rational::new(n as i64, d as i64);
		assert_eq!(
			make_rational(n, d),
			(r.numerator() as i32, r.denominator() as i32)
		);
	}
	for s in ["1/2", "7", "4/2", "junk", "a/b", "1/2/3", "-6/3"] {
		let r = crate::Rational::from_string(s);
		assert_eq!(
			rational_from_string(s),
			(r.numerator() as i32, r.denominator() as i32),
			"input {s:?}"
		);
	}
}

#[test]
fn time_conversion_matches_quercuscore() {
	let mut vp =
		VideoParams::new_with_time_base(1920, 1080, 1001, 30000, PixelFormat::U8, 4, 1, 1, 0, 1);
	vp.set_start_time(11);
	let tb = crate::Rational::new(1001, 30000);
	for (n, d) in [(1i32, 1i32), (1, 2), (24000, 1001), (-3, 1), (0, 1)] {
		let expected = tb.time_to_timestamp(crate::Rational::new(n as i64, d as i64)) + 11;
		assert_eq!(vp.time_in_timebase_units(n, d), Some(expected));
	}
}

// ---- Branch-coverage fill-ins ------------------------------------------

#[test]
fn new_with_time_base_null_par_defaults_to_square() {
	// C++ validate_pixel_aspect_ratio(): a null PAR falls back to 1/1.
	let vp = VideoParams::new_with_time_base(640, 480, 1, 25, PixelFormat::U16, 3, 0, 7, 2, 1);
	assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
	// The BottomFirst interlacing code passes through the ctor too.
	assert_eq!(vp.interlacing(), Interlacing::BottomFirst);
	assert_eq!(vp.time_base(), (1, 25));
}

#[test]
fn square_pixel_width_with_zero_par_denominator() {
	// A (x, 0) PAR is a NaN rational no setter can produce (setters
	// validate); the defensive denominator check must return the raw
	// width instead of propagating NaN.
	let mut vp = default_vp();
	vp.pixel_aspect_ratio = (4, 0);
	assert_eq!(vp.square_pixel_width(), 1920);
}

#[test]
fn load_xml_bad_values_error_for_interlacing_and_colorrange() {
	let mut vp = VideoParams::new();
	// stoi_field failures inside these two arms propagate as errors.
	assert!(vp
		.load_xml("<videoparams><interlacing>notanumber</interlacing></videoparams>")
		.is_err());
	assert!(vp
		.load_xml("<videoparams><colorrange>notanumber</colorrange></videoparams>")
		.is_err());
}

#[test]
fn pixel_format_code_round_trip_all_arms() {
	for (code, expected) in [
		(-1, PixelFormat::Invalid),
		(0, PixelFormat::U8),
		(1, PixelFormat::U10),
		(2, PixelFormat::U16),
		(3, PixelFormat::F16),
		(4, PixelFormat::F32),
		(5, PixelFormat::Count),
		(6, PixelFormat::Invalid),
		(99, PixelFormat::Invalid),
	] {
		assert_eq!(pf_from_code(code), expected, "code {code}");
	}
	for pf in [
		PixelFormat::U8,
		PixelFormat::U10,
		PixelFormat::U16,
		PixelFormat::F16,
		PixelFormat::F32,
	] {
		assert_eq!(pf_from_code(pf_code(pf)), pf);
	}
}

#[test]
fn enum_i32_mapping_all_arms() {
	assert_eq!(interlacing_from_i32(1), Interlacing::TopFirst);
	assert_eq!(interlacing_from_i32(2), Interlacing::BottomFirst);
	assert_eq!(interlacing_from_i32(3), Interlacing::None);
	assert_eq!(video_type_from_i32(1), VideoType::Still);
	assert_eq!(video_type_from_i32(2), VideoType::ImageSequence);
	assert_eq!(video_type_from_i32(9), VideoType::Video);
	assert_eq!(color_range_from_i32(1), ColorRange::Full);
	assert_eq!(color_range_from_i32(0), ColorRange::Limited);
}

#[test]
fn save_xml_escapes_reserved_characters() {
	let mut vp = default_vp();
	vp.set_colorspace("a&b<c>d");
	let xml = vp.save_xml().unwrap();
	assert!(
		xml.contains("<colorspace>a&amp;b&lt;c&gt;d</colorspace>"),
		"escaped text: {xml}"
	);
	let mut loaded = VideoParams::new();
	loaded.load_xml(&xml).unwrap();
	assert_eq!(loaded.colorspace(), "a&b<c>d");
}

#[test]
fn gcd_and_rational_helpers_sign_arms() {
	// Negative second operand exercises the `b = -b` arm.
	assert_eq!(i64_gcd(4, -6), 2);
	assert_eq!(i64_gcd(-4, -6), 2);
	assert_eq!(i64_gcd(0, 5), 5);
	assert_eq!(i64_gcd(7, 0), 7);
	assert_eq!(i64_gcd(0, 0), 0);
	// A negative numerator flows through the reduce path.
	assert_eq!(make_rational(-6, 4), (-3, 2));
}

#[test]
fn rational_flipped_sign_and_degenerate_inputs() {
	// Negative denominator after the swap is sign-fixed.
	assert_eq!(rational_flipped((3, -2)), (-2, 3));
	assert_eq!(rational_flipped((-3, 2)), (-2, 3));
	// Null rational is returned untouched.
	assert_eq!(rational_flipped((0, 5)), (0, 5));
	assert_eq!(rational_flipped((0, 0)), (0, 0));
	// Non-null numerator with a zero denominator swaps to 0/1.
	assert_eq!(rational_flipped((1, 0)), (0, 1));
}

#[test]
fn xml_nested_field_text_and_attributes() {
	let mut vp = VideoParams::new();
	// Nested elements inside a known field: depth-1 text only.
	vp.load_xml("<videoparams><width><inner>9</inner>640</width></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 640);
	// Whitespace after the tag name, around '=', and single-quoted values.
	vp.load_xml("<videoparams version = \"2.0\" xmlns:x='y' ><height>480</height></videoparams>")
		.unwrap();
	assert_eq!(vp.height(), 480);
	// Entities (and plain runs) inside attribute values are validated.
	vp.load_xml("<videoparams note=\"a&amp;b&#65;\" ><depth>2</depth></videoparams>")
		.unwrap();
	assert_eq!(vp.depth(), 2);
	// Unknown entities in an attribute value are parse errors.
	assert!(vp
		.load_xml("<videoparams note=\"a&nope;b\"><depth>2</depth></videoparams>")
		.is_err());
}

#[test]
fn xml_doctype_paths() {
	let mut vp = VideoParams::new();
	vp.load_xml("<!DOCTYPE videoparams><videoparams><width>1</width></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 1);
	// An internal subset keeps the '>' inside the brackets from ending
	// the declaration.
	vp.load_xml(
		"<!DOCTYPE videoparams [ <!ENTITY x \"y\"> ]><videoparams><width>2</width></videoparams>",
	)
	.unwrap();
	assert_eq!(vp.width(), 2);
	// A closing bracket with no open one saturates at zero.
	vp.load_xml("<!DOCTYPE videoparams ]><videoparams><width>3</width></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 3);
	// An unterminated declaration is a parse error.
	assert!(vp.load_xml("<!DOCTYPE videoparams").is_err());
}

#[test]
fn xml_structural_malformations_fail() {
	let mut vp = VideoParams::new();
	// Empty tag name.
	assert!(vp.load_xml("<videoparams>< ></videoparams>").is_err());
	// Unterminated start tag (no '>').
	assert!(vp.load_xml("<videoparams><width").is_err());
	// '/' not followed by '>'.
	assert!(vp
		.load_xml("<videoparams><width/ x></videoparams>")
		.is_err());
	// Attribute without '='.
	assert!(vp
		.load_xml("<videoparams foo bar=\"x\"><width>1</width></videoparams>")
		.is_err());
	// Attribute value without an opening quote.
	assert!(vp
		.load_xml("<videoparams foo=bar><width>1</width></videoparams>")
		.is_err());
	// Unterminated attribute value.
	assert!(vp
		.load_xml("<videoparams foo=\"bar><width>1</width></videoparams>")
		.is_err());
	// Junk after an end-element name.
	assert!(vp
		.load_xml("<videoparams><width>1</width x></videoparams>")
		.is_err());
	// An end tag that never closes errors too.
	assert!(vp.load_xml("<videoparams></videoparams").is_err());
	// Whitespace before the end tag's '>' is fine.
	vp.load_xml("<videoparams><width>5</width ></videoparams>")
		.unwrap();
	assert_eq!(vp.width(), 5);
}

#[test]
fn xml_numeric_entities() {
	let mut vp = VideoParams::new();
	vp.load_xml("<videoparams><colorspace>&#x41;&#66;&#x1F600;</colorspace></videoparams>")
		.unwrap();
	assert_eq!(vp.colorspace(), "AB\u{1F600}");
	// Invalid hex digits / out-of-range code points are parse errors.
	assert!(vp
		.load_xml("<videoparams><colorspace>&#xZZ;</colorspace></videoparams>")
		.is_err());
	assert!(vp
		.load_xml("<videoparams><colorspace>&#x110000;</colorspace></videoparams>")
		.is_err());
	assert!(vp
		.load_xml("<videoparams><colorspace>&#1114112;</colorspace></videoparams>")
		.is_err());
}

#[test]
fn xml_cursor_handles_unclosed_event_streams() {
	// Defensive: a truncated event list reaches EndDocument inside
	// read_element_text.
	let mut cur = XmlCursor::new(vec![XmlEvent::StartElement("a".to_string())]);
	assert!(cur.next_start_element());
	assert_eq!(cur.read_element_text(), "");
	// ... and inside skip_current_element.
	let mut cur = XmlCursor::new(vec![XmlEvent::StartElement("a".to_string())]);
	assert!(cur.next_start_element());
	cur.skip_current_element();
	// Nested elements count depth; only depth-1 characters accumulate.
	let mut cur = XmlCursor::new(vec![
		XmlEvent::StartElement("a".to_string()),
		XmlEvent::Characters("x".to_string()),
		XmlEvent::StartElement("b".to_string()),
		XmlEvent::Characters("ignored".to_string()),
		XmlEvent::EndElement("b".to_string()),
		XmlEvent::Characters("y".to_string()),
		XmlEvent::EndElement("a".to_string()),
	]);
	assert!(cur.next_start_element());
	assert_eq!(cur.read_element_text(), "xy");
}
