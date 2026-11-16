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

//! Colorimetry for the ACEScg + F32 pipeline: the one place the matrices
//! and transfer functions live.
//!
//! Pipeline contract (project-properties driven):
//!
//! * input nodes decode any source colorspace into the **working space**
//!   (ACEScg linear, F32) via [`decode_to_working`];
//! * everything between input and output stays ACEScg + F32;
//! * the output node converts to the project's **output colorspace**
//!   ([`acescg_to_output`]) for export, and the display path converts to
//!   the display target ([`working_to_display_target`]) for presentation.
//!
//! All matrices are derived from the published primaries/white points at
//! build time (no hand-typed composite matrices to drift), with Bradford
//! chromatic adaptation between differing white points (D65 sources → the
//! ACES D60-ish white of AP1). Unit tests pin the results against the
//! published ACES transform values.
//!
//! ## Performance
//!
//! The per-pixel transforms run over full frames on hot paths (decode,
//! the app-side output node). libm `powf` costs ~15 ns per channel —
//! ~375 ms per 1080p frame for a matrix+OETF pass — so the transfer
//! functions are evaluated through 4096-entry LUTs with linear
//! interpolation (max error ≈ 2e-5, far below 10-bit quantization) and
//! large frames are split across scoped threads by row band.

/// A 3x3 row-major matrix.
use crate::cpuparallel;
#[cfg(test)]
use std::sync;
pub type Mat3 = [[f32; 3]; 3];

/// An xy chromaticity pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xy {
	/// The x chromaticity coordinate.
	pub x: f32,
	/// The y chromaticity coordinate.
	pub y: f32,
}

impl Xy {
	/// Build a chromaticity coordinate pair from CIE 1931 xy values.
	///
	/// `x` and `y` are the normalized chromaticity coordinates (for colors
	/// inside the spectral locus `x + y <= 1`); `y` is a chromaticity
	/// component, not luminance. Returns the `Xy` value consumed by
	/// [`Primaries`] and the [`WHITE_D65`]/[`WHITE_ACES`] constants.
	pub const fn new(x: f32, y: f32) -> Self {
		Self { x, y }
	}

	/// The XYZ tristimulus at unit luminance (Y = 1).
	fn xyz_unit_luminance(self) -> [f32; 3] {
		[self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
	}
}

/// The RGB primaries of a colorspace.
#[derive(Clone, Copy, Debug)]
pub struct Primaries {
	/// Red primary.
	pub red: Xy,
	/// Green primary.
	pub green: Xy,
	/// Blue primary.
	pub blue: Xy,
	/// White point.
	pub white: Xy,
}

/// D65 standard illuminant.
pub const WHITE_D65: Xy = Xy::new(0.3127, 0.3290);

/// The ACES white point (≈D60; the exact ACES specification values).
pub const WHITE_ACES: Xy = Xy::new(0.32168, 0.33767);

/// sRGB / Rec.709 primaries (D65).
pub const PRIMARIES_SRGB: Primaries = Primaries {
	red: Xy::new(0.640, 0.330),
	green: Xy::new(0.300, 0.600),
	blue: Xy::new(0.150, 0.060),
	white: WHITE_D65,
};

/// Display P3 primaries (D65).
pub const PRIMARIES_DISPLAY_P3: Primaries = Primaries {
	red: Xy::new(0.680, 0.320),
	green: Xy::new(0.265, 0.690),
	blue: Xy::new(0.150, 0.060),
	white: WHITE_D65,
};

/// Rec.2020 / BT.2020 primaries (D65).
pub const PRIMARIES_BT2020: Primaries = Primaries {
	red: Xy::new(0.708, 0.292),
	green: Xy::new(0.170, 0.797),
	blue: Xy::new(0.131, 0.046),
	white: WHITE_D65,
};

/// ACES AP1 primaries (the ACEScg gamut; ACES white).
pub const PRIMARIES_AP1: Primaries = Primaries {
	red: Xy::new(0.713, 0.293),
	green: Xy::new(0.165, 0.830),
	blue: Xy::new(0.128, 0.044),
	white: WHITE_ACES,
};

// ---------------------------------------------------------------------------
// Matrix plumbing
// ---------------------------------------------------------------------------

/// Multiply two row-major 3x3 matrices (`a * b`).
///
/// `a` is the left matrix and `b` the right matrix, both `[[f32; 3]; 3]`
/// in row-major order. Returns the product; used by the RGB-to-XYZ and
/// chromatic-adaptation builders. No normalization or validation.
fn mat_mul(a: Mat3, b: Mat3) -> Mat3 {
	let mut out = [[0.0f32; 3]; 3];
	for r in 0..3 {
		for c in 0..3 {
			out[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
		}
	}
	out
}

/// Multiply a row-major 3x3 matrix by a length-3 column vector.
///
/// `m` is the matrix and `v` the vector; each output component is the dot
/// product of one matrix row with `v`. Used for XYZ conversions and white
/// point scaling.
fn mat_vec(m: Mat3, v: [f32; 3]) -> [f32; 3] {
	[
		m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
		m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
		m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
	]
}

/// Invert a row-major 3x3 matrix via its adjugate and determinant.
///
/// `m` is the matrix to invert. Returns the inverse. The determinant is not
/// checked for zero or near-zero: a singular input produces non-finite
/// entries instead of `None`.
fn mat_inv(m: Mat3) -> Mat3 {
	let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
		- m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
		+ m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
	let inv_det = 1.0 / det;
	[
		[
			(m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
			(m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
			(m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
		],
		[
			(m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
			(m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
			(m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
		],
		[
			(m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
			(m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
			(m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
		],
	]
}

fn mat_diag(d: [f32; 3]) -> Mat3 {
	[[d[0], 0.0, 0.0], [0.0, d[1], 0.0], [0.0, 0.0, d[2]]]
}

/// The Bradford cone-response matrix (the ICC-recommended adaptation used
/// by the ACES transforms as well).
const BRADFORD_CONE: Mat3 = [
	[0.8951, 0.2664, -0.1614],
	[-0.7502, 1.7135, 0.0367],
	[0.0389, -0.0685, 1.0296],
];

/// Build the Bradford chromatic-adaptation matrix between two white points.
///
/// `src_white` and `dst_white` are CIE 1931 xy white points (for example
/// [`WHITE_D65`] and [`WHITE_ACES`]); both must have `y != 0`. Returns the
/// row-major 3x3 matrix that maps source-white XYZ to destination-white
/// XYZ; [`rgb_to_rgb_matrix`] applies it only when the white points differ.
pub fn chromatic_adaptation(src_white: Xy, dst_white: Xy) -> Mat3 {
	let cone = BRADFORD_CONE;
	let cone_inv = mat_inv(cone);
	let src = mat_vec(cone, src_white.xyz_unit_luminance());
	let dst = mat_vec(cone, dst_white.xyz_unit_luminance());
	let scale = [dst[0] / src[0], dst[1] / src[1], dst[2] / src[2]];
	mat_mul(mat_mul(cone_inv, mat_diag(scale)), cone)
}

/// The RGB→CIE XYZ matrix of a colorspace (columns are the primaries'
/// tristimulus scaled so the white point maps to unit luminance).
pub fn rgb_to_xyz_matrix(primaries: Primaries) -> Mat3 {
	let raw = [
		[
			primaries.red.xyz_unit_luminance()[0],
			primaries.green.xyz_unit_luminance()[0],
			primaries.blue.xyz_unit_luminance()[0],
		],
		[
			primaries.red.xyz_unit_luminance()[1],
			primaries.green.xyz_unit_luminance()[1],
			primaries.blue.xyz_unit_luminance()[1],
		],
		[
			primaries.red.xyz_unit_luminance()[2],
			primaries.green.xyz_unit_luminance()[2],
			primaries.blue.xyz_unit_luminance()[2],
		],
	];
	let white_xyz = primaries.white.xyz_unit_luminance();
	let s = mat_vec(mat_inv(raw), white_xyz);
	mat_mul(raw, mat_diag(s))
}

/// Composite RGB(src) → RGB(dst) matrix: src → XYZ, chromatic adaptation
/// when the white points differ, XYZ → dst.
pub fn rgb_to_rgb_matrix(src: Primaries, dst: Primaries) -> Mat3 {
	let to_xyz = rgb_to_xyz_matrix(src);
	let from_xyz = mat_inv(rgb_to_xyz_matrix(dst));
	let middle = if src.white != dst.white {
		chromatic_adaptation(src.white, dst.white)
	} else {
		[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
	};
	mat_mul(mat_mul(from_xyz, middle), to_xyz)
}

/// Apply a row-major 3x3 color matrix to the RGB components of one pixel.
///
/// `m` is the matrix; `px` is a slice whose first three elements are R, G
/// and B (any further elements, such as alpha in an RGBA row, are left
/// untouched). The transform is in place and requires `px.len() >= 3`.
#[inline]
fn apply_mat(m: Mat3, px: &mut [f32]) {
	let r = px[0];
	let g = px[1];
	let b = px[2];
	px[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
	px[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
	px[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
}

// ---------------------------------------------------------------------------
// Working / output spaces (the project-properties vocabulary)
// ---------------------------------------------------------------------------

/// The pipeline working colorspace (project property). ACEScg is the
/// industrial scene-linear working space; `SrgbLegacy` keeps the old
/// display-referred sRGB passthrough for compatibility / debugging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WorkingColorSpace {
	/// ACEScg (AP1 primaries, linear transfer, F32) — the default.
	#[default]
	AcesCg,
	/// Legacy: gamma-encoded sRGB values pass through untransformed.
	SrgbLegacy,
}

impl WorkingColorSpace {
	/// Parse a persisted working-colorspace project setting.
	///
	/// `value` is matched case-insensitively: `"srgb"`, `"srgb_legacy"` or
	/// `"legacy"` select [`WorkingColorSpace::SrgbLegacy`]; any other value
	/// (including the canonical `"acescg"`) falls back to the default
	/// [`WorkingColorSpace::AcesCg`]. Counterpart of
	/// [`WorkingColorSpace::as_setting`].
	pub fn from_setting(value: &str) -> Self {
		match value.to_ascii_lowercase().as_str() {
			"srgb" | "srgb_legacy" | "legacy" => WorkingColorSpace::SrgbLegacy,
			_ => WorkingColorSpace::AcesCg,
		}
	}

	/// The value persisted into the project settings.
	pub fn as_setting(self) -> &'static str {
		match self {
			WorkingColorSpace::AcesCg => "acescg",
			WorkingColorSpace::SrgbLegacy => "srgb_legacy",
		}
	}
}

/// The output gamut (project property / export target).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputGamut {
	/// Rec.709 / sRGB primaries (D65).
	#[default]
	Srgb,
	/// Display P3 primaries (D65).
	DisplayP3,
	/// Rec.2020 / BT.2020 primaries (D65).
	Bt2020,
}

impl OutputGamut {
	/// Parse a persisted output-gamut project setting.
	///
	/// `value` is matched case-insensitively: `"displayp3"`/`"p3"` select
	/// [`OutputGamut::DisplayP3`], `"bt2020"`/`"rec2020"`/`"2020"` select
	/// [`OutputGamut::Bt2020`]; any other value falls back to
	/// [`OutputGamut::Srgb`]. Counterpart of [`OutputGamut::as_setting`].
	pub fn from_setting(value: &str) -> Self {
		match value.to_ascii_lowercase().as_str() {
			"displayp3" | "p3" => OutputGamut::DisplayP3,
			"bt2020" | "rec2020" | "2020" => OutputGamut::Bt2020,
			_ => OutputGamut::Srgb,
		}
	}

	/// The value persisted into the project settings.
	pub fn as_setting(self) -> &'static str {
		match self {
			OutputGamut::Srgb => "srgb",
			OutputGamut::DisplayP3 => "displayp3",
			OutputGamut::Bt2020 => "bt2020",
		}
	}

	/// Return the RGB primaries and white point for this output gamut.
	///
	/// `Srgb` maps to [`PRIMARIES_SRGB`], `DisplayP3` to
	/// [`PRIMARIES_DISPLAY_P3`] and `Bt2020` to [`PRIMARIES_BT2020`]. The
	/// result feeds the gamut matrices in [`rgb_to_rgb_matrix`] and
	/// [`acescg_to_output`].
	pub fn primaries(self) -> Primaries {
		match self {
			OutputGamut::Srgb => PRIMARIES_SRGB,
			OutputGamut::DisplayP3 => PRIMARIES_DISPLAY_P3,
			OutputGamut::Bt2020 => PRIMARIES_BT2020,
		}
	}

	/// The H.273 / `AVCOL_PRI_*` code point for container tagging (the
	/// mov `colr` atom / H.264-HEVC VUI).
	pub fn av_color_primaries(self) -> i32 {
		match self {
			OutputGamut::Srgb => 1,       // BT.709
			OutputGamut::DisplayP3 => 12, // SMPTE EG 432-1 (Display P3)
			OutputGamut::Bt2020 => 9,     // BT.2020
		}
	}

	/// The H.273 / `AVCOL_SPC_*` matrix-coefficients code point for the
	/// encoder's RGB→YCbCr conversion tag.
	pub fn av_color_space(self) -> i32 {
		match self {
			OutputGamut::Srgb | OutputGamut::DisplayP3 => 1, // BT.709
			OutputGamut::Bt2020 => 9,                        // BT.2020 NCL
		}
	}
}

/// The output transfer characteristic (project property / export target).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputTransfer {
	/// The sRGB piecewise transfer (IEC 61966-2-1).
	#[default]
	Srgb,
	/// Pure gamma 2.2 power function.
	Gamma22,
	/// SMPTE ST 2084 (PQ), normalized so 1.0 = 10 000 nits.
	Pq,
	/// ARIB STD-B67 Hybrid Log-Gamma.
	Hlg,
}

impl OutputTransfer {
	/// Parse a persisted output-transfer project setting.
	///
	/// `value` is matched case-insensitively: `"gamma22"`/`"2.2"` select
	/// [`OutputTransfer::Gamma22`], `"pq"`/`"st2084"` select
	/// [`OutputTransfer::Pq`], `"hlg"` selects [`OutputTransfer::Hlg`]; any
	/// other value falls back to [`OutputTransfer::Srgb`]. Counterpart of
	/// [`OutputTransfer::as_setting`].
	pub fn from_setting(value: &str) -> Self {
		match value.to_ascii_lowercase().as_str() {
			"gamma22" | "2.2" => OutputTransfer::Gamma22,
			"pq" | "st2084" => OutputTransfer::Pq,
			"hlg" => OutputTransfer::Hlg,
			_ => OutputTransfer::Srgb,
		}
	}

	/// The value persisted into the project settings.
	pub fn as_setting(self) -> &'static str {
		match self {
			OutputTransfer::Srgb => "srgb",
			OutputTransfer::Gamma22 => "gamma22",
			OutputTransfer::Pq => "pq",
			OutputTransfer::Hlg => "hlg",
		}
	}

	/// The H.273 / `AVCOL_TRC_*` code point for container tagging.
	pub fn av_color_trc(self) -> i32 {
		match self {
			OutputTransfer::Srgb => 13,   // IEC 61966-2-1 (sRGB)
			OutputTransfer::Gamma22 => 4, // pure gamma 2.2
			OutputTransfer::Pq => 16,     // SMPTE ST 2084 (PQ)
			OutputTransfer::Hlg => 18,    // ARIB STD-B67 (HLG)
		}
	}
}

/// The complete output colorspace specification (the project's delivery
/// target; sRGB by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct OutputColorSpec {
	/// The output gamut.
	pub gamut: OutputGamut,
	/// The output transfer characteristic.
	pub transfer: OutputTransfer,
}

impl OutputColorSpec {
	/// Build a complete output color spec from two persisted project settings.
	///
	/// `gamut` is parsed by [`OutputGamut::from_setting`] (for example
	/// `"srgb"`, `"displayp3"`, `"bt2020"`) and `transfer` by
	/// [`OutputTransfer::from_setting`] (for example `"srgb"`, `"gamma22"`,
	/// `"pq"`, `"hlg"`); unknown strings fall back to the enum defaults
	/// (sRGB gamut / sRGB transfer). Used when loading the project's output
	/// target; persistence uses `OutputGamut::as_setting` / `OutputTransfer::as_setting`.
	pub fn from_settings(gamut: &str, transfer: &str) -> Self {
		Self {
			gamut: OutputGamut::from_setting(gamut),
			transfer: OutputTransfer::from_setting(transfer),
		}
	}
}

// ---------------------------------------------------------------------------
// Transfer functions (component-wise)
// ---------------------------------------------------------------------------

/// sRGB OETF: linear [0,∞) → gamma-encoded code (the piecewise IEC curve,
/// mirrored for negatives so scene values survive).
pub fn srgb_oetf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let v = v.abs();
	sign * if v <= 0.0031308 {
		12.92 * v
	} else {
		1.055 * v.powf(1.0 / 2.4) - 0.055
	}
}

/// Decode an sRGB code value to linear light (IEC 61966-2-1 EOTF).
///
/// `v` is a gamma-encoded component, nominally in `[0, 1]`; the piecewise
/// curve (linear toe at `0.04045`, then the `2.4` power) is applied to the
/// absolute value and mirrored for negatives so scene-linear negatives
/// survive. Returns linear light. Counterpart of [`srgb_oetf`].
pub fn srgb_eotf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let v = v.abs();
	sign * if v <= 0.04045 {
		v / 12.92
	} else {
		((v + 0.055) / 1.055).powf(2.4)
	}
}

/// Encode linear light with a pure-power gamma curve.
///
/// `v` is linear light (nominally `[0, 1]`); `gamma` is the corresponding
/// EOTF exponent (`2.2` for BT.1886-style SDR, `2.8` for some film scans).
/// Returns `v^(1/gamma)` with negatives mirrored. Counterpart of
/// [`gamma_eotf`].
pub fn gamma_oetf(v: f32, gamma: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	sign * v.abs().powf(1.0 / gamma)
}

/// Decode a pure-power gamma code value to linear light.
///
/// `v` is the gamma-encoded component (nominally `[0, 1]`) and `gamma` the
/// EOTF exponent (`2.2` or `2.8` in the LUTs below). Returns
/// `sign(v) * |v|^gamma`, so negatives are mirrored. Counterpart of
/// [`gamma_oetf`].
pub fn gamma_eotf(v: f32, gamma: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	sign * v.abs().powf(gamma)
}

/// SMPTE ST 2084 (PQ) constants (normalized: code 1.0 = 10 000 nits).
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = (2523.0 / 4096.0) * 128.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = (2413.0 / 4096.0) * 32.0;
const PQ_C3: f32 = (2392.0 / 4096.0) * 32.0;

/// PQ EOTF: code → linear scene/display value (1.0 = 10 000 nits).
pub fn pq_eotf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let v = v.abs().clamp(0.0, 1.0).powf(1.0 / PQ_M2);
	let num = (v - PQ_C1).max(0.0);
	let den = PQ_C2 - PQ_C3 * v;
	sign * (num / den).powf(1.0 / PQ_M1)
}

/// Encode linear light with the SMPTE ST 2084 (PQ) OETF.
///
/// `v` is linear light normalized so `1.0 = 10 000 nits`; negatives are
/// mirrored like the other transfer functions. Returns the PQ code value
/// (`0..1` for the nominal range). Inverse of [`pq_eotf`].
pub fn pq_oetf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let y = v.abs().powf(PQ_M1);
	let num = PQ_C1 + PQ_C2 * y;
	let den = 1.0 + PQ_C3 * y;
	sign * (num / den).powf(PQ_M2)
}

/// HLG constants (ARIB STD-B67). `HLG_C` is the precomputed value of
/// `0.5 - a·ln(4a)` (a non-const expression).
const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 1.0 - 4.0 * HLG_A; // 0.28466892
const HLG_C: f32 = 0.55992546;

/// HLG inverse OETF: code → linear (scene-referred half, display 1.0).
pub fn hlg_eotf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let v = v.abs();
	sign * if v <= 0.5 {
		v * v / 3.0
	} else {
		(((v - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
	}
}

/// Encode linear light with the ARIB STD-B67 (HLG) OETF.
///
/// `v` is scene-linear light with the nominal reference white near `1.0`
/// (the curve's own input/output range is `[0, 1]`); negatives are
/// mirrored. Returns the HLG code value. Inverse of [`hlg_eotf`].
pub fn hlg_oetf(v: f32) -> f32 {
	let sign = if v < 0.0 { -1.0 } else { 1.0 };
	let v = v.abs();
	sign * if v <= 1.0 / 12.0 {
		(3.0 * v).sqrt()
	} else {
		HLG_A * (12.0 * v - HLG_B).ln() + HLG_C
	}
}

// ---------------------------------------------------------------------------
// Fast transfer evaluation (LUT) and parallel per-pixel transforms
// ---------------------------------------------------------------------------

/// LUT intervals over the [0, 1] domain (plus the 1.0 endpoint).
const LUT_N: usize = 4096;

/// Declare one lazily-built transfer LUT over [0, 1].
macro_rules! tf_lut {
	($name:ident, $f:expr) => {
		static $name: std::sync::LazyLock<[f32; LUT_N + 1]> = std::sync::LazyLock::new(|| {
			let f: fn(f32) -> f32 = $f;
			let mut t = [0.0f32; LUT_N + 1];
			for (i, e) in t.iter_mut().enumerate() {
				*e = f(i as f32 / LUT_N as f32);
			}
			t
		});
	};
}

tf_lut!(SRGB_EOTF_LUT, srgb_eotf);
tf_lut!(SRGB_OETF_LUT, srgb_oetf);
tf_lut!(GAMMA22_EOTF_LUT, |v| gamma_eotf(v, 2.2));
tf_lut!(GAMMA22_OETF_LUT, |v| gamma_oetf(v, 2.2));
tf_lut!(GAMMA28_EOTF_LUT, |v| gamma_eotf(v, 2.8));
tf_lut!(PQ_EOTF_LUT, pq_eotf);
tf_lut!(PQ_OETF_LUT, pq_oetf);
tf_lut!(HLG_EOTF_LUT, hlg_eotf);
tf_lut!(HLG_OETF_LUT, hlg_oetf);

/// Evaluate a transfer function through its [0, 1] LUT (linear
/// interpolation; max error ≈ 2e-5 — far below 10-bit quantization).
/// Negatives mirror like the exact functions; values above 1.0 fall back
/// to the exact function (HDR super-whites are rare and already clamped
/// by the output node).
#[inline]
fn lut_tf(lut: &[f32; LUT_N + 1], exact: fn(f32) -> f32, v: f32) -> f32 {
	let (sign, a) = if v < 0.0 { (-1.0, -v) } else { (1.0, v) };
	let out = if a >= 1.0 {
		exact(a)
	} else {
		let x = a * LUT_N as f32;
		let i = (x as usize).min(LUT_N - 1);
		let frac = x - i as f32;
		lut[i] + frac * (lut[i + 1] - lut[i])
	};
	sign * out
}

/// Fast [`srgb_eotf`]: the linear toe is exact, the power part is LUT'd.
#[inline]
fn srgb_eotf_fast(v: f32) -> f32 {
	if v.abs() <= 0.04045 {
		v / 12.92
	} else {
		lut_tf(&SRGB_EOTF_LUT, srgb_eotf, v)
	}
}

/// Fast [`srgb_oetf`]: the linear toe is exact, the power part is LUT'd.
#[inline]
fn srgb_oetf_fast(v: f32) -> f32 {
	if v.abs() <= 0.0031308 {
		12.92 * v
	} else {
		lut_tf(&SRGB_OETF_LUT, srgb_oetf, v)
	}
}

/// Select the fast decode-side linearizer for a source transfer.
///
/// `transfer` is the source's transfer characteristic from container/codec
/// metadata. Returns a function pointer mapping one encoded component to
/// linear light: LUT-backed for sRGB/gamma/PQ/HLG (see [`lut_tf`]), the
/// identity for [`SourceTransfer::Linear`], and the SDR-gamma path for
/// [`SourceTransfer::Unknown`] (matching that variant's documented
/// fallback). Used by [`decode_to_acescg`].
fn decode_transfer_fn(transfer: SourceTransfer) -> fn(f32) -> f32 {
	match transfer {
		SourceTransfer::SdrGamma | SourceTransfer::Unknown => srgb_eotf_fast,
		SourceTransfer::Gamma22 => |v| lut_tf(&GAMMA22_EOTF_LUT, |x| gamma_eotf(x, 2.2), v),
		SourceTransfer::Gamma28 => |v| lut_tf(&GAMMA28_EOTF_LUT, |x| gamma_eotf(x, 2.8), v),
		SourceTransfer::Pq => |v| lut_tf(&PQ_EOTF_LUT, pq_eotf, v),
		SourceTransfer::Hlg => |v| lut_tf(&HLG_EOTF_LUT, hlg_eotf, v),
		SourceTransfer::Linear => |v| v,
	}
}

/// Select the fast output-side encoder for an output transfer.
///
/// `transfer` is the delivery target's transfer characteristic. Returns a
/// function pointer mapping one linear component to the encoded code value:
/// LUT-backed for sRGB/gamma/PQ/HLG (see [`lut_tf`]). Used by
/// [`acescg_to_output`] and [`acescg_to_output_bytes`].
fn output_transfer_fn(transfer: OutputTransfer) -> fn(f32) -> f32 {
	match transfer {
		OutputTransfer::Srgb => srgb_oetf_fast,
		OutputTransfer::Gamma22 => |v| lut_tf(&GAMMA22_OETF_LUT, |x| gamma_oetf(x, 2.2), v),
		OutputTransfer::Pq => |v| lut_tf(&PQ_OETF_LUT, pq_oetf, v),
		OutputTransfer::Hlg => |v| lut_tf(&HLG_OETF_LUT, hlg_oetf, v),
	}
}

/// Split `samples` (whole pixels = 4 floats) into row bands and run `f`
/// on each band, on scoped threads when the frame is big enough to be
/// worth the spawn overhead (a single 1080p frame is ~8 Mpx; the per-
/// pixel work below is memory- and ALU-bound, so bands scale).
fn par_pixels_f32(samples: &mut [f32], f: impl Fn(&mut [f32]) + Sync) {
	cpuparallel::for_each_band(samples, 4, f);
}

/// The `&mut [u8]` analog of [`par_pixels_f32`] (16 bytes per pixel).
fn par_pixels_bytes(bytes: &mut [u8], f: impl Fn(&mut [u8]) + Sync) {
	cpuparallel::for_each_band(bytes, 16, f);
}

// ---------------------------------------------------------------------------
// Source decode characterization
// ---------------------------------------------------------------------------

/// The colorimetry of a decoded source, as read from the container /
/// codec metadata (H.264/HEVC VUI, FFmpeg `AVCodecContext` fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourcePrimaries {
	/// BT.709 / sRGB primaries (the SDR HDTV default).
	Bt709,
	/// BT.2020 primaries (UHD / wide-gamut sources).
	Bt2020,
	/// Display P3 primaries.
	DisplayP3,
	/// Unknown primaries — treated as BT.709 (the documented fallback).
	Unknown,
}

/// The transfer characteristic of a decoded source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTransfer {
	/// The sRGB piecewise EOTF (IEC 61966-2-1), applied at decode. Also
	/// used for BT.709 / BT.2020 SDR video: the BT.709 camera OETF inverse
	/// is ≈ gamma 2.2 (sRGB EOTF to within measurement noise), while
	/// decoding with the BT.1886 *display* EOTF (2.4) bakes in the ~1.1
	/// system gamma that belongs at final display only — SDR sources
	/// round-tripped through the working space came out visibly dark.
	SdrGamma,
	/// Pure power 2.2 gamma.
	Gamma22,
	/// Pure power 2.8 gamma.
	Gamma28,
	/// SMPTE ST 2084 (PQ) HDR.
	Pq,
	/// ARIB STD-B67 HLG HDR.
	Hlg,
	/// Linear-light source (EXR-style).
	Linear,
	/// Unknown transfer — treated as SDR gamma.
	Unknown,
}

/// Map FFmpeg/libav `color_primaries` values to [`SourcePrimaries`].
/// (AVCOL_PRI_* numbering, H.273 ISO codes.)
pub fn source_primaries_from_av(color_primaries: i32) -> SourcePrimaries {
	match color_primaries {
		1 => SourcePrimaries::Bt709,      // BT.709
		9 => SourcePrimaries::Bt2020,     // BT.2020
		11 => SourcePrimaries::DisplayP3, // SMPTE RP 431-2 (DCI-P3)
		12 => SourcePrimaries::DisplayP3, // SMPTE EG 432-1 (Display P3)
		_ => SourcePrimaries::Unknown,
	}
}

/// Map FFmpeg/libav `color_trc` values to [`SourceTransfer`].
/// (AVCOL_TRC_* numbering, H.273 ISO codes.)
pub fn source_transfer_from_av(color_trc: i32) -> SourceTransfer {
	match color_trc {
		1 => SourceTransfer::SdrGamma, // BT.709 (see the enum docs: OETF inverse ≈ 2.2)
		4 => SourceTransfer::Gamma22,  // gamma 2.2
		5 => SourceTransfer::Gamma28,  // gamma 2.8
		6 => SourceTransfer::SdrGamma, // SMPTE 170M
		13 => SourceTransfer::SdrGamma, // sRGB (its EOTF is applied at decode)
		14 => SourceTransfer::SdrGamma, // BT.2020 10-bit
		15 => SourceTransfer::SdrGamma, // BT.2020 12-bit
		16 => SourceTransfer::Pq,      // SMPTE ST 2084
		18 => SourceTransfer::Hlg,     // ARIB STD-B67
		8 => SourceTransfer::Linear,   // linear
		_ => SourceTransfer::Unknown,
	}
}

impl SourcePrimaries {
	/// The primaries struct for this source.
	pub fn primaries(self) -> Primaries {
		match self {
			SourcePrimaries::Bt709 | SourcePrimaries::Unknown => PRIMARIES_SRGB,
			SourcePrimaries::Bt2020 => PRIMARIES_BT2020,
			SourcePrimaries::DisplayP3 => PRIMARIES_DISPLAY_P3,
		}
	}
}

// ---------------------------------------------------------------------------
// The pipeline transforms (F32 RGBA, tightly packed or in LE byte buffers)
// ---------------------------------------------------------------------------

/// Decode-direction transform: gamma-encoded source RGB (in the source's
/// own primaries) → ACEScg linear. `samples` is an F32 RGBA buffer,
/// transformed in place. Linearize and the primaries matrix are fused
/// into a single pass (LUT'd transfer, row-band parallel).
pub fn decode_to_acescg(samples: &mut [f32], primaries: SourcePrimaries, transfer: SourceTransfer) {
	let linearize = decode_transfer_fn(transfer);
	let matrix = rgb_to_rgb_matrix(primaries.primaries(), PRIMARIES_AP1);
	par_pixels_f32(samples, move |band| {
		for px in band.as_chunks_mut::<4>().0 {
			for c in 0..3 {
				px[c] = linearize(px[c]);
			}
			apply_mat(matrix, px);
		}
	});
}

/// Decode-direction transform on a little-endian F32 RGBA byte buffer
/// (`pixels * 16` bytes). Avoids alignment requirements of the pipeline's
/// `Vec<u8>` frame storage.
pub fn decode_to_acescg_bytes(
	bytes: &mut [u8],
	pixels: usize,
	primaries: SourcePrimaries,
	transfer: SourceTransfer,
) {
	if bytes.len() < pixels * 16 {
		return;
	}
	let linearize = decode_transfer_fn(transfer);
	let matrix = rgb_to_rgb_matrix(primaries.primaries(), PRIMARIES_AP1);
	par_pixels_bytes(bytes, move |band| {
		for px in band.as_chunks_mut::<16>().0 {
			let mut v = [
				f32::from_le_bytes(px[0..4].try_into().unwrap()),
				f32::from_le_bytes(px[4..8].try_into().unwrap()),
				f32::from_le_bytes(px[8..12].try_into().unwrap()),
				f32::from_le_bytes(px[12..16].try_into().unwrap()),
			];
			for c in 0..3 {
				v[c] = linearize(v[c]);
			}
			apply_mat(matrix, &mut v);
			px[0..4].copy_from_slice(&v[0].to_le_bytes());
			px[4..8].copy_from_slice(&v[1].to_le_bytes());
			px[8..12].copy_from_slice(&v[2].to_le_bytes());
			px[12..16].copy_from_slice(&v[3].to_le_bytes());
		}
	});
}

/// Convert ACEScg linear pixels to the output colorspace in place.
///
/// `samples` is a tightly packed F32 RGBA buffer (4 floats per pixel; a
/// trailing partial pixel is ignored). `spec` selects the target gamut and
/// transfer: the AP1-to-target-gamut matrix is applied to RGB, the result is
/// clamped to `[0, 1]` (the contract's P1 simple-clip gamut mapping, also
/// used for HDR targets) and then encoded with the LUT-backed transfer.
/// Alpha is untouched; the whole transform is fused into one
/// row-band-parallel pass. Counterpart of [`decode_to_acescg`].
pub fn acescg_to_output(samples: &mut [f32], spec: OutputColorSpec) {
	let matrix = rgb_to_rgb_matrix(PRIMARIES_AP1, spec.gamut.primaries());
	let encode = output_transfer_fn(spec.transfer);
	par_pixels_f32(samples, move |band| {
		for px in band.as_chunks_mut::<4>().0 {
			apply_mat(matrix, px);
			for c in 0..3 {
				px[c] = encode(px[c].clamp(0.0, 1.0));
			}
		}
	});
}

/// Output-direction transform on a little-endian F32 RGBA byte buffer
/// (`pixels * 16` bytes) — the worker's end-of-pipe conversion before
/// 8-bit quantization. RGB is clamped to [0, 1] after the gamut matrix and
/// before the transfer encoding, exactly as in [`acescg_to_output`].
pub fn acescg_to_output_bytes(bytes: &mut [u8], pixels: usize, spec: OutputColorSpec) {
	if bytes.len() < pixels * 16 {
		return;
	}
	let matrix = rgb_to_rgb_matrix(PRIMARIES_AP1, spec.gamut.primaries());
	let encode = output_transfer_fn(spec.transfer);
	par_pixels_bytes(bytes, move |band| {
		for px in band.as_chunks_mut::<16>().0 {
			let mut v = [
				f32::from_le_bytes(px[0..4].try_into().unwrap()),
				f32::from_le_bytes(px[4..8].try_into().unwrap()),
				f32::from_le_bytes(px[8..12].try_into().unwrap()),
				f32::from_le_bytes(px[12..16].try_into().unwrap()),
			];
			apply_mat(matrix, &mut v);
			for c in 0..3 {
				v[c] = encode(v[c].clamp(0.0, 1.0));
			}
			px[0..4].copy_from_slice(&v[0].to_le_bytes());
			px[4..8].copy_from_slice(&v[1].to_le_bytes());
			px[8..12].copy_from_slice(&v[2].to_le_bytes());
			px[12..16].copy_from_slice(&v[3].to_le_bytes());
		}
	});
}

/// Apply just the transfer encoding of `transfer` (linear → code). In place
/// on the RGB channels (LUT'd, row-band parallel).
pub fn apply_transfer_oetf(samples: &mut [f32], transfer: OutputTransfer) {
	let encode = output_transfer_fn(transfer);
	par_pixels_f32(samples, move |band| {
		for px in band.as_chunks_mut::<4>().0 {
			for c in 0..3 {
				px[c] = encode(px[c]);
			}
		}
	});
}

/// The presentation transform: working space → the display target colorspace
/// (the project's output spec). With the ACEScg working space this is the
/// same chain as [`acescg_to_output`]; with the legacy sRGB working space it
/// is a pass-through (the content already IS display-referred sRGB).
pub fn working_to_display_target(
	samples: &mut [f32],
	working: WorkingColorSpace,
	spec: OutputColorSpec,
) {
	match working {
		WorkingColorSpace::AcesCg => acescg_to_output(samples, spec),
		WorkingColorSpace::SrgbLegacy => {}
	}
}

// ---------------------------------------------------------------------------
// Content → CIE XYZ (D65)
// ---------------------------------------------------------------------------

/// Encoded content in `spec` → CIE XYZ (D65, unit luminance 1.0), in place
/// on F32 RGBA. This feeds the self-managed display ICC path with its input
/// without depending on an OCIO named colorspace: linearize per the spec's
/// transfer (Srgb → sRGB EOTF, Gamma22 → pure power 2.2, Pq → ST 2084,
/// Hlg → HLG), then convert RGB → XYZ through the gamut's own matrix. All
/// output gamuts share the D65 white point, so no chromatic adaptation is
/// needed; `rgb_to_xyz_matrix` already normalizes the white point to unit
/// luminance. Linearize and matrix are fused into one LUT'd parallel pass.
pub fn output_spec_to_xyz_d65(samples: &mut [f32], spec: OutputColorSpec) {
	let linearize = decode_transfer_fn(match spec.transfer {
		OutputTransfer::Srgb => SourceTransfer::SdrGamma,
		OutputTransfer::Gamma22 => SourceTransfer::Gamma22,
		OutputTransfer::Pq => SourceTransfer::Pq,
		OutputTransfer::Hlg => SourceTransfer::Hlg,
	});
	let matrix = rgb_to_xyz_matrix(spec.gamut.primaries());
	par_pixels_f32(samples, move |band| {
		for px in band.as_chunks_mut::<4>().0 {
			for c in 0..3 {
				px[c] = linearize(px[c]);
			}
			apply_mat(matrix, px);
		}
	});
}

// ---------------------------------------------------------------------------
// YUV → RGB (F32) decode fallback
// ---------------------------------------------------------------------------

/// The YUV matrix-coefficient set (H.273 / `AVCOL_SPC_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
	/// ITU-R BT.601 (SD) coefficients.
	Bt601,
	/// ITU-R BT.709 (HD) coefficients.
	Bt709,
	/// ITU-R BT.2020 (UHD) coefficients.
	Bt2020,
}

impl YuvMatrix {
	/// The (Kr, Kb) luma-coefficient pair of this matrix.
	pub(crate) fn kr_kb(self) -> (f32, f32) {
		match self {
			YuvMatrix::Bt601 => (0.299, 0.114),
			YuvMatrix::Bt709 => (0.2126, 0.0722),
			YuvMatrix::Bt2020 => (0.2627, 0.0593),
		}
	}
}

/// Convert a 16-bit little-endian planar YUV 4:4:4 frame to interleaved F32
/// RGBA (alpha 1.0). This is the decode fallback path: swscale cannot emit
/// RGBA64/F32 for some configurations and aborts, so the matrix conversion
/// happens here in Rust.
///
/// Each plane is a `height × stride` byte region of native-endian `u16`
/// samples (`stride ≥ width * 2`; padding between rows is skipped). Code
/// values are expanded per `full_range` before the matrix conversion:
///
/// * limited: Y ∈ [4096, 60160] (16-bit 16/235 shifted by 8), C ∈
///   [4096, 61440] with the neutral point at 32768;
/// * full: Y ∈ [0, 65535], C centered on 32768.
///
/// R/G/B follow from (Kr, Kb): R = Y' + 2(1−Kr)·Cr', B = Y' + 2(1−Kb)·Cb',
/// G = (Y' − Kr·R − Kb·B)/(1−Kr−Kb). Returns early (no panic, `out`
/// untouched) when any plane or `out` is too short for the frame.
pub fn yuv444p16_to_rgb_f32(
	y_plane: &[u8],
	y_stride: usize,
	u_plane: &[u8],
	u_stride: usize,
	v_plane: &[u8],
	v_stride: usize,
	width: usize,
	height: usize,
	matrix: YuvMatrix,
	full_range: bool,
	out: &mut [f32],
) {
	if out.len() < width.saturating_mul(height).saturating_mul(4) {
		return;
	}
	// Each plane must hold (height - 1) full strides plus the last row's
	// `width` samples.
	let row_bytes = width.saturating_mul(2);
	let rows_after = height.saturating_sub(1);
	if y_plane.len()
		< rows_after
			.saturating_mul(y_stride)
			.saturating_add(row_bytes)
		|| u_plane.len()
			< rows_after
				.saturating_mul(u_stride)
				.saturating_add(row_bytes)
		|| v_plane.len()
			< rows_after
				.saturating_mul(v_stride)
				.saturating_add(row_bytes)
	{
		return;
	}
	let (kr, kb) = matrix.kr_kb();
	let kg = 1.0 - kr - kb;
	// Code → normalized: limited luma spans 219<<8 above 16<<8, chroma spans
	// 224<<8 above its 32768 center; full-range everything spans 65535.
	let (y_offset, c_offset, scale) = if full_range {
		(0.0f32, 32768.0f32, 1.0 / 65535.0)
	} else {
		(4096.0f32, 32768.0f32, 1.0 / 57344.0)
	};
	let y_scale = if full_range {
		1.0 / 65535.0
	} else {
		1.0 / 56064.0
	};
	cpuparallel::for_each_row(out, width * 4, height, width, |row, out_row| {
		let y_row = row * y_stride;
		let u_row = row * u_stride;
		let v_row = row * v_stride;
		for x in 0..width {
			let off = x * 2;
			let y = u16::from_le_bytes([y_plane[y_row + off], y_plane[y_row + off + 1]]) as f32;
			let cb = u16::from_le_bytes([u_plane[u_row + off], u_plane[u_row + off + 1]]) as f32;
			let cr = u16::from_le_bytes([v_plane[v_row + off], v_plane[v_row + off + 1]]) as f32;
			let yn = (y - y_offset) * y_scale;
			let cbn = (cb - c_offset) * scale;
			let crn = (cr - c_offset) * scale;
			let r = yn + 2.0 * (1.0 - kr) * crn;
			let b = yn + 2.0 * (1.0 - kb) * cbn;
			let g = (yn - kr * r - kb * b) / kg;
			let px = x * 4;
			out_row[px] = r;
			out_row[px + 1] = g;
			out_row[px + 2] = b;
			out_row[px + 3] = 1.0;
		}
	});
}

#[cfg(test)]
mod tests;
