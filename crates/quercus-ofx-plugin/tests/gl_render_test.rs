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

//! GL render bridge end-to-end: a real CGL offscreen context + output
//! texture/FBO + readback.
//!
//! Flow (plan B, see the [`quercus_ofx_plugin::gl_bridge`] module docs): scan
//! the GL variant of the minimal test plugin (org.quercus.test-plugin.gl,
//! declaring OpenGLRenderSupported="true" + F32) -> build a RenderJob with the
//! GL backend renderer (a fake `GpuContextLike`, kind=Gl) -> `render_frame`'s
//! use_opengl decision hits -> the bridge creates a CGL context + output GL
//! texture + FBO -> the plugin's render takes the GL path (attach/detach,
//! clipLoadTexture gets the real texture name, glClear clears to a known
//! color) -> the host packs the frame after a glReadPixels readback.
//!
//! The assertions are the evidence of "real GL rendering":
//! 1. the output pixels equal the plugin's GL clear color (0.1, 0.2, 0.3, 1.0)
//!    (the CPU path always fills 0.5, so the two are distinguishable);
//! 2. the gl-source-index / gl-output-index that the plugin reports through
//!    message are both non-zero real GL texture names (the CPU stub is always
//!    0);
//! 3. the gl-attached / gl-detached actions occur (attach/detach pairing).
//!
//! Gates: macOS (the Linux/Windows GL bridge is a stub) + `QUERCUS_GPU_TESTS`
//! (the GL acceptance convention; CI always skips).

mod common;

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Arc;

use quercus_core::backend::{BackendKind, GpuContextLike};
use quercus_core::texture::Frame;
use quercus_core::{PixelFormat, Rational};
use quercus_ofx_plugin::host::Host;
use quercus_ofx_plugin::render::{Renderer, Texture};

const GL_PLUGIN_ID: &str = "org.quercus.test-plugin.gl";

/// GL backend renderer (a fake `GpuContextLike`; it only declares kind=Gl so the
/// use_opengl decision hits. The GL path does not use its upload/download/blit —
/// the output is a CPU frame and rendering happens in the CGL context the bridge
/// builds itself).
struct FakeGlRenderer;

impl GpuContextLike for FakeGlRenderer {
	fn kind(&self) -> BackendKind {
		BackendKind::Gl
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(&self, _token: u64, _frame: &Frame) -> quercus_render::error::Result<()> {
		Ok(())
	}
	fn download(&self, _token: u64) -> quercus_render::error::Result<Frame> {
		Ok(Frame::new())
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

/// Capture plugin messages (the gl-test message stream).
unsafe extern "C" fn capture_msg(
	type_: *const c_char,
	message: *const c_char,
	userdata: *mut c_void,
) -> c_int {
	let v = unsafe { &mut *(userdata as *mut Vec<(String, String)>) };
	v.push((
		unsafe { CStr::from_ptr(type_) }
			.to_string_lossy()
			.into_owned(),
		unsafe { CStr::from_ptr(message) }
			.to_string_lossy()
			.into_owned(),
	));
	1
}

fn first_pixel(texture: &Texture) -> [f32; 4] {
	let Texture::Cpu(frame) = texture else {
		panic!("期望 CPU 帧");
	};
	let mut out = [0f32; 4];
	for i in 0..4 {
		out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

/// Scan + register (idempotent; returns false when the host is unavailable =
/// skip).
fn scan_and_register() -> bool {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("最小测试插件未构建");
		return false;
	};
	if Host::global().cache.scan_path(&dir).is_err() {
		common::skip("测试插件扫描失败");
		return false;
	}
	quercus_ofx_plugin::node_factory::register_plugin_nodes();
	true
}

/// GL end-to-end: the plugin's GL variant really takes the GL path and outputs a
/// known color.
#[test]
fn gl_plugin_renders_through_real_gl_path() {
	common::with_host(|| {
		// Windows 的 WGL 桥现在也是真实实现；无 GL 的机器会在下面
		// gl_available() 处跳过。
		if !common::gpu_available() {
			common::skip("GL 测试需 QUERCUS_GPU_TESTS（本机 GPU 验收；CI 一律跳过）");
			return;
		}
		if !quercus_ofx_plugin::gl_bridge::gl_available() {
			common::skip("本机无可用 GL 上下文");
			return;
		}
		if !scan_and_register() {
			return;
		}

		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let dst =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let src =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let renderer: Renderer = Arc::new(FakeGlRenderer);
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: Some(renderer),
			clear_destination: false,
			interactive: false,
		};
		let (out, _rois) = quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("GL render_frame 应成功");

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		Host::global().shutdown();

		// 1. 输出像素 = 插件 GL 清屏色（CPU 路径恒 0.5，可区分）。
		let px = first_pixel(&out);
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(px[i] - v).abs() < 1e-4,
				"GL 回读像素[{i}] = {}，期望 {v}（GL 路径未生效？）",
				px[i]
			);
		}

		// 2. 真实 GL 纹理名（非零）+ 3. attach/detach 配对。
		let msg: Vec<&str> = captured.iter().map(|(_, m)| m.as_str()).collect();
		for need in [
			"gl-attached",
			"gl-detached",
			"gl-source-index=",
			"gl-output-index=",
		] {
			assert!(
				msg.iter().any(|m| m.contains(need)),
				"应捕获到 {need}，实际 {msg:?}"
			);
		}
		for m in msg.iter().filter(|m| m.contains("gl-source-index=")) {
			let v: i32 = m.rsplit('=').next().unwrap().parse().unwrap();
			assert!(
				v > 0,
				"输入 clip 的 OpenGLTextureIndex 应为真实非零 GL 名，got {v}"
			);
		}
		for m in msg.iter().filter(|m| m.contains("gl-output-index=")) {
			let v: i32 = m.rsplit('=').next().unwrap().parse().unwrap();
			assert!(
				v > 0,
				"输出 clip 的 OpenGLTextureIndex 应为真实非零 GL 名，got {v}"
			);
		}
	});
}

/// Linux/Windows real-GPU zero copy: a real wgpu/Vulkan context + GPU
/// input/output; after the plugin's GL render the output is still the same GPU
/// token, and there is no download / glReadPixels during rendering.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn gl_plugin_renders_zero_copy() {
	common::with_host(|| {
		if !common::gpu_available() {
			common::skip("零拷贝 GL 测试需 QUERCUS_GPU_TESTS");
			return;
		}
		let Some(ctx) = quercus_core::backend::gpu_or_skip("OFX zero-copy") else {
			return;
		};
		if !ctx.supports_gl_zerocopy() {
			common::skip("当前 Vulkan 上下文不支持 OPAQUE_FD 导出");
			return;
		}
		if !quercus_ofx_plugin::gl_bridge::gl_available() {
			common::skip("本机无可用 EGL/GL 上下文");
			return;
		}
		if !scan_and_register() {
			return;
		}

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let (w, h) = (64, 64);
		let src_token = ctx.create_texture(w, h).expect("src texture");
		let mut src_frame =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32)
				.expect("source frame");
		for px in src_frame.data.chunks_exact_mut(16) {
			px[0..4].copy_from_slice(&0.05f32.to_le_bytes());
			px[4..8].copy_from_slice(&0.10f32.to_le_bytes());
			px[8..12].copy_from_slice(&0.15f32.to_le_bytes());
			px[12..16].copy_from_slice(&1.0f32.to_le_bytes());
		}
		ctx.upload(src_token, &src_frame).expect("src upload");
		let dst_token = ctx.create_texture(w, h).expect("dst texture");
		let src = Texture::gpu(ctx.clone(), src_token, w, h, PixelFormat::F32);
		let dst = Texture::gpu(ctx.clone(), dst_token, w, h, PixelFormat::F32);
		let renderer: Renderer = Arc::new(quercus_ofx_plugin::render::GlKindMarker);
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: dst.clone(),
			src: Some(src),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: Some(renderer),
			clear_destination: false,
			interactive: false,
		};

		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);
		#[cfg(target_os = "linux")]
		let fds_before = std::fs::read_dir("/proc/self/fd")
			.map(|d| d.count())
			.unwrap_or(0);
		let before = quercus_core::backend::gpu_transfer_counters();
		let readbacks_before = quercus_ofx_plugin::gl_bridge::gl_readback_count();
		let mut out = None;
		for _ in 0..25 {
			let (rendered, _rois) =
				quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
					.expect("zero-copy render_frame");
			out = Some(rendered);
		}
		let out = out.expect("at least one frame");
		#[cfg(target_os = "linux")]
		{
			let fds_after = std::fs::read_dir("/proc/self/fd")
				.map(|d| d.count())
				.unwrap_or(0);
			assert!(
				fds_after <= fds_before + 4,
				"zero-copy leaked file descriptors: {fds_before} -> {fds_after}"
			);
		}
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		let after = quercus_core::backend::gpu_transfer_counters();
		assert_eq!(
			quercus_ofx_plugin::gl_bridge::gl_readback_count(),
			readbacks_before,
			"zero-copy must not call glReadPixels"
		);
		assert_eq!(after.0, before.0, "zero-copy must not upload a CPU frame");
		assert_eq!(after.1, before.1, "zero-copy must not download a CPU frame");
		let Texture::Gpu { token, .. } = &out else {
			panic!("zero-copy output must stay a GPU texture");
		};
		assert_eq!(*token, dst_token, "output must be the destination token");

		// The output must be sampleable by the next wgpu graph node (not
		// just copyable). A GPU blit exercises the RESOURCE transition and
		// catches the UNINITIALIZED-discard bug.
		let sampled = ctx.create_texture(w, h).expect("sampled texture");
		ctx.blit(dst_token, sampled, None)
			.expect("downstream GPU sample/blit");
		ctx.wait_idle();
		let sampled_frame = ctx.download(sampled).expect("download sampled output");
		let sampled_px: Vec<f32> = sampled_frame.data[0..16]
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
			.collect();
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(sampled_px[i] - v).abs() < 1e-4,
				"downstream sample pixel[{i}] = {} expected {v}",
				sampled_px[i]
			);
		}
		ctx.destroy_texture(sampled);

		// Test-only readback to verify the plugin's GL clear color.
		let frame = ctx.download(dst_token).expect("download zero-copy output");
		let px: Vec<f32> = frame.data[0..16]
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
			.collect();
		for (i, v) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
			assert!(
				(px[i] - v).abs() < 1e-4,
				"zero-copy pixel[{i}] = {} expected {v}",
				px[i]
			);
		}

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		Host::global().shutdown();
		ctx.destroy_texture(src_token);
		ctx.destroy_texture(dst_token);
	});
}

/// CPU fallback control: with no GL renderer (renderer=None) the GL-variant
/// plugin takes the CPU path (actionRenderGL's !gl_enabled fallback to
/// actionRender) and the output is always 0.5 — distinguishable from the
/// 0.1,0.2,0.3 of [`gl_plugin_renders_through_real_gl_path`], proving the GL
/// branch is only enabled when the renderer is GL.
#[test]
fn gl_plugin_falls_back_to_cpu_without_gl_renderer() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let mut captured: Vec<(String, String)> = Vec::new();
		quercus_ofx_plugin::suites::message::set_handler(
			Some(capture_msg),
			&mut captured as *mut _ as *mut c_void,
		);

		let inst = Host::global()
			.create_instance(GL_PLUGIN_ID, None)
			.expect("GL 变体实例应可建");
		let reg = quercus_ofx_plugin::node_factory::register_instance(inst.clone());

		let dst =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let src =
			quercus_render::eval::generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32)
				.unwrap();
		let job = quercus_ofx_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: Vec::new(),
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		let (out, _rois) = quercus_ofx_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("CPU 回退 render_frame 应成功");

		quercus_ofx_plugin::node_factory::unregister_instance(reg);
		quercus_ofx_plugin::suites::message::set_handler(None, std::ptr::null_mut());
		Host::global().shutdown();

		let px = first_pixel(&out);
		assert_eq!(px, [0.5, 0.5, 0.5, 1.0], "无 GL 渲染器应走 CPU 恒 0.5");
		// 未走 GL：不应有 gl-* 消息流。
		assert!(
			!captured.iter().any(|(_, m)| m.contains("gl-output-index")),
			"CPU 回退不应产生 GL suite 消息"
		);
	});
}
