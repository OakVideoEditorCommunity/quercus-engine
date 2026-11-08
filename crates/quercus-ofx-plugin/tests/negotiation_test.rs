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

//! Negotiation semantics: the clip-preference negotiation matrix, RoD/RoI, isIdentity and
//! field passthrough. This was historically the worst debugging area; each case is matched against HostSupport behavior (line numbers noted in comments).
//!
//! The minimal test plugin always declares RGBA/F32, implements RoD (1920×1080) and RoI
//! (writing the region back), isIdentity always non-passthrough, no field — the matrix cases that need plugin variants
//! (fallback chain, identity passthrough, field) are marked `// TODO(plugin)` and
//! land with the M11 §2.4 test-plugin variants.
//!
//! After the single-lib unification the tests go through the Rust API directly (`Host::global().create_instance`) instead of
//! the removed `quercusplugin_host_init`/`quercusplugin_instance_create` C ABI.

mod common;

use std::sync::Arc;

use quercus_ofx_plugin::handle::RefBox;
use quercus_ofx_plugin::host::Host;
use quercus_ofx_plugin::instance::Instance;

const TEST_PLUGIN_ID: &str = "org.quercus.test-plugin";

/// Scan and create an instance; returns None (skip) when unavailable.
fn create_instance() -> Option<Arc<RefBox<Instance>>> {
	if common::test_plugin_scan_dir().is_none() {
		common::skip("最小测试插件未构建");
		return None;
	}
	let dir = common::test_plugin_scan_dir().unwrap();
	let host = Host::global();
	let _ = host.cache.scan();
	host.cache.scan_path(&dir).ok()?;
	host.create_instance(TEST_PLUGIN_ID, None).ok()
}

/// Negotiation order: the test plugin declares RGBA/F32 and frame rate 24 — the negotiated result must be
/// adopted verbatim (HS: ofxhImageEffect.cpp:1686-1740's write-back path).
/// The full combination matrix (RGBA/RGB/Alpha × Float/Half/Byte) needs plugin variants:
/// `// TODO(plugin)`.
#[test]
fn clip_preferences_component_depth_matrix() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().expect("协商应成功");
		assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		assert_eq!(prefs.frame_rate, 24.0);
		// 回灌：clip 实例属性带上了协商结果（clipGetPropertySet 读）。
		let output = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Output")
			.unwrap();
		assert_eq!(
			output
				.props
				.get("OfxImageEffectPropComponents", 0)
				.map(|v| format!("{v:?}")),
			Some("String(\"OfxImageComponentRGBA\")".into())
		);
		Host::global().shutdown();
	});
}

/// Bit-depth fallback chain (Half→Byte): the host currently only announces F32 support and the plugin declares
/// F32 — passthrough. The plugin-side fallback-chain variants are `// TODO(plugin)`.
#[test]
fn bit_depth_fallback_chain() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().unwrap();
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		Host::global().shutdown();
	});
}

/// RoD: the plugin implements getRegionOfDefinition → use the plugin value (1920×1080,
/// HS: ofxhImageEffect.cpp:1087-1130's out-args read).
/// The default semantics ("union of input RoDs when not implemented") need plugin variants: `// TODO(plugin)`.
#[test]
fn region_of_definition_default_and_override() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let rod = inst
			.value
			.get_region_of_definition(0.0, quercus_ofx_plugin::instance::RenderScale { x: 1.0, y: 1.0 })
			.expect("getRoD 应成功");
		assert_eq!((rod.x1, rod.y1, rod.x2, rod.y2), (0.0, 0.0, 1920.0, 1080.0));
		Host::global().shutdown();
	});
}

/// RoI: the plugin writes the region back verbatim into Source's per-clip property
/// ("OfxImageEffectPropRegionOfInterest_Source"); the returned value matches the
/// requested region (HS: getRegionsOfInterest's per-clip prefix).
#[test]
fn regions_of_interest_writeback() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let region = quercus_ofx_plugin::instance::OfxRectD {
			x1: 10.0,
			y1: 20.0,
			x2: 100.0,
			y2: 200.0,
		};
		let rois = inst
			.value
			.get_regions_of_interest(
				0.0,
				quercus_ofx_plugin::instance::RenderScale { x: 1.0, y: 1.0 },
				region,
			)
			.expect("getRoI 应成功");
		assert_eq!(rois.len(), 2, "clips 数与 RoI 数一致");
		// RoI 只对输入 clip 有意义：Source 回写 region；Output 是
		// 宿主预定义的默认（零），渲染驱动只用输入条目。
		let source = rois
			.get(
				inst.value
					.clips
					.iter()
					.position(|c| c.name == "Source")
					.unwrap(),
			)
			.unwrap();
		assert_eq!(
			(source.x1, source.y1, source.x2, source.y2),
			(10.0, 20.0, 100.0, 200.0)
		);
		Host::global().shutdown();
	});
}

/// isIdentity: the test plugin is always non-passthrough → None (render must actually run).
/// The passthrough short-circuit (Some) needs plugin variants: `// TODO(plugin)`.
#[test]
fn is_identity_shortcircuit() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let identity = inst.value.is_identity(1.5).expect("isIdentity 应成功");
		assert!(identity.is_none(), "测试插件恒非透传：{identity:?}");
		Host::global().shutdown();
	});
}

/// field passthrough: the plugin-declared field order (OfxFieldNone) is passed through verbatim into
/// the negotiated result (the host only passes it through, never processes it). The multi-value matrix is `// TODO(plugin)`.
#[test]
fn field_passthrough() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().unwrap();
		assert_eq!(prefs.field, "OfxFieldNone", "插件声明的 field 应透传");
		Host::global().shutdown();
	});
}

/// begin/endSequenceRender: the paired calls succeed; after begin the timeline
/// getTimeBounds returns that sequence range (the sequence_range wiring).
#[test]
fn sequence_render_brackets() {
	common::with_host(|| {
		use quercus_ofx_plugin::instance::OfxRangeD;
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let range = OfxRangeD {
			min: 10.0,
			max: 200.0,
		};
		assert!(inst.value.begin_sequence_render(range).is_ok());
		// begin 后实例记录该序列范围（timeline 上下文的 getTimeBounds
		// 读它）。
		let stored = *inst.value.sequence_range.lock().unwrap();
		let stored = stored.expect("begin_sequence_render 必须登记范围");
		assert_eq!((stored.min, stored.max), (10.0, 200.0));
		assert!(inst.value.end_sequence_render(range).is_ok());
		// end 后清除。
		assert!(inst.value.sequence_range.lock().unwrap().is_none());

		Host::global().shutdown();
	});
}
