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

//! Optional mask clip fixture (`org.quercus.test-plugin.mask`): descriptor flags and the
//! `OfxImageClipPropConnected` lifecycle (attach, clear, reattach) driven by synthetic textures.
//!
//! Scope: fixture and connection-level coverage only (no GPU, no third-party plugin). The
//! fixture's documented behaviour is: mask connected → output RGB = Source RGB × Mask red,
//! alpha from Source; mask absent → Source passes through. Render-driver mask mapping and
//! CPU/GL mask rendering belong to the follow-up mask integration work.

mod common;

use std::sync::Arc;

use quercus_ofx_plugin::handle::RefBox;
use quercus_ofx_plugin::host::Host;
use quercus_ofx_plugin::instance::Instance;
use quercus_ofx_plugin::property::Value;
use quercus_ofx_plugin::render::{Frame, Texture, VideoParams, PIXEL_FORMAT_F32};

const MASK_PLUGIN_ID: &str = "org.quercus.test-plugin.mask";
const PLAIN_PLUGIN_ID: &str = "org.quercus.test-plugin";

fn create_instance(id: &str) -> Option<Arc<RefBox<Instance>>> {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("最小测试插件未构建");
		return None;
	};
	let host = Host::global();
	let _ = host.cache.scan();
	host.cache.scan_path(&dir).ok()?;
	host.create_instance(id, None).ok()
}

/// A synthetic CPU texture (no GPU involved).
fn synthetic_texture(w: i32, h: i32) -> Texture {
	let mut frame = Frame::new();
	frame.set_video_params(VideoParams {
		width: w,
		height: h,
		format: PIXEL_FORMAT_F32,
		..Default::default()
	});
	assert!(frame.allocate(), "合成帧应可分配");
	Texture::wrap_frame(frame)
}

fn int_prop(clip: &quercus_ofx_plugin::clip::ClipInstance, name: &str) -> Option<i32> {
	match clip.props.get(name, 0)? {
		Value::Int(v) => Some(v),
		_ => None,
	}
}

/// The Mask clip is optional and flagged as a mask; Source/Output are neither.
#[test]
fn mask_clip_descriptor_flags() {
	common::with_host(|| {
		let Some(inst) = create_instance(MASK_PLUGIN_ID) else {
			Host::global().shutdown();
			return;
		};
		let mask = inst.value.clips.iter().find(|c| c.name == "Mask");
		let mask = mask.expect("fixture 应暴露 Mask clip");
		assert_eq!(int_prop(mask, "OfxImageClipPropOptional"), Some(1));
		assert_eq!(int_prop(mask, "OfxImageClipPropIsMask"), Some(1));
		for name in ["Source", "Output"] {
			let clip = inst.value.clips.iter().find(|c| c.name == name).unwrap();
			assert_eq!(int_prop(clip, "OfxImageClipPropIsMask"), Some(0), "{name}");
		}
		Host::global().shutdown();
	});
}

/// With no texture the mask is disconnected; attach → connected; clear → disconnected;
/// reattach → connected again.
#[test]
fn mask_clip_connection_lifecycle() {
	common::with_host(|| {
		let Some(inst) = create_instance(MASK_PLUGIN_ID) else {
			Host::global().shutdown();
			return;
		};
		let mask = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Mask")
			.expect("fixture 应暴露 Mask clip");
		let connected = || int_prop(mask, "OfxImageClipPropConnected");

		assert_eq!(connected(), Some(0), "初始应未连接");
		assert!(mask.input_texture_value().is_none());

		mask.set_input_texture(Some(synthetic_texture(4, 4)), 0.0);
		assert_eq!(connected(), Some(1), "挂接纹理后应已连接");
		assert!(mask.input_texture_value().is_some());

		mask.set_input_texture(None, 0.0);
		assert_eq!(connected(), Some(0), "清除后应回到未连接");
		assert!(mask.input_texture_value().is_none());

		mask.set_input_texture(Some(synthetic_texture(8, 2)), 0.0);
		assert_eq!(connected(), Some(1), "重新挂接后应再次连接");
		Host::global().shutdown();
	});
}

/// Mask connection state is per-clip: toggling the mask never touches Source, and the
/// existing plain test plugin keeps exposing no Mask clip.
#[test]
fn mask_connection_is_independent_and_plain_plugin_unchanged() {
	common::with_host(|| {
		let Some(inst) = create_instance(MASK_PLUGIN_ID) else {
			Host::global().shutdown();
			return;
		};
		let source = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Source")
			.unwrap();
		let mask = inst.value.clips.iter().find(|c| c.name == "Mask").unwrap();
		mask.set_input_texture(Some(synthetic_texture(4, 4)), 0.0);
		assert_eq!(int_prop(source, "OfxImageClipPropConnected"), Some(0));
		assert_eq!(int_prop(mask, "OfxImageClipPropConnected"), Some(1));
		Host::global().shutdown();

		let Some(plain) = create_instance(PLAIN_PLUGIN_ID) else {
			Host::global().shutdown();
			return;
		};
		let mut names: Vec<_> = plain.value.clips.iter().map(|c| c.name.as_str()).collect();
		names.sort_unstable();
		assert_eq!(names, ["Output", "Source"]);
		Host::global().shutdown();
	});
}
