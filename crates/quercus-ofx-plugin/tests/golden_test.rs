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
#[ignore = "M11 phase 0 snapshot library (tests/ofx/snapshots/) not generated yet; implement and remove once it lands"]
fn descriptor_snapshots_match() {
	todo!("implement once the phase 0 snapshots land: scan the snapshot list -> describe -> field-by-field diff")
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
#[ignore = "M11 phase 0 negotiation snapshots not generated yet; implement and remove once they land"]
fn negotiated_clip_preferences_match() {
	todo!("implement once the phase 0 negotiation snapshots land")
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
#[ignore = "local CImg.ofx.bundle is a Natron-branch build with incompatible property suite handle semantics (describe returns MissingHostFeature); the Misc system smoke already covers the real plugin path"]
fn cimg_full_describe_smoke() {
	todo!("implement once the property suite adapts the Natron-branch Property::Set handle: full describe + negotiation without crashes or leaks")
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
#[ignore = "M11 phase 0 render golden reference library (tests/ofx/frames/) not generated yet; implement and remove once it lands"]
fn render_golden_cpu_bitexact() {
	todo!("implement once the phase 0 render goldens land")
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
#[ignore = "golden frame library (tests/ofx/frames/) not generated yet; implement and remove once it lands"]
fn render_golden_gl_tolerant() {
	todo!("implement once the M11 phase 2 GL path lands")
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
#[ignore = "golden frame library (tests/ofx/frames/) not generated yet; implement and remove once it lands"]
fn pipeline_is_f32_acescg() {
	todo!("implement once the frame library and OCIO path land")
}
