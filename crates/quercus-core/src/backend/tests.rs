use super::*;

/// Serializes the tests that assert on the process-global GPU
/// transfer counters (and install a display LUT): parallel tests would
/// otherwise see each other's transfers.
static GPU_COUNTER_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn backend_string_roundtrip() {
	for (s, kind) in [
		("auto", BackendKind::Auto),
		("metal", BackendKind::Metal),
		("vulkan", BackendKind::Vulkan),
		("opengl", BackendKind::Gl),
		("gl", BackendKind::Gl),
		("cpu", BackendKind::Cpu),
	] {
		assert_eq!(BackendKind::from_config_string(s), kind);
		assert_eq!(BackendKind::from_config_string(&s.to_uppercase()), kind);
	}
	// C++ legacy values map onto the CPU fallback (documented).
	assert_eq!(BackendKind::from_config_string("dummy"), BackendKind::Cpu);
	assert_eq!(
		BackendKind::from_config_string("multiprocess"),
		BackendKind::Cpu
	);
	// Unknown → Auto.
	assert_eq!(BackendKind::from_config_string("bogus"), BackendKind::Auto);
	assert_eq!(BackendKind::from_config_string(""), BackendKind::Auto);
	// to_config_string round-trips.
	assert_eq!(
		BackendKind::from_config_string(BackendKind::Metal.to_config_string()),
		BackendKind::Metal
	);
}

/// Restores an environment variable on drop, so a test that toggles
/// process-wide configuration cannot leak the value into parallel
/// tests or into a later test when an assertion panics.
struct EnvVarGuard {
	key: &'static str,
	saved: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
	fn set(key: &'static str, value: &str) -> Self {
		let saved = std::env::var_os(key);
		std::env::set_var(key, value);
		Self { key, saved }
	}
}

impl Drop for EnvVarGuard {
	fn drop(&mut self) {
		match self.saved.take() {
			Some(v) => std::env::set_var(self.key, v),
			None => std::env::remove_var(self.key),
		}
	}
}

#[test]
fn user_config_env_override() {
	// The tests that let the user config pick the shared context read
	// the same variable; hold their lock while it is overridden.
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let _env = EnvVarGuard::set("QUERCUS_RENDER_BACKEND", "vulkan");
	assert_eq!(BackendKind::from_user_config(), BackendKind::Vulkan);
}

#[test]
fn display_bit_depth_string_roundtrip() {
	assert_eq!(
		DisplayBitDepth::from_config_string("8"),
		DisplayBitDepth::Bit8
	);
	assert_eq!(
		DisplayBitDepth::from_config_string("10"),
		DisplayBitDepth::Bit10
	);
	// Unknown / missing → the 10-bit default.
	assert_eq!(
		DisplayBitDepth::from_config_string(""),
		DisplayBitDepth::Bit10
	);
	assert_eq!(
		DisplayBitDepth::from_config_string("bogus"),
		DisplayBitDepth::Bit10
	);
	assert_eq!(
		DisplayBitDepth::from_config_string(DisplayBitDepth::Bit8.to_config_string()),
		DisplayBitDepth::Bit8
	);
	assert_eq!(
		DisplayBitDepth::from_config_string(DisplayBitDepth::Bit10.to_config_string()),
		DisplayBitDepth::Bit10
	);
}

#[test]
fn display_bit_depth_present_formats() {
	// 10-bit → the 10-bit (RGB10A2) swapchain format, HDR float fallback.
	assert_eq!(
		DisplayBitDepth::Bit10.present_formats(),
		&[
			wgpu::TextureFormat::Rgb10a2Unorm,
			wgpu::TextureFormat::Rgba16Float
		]
	);
	// 8-bit → the engine's current default preference list.
	assert_eq!(
		DisplayBitDepth::Bit8.present_formats(),
		&[
			wgpu::TextureFormat::Bgra8Unorm,
			wgpu::TextureFormat::Rgba8Unorm
		]
	);
}

#[test]
fn cpu_backend_has_no_adapter() {
	assert!(GpuContext::create(BackendKind::Cpu).is_none());
}

/// The M2 timing guard: an engine-created context that has not touched
/// the GPU is replaced when the UI installs its adopted device, so an
/// early `shared()` cannot silently cost zero-copy present. A context
/// that has created a texture is no longer replaceable.
#[test]
fn shared_slot_replaces_an_unused_engine_context() {
	// The same shared-state lock as the other slot tests
	// (`shared_slot_host_marking_and_queries`): the slot is process-wide.
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(base) = any_gpu() else {
		return;
	};
	assert!(
		GpuContext::install_shared(Some(base.clone())),
		"the slot starts undecided"
	);
	assert!(!GpuContext::shared().unwrap().is_adopted());

	let (device, queue) = base.device_queue();
	let adopted = GpuContext::adopt(device, queue, BackendKind::Auto);
	assert!(
		GpuContext::install_shared(Some(adopted.clone())),
		"an unused engine context is replaceable by the UI device"
	);
	assert!(
		GpuContext::shared().unwrap().is_adopted(),
		"the adopted context is now shared"
	);

	// Once the context has created GPU resources it must not be
	// pulled out from under in-flight work.
	let _texture = adopted.create_texture(2, 2).unwrap();
	let (device, queue) = adopted.device_queue();
	let second = GpuContext::adopt(device, queue, BackendKind::Auto);
	assert!(
		!GpuContext::install_shared(Some(second)),
		"a used context is not replaceable"
	);
	assert!(GpuContext::shared().unwrap().is_adopted());
}

fn any_gpu() -> Option<Arc<GpuContext>> {
	// `gpu_or_skip` hard-fails when `QUERCUS_REQUIRE_GPU` is set (CI).
	gpu_or_skip("a backend GPU test")
}

#[test]
fn gpu_texture_upload_download_roundtrip() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let w = 8;
	let h = 4;
	let token = ctx.create_texture(w, h).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	// Distinct bit pattern per pixel (F32 RGBA).
	for i in 0..frame.data.len() / 4 {
		let f = (i as f32) * 0.25 + 0.125;
		let bytes = f.to_le_bytes();
		frame.data[i * 4] = bytes[0];
		frame.data[i * 4 + 1] = bytes[1];
		frame.data[i * 4 + 2] = bytes[2];
		frame.data[i * 4 + 3] = bytes[3];
	}
	ctx.upload(token, &frame).unwrap();
	let out = ctx.download(token).unwrap();
	assert_eq!(out.data.len(), frame.data.len());
	assert_eq!(out.data, frame.data, "F32 bit-exact round-trip");
	ctx.destroy_texture(token);
	assert!(!ctx.has_texture(token));
}

#[test]
fn gpu_blit_copies_pixels() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let w = 8;
	let h = 4;
	let src = ctx.create_texture(w, h).unwrap();
	let dst = ctx.create_texture(w, h).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	for i in 0..frame.data.len() / 4 {
		frame.data[i * 4] = (i % 251) as u8;
		frame.data[i * 4 + 1] = (i * 3 % 251) as u8;
		frame.data[i * 4 + 2] = (i * 7 % 251) as u8;
		frame.data[i * 4 + 3] = 255;
	}
	ctx.upload(src, &frame).unwrap();
	ctx.blit(src, dst, None).unwrap();
	let out = ctx.download(dst).unwrap();
	assert_eq!(out.data, frame.data, "plain-copy blit is pixel-exact");
	// Color-managed blit is documented-deferred.
	assert!(ctx
		.blit(
			src,
			dst,
			Some(&crate::color::ColorProcessor::pass_through())
		)
		.is_err());
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// The GPU present pass reproduces the CPU 3D-LUT evaluation without
/// leaving F32 or performing a CPU transfer.
#[test]
fn gpu_present_lut_matches_cpu_trilinear() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	// A non-trivial LUT (channel mix + gamma-ish curve).
	let lut = crate::lut::Lut3d::build(9, [0.0; 3], [1.0; 3], |c| {
		[
			(0.5 * c[0] + 0.25 * c[1] + 0.25 * c[2]).powf(0.8),
			c[1].powf(1.2),
			(0.2 * c[0] + 0.8 * c[2]).powf(0.9),
		]
	});
	ctx.set_display_lut(&lut).unwrap();
	assert!(ctx.has_display_lut());

	let w = 4;
	let h = 3;
	let src = ctx.create_texture(w, h).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	let sample = |i: usize| -> [f32; 4] {
		[
			(i as f32 * 0.13) % 1.0,
			(i as f32 * 0.29) % 1.0,
			(i as f32 * 0.47) % 1.0,
			1.0,
		]
	};
	for i in 0..(w * h) as usize {
		let px = sample(i);
		for (c, v) in px.iter().enumerate() {
			frame.data[i * 16 + c * 4..i * 16 + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	ctx.upload(src, &frame).unwrap();
	// Installation and input upload are one-time boundaries; the
	// present itself must transfer nothing to/from the CPU.
	reset_gpu_transfer_counters();
	let dst = ctx.present_texture(src).unwrap();
	assert_eq!(
		ctx.texture_handle(dst).unwrap().format(),
		wgpu::TextureFormat::Rgba32Float
	);
	assert_eq!(gpu_transfer_counters(), (0, 0), "present is GPU→GPU");
	let out = ctx.download(dst).unwrap();
	for i in 0..(w * h) as usize {
		let px = sample(i);
		let expect = lut.eval([px[0], px[1], px[2]]);
		let got = [
			f32::from_le_bytes(out.data[i * 16..i * 16 + 4].try_into().unwrap()),
			f32::from_le_bytes(out.data[i * 16 + 4..i * 16 + 8].try_into().unwrap()),
			f32::from_le_bytes(out.data[i * 16 + 8..i * 16 + 12].try_into().unwrap()),
		];
		for c in 0..3 {
			assert!(
				(got[c] - expect[c]).abs() < 5e-3,
				"px {i} c{c}: {} vs {}",
				got[c],
				expect[c]
			);
		}
	}
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

#[test]
fn planar_passes_serialize_concurrent_uniform_uploads() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let mut workers = Vec::new();
	for _ in 0..8 {
		let ctx = ctx.clone();
		workers.push(std::thread::spawn(move || {
			let pipeline = ctx.planar_yuv_pipeline().unwrap();
			let src = ctx.create_texture(2, 2).unwrap();
			let dst = ctx.create_texture(2, 2).unwrap();
			let src_view = ctx.texture_view(src).unwrap();
			let params = [0.0f32; 16];
			for _ in 0..16 {
				ctx.run_planar_views_pass(
					&[&src_view, &src_view],
					dst,
					&pipeline,
					&params,
					"quercusrender-planar-concurrency-test",
				)
				.unwrap();
			}
			ctx.destroy_texture(src);
			ctx.destroy_texture(dst);
		}));
	}
	for worker in workers {
		worker.join().expect("planar worker panicked");
	}
}

#[test]
fn gpu_present_requires_a_lut() {
	let Some(ctx) = any_gpu() else {
		return;
	};
	assert!(!ctx.has_display_lut());
	let tex = ctx.create_texture(2, 2).unwrap();
	assert!(ctx.present_texture(tex).is_err());
	ctx.destroy_texture(tex);
}

#[test]
fn gpu_copy_counters_track_cpu_transfers() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	reset_gpu_transfer_counters();
	let token = ctx.create_texture(2, 2).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = 2;
	pod.height = 2;
	frame.set_video_params(pod);
	frame.allocate();
	ctx.upload(token, &frame).unwrap();
	assert_eq!(gpu_transfer_counters(), (1, 0));
	ctx.download(token).unwrap();
	assert_eq!(gpu_transfer_counters(), (1, 1));
}

/// The GPU YUV→RGB pass reproduces the CPU reference conversion
/// (`colormath::yuv444p16_to_rgb_f32`) exactly: same coefficients, same
/// limited/full-range expansion, 4:4:4 planes sampled 1:1.
#[test]
fn gpu_yuv_to_rgb_matches_cpu_reference() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let (w, h) = (8usize, 4usize);
	let mut y_code_plane = vec![0u8; w * h * 2];
	let mut u_code_plane = vec![0u8; w * h * 2];
	let mut v_code_plane = vec![0u8; w * h * 2];
	let mut y_plane = vec![0u8; w * h * 2];
	let mut u_plane = vec![0u8; w * h * 2];
	let mut v_plane = vec![0u8; w * h * 2];
	for row in 0..h {
		for x in 0..w {
			let i = (row * w + x) * 2;
			let y_code = (4096 + (x * 8191) % 56064) as u16;
			let u_code = (32768i32 - 20000 + (row * 4000) as i32) as u16;
			let v_code = (32768i32 + (x * 5000) as i32 - 20000) as u16;
			for (code_plane, plane, code) in [
				(&mut y_code_plane, &mut y_plane, y_code),
				(&mut u_code_plane, &mut u_plane, u_code),
				(&mut v_code_plane, &mut v_plane, v_code),
			] {
				code_plane[i..i + 2].copy_from_slice(&code.to_le_bytes());
				let norm = code as f32 / 65535.0;
				plane[i..i + 2].copy_from_slice(&half::f16::from_f32(norm).to_bits().to_le_bytes());
			}
		}
	}
	let y = ctx
		.create_texture_format(
			w as i32,
			h as i32,
			1,
			wgpu::TextureFormat::R16Float,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		)
		.unwrap();
	let u = ctx
		.create_texture_format(
			w as i32,
			h as i32,
			1,
			wgpu::TextureFormat::R16Float,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		)
		.unwrap();
	let v = ctx
		.create_texture_format(
			w as i32,
			h as i32,
			1,
			wgpu::TextureFormat::R16Float,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		)
		.unwrap();
	ctx.upload_plane(y, &y_plane).unwrap();
	ctx.upload_plane(u, &u_plane).unwrap();
	ctx.upload_plane(v, &v_plane).unwrap();
	let dst = ctx.create_texture(w as i32, h as i32).unwrap();

	let mut expected = vec![0.0f32; w * h * 4];
	let mut compare = |ctx: &GpuContext,
	                   tag: &str,
	                   matrix: crate::colormath::YuvMatrix,
	                   transform: YuvTransform,
	                   full_range: bool| {
		crate::colormath::yuv444p16_to_rgb_f32(
			&y_code_plane,
			w * 2,
			&u_code_plane,
			w * 2,
			&v_code_plane,
			w * 2,
			w,
			h,
			matrix,
			full_range,
			&mut expected,
		);
		ctx.run_yuv_to_rgb(y, u, v, dst, &transform).unwrap();
		let out = ctx.download(dst).unwrap();
		for i in 0..w * h {
			let got = [
				f32::from_le_bytes(out.data[i * 16..i * 16 + 4].try_into().unwrap()),
				f32::from_le_bytes(out.data[i * 16 + 4..i * 16 + 8].try_into().unwrap()),
				f32::from_le_bytes(out.data[i * 16 + 8..i * 16 + 12].try_into().unwrap()),
			];
			for (got_c, want) in got.iter().zip(&expected[i * 4..i * 4 + 3]) {
				assert!(
					(got_c - want).abs() < 4e-3,
					"{tag}: px {i}: got {got_c} want {want}"
				);
			}
		}
	};
	use crate::colormath::YuvMatrix;
	compare(
		&ctx,
		"bt601 limited",
		YuvMatrix::Bt601,
		YuvTransform::bt601_limited(),
		false,
	);
	compare(
		&ctx,
		"bt601 full",
		YuvMatrix::Bt601,
		YuvTransform::bt601_full(),
		true,
	);
	compare(
		&ctx,
		"bt709 limited",
		YuvMatrix::Bt709,
		YuvTransform::bt709_limited(),
		false,
	);
	compare(
		&ctx,
		"bt709 full",
		YuvMatrix::Bt709,
		YuvTransform::bt709_full(),
		true,
	);
	compare(
		&ctx,
		"bt2020 limited",
		YuvMatrix::Bt2020,
		YuvTransform::bt2020_limited(),
		false,
	);
	compare(
		&ctx,
		"bt2020 full",
		YuvMatrix::Bt2020,
		YuvTransform::bt2020_full(),
		true,
	);

	ctx.destroy_texture(y);
	ctx.destroy_texture(u);
	ctx.destroy_texture(v);
	ctx.destroy_texture(dst);
}

/// The bit-depth-aware YUV transform: the 8-bit variant matches the
/// exact 8-bit code-value expansion (proved against ffmpeg/swscale on
/// real media), and the 16-bit variant is bit-identical with the
/// original `from_matrix` the M2 pass was validated with.
#[test]
fn yuv_transform_depth_matches_reference_math() {
	use crate::colormath::YuvMatrix;
	let t8 = YuvTransform::from_matrix_depth(YuvMatrix::Bt709, false, 8);
	// demo.mp4 @ (960,540): Y=145, U=117, V=164 (limited BT.709).
	let (y, u, v) = (145.0f32 / 255.0, 117.0 / 255.0, 164.0 / 255.0);
	let rgb = [
		t8.matrix[0][0] * y + t8.matrix[0][2] * v + t8.offset[0],
		t8.matrix[1][0] * y + t8.matrix[1][1] * u + t8.matrix[1][2] * v + t8.offset[1],
		t8.matrix[2][0] * y + t8.matrix[2][1] * u + t8.offset[2],
	];
	let expect = [0.8421f32, 0.5230, 0.4979];
	for c in 0..3 {
		assert!(
			(rgb[c] - expect[c]).abs() < 1e-3,
			"channel {c}: {} vs {}",
			rgb[c],
			expect[c]
		);
	}

	// The depth-16 constructor is exactly the original formula.
	for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
		for full in [false, true] {
			assert_eq!(
				YuvTransform::from_matrix(matrix, full),
				YuvTransform::from_matrix_depth(matrix, full, 16)
			);
		}
	}
}

/// The generic color LUT pass (graph `ColorTransformJob`): trilinear
/// LUT application into an `Rgba32Float` texture, GPU→GPU.
#[test]
fn gpu_apply_color_lut_matches_cpu() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let lut = crate::lut::Lut3d::build(9, [0.0; 3], [1.0; 3], |c| [c[1], c[2], c[0]]);
	let (w, h) = (4, 2);
	let src = ctx.create_texture(w, h).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	for i in 0..(w * h) as usize {
		let px = [
			(i as f32 * 0.11) % 1.0,
			(i as f32 * 0.23) % 1.0,
			(i as f32 * 0.37) % 1.0,
			1.0,
		];
		for (c, v) in px.iter().enumerate() {
			frame.data[i * 16 + c * 4..i * 16 + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	ctx.upload(src, &frame).unwrap();
	reset_gpu_transfer_counters();
	let dst = ctx.apply_color_lut(src, "test/swap", &lut).unwrap();
	assert_eq!(
		gpu_transfer_counters(),
		(1, 0),
		"the first apply uploads the LUT once and never reads back"
	);
	reset_gpu_transfer_counters();
	let dst2 = ctx.apply_color_lut(src, "test/swap", &lut).unwrap();
	assert_eq!(gpu_transfer_counters(), (0, 0), "cached apply is GPU→GPU");
	let out = ctx.download(dst).unwrap();
	let out2 = ctx.download(dst2).unwrap();
	for got in [&out, &out2] {
		for i in 0..(w * h) as usize {
			let src_px = [
				f32::from_le_bytes(frame.data[i * 16..i * 16 + 4].try_into().unwrap()),
				f32::from_le_bytes(frame.data[i * 16 + 4..i * 16 + 8].try_into().unwrap()),
				f32::from_le_bytes(frame.data[i * 16 + 8..i * 16 + 12].try_into().unwrap()),
			];
			let want = lut.eval(src_px);
			for c in 0..3 {
				let g = f32::from_le_bytes(
					got.data[i * 16 + c * 4..i * 16 + c * 4 + 4]
						.try_into()
						.unwrap(),
				);
				assert!(
					(g - want[c]).abs() < 1e-4,
					"px {i} c{c}: {g} vs {}",
					want[c]
				);
			}
		}
	}
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
	ctx.destroy_texture(dst2);
}

#[test]
fn gpu_missing_texture_errors() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	assert_eq!(
		ctx.download(999999).unwrap_err().code(),
		Error::NotFound.code()
	);
	assert!(!ctx.has_texture(999999));
	ctx.destroy_texture(999999); // idempotent
}

#[test]
fn display_renderer_cpu_path() {
	let mut r = DisplayRenderer::new(BackendKind::Cpu);
	// CPU renderer has no GPU context.
	assert!(r.init(std::ptr::null_mut()).is_err());
	let _ = &mut r;
	let mut pod = VideoParamsPod::default();
	pod.width = 4;
	pod.height = 4;
	let tex = r.create_texture(&pod, None).unwrap();
	assert!(matches!(tex, Texture::Cpu(_)));
	let frame = tex.to_frame().unwrap();
	assert_eq!(frame.width, 4);
	assert_eq!(frame.format, PixelFormat::F32);
}

#[test]
fn display_renderer_rejects_foreign_gl_context() {
	let mut r = DisplayRenderer::new(BackendKind::Gl);
	let fake = std::ptr::dangling_mut::<std::ffi::c_void>();
	assert_eq!(r.init(fake).unwrap_err().code(), Error::Invalid.code());
}

#[test]
fn display_renderer_queries_and_texture_id() {
	let r = DisplayRenderer::new(BackendKind::Gl);
	assert!(r.is_open_gl());
	assert!(!r.is_vulkan());
	assert!(!r.is_initialized());
	let r2 = DisplayRenderer::new(BackendKind::Vulkan);
	assert!(r2.is_vulkan());
	assert!(!r2.is_open_gl());

	let r3 = DisplayRenderer::new(BackendKind::Cpu);
	let mut pod = VideoParamsPod::default();
	pod.width = 4;
	pod.height = 4;
	let tex = r3.create_texture(&pod, None).unwrap();
	// CPU textures have no GPU id.
	assert_eq!(crate::backend::texture_id_of(&tex), 0);
	// Invalid params rejected.
	let mut bad = VideoParamsPod::default();
	bad.width = 0;
	assert!(r3.create_texture(&bad, None).is_err());
	// upload/download round-trip through the display renderer.
	let mut frame = Frame::new();
	frame.set_video_params(pod);
	frame.allocate();
	frame.data[0] = 0x77;
	let mut upload_target = r3.create_texture(&pod, None).unwrap();
	unsafe {
		r3.upload_texture(
			&mut upload_target,
			frame.data.as_ptr(),
			frame.linesize_bytes(),
		)
	}
	.unwrap();
	let mut buf = vec![0u8; frame.linesize_bytes() * 4];
	unsafe { r3.download_texture(&upload_target, buf.as_mut_ptr(), frame.linesize_bytes()) }
		.unwrap();
	assert_eq!(buf[0], 0x77);
	// Stride mismatch rejected.
	assert!(unsafe { r3.upload_texture(&mut upload_target, frame.data.as_ptr(), 1) }.is_err());
}

#[test]
fn cpu_blit_applies_color_and_copy() {
	let r = DisplayRenderer::new(BackendKind::Cpu);
	let mut pod = VideoParamsPod::default();
	pod.width = 2;
	pod.height = 2;
	let mut src = r.create_texture(&pod, None).unwrap();
	let mut dst = r.create_texture(&pod, None).unwrap();
	let Texture::Cpu(sf) = &mut src else {
		unreachable!()
	};
	sf.data[0] = 0x11;
	sf.data[4] = 0x22;
	// Plain copy.
	r.blit_color_managed(Some(&src), &mut dst, None).unwrap();
	let Texture::Cpu(df) = &dst else {
		unreachable!()
	};
	assert_eq!(df.data[0], 0x11);
	assert_eq!(df.data[4], 0x22);
	// Pass-through processor is a no-op.
	r.blit_color_managed(
		Some(&src),
		&mut dst,
		Some(&crate::color::ColorProcessor::pass_through()),
	)
	.unwrap();
	// Size mismatch rejected.
	let mut pod2 = VideoParamsPod::default();
	pod2.width = 3;
	pod2.height = 2;
	let mut other = r.create_texture(&pod2, None).unwrap();
	assert!(r.blit_color_managed(Some(&src), &mut other, None).is_err());
}

// ---- Branch-coverage fill-ins ------------------------------------------

#[test]
fn backend_kind_strings_cover_every_variant() {
	for (kind, s) in [
		(BackendKind::Auto, "auto"),
		(BackendKind::Metal, "metal"),
		(BackendKind::Vulkan, "vulkan"),
		(BackendKind::Gl, "opengl"),
		(BackendKind::Cpu, "cpu"),
	] {
		assert_eq!(kind.to_config_string(), s);
		assert_eq!(BackendKind::from_config_string(s), kind);
	}
	// The parser trims whitespace and folds case.
	assert_eq!(
		BackendKind::from_config_string("  VULKAN  "),
		BackendKind::Vulkan
	);
	// `Auto` and `Metal` share the same fallback list; `Cpu` has none.
	assert_eq!(
		BackendKind::Auto.wgpu_fallbacks(),
		BackendKind::Metal.wgpu_fallbacks()
	);
	assert_eq!(
		BackendKind::Vulkan.wgpu_fallbacks().first(),
		Some(&wgpu::Backends::VULKAN)
	);
	assert_eq!(
		BackendKind::Gl.wgpu_fallbacks().first(),
		Some(&wgpu::Backends::GL)
	);
	assert!(BackendKind::Cpu.wgpu_fallbacks().is_empty());
}

/// The `QUERCUS_REQUIRE_GPU` policy is a pure parser plus two handling
/// arms. Driving both from arguments (instead of flipping the real
/// environment variable) keeps this test from racing the GPU
/// acceptance tests, which read the variable from parallel threads and
/// would panic if they observed a transient `1`.
#[test]
fn require_gpu_adapter_parses_env_values() {
	for (value, expected) in [
		(None, false), // unset means "skipping is allowed"
		(Some("0"), false),
		(Some("false"), false),
		(Some("FALSE"), false),
		(Some("1"), true),
		(Some("yes"), true),
		(Some(""), true),
	] {
		assert_eq!(require_gpu_from_value(value), expected, "value {value:?}");
	}
	// The soft-skip arm logs and returns instead of failing.
	skip_or_fail_gpu_with(false, "a coverage probe");
	// The hard-fail arm panics when a GPU is required.
	let panicked = std::panic::catch_unwind(|| skip_or_fail_gpu_with(true, "a coverage probe"));
	assert!(
		panicked.is_err(),
		"a required GPU must hard-fail a missing adapter"
	);
}

#[test]
fn shared_slot_host_marking_and_queries() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	// `mark_host_context` creates the process context on first use and
	// marks it as the host's render device; the three query helpers
	// then agree on it.
	let marked = GpuContext::mark_host_context();
	assert_eq!(marked, GpuContext::shared().is_some());
	assert_eq!(GpuContext::host_gpu_installed(), marked);
	assert_eq!(GpuContext::shared_is_installed(), marked);
}

#[test]
fn future_executor_drives_pending_futures() {
	use std::future::Future;
	use std::pin::Pin;
	use std::task::{Context, Poll};

	/// Pending once (forcing the executor's yield arm), then ready;
	/// also clones the waker to exercise the no-op vtable.
	struct CloneWakerThenYield {
		yielded: bool,
	}

	impl Future for CloneWakerThenYield {
		type Output = u32;

		fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
			let waker = cx.waker().clone();
			assert!(waker.will_wake(cx.waker()));
			drop(waker);
			if self.yielded {
				Poll::Ready(42)
			} else {
				self.yielded = true;
				Poll::Pending
			}
		}
	}

	assert_eq!(
		pollster_block_on(CloneWakerThenYield { yielded: false }),
		42
	);
	// The packing helper is little-endian f32 bytes.
	assert_eq!(f32_uniform_bytes(&[1.0f32]), vec![0x00, 0x00, 0x80, 0x3f]);
}

#[test]
fn frame_from_pixels_for_upload_validates_arguments() {
	let mut frame = Frame::new();
	let pod = VideoParamsPod {
		width: 2,
		height: 2,
		..Default::default()
	};
	frame.set_video_params(pod);
	frame.allocate();
	let stride = frame.linesize_bytes();
	// Null pointer / non-positive geometry.
	assert!(unsafe { frame_from_pixels_for_upload((2, 2), std::ptr::null(), stride) }.is_err());
	assert!(unsafe { frame_from_pixels_for_upload((0, 2), frame.data.as_ptr(), stride) }.is_err());
	assert!(unsafe { frame_from_pixels_for_upload((2, -1), frame.data.as_ptr(), stride) }.is_err());
	// Stride must match the F32 line size.
	assert!(unsafe { frame_from_pixels_for_upload((2, 2), frame.data.as_ptr(), 3) }.is_err());
	// The valid shape copies the pixels.
	let built =
		unsafe { frame_from_pixels_for_upload((2, 2), frame.data.as_ptr(), stride) }.unwrap();
	assert_eq!(built.width, 2);
	assert_eq!(built.height, 2);
	assert_eq!(built.data.len(), frame.data.len());
}

#[test]
fn gpu_context_accessors_and_registry_errors() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	// Engine-created contexts host a non-CPU adapter and are unused
	// until the first texture.
	assert!(ctx.is_gpu());
	assert_ne!(ctx.kind(), BackendKind::Cpu);
	assert!(!ctx.is_adopted());
	assert!(!ctx.is_used());
	let _ = ctx.is_filterable();
	let (device, queue) = ctx.device_queue();
	assert!(Arc::strong_count(&device) >= 1);
	assert!(Arc::strong_count(&queue) >= 1);

	// Invalid create parameters are rejected before any GPU work.
	assert_eq!(
		ctx.create_texture(0, 4).unwrap_err().code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.create_texture(4, -1).unwrap_err().code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.create_texture_format(
			4,
			4,
			0,
			wgpu::TextureFormat::R8Unorm,
			wgpu::TextureUsages::TEXTURE_BINDING,
		)
		.unwrap_err()
		.code(),
		Error::Invalid.code()
	);

	let token = ctx.create_texture(4, 3).unwrap();
	assert!(ctx.is_used());
	assert_eq!(ctx.texture_size(token), Some((4, 3)));
	assert_eq!(
		ctx.texture_format(token),
		Some(wgpu::TextureFormat::Rgba32Float)
	);
	assert!(ctx.texture_handle(token).is_some());
	assert_eq!(ctx.texture_size(999999), None);
	assert_eq!(ctx.texture_format(999999), None);
	assert!(!ctx.has_texture(999999));

	// The placeholder is created once and cached.
	let placeholder = ctx.placeholder_texture().unwrap();
	assert_eq!(ctx.placeholder_texture().unwrap(), placeholder);
	assert!(ctx.has_texture(placeholder));

	// Clearing an existing texture succeeds; a missing token reports
	// NotFound.
	ctx.clear_texture(token).unwrap();
	assert_eq!(
		ctx.clear_texture(999999).unwrap_err().code(),
		Error::NotFound.code()
	);

	// The trait-object surface used by fakes/hardware import.
	let like: &dyn GpuContextLike = ctx.as_ref();
	assert_eq!(like.kind(), ctx.kind());
	assert!(like.as_any().is_some());
	assert!(like.texture_handle(placeholder).is_some());
}

#[test]
fn gpu_plane_upload_formats_and_bounds() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
	// Every single-channel format the bpp table accepts (R16Float
	// covers the 2-byte arm; the raw planar upload API exercises
	// R16Unorm/Rg16Unorm separately).
	let r8 = ctx
		.create_texture_format(4, 2, 1, wgpu::TextureFormat::R8Unorm, usage)
		.unwrap();
	ctx.upload_plane(r8, &[7u8; 8]).unwrap();
	let r16f = ctx
		.create_texture_format(4, 2, 1, wgpu::TextureFormat::R16Float, usage)
		.unwrap();
	ctx.upload_plane(r16f, &[0u8; 16]).unwrap();
	let r32f = ctx
		.create_texture_format(4, 2, 1, wgpu::TextureFormat::R32Float, usage)
		.unwrap();
	ctx.upload_plane(r32f, &[0u8; 32]).unwrap();

	// A short buffer and an unsupported format are rejected.
	assert_eq!(
		ctx.upload_plane(r8, &[0u8; 7]).unwrap_err().code(),
		Error::Invalid.code()
	);
	let rgba = ctx
		.create_texture_format(4, 2, 1, wgpu::TextureFormat::Rgba8Unorm, usage)
		.unwrap();
	assert_eq!(
		ctx.upload_plane(rgba, &[0u8; 32]).unwrap_err().code(),
		Error::Invalid.code()
	);
	// Missing tokens report NotFound.
	assert_eq!(
		ctx.upload_plane(999999, &[0u8; 8]).unwrap_err().code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.download(999999).unwrap_err().code(),
		Error::NotFound.code()
	);
	// Only Rgba32Float/Rgba16Float are readable back.
	let err = ctx.download(r8).unwrap_err();
	assert!(
		err.to_string().contains("unsupported format"),
		"the format error is named: {err}"
	);
}

/// The export pack pass quantizes into the delivery container. For 10-bit
/// delivery the half-float container must stay well inside one 10-bit
/// code (the F32→RGBA64 step in the encoder is exact relative to it).
#[test]
fn pack_output_precision_for_10_bit_delivery() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	fn srgb_reference(x: f32) -> f32 {
		let v = x.clamp(0.0, 1.0);
		if v <= 0.003_130_8 {
			12.92 * v
		} else {
			1.055 * v.powf(1.0 / 2.4) - 0.055
		}
	}
	let w = 16usize;
	let src = ctx.create_texture(w as i32, 1).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w as i32;
	pod.height = 1;
	pod.format = PixelFormat::F32 as i32;
	frame.set_video_params(pod);
	frame.allocate();
	let input: Vec<f32> = (0..w).map(|i| 0.30 + 0.02 * i as f32).collect();
	for (i, v) in input.iter().enumerate() {
		let off = i * 16;
		for c in 0..3 {
			frame.data[off + c * 4..off + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
		frame.data[off + 12..off + 16].copy_from_slice(&1.0f32.to_le_bytes());
	}
	ctx.upload(src, &frame).unwrap();
	let params = PackParams {
		matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
		transfer: PACK_TRANSFER_SRGB,
	};
	let out = ctx.pack_output_cpu(src, w as i32, 1, 16, &params).unwrap();
	assert_eq!(
		out.format,
		PixelFormat::F32,
		"16-bit container reads back as F32"
	);
	let mut worst = 0.0f32;
	for (i, v) in input.iter().enumerate() {
		let got = f32::from_le_bytes(out.data[i * 16..i * 16 + 4].try_into().unwrap());
		let want = srgb_reference(*v);
		worst = worst.max((got - want).abs());
	}
	eprintln!(
		"pack 10-bit container worst error {worst:.7} ({:.3} of a 10-bit code)",
		worst * 1023.0
	);
	// Float packing must be effectively exact; the only remaining
	// quantization is the encoder's F32→16-bit handoff (~1e-5) and the
	// final N-bit swscale output.
	assert!(
		worst <= 1e-5,
		"pack error {worst} is above the float container's rounding"
	);
	ctx.destroy_texture(src);
}

#[test]
fn gpu_lut_lifecycle_cache_and_validation() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let lut = crate::lut::Lut3d::build(3, [0.0; 3], [1.0; 3], |c| [c[1], c[2], c[0]]);
	// Installing twice replaces (and destroys) the previous LUT texture.
	ctx.set_display_lut(&lut).unwrap();
	assert!(ctx.has_display_lut());
	ctx.set_display_lut(&lut).unwrap();
	assert!(ctx.has_display_lut());

	let src = ctx.create_texture(2, 2).unwrap();
	// Two present calls: the first builds the display pipeline, the
	// second reuses the cached one.
	let presented = ctx.present_texture(src).unwrap();
	let presented2 = ctx.present_texture(src).unwrap();
	assert_ne!(presented, presented2);

	// The caller-keyed cache uploads once per key.
	reset_gpu_transfer_counters();
	let first = ctx.apply_color_lut(src, "swap", &lut).unwrap();
	assert_eq!(gpu_transfer_counters(), (1, 0));
	reset_gpu_transfer_counters();
	let again = ctx.apply_color_lut(src, "swap", &lut).unwrap();
	assert_eq!(gpu_transfer_counters(), (0, 0));
	assert_ne!(first, again);

	// Nine more keys exceed the 8-entry cache: the oldest is evicted.
	for i in 0..9 {
		ctx.apply_color_lut(src, &format!("key-{i}"), &lut).unwrap();
	}
	// The original key was evicted and re-uploads.
	reset_gpu_transfer_counters();
	ctx.apply_color_lut(src, "swap", &lut).unwrap();
	assert_eq!(gpu_transfer_counters(), (1, 0));

	// A malformed LUT is rejected before any upload.
	let mut malformed = lut.clone();
	malformed.data.truncate(3);
	assert_eq!(
		ctx.upload_lut(&malformed).unwrap_err().code(),
		Error::Invalid.code()
	);
	// An unknown source texture is NotFound.
	assert_eq!(
		ctx.apply_color_lut(999999, "swap", &lut)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
}

#[test]
fn gpu_upload_download_and_blit_validate_arguments() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let token = ctx.create_texture(4, 4).unwrap();
	let other = ctx.create_texture(4, 4).unwrap();

	let mut frame = Frame::new();
	let pod = VideoParamsPod {
		width: 4,
		height: 4,
		..Default::default()
	};
	frame.set_video_params(pod);
	frame.allocate();
	ctx.upload(token, &frame).unwrap();

	// Non-F32 input.
	let mut wrong_format = frame.clone();
	wrong_format.format = PixelFormat::U8;
	assert_eq!(
		ctx.upload(token, &wrong_format).unwrap_err().code(),
		Error::Invalid.code()
	);
	// Geometry mismatch with the texture.
	let mut wrong_size = Frame::new();
	let small = VideoParamsPod {
		width: 2,
		height: 4,
		..Default::default()
	};
	wrong_size.set_video_params(small);
	wrong_size.allocate();
	assert_eq!(
		ctx.upload(token, &wrong_size).unwrap_err().code(),
		Error::Invalid.code()
	);
	// Declared F32 geometry but truncated pixels.
	let mut truncated = frame.clone();
	truncated.data.truncate(frame.linesize_bytes() * 4 - 1);
	assert_eq!(
		ctx.upload(token, &truncated).unwrap_err().code(),
		Error::Invalid.code()
	);
	// Unknown tokens.
	assert_eq!(
		ctx.upload(999999, &frame).unwrap_err().code(),
		Error::NotFound.code()
	);

	// The trait-object upload/download/blit paths used by callers that
	// only know `GpuContextLike`.
	let like: &dyn GpuContextLike = ctx.as_ref();
	like.upload(token, &frame).unwrap();
	let out = like.download(token).unwrap();
	assert_eq!(out.data, frame.data);
	like.blit(token, other, None).unwrap();
	assert!(like
		.blit(
			token,
			other,
			Some(&crate::color::ColorProcessor::pass_through())
		)
		.is_err());
	// Unknown blit endpoints are NotFound.
	assert_eq!(
		ctx.blit(999999, other, None).unwrap_err().code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.blit(token, 999999, None).unwrap_err().code(),
		Error::NotFound.code()
	);
}

#[test]
fn gpu_compile_and_run_shader_pass() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let wgsl = r#"
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;
@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(src_tex, vec2<i32>(i32(frag.x), i32(frag.y)), 0);
}
"#;
	let program = ctx
		.compile_shader_pass("test/pass", wgsl, 1, false, false)
		.unwrap();
	assert_eq!(program.texture_count, 1);
	assert!(!program.has_uniforms);
	assert!(!program.filtering);
	// The program cache returns the same compiled pass.
	let cached = ctx
		.compile_shader_pass("test/pass", wgsl, 1, false, false)
		.unwrap();
	assert!(Arc::ptr_eq(&program, &cached));

	let src = ctx.create_texture(2, 2).unwrap();
	let dst = ctx.create_texture(2, 2).unwrap();
	// The input texture count must match the compiled layout.
	assert_eq!(
		ctx.run_shader_pass(&program, &[], &[], dst)
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	// Missing input/destination textures are NotFound.
	assert_eq!(
		ctx.run_shader_pass(&program, &[], &[999999], dst)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_shader_pass(&program, &[], &[src], 999999)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	ctx.run_shader_pass(&program, &[], &[src], dst).unwrap();

	// A uniform-declaring, filtering pass; an empty uniform block is
	// still uploaded as the minimum 16-byte binding.
	let with_uniform = r#"
struct U { v: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;
@fragment
fn main() -> @location(0) vec4<f32> { return u.v; }
"#;
	let uniform_program = ctx
		.compile_shader_pass("test/uniform", with_uniform, 1, true, true)
		.unwrap();
	assert!(uniform_program.has_uniforms);
	assert!(uniform_program.filtering);
	ctx.run_shader_pass(&uniform_program, &[], &[src], dst)
		.unwrap();
	ctx.run_shader_pass(&uniform_program, &[0u8; 16], &[src], dst)
		.unwrap();

	// A pipeline that fails device validation is a fallible result (the
	// validation error scope), not a panic: this shader parses but has
	// no `main` fragment entry point.
	let bad = ctx.compile_shader_pass(
		"test/bad",
		"@fragment fn not_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
		0,
		false,
		false,
	);
	assert!(bad.is_err(), "a missing entry point must fail the compile");
}

#[test]
fn gpu_yuv_and_planar_passes_cover_error_arms() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
	let (w, h) = (4u32, 2u32);

	// Planar pass: R16Float luma + interleaved RG8 chroma.
	let y = ctx
		.create_texture_format(w as i32, h as i32, 1, wgpu::TextureFormat::R16Float, usage)
		.unwrap();
	let uv = ctx
		.create_texture_format(w as i32, h as i32, 1, wgpu::TextureFormat::Rg8Unorm, usage)
		.unwrap();
	let dst = ctx.create_texture(w as i32, h as i32).unwrap();
	let y_data: Vec<u8> = (0..w * h)
		.flat_map(|_| half::f16::from_f32(0.5).to_bits().to_le_bytes())
		.collect();
	ctx.upload_plane(y, &y_data).unwrap();
	// upload_plane has no Rg8 entry; write the chroma plane directly
	// (neutral 128 = limited-range zero chroma).
	let uv_data = vec![128u8; (w * h * 2) as usize];
	ctx.queue.write_texture(
		wgpu::TexelCopyTextureInfo {
			texture: &ctx.texture_handle(uv).unwrap(),
			mip_level: 0,
			origin: wgpu::Origin3d::ZERO,
			aspect: wgpu::TextureAspect::All,
		},
		&uv_data,
		wgpu::TexelCopyBufferLayout {
			offset: 0,
			bytes_per_row: Some(w * 2),
			rows_per_image: None,
		},
		wgpu::Extent3d {
			width: w,
			height: h,
			depth_or_array_layers: 1,
		},
	);
	let transform = YuvTransform::bt709_limited();
	// Two runs reuse the cached planar pipeline.
	ctx.run_planar_yuv_to_rgb(y, uv, dst, &transform).unwrap();
	ctx.run_planar_yuv_to_rgb(y, uv, dst, &transform).unwrap();
	let out = ctx.download(dst).unwrap();
	assert_eq!(out.data.len(), (w * h) as usize * 16);
	assert!(
		out.data.iter().any(|&b| b != 0),
		"the planar pass writes pixels"
	);
	// Missing plane/destination tokens are NotFound.
	assert_eq!(
		ctx.run_planar_yuv_to_rgb(999999, uv, dst, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_planar_yuv_to_rgb(y, 999999, dst, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_planar_yuv_to_rgb(y, uv, 999999, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);

	// The 3-plane entry point validates every token too.
	assert_eq!(
		ctx.run_yuv_to_rgb(999999, uv, uv, dst, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_yuv_to_rgb(y, 999999, uv, dst, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_yuv_to_rgb(y, uv, 999999, dst, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_yuv_to_rgb(y, uv, uv, 999999, &transform)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
}

#[test]
fn display_renderer_gpu_texture_paths() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let mut renderer = DisplayRenderer::new(BackendKind::Cpu);
	// The tests inject the context directly; the public accessors must
	// report it.
	renderer.ctx = Some(ctx.clone());
	assert!(renderer.is_initialized());
	assert_eq!(renderer.backend(), BackendKind::Cpu);
	assert_eq!(renderer.context().map(|c| c.kind()), Some(ctx.kind()));

	let pod = VideoParamsPod {
		width: 4,
		height: 4,
		..Default::default()
	};
	let mut frame = Frame::new();
	frame.set_video_params(pod);
	frame.allocate();
	frame.data[0] = 0x5a;

	// A pixel-initialized GPU texture uploads through the constructor.
	let src = renderer
		.create_texture(&pod, Some((frame.data.as_ptr(), frame.linesize_bytes())))
		.unwrap();
	assert!(matches!(src, Texture::Gpu { .. }));
	let Texture::Gpu { token, .. } = &src else {
		unreachable!()
	};
	let token = *token;
	assert_eq!(crate::backend::texture_id_of(&src), token as i32);
	// A mismatched initializing linesize destroys the new token and
	// fails.
	assert_eq!(
		renderer
			.create_texture(&pod, Some((frame.data.as_ptr(), 1)))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);

	// upload_texture: the GPU branch requires the frame line size.
	let mut target = renderer.create_texture(&pod, None).unwrap();
	unsafe { renderer.upload_texture(&mut target, frame.data.as_ptr(), frame.linesize_bytes()) }
		.unwrap();
	let _ = unsafe { renderer.upload_texture(&mut target, frame.data.as_ptr(), 1) }.unwrap_err();
	// download_texture: the stride must match the frame line size.
	let mut buf = vec![0u8; frame.linesize_bytes() * 4];
	unsafe { renderer.download_texture(&target, buf.as_mut_ptr(), frame.linesize_bytes()) }
		.unwrap();
	assert_eq!(buf[0], 0x5a);
	let _ = unsafe { renderer.download_texture(&target, buf.as_mut_ptr(), 1) }.unwrap_err();

	// GPU→GPU blit through the renderer (same context).
	let mut other = renderer.create_texture(&pod, None).unwrap();
	renderer
		.blit_color_managed(Some(&target), &mut other, None)
		.unwrap();

	// Planar textures are never uploadable and have no display id.
	let y = ctx
		.create_texture_format(
			4,
			2,
			1,
			wgpu::TextureFormat::R8Unorm,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		)
		.unwrap();
	let uv = ctx
		.create_texture_format(
			4,
			2,
			1,
			wgpu::TextureFormat::Rg8Unorm,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		)
		.unwrap();
	let planar = crate::texture::PlanarTexture::new(
		ctx.clone(),
		crate::texture::PlanarFormat::Nv12,
		(4, 2),
		(y, uv),
		YuvTransform::bt709_limited(),
		(1, 1),
	);
	let mut planar_texture = Texture::wrap_planar(planar);
	assert_eq!(crate::backend::texture_id_of(&planar_texture), 0);
	assert!(planar_texture.is_planar());
	assert!(planar_texture.to_frame().is_err());
	let _ =
		unsafe { renderer.upload_texture(&mut planar_texture, std::ptr::null(), 0) }.unwrap_err();

	// Mixed CPU/GPU blits are rejected in both directions.
	let cpu_renderer = DisplayRenderer::new(BackendKind::Cpu);
	let mut cpu_src = cpu_renderer.create_texture(&pod, None).unwrap();
	assert_eq!(
		renderer
			.blit_color_managed(Some(&cpu_src), &mut other, None)
			.unwrap_err()
			.code(),
		crate::error::QUERCUSCORE_E_FAILED
	);
	assert!(renderer
		.blit_color_managed(Some(&target), &mut cpu_src, None)
		.is_err());

	// Cross-backend readback: CPU renderers have no GPU registry.
	let mut readback = vec![0u8; frame.linesize_bytes() * 4];
	assert_eq!(
		unsafe {
			cpu_renderer.download_from_texture(
				token as i32,
				&pod,
				readback.as_mut_ptr(),
				frame.linesize_bytes(),
			)
		}
		.unwrap_err()
		.code(),
		crate::error::QUERCUSCORE_E_FAILED
	);
	// The GPU renderer downloads by id; the stride must be F32 RGBA.
	unsafe {
		renderer.download_from_texture(
			token as i32,
			&pod,
			readback.as_mut_ptr(),
			frame.linesize_bytes(),
		)
	}
	.unwrap();
	assert_eq!(readback[0], 0x5a);
	assert!(unsafe {
		renderer.download_from_texture(token as i32, &pod, readback.as_mut_ptr(), 4)
	}
	.is_err());
}

#[test]
fn display_renderer_cpu_pixel_initialization() {
	let renderer = DisplayRenderer::new(BackendKind::Cpu);
	let pod = VideoParamsPod {
		width: 2,
		height: 2,
		..Default::default()
	};
	let mut frame = Frame::new();
	frame.set_video_params(pod);
	frame.allocate();
	frame.data[3] = 0x33;
	// CPU textures accept pixel data with a matching line size.
	let texture = renderer
		.create_texture(&pod, Some((frame.data.as_ptr(), frame.linesize_bytes())))
		.unwrap();
	let Texture::Cpu(cpu) = &texture else {
		unreachable!()
	};
	assert_eq!(cpu.data[3], 0x33);
	// A mismatched line size is rejected (nothing was allocated).
	assert_eq!(
		renderer
			.create_texture(&pod, Some((frame.data.as_ptr(), 3)))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
}

#[test]
fn display_bit_depth_from_user_config_reads_the_store() {
	// The store defaults to 10-bit when the key is missing; the call
	// must never panic and always resolve a depth.
	let depth = DisplayBitDepth::from_user_config();
	assert!(matches!(
		depth,
		DisplayBitDepth::Bit8 | DisplayBitDepth::Bit10
	));
	assert_eq!(
		depth.to_config_string(),
		if depth == DisplayBitDepth::Bit8 {
			"8"
		} else {
			"10"
		}
	);
}

/// A minimal trait-only context: the default `as_any`/`texture_handle`
/// methods must return `None` (callers then fall back to CPU delivery).
struct FakeContext;

impl GpuContextLike for FakeContext {
	fn kind(&self) -> BackendKind {
		BackendKind::Cpu
	}

	fn destroy_texture(&self, _token: u64) {}

	fn upload(&self, _token: u64, _frame: &Frame) -> Result<()> {
		Ok(())
	}

	fn download(&self, _token: u64) -> Result<Frame> {
		Ok(Frame::dummy())
	}

	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&crate::color::ColorProcessor>,
	) -> Result<()> {
		Ok(())
	}
}

#[test]
fn trait_only_context_uses_the_default_downcast_hooks() {
	let fake = FakeContext;
	assert_eq!(fake.kind(), BackendKind::Cpu);
	assert!(fake.as_any().is_none());
	assert!(fake.texture_handle(1).is_none());
	let texture = Texture::gpu(Arc::new(FakeContext), 7, 2, 2, PixelFormat::F32);
	assert_eq!(texture_id_of(&texture), 7);
	assert!(format!("{texture:?}").contains("Texture::Gpu"));
}

#[test]
fn display_renderer_init_uses_the_shared_context_for_the_user_backend() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	// A renderer whose backend equals the user's configured choice
	// adopts the process-wide shared context (one device per process).
	let kind = BackendKind::from_user_config();
	let mut renderer = DisplayRenderer::new(kind);
	let result = renderer.init(std::ptr::null_mut());
	assert_eq!(
		result.is_ok(),
		GpuContext::shared().is_some(),
		"init follows the shared slot"
	);
	assert_eq!(renderer.is_initialized(), result.is_ok());
}

/// Read one `Rgba32Float` output pixel.
fn pixel_f32(data: &[u8], index: usize) -> [f32; 4] {
	let base = index * 16;
	[
		f32::from_le_bytes(data[base..base + 4].try_into().unwrap()),
		f32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()),
		f32::from_le_bytes(data[base + 8..base + 12].try_into().unwrap()),
		f32::from_le_bytes(data[base + 12..base + 16].try_into().unwrap()),
	]
}

/// CPU reference for the matrix shader's edge-clamped bilinear sample
/// of a 16-bit unorm code plane.
fn bilinear_code_ref(codes: &[u16], width: usize, height: usize, pos: (f32, f32)) -> f32 {
	let max_x = (width - 1) as f32;
	let max_y = (height - 1) as f32;
	let x = pos.0.clamp(0.0, max_x);
	let y = pos.1.clamp(0.0, max_y);
	let x0 = x.floor() as usize;
	let y0 = y.floor() as usize;
	let fx = x - x0 as f32;
	let fy = y - y0 as f32;
	let x1 = (x0 + 1).min(width - 1);
	let y1 = (y0 + 1).min(height - 1);
	let s = |px: usize, py: usize| codes[py * width + px] as f32 / 65535.0;
	let top = s(x0, y0) * (1.0 - fx) + s(x1, y0) * fx;
	let bottom = s(x0, y1) * (1.0 - fx) + s(x1, y1) * fx;
	top * (1.0 - fy) + bottom * fy
}

/// The identity YUV→RGB matrix: the pass outputs raw plane values.
fn identity_yuv_transform() -> YuvTransform {
	YuvTransform {
		matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
		offset: [0.0, 0.0, 0.0],
	}
}

fn u16_bytes(values: &[u16]) -> Vec<u8> {
	values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn plane_row_packing_and_rgb_expansion() {
	// R8 rows with a padded source stride 5 (3 valid bytes per row).
	let src = [1u8, 2, 3, 0xFF, 0xFF, 4, 5, 6, 0xFF, 0xFF];
	let staged = repack_plane_rows(&src, 5, 3, 2).unwrap();
	assert_eq!(staged.len(), 2 * PLANE_ROW_ALIGNMENT);
	assert_eq!(&staged[..3], &[1, 2, 3]);
	assert_eq!(
		&staged[PLANE_ROW_ALIGNMENT..PLANE_ROW_ALIGNMENT + 3],
		&[4, 5, 6]
	);
	assert!(staged[3..PLANE_ROW_ALIGNMENT].iter().all(|&b| b == 0));

	// Tightly packed rows keep their bytes and get aligned rows.
	let tight = repack_plane_rows(&[7, 8, 9, 10], 0, 2, 2).unwrap();
	assert_eq!(&tight[..2], &[7, 8]);
	assert_eq!(
		&tight[PLANE_ROW_ALIGNMENT..PLANE_ROW_ALIGNMENT + 2],
		&[9, 10]
	);

	// Any row size rounds up to the 256-byte copy alignment.
	assert_eq!(aligned_plane_row_bytes(1), PLANE_ROW_ALIGNMENT);
	assert_eq!(aligned_plane_row_bytes(256), 256);
	assert_eq!(aligned_plane_row_bytes(257), 512);

	// Short buffers and invalid source strides are rejected.
	assert!(repack_plane_rows(&[0u8; 3], 0, 2, 2).is_err());
	assert!(repack_plane_rows(&[0u8; 4], 1, 2, 2).is_err());
	assert!(repack_plane_rows(&[], 0, 0, 0).is_err());

	// RGB (3-channel) -> RGBA texture expansion, alpha 1.0 per depth.
	let rgb8 = [1u8, 2, 3, 4, 5, 6];
	assert_eq!(
		expand_rgb_to_rgba(&rgb8, 0, 2, 1, PlaneStorage::U8).unwrap(),
		vec![1, 2, 3, 255, 4, 5, 6, 255]
	);
	let rgb16 = [0x34u8, 0x12, 0x78, 0x56, 0xBC, 0x9A];
	assert_eq!(
		expand_rgb_to_rgba(&rgb16, 0, 1, 1, PlaneStorage::U16).unwrap(),
		vec![0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A, 0xFF, 0xFF]
	);
	let rgb_f16 = [0u8; 6];
	assert_eq!(
		expand_rgb_to_rgba(&rgb_f16, 0, 1, 1, PlaneStorage::F16).unwrap(),
		vec![0, 0, 0, 0, 0, 0, 0x00, 0x3C]
	);
	let rgb_f32 = [0u8; 12];
	assert_eq!(
		expand_rgb_to_rgba(&rgb_f32, 0, 1, 1, PlaneStorage::F32).unwrap(),
		vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x80, 0x3F]
	);

	// Strided RGB rows expand per row; short/bad-stride data fails.
	let mut padded = vec![0u8; 8 * 2];
	padded[0..3].copy_from_slice(&[10, 11, 12]);
	padded[8..11].copy_from_slice(&[20, 21, 22]);
	let expanded = expand_rgb_to_rgba(&padded, 8, 1, 2, PlaneStorage::U8).unwrap();
	assert_eq!(expanded, vec![10, 11, 12, 255, 20, 21, 22, 255]);
	assert!(expand_rgb_to_rgba(&[1u8, 2, 3], 0, 2, 1, PlaneStorage::U8).is_err());
	assert!(expand_rgb_to_rgba(&rgb8, 2, 2, 1, PlaneStorage::U8).is_err());
}

/// The legacy NV12 upload path (tightly packed CPU bytes, unaligned
/// rows) feeds both the 1:1 native pass and the scaled adapter.
#[test]
fn gpu_planar_nv12_cpu_upload_native_and_scaled() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let (w, h) = (10usize, 6usize);
	let y_bytes: Vec<u8> = (0..w * h).map(|i| ((i * 7) % 251) as u8).collect();
	let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
	let uv_bytes = vec![128u8; cw * ch * 2];

	let (y, uv) = ctx
		.upload_planar_frame(
			PlanarFormat::Nv12,
			w as i32,
			h as i32,
			&y_bytes,
			0,
			&uv_bytes,
			0,
		)
		.unwrap();
	assert_eq!(ctx.texture_size(y), Some((w as u32, h as u32)));
	assert_eq!(ctx.texture_size(uv), Some((cw as u32, ch as u32)));
	assert_eq!(ctx.texture_format(y), Some(wgpu::TextureFormat::R8Unorm));

	let identity = identity_yuv_transform();
	let dst = ctx.create_texture(w as i32, h as i32).unwrap();
	ctx.run_planar_yuv_to_rgb(y, uv, dst, &identity).unwrap();
	let native = ctx.download(dst).unwrap();
	for row in 0..h {
		for col in 0..w {
			let i = row * w + col;
			let got = pixel_f32(&native.data, i);
			assert!((got[0] - y_bytes[i] as f32 / 255.0).abs() < 1e-6);
			assert!((got[1] - 128.0 / 255.0).abs() < 1e-6);
			assert!((got[2] - 128.0 / 255.0).abs() < 1e-6);
			assert_eq!(got[3], 1.0);
		}
	}

	// The scaled adapter at 1:1 reproduces the native pass for a
	// constant-chroma frame.
	let scaled = ctx.create_texture(w as i32, h as i32).unwrap();
	ctx.run_planar_yuv_to_rgb_scaled(
		y,
		uv,
		scaled,
		&identity,
		(w as u32, h as u32),
		(w as u32, h as u32),
	)
	.unwrap();
	let scaled_out = ctx.download(scaled).unwrap();
	for i in 0..w * h {
		let a = pixel_f32(&native.data, i);
		let b = pixel_f32(&scaled_out.data, i);
		for channel in 0..3 {
			assert!(
				(a[channel] - b[channel]).abs() < 1e-6,
				"px {i} channel {channel}: {a:?} vs {b:?}"
			);
		}
	}

	for token in [y, uv, dst, scaled] {
		ctx.destroy_texture(token);
	}
}

/// 4:2:2 U16 raw upload scaled to an arbitrary destination: verifies
/// the half-width/full-height chroma mapping, unorm 16-bit
/// preservation and the normalized bilinear filter against a CPU
/// reference.
#[test]
fn gpu_planar_yuv422p10_scaled_bilinear_matches_reference() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	if !ctx.supports_16bit_norm_planes() {
		eprintln!("16-bit normalized texture formats unavailable; skipping");
		return;
	}
	let (w, h) = (6usize, 4usize);
	let (cw, ch) = (3usize, 4usize);
	let (dw, dh) = (9usize, 7usize);

	let y_codes: Vec<u16> = (0..w * h)
		.map(|i| 1000 + ((i as u16) * 977) % 60000)
		.collect();
	let uv_codes: Vec<u16> = (0..cw * ch).map(|i| 4000 + (i as u16) * 2500).collect();
	let mut uv_bytes = Vec::with_capacity(cw * ch * 4);
	for u in &uv_codes {
		uv_bytes.extend_from_slice(&u.to_le_bytes());
		uv_bytes.extend_from_slice(&0x8000u16.to_le_bytes());
	}

	let (y, uv) = ctx
		.upload_planar_frame(
			PlanarFormat::Yuv422P10,
			w as i32,
			h as i32,
			&u16_bytes(&y_codes),
			0,
			&uv_bytes,
			0,
		)
		.unwrap();
	assert_eq!(ctx.texture_size(y), Some((w as u32, h as u32)));
	assert_eq!(ctx.texture_size(uv), Some((cw as u32, ch as u32)));
	assert_eq!(ctx.texture_format(uv), Some(wgpu::TextureFormat::Rg16Unorm));

	let dst = ctx.create_texture(dw as i32, dh as i32).unwrap();
	ctx.run_planar_yuv_to_rgb_scaled(
		y,
		uv,
		dst,
		&identity_yuv_transform(),
		(w as u32, h as u32),
		(dw as u32, dh as u32),
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	for row in 0..dh {
		for col in 0..dw {
			let fx = (col as f32 + 0.5) * (w as f32 / dw as f32) - 0.5;
			let fy = (row as f32 + 0.5) * (h as f32 / dh as f32) - 0.5;
			let cx = (col as f32 + 0.5) * (cw as f32 / dw as f32) - 0.5;
			let cy = (row as f32 + 0.5) * (ch as f32 / dh as f32) - 0.5;
			let want_y = bilinear_code_ref(&y_codes, w, h, (fx, fy));
			let want_u = bilinear_code_ref(&uv_codes, cw, ch, (cx, cy));
			let got = pixel_f32(&out.data, row * dw + col);
			assert!(
				(got[0] - want_y).abs() < 1e-4,
				"Y px ({col},{row}): got {} want {want_y}",
				got[0]
			);
			assert!(
				(got[1] - want_u).abs() < 1e-4,
				"U px ({col},{row}): got {} want {want_u}",
				got[1]
			);
			assert!((got[2] - 0.5).abs() < 2e-5, "V px ({col},{row})");
			assert_eq!(got[3], 1.0);
		}
	}

	for token in [y, uv, dst] {
		ctx.destroy_texture(token);
	}
}

/// Padded/aligned hardware plane allocations and odd visible sizes:
/// the pass samples only the visible top-left region, so padding rows
/// and columns never leak in, at native size and when scaling.
#[test]
fn gpu_planar_padded_planes_and_odd_visible_size() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
	// Visible 7x5 (odd) 4:2:0: nominal chroma 4x3. The allocations are
	// padded to 9x7 luma / 5x4 chroma, as hardware surfaces can be.
	let (vw, vh) = (7usize, 5usize);
	let (cw, ch) = (4usize, 3usize);
	let (yw, yh) = (9usize, 7usize);
	let (uw, uh) = (5usize, 4usize);
	let y = ctx
		.create_texture_format(yw as i32, yh as i32, 1, wgpu::TextureFormat::R8Unorm, usage)
		.unwrap();
	let uv = ctx
		.create_texture_format(
			uw as i32,
			uh as i32,
			1,
			wgpu::TextureFormat::Rg8Unorm,
			usage,
		)
		.unwrap();
	let mut y_data = vec![255u8; yw * yh];
	for row in 0..vh {
		for col in 0..vw {
			y_data[row * yw + col] = (col * 17 + row * 31) as u8;
		}
	}
	let mut uv_data = vec![255u8; uw * uh * 2];
	for row in 0..ch {
		for col in 0..cw {
			let offset = (row * uw + col) * 2;
			uv_data[offset] = 128;
			uv_data[offset + 1] = 64;
		}
	}
	let descriptor = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U8);
	ctx.upload_planar_matrix_planes(
		&[y, uv],
		descriptor,
		&[PlaneData::tight(&y_data), PlaneData::tight(&uv_data)],
	)
	.unwrap();
	let identity = identity_yuv_transform();

	// 1:1 using the explicit visible size: exact visible-region pixels.
	let dst = ctx.create_texture(vw as i32, vh as i32).unwrap();
	ctx.run_planar_matrix_to_rgb(
		&[y, uv],
		dst,
		descriptor,
		&identity,
		(vw as u32, vh as u32),
		(vw as u32, vh as u32),
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	for row in 0..vh {
		for col in 0..vw {
			let i = row * vw + col;
			let got = pixel_f32(&out.data, i);
			let want = y_data[row * yw + col] as f32 / 255.0;
			assert!((got[0] - want).abs() < 1e-6, "Y px ({col},{row})");
			assert!((got[1] - 128.0 / 255.0).abs() < 1e-6, "U px ({col},{row})");
			assert!((got[2] - 64.0 / 255.0).abs() < 1e-6, "V px ({col},{row})");
			assert_eq!(got[3], 1.0);
		}
	}

	// Scaled 7x5 -> 11x9: every sample must stay inside the visible
	// region (max visible Y = 226/255, visible U = 128/255); padded
	// 255 bytes would show up as brighter values.
	let scaled = ctx.create_texture(11, 9).unwrap();
	ctx.run_planar_matrix_to_rgb(
		&[y, uv],
		scaled,
		descriptor,
		&identity,
		(vw as u32, vh as u32),
		(11, 9),
	)
	.unwrap();
	let scaled_out = ctx.download(scaled).unwrap();
	let max_visible_y = 226.0 / 255.0;
	for i in 0..11 * 9 {
		let got = pixel_f32(&scaled_out.data, i);
		assert!(
			got[0] <= max_visible_y + 1e-6,
			"padding leaked into Y at px {i}: {}",
			got[0]
		);
		assert!(
			got[1] <= 128.0 / 255.0 + 1e-6,
			"padding leaked into U at px {i}: {}",
			got[1]
		);
	}

	for token in [y, uv, dst, scaled] {
		ctx.destroy_texture(token);
	}
}

/// The descriptor-driven pass for the remaining matrix cells: 4:4:4
/// planar U16, 4:2:0 semi-planar F16, interleaved RGB U8 (expanded)
/// and interleaved RGBA F32 (alpha preserved).
#[test]
fn gpu_planar_matrix_cells_444_rgb_rgba() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let identity = identity_yuv_transform();

	// 4:4:4 planar U16: three full-size planes, exact native mapping.
	if ctx.supports_16bit_norm_planes() {
		let (w, h) = (7usize, 5usize);
		let descriptor = PlanarDescriptor::yuv_planar(Subsampling::Cs444, PlaneStorage::U16);
		let y_codes: Vec<u16> = (0..w * h).map(|i| 1000 + (i as u16 % 50000)).collect();
		let u_codes: Vec<u16> = (0..w * h).map(|i| 500 + (i as u16 * 3) % 40000).collect();
		let v_codes: Vec<u16> = (0..w * h).map(|i| 30000 - (i as u16 * 2) % 20000).collect();
		let planes = ctx
			.upload_planar_matrix_frame(
				descriptor,
				w as i32,
				h as i32,
				&[
					PlaneData::tight(&u16_bytes(&y_codes)),
					PlaneData::tight(&u16_bytes(&u_codes)),
					PlaneData::tight(&u16_bytes(&v_codes)),
				],
			)
			.unwrap();
		assert_eq!(planes.len(), 3);
		let dst = ctx.create_texture(w as i32, h as i32).unwrap();
		ctx.run_planar_matrix_to_rgb_native(&planes, dst, descriptor, &identity)
			.unwrap();
		let out = ctx.download(dst).unwrap();
		for i in 0..w * h {
			let got = pixel_f32(&out.data, i);
			assert!((got[0] - y_codes[i] as f32 / 65535.0).abs() < 1e-6);
			assert!((got[1] - u_codes[i] as f32 / 65535.0).abs() < 1e-6);
			assert!((got[2] - v_codes[i] as f32 / 65535.0).abs() < 1e-6);
			assert_eq!(got[3], 1.0);
		}
		for token in planes.iter().chain([&dst]) {
			ctx.destroy_texture(*token);
		}
	}

	// 4:2:0 semi-planar F16: exact half values survive as-is.
	{
		let (w, h) = (4usize, 2usize);
		let (cw, ch) = (2usize, 1usize);
		let descriptor = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::F16);
		let half = |v: f32| half::f16::from_f32(v).to_bits().to_le_bytes();
		let y_bytes: Vec<u8> = (0..w * h).flat_map(|_| half(0.5)).collect();
		let mut uv_bytes = Vec::with_capacity(cw * ch * 4);
		for _ in 0..cw * ch {
			uv_bytes.extend_from_slice(&half(0.25));
			uv_bytes.extend_from_slice(&half(0.75));
		}
		let planes = ctx
			.upload_planar_matrix_frame(
				descriptor,
				w as i32,
				h as i32,
				&[PlaneData::tight(&y_bytes), PlaneData::tight(&uv_bytes)],
			)
			.unwrap();
		let dst = ctx.create_texture(w as i32, h as i32).unwrap();
		ctx.run_planar_matrix_to_rgb_native(&planes, dst, descriptor, &identity)
			.unwrap();
		let out = ctx.download(dst).unwrap();
		for i in 0..w * h {
			let got = pixel_f32(&out.data, i);
			assert_eq!(got, [0.5, 0.25, 0.75, 1.0], "px {i}");
		}
		for token in planes.iter().chain([&dst]) {
			ctx.destroy_texture(*token);
		}
	}

	// Interleaved RGB U8 with a padded source stride: expanded to
	// RGBA with alpha 1.0, RGB passthrough.
	{
		let (w, h) = (5usize, 3usize);
		let descriptor = PlanarDescriptor::rgb(PlaneStorage::U8);
		let stride = w * 3 + 2;
		let mut rgb = vec![0u8; stride * h];
		for row in 0..h {
			for col in 0..w {
				let i = row * w + col;
				let o = row * stride + col * 3;
				rgb[o] = (i * 3) as u8;
				rgb[o + 1] = (i * 5 + 1) as u8;
				rgb[o + 2] = (i * 7 + 2) as u8;
			}
		}
		let planes = ctx
			.upload_planar_matrix_frame(
				descriptor,
				w as i32,
				h as i32,
				&[PlaneData::strided(&rgb, stride)],
			)
			.unwrap();
		assert_eq!(planes.len(), 1);
		assert_eq!(
			ctx.texture_format(planes[0]),
			Some(wgpu::TextureFormat::Rgba8Unorm)
		);
		let dst = ctx.create_texture(w as i32, h as i32).unwrap();
		ctx.run_planar_matrix_to_rgb_native(&planes, dst, descriptor, &identity)
			.unwrap();
		let out = ctx.download(dst).unwrap();
		for row in 0..h {
			for col in 0..w {
				let i = row * w + col;
				let o = row * stride + col * 3;
				let got = pixel_f32(&out.data, i);
				assert!((got[0] - rgb[o] as f32 / 255.0).abs() < 1e-6);
				assert!((got[1] - rgb[o + 1] as f32 / 255.0).abs() < 1e-6);
				assert!((got[2] - rgb[o + 2] as f32 / 255.0).abs() < 1e-6);
				assert_eq!(got[3], 1.0);
			}
		}
		for token in planes.iter().chain([&dst]) {
			ctx.destroy_texture(*token);
		}
	}

	// Interleaved RGBA F32: direct passthrough with alpha preserved.
	{
		let (w, h) = (4usize, 2usize);
		let descriptor = PlanarDescriptor::rgba(PlaneStorage::F32);
		let mut rgba = vec![0u8; w * h * 16];
		for i in 0..w * h {
			for channel in 0..4 {
				let value = (i * 4 + channel) as f32 * 0.25;
				let base = i * 16 + channel * 4;
				rgba[base..base + 4].copy_from_slice(&value.to_le_bytes());
			}
		}
		let planes = ctx
			.upload_planar_matrix_frame(descriptor, w as i32, h as i32, &[PlaneData::tight(&rgba)])
			.unwrap();
		let dst = ctx.create_texture(w as i32, h as i32).unwrap();
		ctx.run_planar_matrix_to_rgb_native(&planes, dst, descriptor, &identity)
			.unwrap();
		let out = ctx.download(dst).unwrap();
		assert_eq!(&out.data, &rgba, "RGBA F32 is a direct passthrough");
		for token in planes.iter().chain([&dst]) {
			ctx.destroy_texture(*token);
		}
	}
}

/// Validation arms for the descriptor-driven allocation, upload and
/// pass APIs (sizes, formats, missing tokens, feature gating).
#[test]
fn gpu_planar_matrix_creation_upload_and_pass_validation() {
	let _guard = GPU_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some(ctx) = any_gpu() else {
		return;
	};
	let identity = identity_yuv_transform();

	// Legacy creation geometry (6x4 NV12 -> 3x2 chroma).
	let (y, uv) = ctx
		.create_planar_textures(PlanarFormat::Nv12, 6, 4)
		.unwrap();
	assert_eq!(ctx.texture_size(y), Some((6, 4)));
	assert_eq!(ctx.texture_size(uv), Some((3, 2)));
	assert_eq!(ctx.texture_format(uv), Some(wgpu::TextureFormat::Rg8Unorm));

	// Upload validation: short data, missing token, invalid dims.
	assert_eq!(
		ctx.upload_planar_planes(y, uv, &[0u8; 24], &[0u8; 11])
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.upload_planar_planes(999_999, uv, &[0u8; 24], &[0u8; 12])
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.create_planar_textures(PlanarFormat::Nv12, 0, 4)
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);

	// U16 storage is gated on the 16-bit norm feature.
	let u16_descriptor = PlanarDescriptor::yuv_planar(Subsampling::Cs422, PlaneStorage::U16);
	if ctx.supports_16bit_norm_planes() {
		let planes = ctx
			.create_planar_matrix_textures(u16_descriptor, 5, 3)
			.unwrap();
		assert_eq!(planes.len(), 3);
		assert_eq!(ctx.texture_size(planes[1]), Some((3, 3)));
		assert_eq!(
			ctx.texture_format(planes[1]),
			Some(wgpu::TextureFormat::R16Unorm)
		);
		// A plane without COPY_DST is rejected before any GPU write.
		let readonly = ctx
			.create_texture_format(
				3,
				3,
				1,
				wgpu::TextureFormat::R16Unorm,
				wgpu::TextureUsages::TEXTURE_BINDING,
			)
			.unwrap();
		assert_eq!(
			ctx.upload_planar_matrix_planes(
				&[planes[0], readonly],
				u16_descriptor,
				&[PlaneData::tight(&[0u8; 30]), PlaneData::tight(&[0u8; 36])],
			)
			.unwrap_err()
			.code(),
			Error::Invalid.code()
		);
		ctx.destroy_texture(readonly);
		for token in &planes {
			ctx.destroy_texture(*token);
		}

		// A frame-level upload failure destroys every token it made.
		let before = lock(&ctx.textures).len();
		assert_eq!(
			ctx.upload_planar_matrix_frame(
				u16_descriptor,
				5,
				3,
				&[PlaneData::tight(&[0u8; 30]), PlaneData::tight(&[0u8; 17])],
			)
			.unwrap_err()
			.code(),
			Error::Invalid.code()
		);
		assert_eq!(lock(&ctx.textures).len(), before, "no leaked plane tokens");
	} else {
		assert!(matches!(
			ctx.create_planar_matrix_textures(u16_descriptor, 5, 3),
			Err(Error::Failed(_))
		));
	}

	// Pass validation: plane count / sizes / formats / missing tokens.
	let descriptor = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U8);
	let dst = ctx.create_texture(6, 4).unwrap();
	// The legacy planes are 4:2:0; claiming 4:2:2 makes the chroma
	// plane dimensions mismatch and is rejected.
	let wrong_subsampling = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs422, PlaneStorage::U8);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[y, uv], dst, wrong_subsampling, &identity, (6, 4), (6, 4),)
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[999_999, uv], dst, descriptor, &identity, (6, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[y], dst, descriptor, &identity, (6, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[y, uv], dst, descriptor, &identity, (7, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[y, uv], dst, descriptor, &identity, (6, 4), (7, 4))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	// A non-Rgba32Float destination is rejected.
	let wrong_dst = ctx
		.create_texture_format(
			6,
			4,
			1,
			wgpu::TextureFormat::Rgba16Float,
			wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
		)
		.unwrap();
	assert_eq!(
		ctx.run_planar_matrix_to_rgb(&[y, uv], wrong_dst, descriptor, &identity, (6, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.run_planar_matrix_to_rgb_native(&[y, uv], wrong_dst, descriptor, &identity)
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	// Missing tokens are NotFound even before size checks.
	assert_eq!(
		ctx.run_planar_matrix_to_rgb_native(&[999_999], dst, descriptor, &identity)
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);
	assert_eq!(
		ctx.run_planar_yuv_to_rgb_scaled(y, uv, dst, &identity, (0, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::Invalid.code()
	);
	assert_eq!(
		ctx.run_planar_yuv_to_rgb_scaled(999_999, uv, dst, &identity, (6, 4), (6, 4))
			.unwrap_err()
			.code(),
		Error::NotFound.code()
	);

	for token in [y, uv, dst, wrong_dst] {
		ctx.destroy_texture(token);
	}
}
