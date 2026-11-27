use super::*;

fn utils() -> OIIOUtils {
	OIIOUtils::new()
}

#[test]
fn base_type_from_format_mapping() {
	let u = utils();
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::U8).unwrap(),
		2
	); // UINT8
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::U10).unwrap(),
		0
	); // UNKNOWN
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::U16).unwrap(),
		4
	); // UINT16
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::F16).unwrap(),
		10
	); // HALF
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::F32).unwrap(),
		11
	); // FLOAT
	   // Invalid / count fall through to UNKNOWN.
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::Invalid)
			.unwrap(),
		0
	);
	assert_eq!(
		u.get_oiio_base_type_from_format(PixelFormat::Count)
			.unwrap(),
		0
	);
}

#[test]
fn format_from_oiio_basetype_mapping() {
	let u = utils();
	assert_eq!(u.get_format_from_oiio_basetype(2).unwrap(), PixelFormat::U8);
	assert_eq!(
		u.get_format_from_oiio_basetype(4).unwrap(),
		PixelFormat::U16
	);
	assert_eq!(
		u.get_format_from_oiio_basetype(10).unwrap(),
		PixelFormat::F16
	);
	assert_eq!(
		u.get_format_from_oiio_basetype(11).unwrap(),
		PixelFormat::F32
	);
	// Unknown / unmappable base types map to Invalid.
	assert_eq!(
		u.get_format_from_oiio_basetype(0).unwrap(),
		PixelFormat::Invalid
	); // UNKNOWN
	assert_eq!(
		u.get_format_from_oiio_basetype(1).unwrap(),
		PixelFormat::Invalid
	); // NONE
	assert_eq!(
		u.get_format_from_oiio_basetype(3).unwrap(),
		PixelFormat::Invalid
	); // INT8
	assert_eq!(
		u.get_format_from_oiio_basetype(12).unwrap(),
		PixelFormat::Invalid
	); // DOUBLE
	assert_eq!(
		u.get_format_from_oiio_basetype(15).unwrap(),
		PixelFormat::Invalid
	); // USTRINGHASH
	assert_eq!(
		u.get_format_from_oiio_basetype(99).unwrap(),
		PixelFormat::Invalid
	);
}

#[test]
fn format_from_oiio_basetype_negative_is_error() {
	let u = utils();
	assert!(u.get_format_from_oiio_basetype(-1).is_err());
}

#[test]
fn pixel_aspect_ratio_common_values() {
	let u = utils();
	// 1:1
	assert_eq!(u.get_pixel_aspect_ratio(1.0).unwrap(), (1, 1));
	// 16:9
	assert_eq!(u.get_pixel_aspect_ratio(16.0 / 9.0).unwrap(), (16, 9));
	// 4:3
	assert_eq!(u.get_pixel_aspect_ratio(4.0 / 3.0).unwrap(), (4, 3));
	// 2:1
	assert_eq!(u.get_pixel_aspect_ratio(2.0).unwrap(), (2, 1));
	// 1:2
	assert_eq!(u.get_pixel_aspect_ratio(0.5).unwrap(), (1, 2));
	// 1:1.5 = 2:3
	assert_eq!(u.get_pixel_aspect_ratio(2.0 / 3.0).unwrap(), (2, 3));
}

#[test]
fn pixel_aspect_ratio_zero() {
	let u = utils();
	assert_eq!(u.get_pixel_aspect_ratio(0.0).unwrap(), (0, 1));
}

#[test]
fn pixel_aspect_ratio_nan_returns_nan_rational() {
	let u = utils();
	assert_eq!(u.get_pixel_aspect_ratio(f64::NAN).unwrap(), (0, 0));
}

#[test]
fn pixel_aspect_ratio_out_of_range_returns_nan_rational() {
	let u = utils();
	let too_big = i32::MAX as f64 + 4.0;
	assert_eq!(u.get_pixel_aspect_ratio(too_big).unwrap(), (0, 0));
	assert_eq!(u.get_pixel_aspect_ratio(-too_big).unwrap(), (0, 0));
}

#[test]
fn pixel_aspect_ratio_reduction() {
	let u = utils();
	// 4.0 / 6.0 reduces to 2:3.
	assert_eq!(u.get_pixel_aspect_ratio(4.0 / 6.0).unwrap(), (2, 3));
	// 0.25 = 1:4.
	assert_eq!(u.get_pixel_aspect_ratio(0.25).unwrap(), (1, 4));
	// 1.5 = 3:2.
	assert_eq!(u.get_pixel_aspect_ratio(1.5).unwrap(), (3, 2));
}

#[test]
fn pixel_aspect_ratio_round_trip() {
	let u = utils();
	// The recovered rational should reproduce the input within the
	// precision of a reduced fraction.
	for input in [
		0.5,
		1.0,
		1.333_333_333_333_333_3,
		1.777_777_777_777_777_7,
		2.0,
		2.35,
	] {
		let (n, d) = u.get_pixel_aspect_ratio(input).unwrap();
		if d == 0 {
			continue;
		}
		let recovered = n as f64 / d as f64;
		let err = (recovered - input).abs();
		assert!(
			err < 1e-9,
			"input={input} recovered={recovered} ({n}/{d}) err={err}"
		);
	}
}

#[test]
fn oiioutils_new_is_stateless() {
	// The object is a stateless unit; construction must succeed and be
	// cheap to repeat.
	let _a = OIIOUtils::new();
	let _b = OIIOUtils::new();
}

#[test]
fn image_color_type_maps_formats() {
	assert_eq!(
		image_color_type_for(PixelFormat::U8),
		Some(ExtendedColorType::L8)
	);
	assert_eq!(
		image_color_type_for(PixelFormat::U16),
		Some(ExtendedColorType::L16)
	);
	assert_eq!(
		image_color_type_for(PixelFormat::F32),
		Some(ExtendedColorType::Rgb32F)
	);
	// No image representation.
	assert_eq!(image_color_type_for(PixelFormat::U10), None);
	assert_eq!(image_color_type_for(PixelFormat::F16), None);
	assert_eq!(image_color_type_for(PixelFormat::Invalid), None);
	assert_eq!(image_color_type_for(PixelFormat::Count), None);
}

#[test]
fn bits_per_channel_matches_crate_tables() {
	// Derived from the image crate's own bits-per-pixel / channel tables.
	assert_eq!(bits_per_channel(ExtendedColorType::L8), 8);
	assert_eq!(bits_per_channel(ExtendedColorType::L16), 16);
	assert_eq!(bits_per_channel(ExtendedColorType::Rgb32F), 32);
	assert_eq!(bits_per_channel(ExtendedColorType::Rgba32F), 32);
	// A channel-less type yields 0 (the upstream fallback to UNKNOWN).
	assert_eq!(bits_per_channel(ExtendedColorType::Unknown(0)), 0);
}

#[test]
fn pixel_aspect_ratio_tiny_value_stays_representable() {
	// Very small magnitudes must not panic or produce a NaN rational
	// (av_d2q rescales internally).
	let u = utils();
	let (n, d) = u.get_pixel_aspect_ratio(1e-9).unwrap();
	assert_ne!(d, 0);
	let recovered = n as f64 / d as f64;
	assert!((recovered - 1e-9).abs() / 1e-9 < 1e-6, "got {recovered}");
}

fn temp_tiff_path(name: &str) -> (std::path::PathBuf, String) {
	let dir = std::env::temp_dir().join("quercus_core-oiioutils");
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join(name);
	let path_str = path.to_str().unwrap().to_string();
	(path, path_str)
}

#[test]
fn image_f32_write_read_round_trip() {
	// 2x2 RGBA float image through a temp TIFF.
	let w = 2;
	let h = 2;
	let c = 4;
	let pixels: Vec<f32> = vec![
		0.0, 0.25, 0.5, 1.0, 0.75, 0.5, 0.25, 1.0, 1.0, 0.0, 0.5, 0.0, 0.125, 0.625, 0.875, 1.0,
	];

	let (path, path_str) = temp_tiff_path("roundtrip.tif");
	write_image_f32(&path_str, w, h, c, &pixels).expect("write should succeed");

	let img = read_image_f32(&path_str).expect("read should succeed");
	assert_eq!(img.width, w);
	assert_eq!(img.height, h);
	assert_eq!(img.channels, c);
	assert_eq!(img.pixels.len(), (w * h * c) as usize);
	assert_eq!(img.len(), img.pixels.len());
	assert!(!img.is_empty());

	for (i, (a, b)) in img.pixels.iter().zip(pixels.iter()).enumerate() {
		let diff = (a - b).abs();
		assert!(diff < 1e-6, "pixel {i}: wrote {b}, read back {a}");
	}

	std::fs::remove_file(&path).ok();
}

#[test]
fn image_f32_write_rgb_round_trip() {
	// RGB (3-channel) writes are supported too; the read reports the
	// channel count from the file.
	let pixels: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
	let (path, path_str) = temp_tiff_path("roundtrip_rgb.tif");
	write_image_f32(&path_str, 2, 1, 3, &pixels).expect("write should succeed");
	let img = read_image_f32(&path_str).expect("read should succeed");
	assert_eq!((img.width, img.height, img.channels), (2, 1, 3));
	for (a, b) in img.pixels.iter().zip(pixels.iter()) {
		assert!((a - b).abs() < 1e-6, "wrote {b}, read back {a}");
	}
	std::fs::remove_file(&path).ok();
}

#[test]
fn image_f32_write_rejects_invalid_dimensions() {
	let err = write_image_f32("/unused.tif", 0, 1, 4, &[]).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
	let err = write_image_f32("/unused.tif", 1, -2, 4, &[]).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
}

#[test]
fn image_f32_write_rejects_unsupported_channels() {
	let pixels = vec![0.0f32; 2];
	let err = write_image_f32("/unused.tif", 1, 1, 1, &pixels).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
	let err = write_image_f32("/unused.tif", 1, 1, 2, &pixels).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
}

#[test]
fn image_f32_write_rejects_length_mismatch() {
	let err = write_image_f32("/unused.tif", 2, 2, 4, &[0.0f32; 3]).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
}

#[test]
fn image_f32_write_rejects_non_tiff_extension() {
	let pixels = vec![0.0f32; 4];
	let err = write_image_f32("/unused.png", 1, 1, 4, &pixels).unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
}

#[test]
fn image_f32_read_missing_file_errors() {
	let err = read_image_f32("/nonexistent/quercus_core-oiioutils.tif").unwrap_err();
	assert!(matches!(err, Error::Failed(_)));
}
