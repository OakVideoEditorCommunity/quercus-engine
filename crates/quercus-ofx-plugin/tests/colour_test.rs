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

//! ofxColour (M11 §4): host OCIO capability declaration, instance negotiation
//! properties, input clip colorspace (ACEScg), and the GetOutputColourspace
//! round trip (preference adoption + cross-reference resolution + output
//! write-back).
//!
//! After the single-library consolidation this connects directly through the
//! Rust API (`Host::global().create_instance`) instead of the removed
//! `quercusplugin_host_scan`/`quercusplugin_instance_create` C ABI.

mod common;

use std::sync::Arc;

use quercus_ofx_plugin::handle::RefBox;
use quercus_ofx_plugin::host::Host;
use quercus_ofx_plugin::instance::Instance;
use quercus_ofx_plugin::property::Value;

const TEST_PLUGIN_ID: &str = "org.quercus.test-plugin";

/// Scans and creates an instance; returns None when unavailable (skip).
fn scan_and_create(id: &str) -> Option<Arc<RefBox<Instance>>> {
	if common::test_plugin_scan_dir().is_none() {
		common::skip("最小测试插件未构建");
		return None;
	}
	let dir = common::test_plugin_scan_dir().unwrap();
	let host = Host::global();
	host.cache.scan_path(&dir).ok()?;
	host.create_instance(id, None).ok()
}

fn prop_str(props: &quercus_ofx_plugin::property::PropertySet, name: &str) -> Option<String> {
	match props.get(name, 0)? {
		Value::String(s) => Some(s.to_string_lossy().into_owned()),
		_ => None,
	}
}

/// Host capability declaration: OCIO mode + native config list + GL support.
#[test]
fn host_declares_ocio_capability() {
	let host = Host::global();
	assert_eq!(
		prop_str(&host.props, "OfxImageEffectPropColourManagementStyle").as_deref(),
		Some("OfxImageEffectColourManagementOCIO")
	);
	assert_eq!(
		host.props
			.dimension("OfxImageEffectPropColourManagementAvailableConfigs"),
		1,
		"可用配置至少含 ofx-native-v1.5_aces-v1.3_ocio-v2.3"
	);
	assert_eq!(
		prop_str(&host.props, "OfxImageEffectPropOpenGLRenderSupported").as_deref(),
		Some("true")
	);
}

/// Instance-time negotiation properties: style/config/OCIOConfig are written;
/// the input clip colorspace = the working space ACEScg; the output clip is not
/// yet set (pending GetOutputColourspace).
#[test]
fn instance_and_clip_colourspace_props() {
	common::with_host(|| {
		let Some(inst) = scan_and_create(TEST_PLUGIN_ID) else {
			return;
		};
		assert_eq!(
			prop_str(&inst.value.props, "OfxImageEffectPropColourManagementStyle").as_deref(),
			Some("OfxImageEffectColourManagementOCIO")
		);
		assert_eq!(
			prop_str(
				&inst.value.props,
				"OfxImageEffectPropColourManagementConfig"
			)
			.as_deref(),
			Some("ofx-native-v1.5_aces-v1.3_ocio-v2.3")
		);
		assert_eq!(
			prop_str(&inst.value.props, "OfxImageEffectPropOCIOConfig").as_deref(),
			Some("ocio://default")
		);
		let source = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Source")
			.unwrap();
		assert_eq!(
			prop_str(&source.props, "OfxImageClipPropColourspace").as_deref(),
			Some("ACEScg"),
			"输入 clip 色彩空间 = 工作空间"
		);
		let output = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Output")
			.unwrap();
		assert!(
			prop_str(&output.props, "OfxImageClipPropColourspace")
				.map(|s| s.is_empty())
				.unwrap_or(true),
			"输出 clip 色彩空间在 GetOutputColourspace 前未设"
		);
	});
}

/// GetOutputColourspace round trip: without a preference → the plugin returns
/// "OfxColourspace_Source" cross-reference → the host resolves it to the
/// Source's actual colorspace (ACEScg) and writes it back to the output.
#[test]
fn get_output_colourspace_cross_reference() {
	common::with_host(|| {
		let Some(inst) = scan_and_create(TEST_PLUGIN_ID) else {
			return;
		};
		let cs = inst
			.value
			.get_output_colourspace(&[])
			.expect("GetOutputColourspace 应成功");
		assert_eq!(cs, "ACEScg", "交叉引用 OfxColourspace_Source → ACEScg");
		inst.value.set_output_colourspace(&cs);
		let output = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Output")
			.unwrap();
		assert_eq!(
			prop_str(&output.props, "OfxImageClipPropColourspace").as_deref(),
			Some("ACEScg"),
			"输出 clip 已写回解析后的色彩空间"
		);
	});
}

/// GetOutputColourspace preference adoption: the plugin prefers the host's
/// first preferred colorspace.
#[test]
fn get_output_colourspace_preferred() {
	common::with_host(|| {
		let Some(inst) = scan_and_create(TEST_PLUGIN_ID) else {
			return;
		};
		let cs = inst
			.value
			.get_output_colourspace(&["ACEScg".to_string(), "linear".to_string()])
			.expect("GetOutputColourspace 应成功");
		assert_eq!(cs, "ACEScg", "偏好列表第一个被采纳");
	});
}

/// Cross-reference resolution edge cases: an unknown clip reference is returned unchanged; a non-cross-reference is returned unchanged.
#[test]
fn resolve_colourspace_edge_cases() {
	common::with_host(|| {
		let Some(inst) = scan_and_create(TEST_PLUGIN_ID) else {
			return;
		};
		assert_eq!(
			inst.value
				.resolve_colourspace("OfxColourspace_NoSuchClip".to_string()),
			"OfxColourspace_NoSuchClip",
			"未知 clip 引用保持原样"
		);
		assert_eq!(
			inst.value.resolve_colourspace("ACEScg".to_string()),
			"ACEScg",
			"非交叉引用原样返回"
		);
	});
}
