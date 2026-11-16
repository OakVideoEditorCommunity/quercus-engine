use super::*;
use crate::image::{BitDepth, Components};
use crate::render::VideoParams;

/// Skips when GL is unavailable (headless environment without a GPU);
/// returns a guard when available.
///
/// macOS real-CGL acceptance shares the same gate as the integration GL
/// tests: CI is headless and tests run in parallel, so by default only an
/// availability probe is done; set `QUERCUS_GPU_TESTS=1` to run real GL
/// cases (same convention as
/// `crates/quercus-ofx-plugin/tests/gl_render_test.rs`).
fn maybe_gl() -> Option<GlGuard> {
	#[cfg(target_os = "macos")]
	if std::env::var_os("QUERCUS_GPU_TESTS").is_none() {
		println!("SKIP: 真实 GL 桥用例需 QUERCUS_GPU_TESTS（本机 GPU 验收；CI 一律跳过）");
		return None;
	}
	match acquire() {
		Ok(g) => Some(g),
		Err(e) => {
			println!("SKIP: GL 上下文不可用：{e}");
			None
		}
	}
}

fn f32_params(w: i32, h: i32) -> VideoParams {
	let mut p = VideoParams::default();
	p.width = w;
	p.height = h;
	p.format = crate::render::PIXEL_FORMAT_F32;
	p
}

fn u8_params(w: i32, h: i32) -> VideoParams {
	let mut p = VideoParams::default();
	p.width = w;
	p.height = h;
	p.format = crate::render::PIXEL_FORMAT_U8;
	p
}

/// Real GL context evidence: a non-empty version string is available after
/// acquire.
#[test]
fn gl_context_is_real() {
	let Some(_g) = maybe_gl() else {
		return;
	};
	let v = gl_version_string();
	assert!(v.is_some(), "真实 CGL 上下文应返回 GL_VERSION");
	let v = v.unwrap();
	// macOS 上 Apple 的 OpenGL 实现构建于 Metal 之上，GL_VERSION
	// 形如 "4.1 Metal - 90.5"（无头/有头皆然）——只断言非空。
	assert!(!v.trim().is_empty(), "GL_VERSION 应非空，got {v:?}");
	assert!(gl_available(), "上下文可用后 gl_available 应为 true");
}

/// Context singleton and cross-thread current serialization: two threads
/// acquiring in turns both get a real context (no crash, same version).
#[test]
fn gl_context_thread_serialized() {
	let mut handles = Vec::new();
	for _ in 0..4 {
		handles.push(std::thread::spawn(|| {
			let Some(_g) = maybe_gl() else {
				return;
			};
			let v = gl_version_string().unwrap_or_default();
			assert!(!v.is_empty());
		}));
	}
	for h in handles {
		h.join().unwrap();
	}
}

/// Output texture + FBO + clear + readback round-trip (F32 target): clear
/// to a known color → read back with glReadPixels, assert pixel by pixel.
#[test]
fn fbo_clear_readback_f32() {
	let Some(_g) = maybe_gl() else {
		return;
	};
	let (w, h) = (4i32, 2i32);
	let params = f32_params(w, h);
	let tex = create_output_texture(w, h, &params).expect("输出纹理应可建");
	assert!(tex > 0, "GL 纹理名必须为真实非零 GL 名，got {tex}");
	let fbo = create_fbo(tex, w, h).expect("FBO 应完整");
	bind_fbo(fbo);
	set_viewport(w, h);
	gl_clear_color(0.1, 0.2, 0.3, 1.0);
	gl_clear();
	let img = read_pixels_to_image(w, h, &params).expect("回读应成功");
	delete_fbo(fbo);
	delete_gl_texture(tex);
	assert_eq!(img.components(), Components::Rgba);
	assert_eq!(img.depth(), BitDepth::Float);
	assert_eq!(img.bounds().x2, w as f64);
	assert_eq!(img.bounds().y2, h as f64);
	let px = &img.pixels()[0..16];
	let px_f: Vec<f32> = px
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	assert!((px_f[0] - 0.1).abs() < 1e-6, "R={}", px_f[0]);
	assert!((px_f[1] - 0.2).abs() < 1e-6, "G={}", px_f[1]);
	assert!((px_f[2] - 0.3).abs() < 1e-6, "B={}", px_f[2]);
	assert!((px_f[3] - 1.0).abs() < 1e-6, "A={}", px_f[3]);
	// 整帧同色（clear 覆盖全 FBO）。
	let all: Vec<f32> = img.pixels()[..]
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	assert!(
		all.chunks(4).all(|px| (px[0] - 0.1).abs() < 1e-6),
		"整帧应为清屏色"
	);
}

/// U8 target: clear → GL_RGBA8 storage → read back and normalize to F32
/// (within tolerance).
#[test]
fn fbo_clear_readback_u8() {
	let Some(_g) = maybe_gl() else {
		return;
	};
	let (w, h) = (3i32, 3i32);
	let params = u8_params(w, h);
	let tex = create_output_texture(w, h, &params).expect("U8 输出纹理应可建");
	let fbo = create_fbo(tex, w, h).expect("FBO 应完整");
	bind_fbo(fbo);
	set_viewport(w, h);
	gl_clear_color(0.5, 0.25, 0.0, 1.0);
	gl_clear();
	let img = read_pixels_to_image(w, h, &params).expect("U8 回读应成功");
	delete_fbo(fbo);
	delete_gl_texture(tex);
	let px_f: Vec<f32> = img.pixels()[0..16]
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	// 0.5*255=127.5→128（round），128/255≈0.50196；0.25→64→0.25098。
	assert!((px_f[0] - 0.502).abs() < 0.01, "R={}", px_f[0]);
	assert!((px_f[1] - 0.251).abs() < 0.01, "G={}", px_f[1]);
	assert!(px_f[2].abs() < 0.01, "B={}", px_f[2]);
	assert!((px_f[3] - 1.0).abs() < 0.01, "A={}", px_f[3]);
}

/// Input texture upload: create an RGBA32F texture + upload known pixels,
/// read the texture content back (via FBO attachment + glReadPixels) and
/// compare with the source pixels.
#[test]
fn input_texture_upload_roundtrip() {
	let Some(_g) = maybe_gl() else {
		return;
	};
	let (w, h) = (2i32, 2i32);
	let mut px = Vec::new();
	for i in 0..(w * h) as usize {
		for c in 0..4 {
			px.extend_from_slice(&((i * 4 + c) as f32 * 0.1).to_ne_bytes());
		}
	}
	let tex = create_input_texture(w, h, &px).expect("输入纹理应可建");
	let params = f32_params(w, h);
	let fbo = create_fbo(tex, w, h).expect("FBO 应完整");
	bind_fbo(fbo);
	let img = read_pixels_to_image(w, h, &params).expect("回读应成功");
	delete_fbo(fbo);
	delete_gl_texture(tex);
	let got: Vec<f32> = img.pixels()[0..w as usize * h as usize * 16]
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	let want: Vec<f32> = px
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	// 源是逐像素相异序列：read_pixels_to_image 垂直翻转（GL 左下 →
	// 帧左上）后，最终图像第 r 行 = 源第 (h-1-r) 行。
	let stride = w as usize * 4;
	for (i, g) in got.iter().enumerate() {
		let row = i / stride;
		let inv_row = h as usize - 1 - row;
		let expected = want[inv_row * stride + i % stride];
		assert!(
			(g - expected).abs() < 1e-6,
			"pixel {i}: got {g} want {expected}"
		);
	}
}

/// Linux real-GPU zero-copy: an `OPAQUE_FD` exported by wgpu/Vulkan is
/// imported by GL as the same GPU memory, and the Vulkan/wgpu side can
/// still use it after the plugin writes the GL texture.
///
/// This case only verifies import/render/readback; the readback is a test
/// assertion, not part of the production path.
#[cfg(target_os = "linux")]
#[test]
fn import_opaque_fd_texture_roundtrip() {
	if std::env::var_os("QUERCUS_GPU_TESTS").is_none() {
		println!("SKIP: 零拷贝 GL 用例需 QUERCUS_GPU_TESTS");
		return;
	}
	let Some(ctx) = quercus_core::backend::gpu_or_skip("GL opaque-fd import") else {
		return;
	};
	if !ctx.supports_gl_zerocopy() {
		println!("SKIP: context 不支持 Vulkan OPAQUE_FD 导出");
		return;
	}
	let _g = match acquire() {
		Ok(g) => g,
		Err(e) => {
			println!("SKIP: GL 上下文不可用: {e}");
			return;
		}
	};
	let w = 32;
	let h = 32;
	let token = ctx.create_texture(w, h).expect("exportable texture");
	ctx.clear_texture(token).expect("clear output");
	ctx.wait_idle();
	let export = ctx.export_gl_memory(token).expect("opaque fd export");
	let params = f32_params(w, h);
	let gl = import_memory_texture(&export, w, h, &params).expect("GL import");
	let fbo = create_fbo(gl.name, w, h).expect("FBO");
	bind_fbo(fbo);
	set_viewport(w, h);
	gl_clear_color(0.25, 0.5, 0.75, 1.0);
	gl_clear();
	gl_finish();
	let img = read_pixels_to_image(w, h, &params).expect("readback");
	let px: Vec<f32> = img.pixels()[0..16]
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	assert!((px[0] - 0.25).abs() < 1e-3, "R={}", px[0]);
	assert!((px[1] - 0.5).abs() < 1e-3, "G={}", px[1]);
	assert!((px[2] - 0.75).abs() < 1e-3, "B={}", px[2]);
	assert!((px[3] - 1.0).abs() < 1e-3, "A={}", px[3]);
	delete_fbo(fbo);
	delete_memory_texture(&gl);
	// Cross-API visibility: the plugin's GL writes must be visible to
	// wgpu/Vulkan through the same allocation.
	ctx.wait_idle();
	let frame = ctx.download(token).expect("vulkan download");
	let vk_px: Vec<f32> = frame.data[0..16]
		.chunks_exact(4)
		.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
		.collect();
	assert!((vk_px[0] - 0.25).abs() < 1e-3, "Vulkan R={}", vk_px[0]);
	assert!((vk_px[1] - 0.5).abs() < 1e-3, "Vulkan G={}", vk_px[1]);
	assert!((vk_px[2] - 0.75).abs() < 1e-3, "Vulkan B={}", vk_px[2]);
	ctx.destroy_texture(token);
}

/// The reverse direction: pixels uploaded by wgpu are visible in GL
/// through the same `OPAQUE_FD`.
#[cfg(target_os = "linux")]
#[test]
fn import_opaque_fd_reads_wgpu_content() {
	if std::env::var_os("QUERCUS_GPU_TESTS").is_none() {
		println!("SKIP: 零拷贝 GL 用例需 QUERCUS_GPU_TESTS");
		return;
	}
	let Some(ctx) = quercus_core::backend::gpu_or_skip("GL opaque-fd read") else {
		return;
	};
	if !ctx.supports_gl_zerocopy() {
		println!("SKIP: context 不支持 Vulkan OPAQUE_FD 导出");
		return;
	}
	let _g = match acquire() {
		Ok(g) => g,
		Err(e) => {
			println!("SKIP: GL 上下文不可用: {e}");
			return;
		}
	};
	let (w, h) = (16, 16);
	let token = ctx.create_texture(w, h).expect("texture");
	let mut frame = crate::render::Frame::new();
	frame.set_video_params(f32_params(w, h));
	assert!(frame.allocate());
	for px in frame.data.chunks_exact_mut(16) {
		px[0..4].copy_from_slice(&0.2f32.to_le_bytes());
		px[4..8].copy_from_slice(&0.4f32.to_le_bytes());
		px[8..12].copy_from_slice(&0.6f32.to_le_bytes());
		px[12..16].copy_from_slice(&1.0f32.to_le_bytes());
	}
	ctx.upload(token, &frame).expect("upload");
	ctx.wait_idle();
	let export = ctx.export_gl_memory(token).expect("export");
	let params = f32_params(w, h);
	let gl = import_memory_texture(&export, w, h, &params).expect("GL import");
	let fbo = create_fbo(gl.name, w, h).expect("FBO");
	bind_fbo(fbo);
	gl_memory_barrier();
	gl_finish();
	let img = read_pixels_to_image(w, h, &params).expect("readback");
	let px: Vec<f32> = img.pixels()[0..16]
		.chunks_exact(4)
		.map(|c| f32::from_ne_bytes(c.try_into().unwrap()))
		.collect();
	assert!((px[0] - 0.2).abs() < 1e-4, "R={}", px[0]);
	assert!((px[1] - 0.4).abs() < 1e-4, "G={}", px[1]);
	assert!((px[2] - 0.6).abs() < 1e-4, "B={}", px[2]);
	delete_fbo(fbo);
	delete_memory_texture(&gl);
	ctx.destroy_texture(token);
}

/// Re-entrant acquire on the same thread: acquiring again while an outer
/// guard is held does not deadlock and GL calls still work; after the
/// nested guard drops the context stays current (the outer one is still
/// valid), and after the outer drop current is cleared. Simulates the
/// interact draw "one acquire covering FBO setup → plugin drawing →
/// readback" scenario.
#[test]
fn acquire_is_reentrant_on_same_thread() {
	let Some(_outer) = maybe_gl() else {
		return;
	};
	// 嵌套 acquire（interact::draw 内部路径）。
	let inner = acquire().expect("嵌套 acquire 应成功（同线程重入）");
	let v1 = gl_version_string();
	assert!(v1.is_some(), "嵌套 guard 期间 GL 应仍 current");
	drop(inner);
	// 外层仍持有：GL 仍 current。
	let v2 = gl_version_string();
	assert!(v2.is_some(), "嵌套 guard drop 后外层仍应 current");
	// 再套一层（深度 2）验证深度计数不串。
	{
		let inner2 = acquire().expect("二次嵌套应成功");
		assert!(gl_version_string().is_some());
		drop(inner2);
	}
}
