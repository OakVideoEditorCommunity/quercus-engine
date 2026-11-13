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

//! Golden master: descriptor-snapshot diffing and rendered-frame comparison
//! (M11 §2.2/§2.3).
//!
//! The snapshots were captured from the **current C++ implementation** and
//! checked in during phase 0 (tests/ofx/snapshots/ and tests/ofx/frames/);
//! the tests in this file assert that the Rust implementation matches them
//! field-by-field/pixel-by-pixel. **The phase-0 infrastructure is not in
//! place yet** (tests/ofx/ is empty) — any test that depends on the
//! snapshot/frame libraries is `#[ignore]`d; remove the annotation and
//! complete the assertions once they land (see the notes on
//! [`descriptor_snapshots_match`]). Environments without a real bundle (CI)
//! skip this whole file.
//!
//! No test in this file currently runs: the render/GL/colour pipelines are
//! implemented and covered by `gl_render_test.rs`/`colour_test.rs`; what is
//! missing here is only the golden reference material.

mod common;

/// Descriptor snapshots: for every plugin in the snapshot list, a field-level
/// diff — identifier, version, context set, parameter matrix (name/type/
/// default/labels/hint/parent group/secret/display range/choice
/// labels+values+sorting result), clip table. Any diff is printed as a
/// unified diff to help locate it.
///
/// # Why ignored (M11 phase-0 infrastructure missing)
///
/// `tests/ofx/snapshots/` (the descriptor JSON library captured from the C++
/// implementation) has not been generated yet. Once the snapshots land,
/// remove this annotation and implement: scan the snapshot list → describe
/// each plugin → compare field by field against the JSON. There are no
/// snapshots to compare yet, so running it now would always fail spuriously.
#[test]
#[ignore = "M11 0 期快照库（tests/ofx/snapshots/）尚未生成；落地后实现并摘除"]
fn descriptor_snapshots_match() {
	todo!("0 期快照落地后实现：扫描快照清单 → describe → 逐字段 diff")
}

/// Clip negotiation snapshots: after getClipPreferences, the output
/// components/bit depth/pixel aspect/frame rate/field must match the
/// snapshot (the main regression guard for implicit behavior).
///
/// # Why ignored
///
/// Same as [`descriptor_snapshots_match`]: the negotiation snapshots
/// (including the post-getClipPreferences results) live in
/// `tests/ofx/snapshots/` and have not been generated.
#[test]
#[ignore = "M11 0 期协商快照尚未生成；落地后实现并摘除"]
fn negotiated_clip_preferences_match() {
	todo!("0 期协商快照落地后实现")
}

/// CImg full-plugin smoke: describe + negotiate every plugin without
/// crashing or leaking (no snapshot comparison, only robustness — covers
/// quirks outside the snapshot set).
///
/// # Why ignored (host-compatibility gap of the local CImg bundle)
///
/// The locally installed CImg.ofx.bundle is a Natron-branch build: during
/// describe it writes the private property
/// `NatronOfxImageEffectPropDeprecated`, and the `Property::Set` handle
/// semantics of its OFX support are incompatible with this crate's
/// tagged-handle convention (the plugin-side property write fails before
/// reaching this host's property suite → describe returns
/// kOfxStatErrMissingHostFeature). The end-to-end load/create-instance/
/// negotiate path for real plugins is covered by the Misc.ofx.bundle smoke
/// (the standard openfx-misc build). CImg compatibility is a host-fidelity
/// gap for M11 §3.5 "CImg full describe/negotiate smoke" — remove this
/// annotation once the property suite adapts the plugin-side
/// `Property::Set` handle.
#[test]
#[ignore = "本机 CImg.ofx.bundle 为 Natron 分支构建，属性套件句柄语义不兼容（describe 返回 MissingHostFeature）；Misc 系统冒烟已覆盖真实插件路径"]
fn cimg_full_describe_smoke() {
	todo!("属性套件适配 Natron 分支的 Property::Set 句柄后实现：全量 describe + 协商不崩、不泄漏")
}

/// CPU render golden: the representative effect set matches frame-by-frame
/// SHA256 (bit-exact). Input frames are a synthetic gradient + color blocks
/// (the deterministic generator lives in common).
///
/// # Why ignored
///
/// `tests/ofx/frames/` (the EXR + SHA256 reference library captured from the
/// C++ implementation) has not been generated yet—the render pipeline itself
/// is a real implementation (render_driver + quercus-render eval); only the
/// golden reference material is missing. Implement and remove this annotation
/// once the reference library lands.
#[test]
#[ignore = "M11 0 期渲染 golden 参照库（tests/ofx/frames/）尚未生成；落地后实现并摘除"]
fn render_golden_cpu_bitexact() {
	todo!("0 期渲染 golden 落地后实现")
}

/// GL render golden: 1e-4 tolerance (driver differences); skipped on
/// machines with no GPU.
///
/// # Why ignored
///
/// The GL render path is implemented (suites/gl_render.rs, macOS CGL + CPU
/// fallback); only the golden frame library is missing (tests/ofx/frames/ not
/// generated). Implement and remove this annotation once the frame library
/// lands.
#[test]
#[ignore = "golden 帧库（tests/ofx/frames/）未生成；落地后实现并摘除"]
fn render_golden_gl_tolerant() {
	todo!("M11 第 2 期 GL 路径落地后实现")
}

/// F32+ACEScg pipeline assertion: golden frames use the F32 pixel format and,
/// after the OCIO display transform, stay within tolerance of the reference
/// values (the regression guard against the colour pipeline being swapped for
/// 8-bit somewhere).
///
/// # Why ignored
///
/// Same as [`render_golden_cpu_bitexact`]: only the golden reference frame
/// library is missing — the F32 pipeline and the OCIO/ofxColour path are both
/// implemented (covered by colour_test.rs).
#[test]
#[ignore = "golden 帧库（tests/ofx/frames/）未生成；落地后实现并摘除"]
fn pipeline_is_f32_acescg() {
	todo!("帧库与 OCIO 链路落地后实现")
}
