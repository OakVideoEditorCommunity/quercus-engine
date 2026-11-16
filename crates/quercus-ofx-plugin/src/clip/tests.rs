use super::*;

#[test]
fn f16_to_f32_covers_special_values() {
	// 常规值：1.0 = 0x3C00，-2.0 = 0xC000，0.5 = 0x3800。
	assert_eq!(f16_to_f32(0x3C00), 1.0);
	assert_eq!(f16_to_f32(0xC000), -2.0);
	assert_eq!(f16_to_f32(0x3800), 0.5);
	// 零与负零。
	assert_eq!(f16_to_f32(0x0000), 0.0);
	assert_eq!(f16_to_f32(0x8000).to_bits(), (0.0f32).to_bits() | (1 << 31));
	// 非规格数：最小正规格数 2^-14 ≈ 0.00006104；2^-24 是最小非
	// 规格数之一。
	assert!((f16_to_f32(0x0400) - 2f32.powi(-14)).abs() < 1e-12);
	assert!((f16_to_f32(0x0001) - 2f32.powi(-24)).abs() < 1e-12);
	// Inf/NaN。
	assert!(f16_to_f32(0x7C00).is_infinite() && f16_to_f32(0x7C00) > 0.0);
	assert!(f16_to_f32(0xFC00).is_infinite() && f16_to_f32(0xFC00) < 0.0);
	assert!(f16_to_f32(0x7E00).is_nan());
}

// ---- fetch_image / store_output_image / 协商矩阵 ----------------------

fn cs(s: &str) -> std::ffi::CString {
	std::ffi::CString::new(s).unwrap()
}

fn test_clip(name: &str) -> ClipInstance {
	ClipInstance::from_descriptor(&crate::descriptor::ClipDescriptor::new(name))
}

fn video_params(w: i32, h: i32, format: i32) -> crate::render::VideoParams {
	crate::render::VideoParams {
		width: w,
		height: h,
		format,
		..Default::default()
	}
}

fn frame_with(
	w: i32,
	h: i32,
	format: quercus_core::PixelFormat,
	bytes: &[u8],
) -> crate::render::Frame {
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(video_params(w, h, format as i32));
	assert!(frame.allocate(), "测试帧应可分配");
	assert_eq!(frame.data.len(), bytes.len(), "测试字节长度");
	frame.data.copy_from_slice(bytes);
	frame
}

fn f32_frame(w: i32, h: i32, fill: f32) -> crate::render::Frame {
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(video_params(w, h, crate::render::PIXEL_FORMAT_F32));
	assert!(frame.allocate());
	for c in frame.data.as_chunks_mut::<4>().0 {
		c.copy_from_slice(&fill.to_le_bytes());
	}
	frame
}

fn f32_image(w: i32, h: i32, fill: f32) -> crate::image::Image {
	let mut img = crate::image::Image::allocate(
		crate::image::BitDepth::Float,
		crate::image::Components::Rgba,
		OfxRectD {
			x1: 0.0,
			y1: 0.0,
			x2: w as f64,
			y2: h as f64,
		},
	);
	for c in img.pixels_mut().as_chunks_mut::<4>().0 {
		c.copy_from_slice(&fill.to_le_bytes());
	}
	img
}

fn connect(clip: &ClipInstance, frame: crate::render::Frame) {
	clip.set_input_texture(Some(crate::render::Texture::wrap_frame(frame)), 0.0);
}

fn samples(image: &crate::image::Image) -> Vec<f32> {
	image
		.pixels()
		.as_chunks::<4>()
		.0
		.iter()
		.map(|c| f32::from_le_bytes(*c))
		.collect()
}

/// components_from_props: all three OFX component strings hit; unknown strings
/// and non-string values return None.
#[test]
fn components_from_props_matrix() {
	use crate::property::Value;
	for (s, want) in [
		(
			"OfxImageComponentRGBA",
			Some(crate::image::Components::Rgba),
		),
		("OfxImageComponentRGB", Some(crate::image::Components::Rgb)),
		(
			"OfxImageComponentAlpha",
			Some(crate::image::Components::Alpha),
		),
		("OfxImageComponentBogus", None),
	] {
		let set = PropertySet::new();
		set.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
			Value::String(cs(s)),
		);
		assert_eq!(components_from_props(&set), want, "{s}");
	}
	let set = PropertySet::new();
	set.set_one(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, Value::Int(1));
	assert_eq!(components_from_props(&set), None);
}

/// set_video_params: every bit-depth number and component-count branch is
/// written back into the clip props (0/2/3 official mapping + Float fallback;
/// 1/3 components + RGBA fallback).
#[test]
fn set_video_params_depth_and_component_matrix() {
	use crate::property::Value;
	for (fmt, want) in [
		(0, "OfxBitDepthByte"),
		(2, "OfxBitDepthShort"),
		(3, "OfxBitDepthHalf"),
		(4, "OfxBitDepthFloat"),
		(99, "OfxBitDepthFloat"),
	] {
		let c = test_clip("Source");
		c.set_video_params(fmt, 4);
		let got = c
			.props
			.get(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0);
		assert!(
			matches!(got, Some(Value::String(s)) if s.to_string_lossy() == want),
			"format {fmt}"
		);
	}
	for (ch, want) in [
		(1, "OfxImageComponentAlpha"),
		(3, "OfxImageComponentRGB"),
		(4, "OfxImageComponentRGBA"),
		(0, "OfxImageComponentRGBA"),
	] {
		let c = test_clip("Source");
		c.set_video_params(4, ch);
		let got = c.props.get(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0);
		assert!(
			matches!(got, Some(Value::String(s)) if s.to_string_lossy() == want),
			"channels {ch}"
		);
	}
}

/// fetch_image: sub-region requests, missing input and dummy placeholders all
/// fail per contract.
#[test]
fn fetch_image_rejects_region_and_unusable_input() {
	let c = test_clip("Source");
	assert!(matches!(
		c.fetch_image(
			0.0,
			RenderScale { x: 1.0, y: 1.0 },
			Some(OfxRectD::default())
		),
		Err(crate::error::Error::Failed(_))
	));
	assert!(matches!(
		c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None),
		Err(crate::error::Error::NotFound)
	));
	c.set_input_texture(Some(crate::render::Texture::dummy()), 0.0);
	assert!(matches!(
		c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None),
		Err(crate::error::Error::NotFound)
	));
}

/// fetch_image: all four input conversion paths U8/U16/F16/F32 plus NaN/Inf
/// scrubbing.
#[test]
fn fetch_image_converts_all_input_depths() {
	// U8：归一化到 [0,1]。
	let c = test_clip("Source");
	connect(
		&c,
		frame_with(1, 1, quercus_core::PixelFormat::U8, &[0, 255, 128, 64]),
	);
	let got = samples(
		&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap(),
	);
	assert_eq!(got[0], 0.0);
	assert_eq!(got[1], 1.0);
	assert!((got[2] - 128.0 / 255.0).abs() < 1e-7);
	assert!((got[3] - 64.0 / 255.0).abs() < 1e-7);

	// U16。
	let c = test_clip("Source");
	let mut bytes = Vec::new();
	for v in [0u16, 65535, 32768, 1] {
		bytes.extend_from_slice(&v.to_le_bytes());
	}
	connect(&c, frame_with(1, 1, quercus_core::PixelFormat::U16, &bytes));
	let got = samples(
		&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap(),
	);
	assert_eq!(got[0], 0.0);
	assert_eq!(got[1], 1.0);
	assert!((got[2] - 32768.0 / 65535.0).abs() < 1e-7);
	assert!((got[3] - 1.0 / 65535.0).abs() < 1e-7);

	// F16：NaN/Inf 清洗为 0，普通值与 -0 保留。
	let c = test_clip("Source");
	let mut bytes = Vec::new();
	for v in [0x3C00u16, 0x7E00, 0x7C00, 0x8000] {
		bytes.extend_from_slice(&v.to_le_bytes());
	}
	connect(&c, frame_with(1, 1, quercus_core::PixelFormat::F16, &bytes));
	let got = samples(
		&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap(),
	);
	assert_eq!(got[0], 1.0);
	assert_eq!(got[1], 0.0, "F16 NaN 应清洗为 0");
	assert_eq!(got[2], 0.0, "F16 Inf 应清洗为 0");
	assert_eq!(got[3].to_bits(), 0x8000_0000, "-0 应保留");

	// F32：NaN/Inf 清洗为 0。
	let c = test_clip("Source");
	let mut bytes = Vec::new();
	for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.5] {
		bytes.extend_from_slice(&v.to_le_bytes());
	}
	connect(&c, frame_with(1, 1, quercus_core::PixelFormat::F32, &bytes));
	let got = samples(
		&c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
			.unwrap(),
	);
	assert_eq!(got[0], 0.0);
	assert_eq!(got[1], 0.0);
	assert_eq!(got[2], 0.0);
	assert_eq!(got[3], 1.5);
}

/// fetch_image: an unsupported input format (U10) fails explicitly.
#[test]
fn fetch_image_rejects_unsupported_format() {
	let c = test_clip("Source");
	// U10 = 4 字节/通道打包格式；管线仅支持 U8/U16/F16/F32。
	connect(
		&c,
		frame_with(1, 1, quercus_core::PixelFormat::U10, &[0u8; 16]),
	);
	let res = c.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None);
	assert!(matches!(res, Err(crate::error::Error::Failed(_))));
}

/// fetch_image: the negotiated bit depth is read from the clip props — Byte
/// lowers the depth; unknown strings and non-string values fall back to F32.
#[test]
fn fetch_image_negotiated_depth() {
	use crate::property::Value;

	let c = test_clip("Source");
	connect(&c, f32_frame(1, 1, 0.5));
	c.props.set_one(
		crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
		Value::String(cs("OfxBitDepthByte")),
	);
	let img = c
		.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
		.unwrap();
	assert_eq!(img.depth(), crate::image::BitDepth::Byte);

	let c = test_clip("Source");
	connect(&c, f32_frame(1, 1, 0.5));
	c.props.set_one(
		crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
		Value::String(cs("OfxBitDepthBogus")),
	);
	let img = c
		.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
		.unwrap();
	assert_eq!(img.depth(), crate::image::BitDepth::Float);

	let c = test_clip("Source");
	connect(&c, f32_frame(1, 1, 0.5));
	c.props
		.set_one(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, Value::Int(4));
	let img = c
		.fetch_image(0.0, RenderScale { x: 1.0, y: 1.0 }, None)
		.unwrap();
	assert_eq!(img.depth(), crate::image::BitDepth::Float);
}

/// store_output_image: the no-output/dummy/non-F32/size-mismatch error branches
/// plus the CPU whole-frame copy.
#[test]
fn store_output_image_cpu_paths() {
	let image = f32_image(2, 2, 0.25);

	// 未挂输出纹理 → NotFound。
	let c = test_clip("Output");
	assert!(matches!(
		c.store_output_image(&image),
		Err(crate::error::Error::NotFound)
	));

	// dummy 输出纹理 → NotFound。
	c.set_output_texture(Some(crate::render::Texture::dummy()), 0.0);
	assert!(matches!(
		c.store_output_image(&image),
		Err(crate::error::Error::NotFound)
	));

	// 非 F32 输出帧 → Failed。
	let c = test_clip("Output");
	c.set_output_texture(
		Some(crate::render::Texture::wrap_frame(frame_with(
			2,
			2,
			quercus_core::PixelFormat::U8,
			&[0u8; 16],
		))),
		0.0,
	);
	assert!(matches!(
		c.store_output_image(&image),
		Err(crate::error::Error::Failed(_))
	));

	// 尺寸不符 → Failed。
	let c = test_clip("Output");
	c.set_output_texture(
		Some(crate::render::Texture::wrap_frame(f32_frame(3, 1, 0.0))),
		0.0,
	);
	assert!(matches!(
		c.store_output_image(&image),
		Err(crate::error::Error::Failed(_))
	));

	// 成功：整帧拷贝回输出纹理。
	let c = test_clip("Output");
	c.set_output_texture(
		Some(crate::render::Texture::wrap_frame(f32_frame(2, 2, 0.0))),
		0.0,
	);
	let stored = c.store_output_image(&image).unwrap();
	let frame = crate::render::texture_get_frame(&stored).unwrap();
	assert_eq!(frame.data.as_slice(), image.pixels());
}

struct FakeGpu {
	uploads: std::sync::Mutex<Vec<(u64, crate::render::Frame)>>,
	fail_upload: bool,
}

impl quercus_core::backend::GpuContextLike for FakeGpu {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Cpu
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(
		&self,
		token: u64,
		frame: &crate::render::Frame,
	) -> quercus_render::error::Result<()> {
		if self.fail_upload {
			return Err(quercus_render::error::Error::State);
		}
		self.uploads
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push((token, frame.clone()));
		Ok(())
	}
	fn download(&self, _token: u64) -> quercus_render::error::Result<crate::render::Frame> {
		Ok(f32_frame(2, 2, 0.0))
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&quercus_core::color::ColorProcessor>,
	) -> quercus_render::error::Result<()> {
		Ok(())
	}
}

/// store_output_image: a GPU output texture is written back through the backend
/// upload; an upload failure is propagated.
#[test]
fn store_output_image_gpu_upload() {
	let image = f32_image(2, 2, 0.5);

	let ctx = std::sync::Arc::new(FakeGpu {
		uploads: std::sync::Mutex::new(Vec::new()),
		fail_upload: false,
	});
	let c = test_clip("Output");
	let tex = crate::render::Texture::gpu(ctx.clone(), 7, 2, 2, quercus_core::PixelFormat::F32);
	c.set_output_texture(Some(tex), 0.0);
	c.store_output_image(&image).unwrap();
	{
		let uploads = ctx.uploads.lock().unwrap();
		assert_eq!(uploads.len(), 1);
		assert_eq!(uploads[0].0, 7);
		assert_eq!(uploads[0].1.data.as_slice(), image.pixels());
	}

	// 上传失败 → Failed。
	let ctx = std::sync::Arc::new(FakeGpu {
		uploads: std::sync::Mutex::new(Vec::new()),
		fail_upload: true,
	});
	let c = test_clip("Output");
	c.set_output_texture(
		Some(crate::render::Texture::gpu(
			ctx,
			8,
			2,
			2,
			quercus_core::PixelFormat::F32,
		)),
		0.0,
	);
	assert!(matches!(
		c.store_output_image(&image),
		Err(crate::error::Error::Failed(_))
	));
}

/// frame_range: explicitly unsupported in phase 1 (the renderer bridge is
/// pending).
#[test]
fn frame_range_is_unsupported() {
	let c = test_clip("Source");
	assert!(matches!(
		c.frame_range(),
		Err(crate::error::Error::Failed(_))
	));
}
