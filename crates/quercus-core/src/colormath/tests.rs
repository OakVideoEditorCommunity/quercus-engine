use super::*;

fn approx(a: f32, b: f32, eps: f32) -> bool {
	(a - b).abs() < eps
}

#[test]
fn rgb_to_xyz_srgb_white_is_unit_luminance() {
	let m = rgb_to_xyz_matrix(PRIMARIES_SRGB);
	let white = mat_vec(m, [1.0, 1.0, 1.0]);
	let expected = WHITE_D65.xyz_unit_luminance();
	for i in 0..3 {
		assert!(
			approx(white[i], expected[i], 1e-4),
			"sRGB white → XYZ mismatch at {i}: {} vs {}",
			white[i],
			expected[i]
		);
	}
}

#[test]
fn srgb_to_acescg_matches_published_matrix() {
	// Published sRGB → ACEScg transform (ACES transform docs):
	// 0.6131324224 0.3395230762 0.0473445014
	// 0.0701922769 0.9163536767 0.0134540464
	// 0.0206157712 0.1095697056 0.8698145232
	let m = rgb_to_rgb_matrix(PRIMARIES_SRGB, PRIMARIES_AP1);
	let expected: Mat3 = [
		[0.613_132_4, 0.339_523_08, 0.047_344_502],
		[0.070_192_28, 0.916_353_7, 0.013_454_046],
		[0.020_615_771, 0.109_569_706, 0.869_814_5],
	];
	for r in 0..3 {
		for c in 0..3 {
			assert!(
				approx(m[r][c], expected[r][c], 2e-3),
				"matrix[{r}][{c}] = {} vs published {}",
				m[r][c],
				expected[r][c]
			);
		}
	}
}

#[test]
fn srgb_red_maps_to_published_acescg() {
	let mut samples = [1.0f32, 0.0, 0.0, 1.0];
	decode_to_acescg(&mut samples, SourcePrimaries::Bt709, SourceTransfer::Linear);
	// sRGB red in ACEScg ≈ (0.6131, 0.0702, 0.0206).
	assert!(approx(samples[0], 0.6131, 2e-3), "R = {}", samples[0]);
	assert!(approx(samples[1], 0.0702, 2e-3), "G = {}", samples[1]);
	assert!(approx(samples[2], 0.0206, 2e-3), "B = {}", samples[2]);
}

#[test]
fn round_trip_srgb_acescg_srgb() {
	// Gamma-encoded sRGB codes → (EOTF) → linear → ACEScg → (matrix)
	// → sRGB linear → (OETF) → the original codes.
	let original = [0.25f32, 0.5, 0.75, 1.0];
	let mut samples = original;
	decode_to_acescg(
		&mut samples,
		SourcePrimaries::Bt709,
		SourceTransfer::SdrGamma,
	);
	acescg_to_output(
		&mut samples,
		OutputColorSpec {
			gamut: OutputGamut::Srgb,
			transfer: OutputTransfer::Srgb,
		},
	);
	for i in 0..3 {
		assert!(
			approx(samples[i], original[i], 1e-4),
			"round trip channel {i}: {} vs {}",
			samples[i],
			original[i]
		);
	}
}

#[test]
fn round_trip_bt2020_acescg_bt2020() {
	let original = [0.2f32, 0.6, 0.9, 1.0];
	let mut samples = original;
	decode_to_acescg(
		&mut samples,
		SourcePrimaries::Bt2020,
		SourceTransfer::SdrGamma,
	);
	acescg_to_output(
		&mut samples,
		OutputColorSpec {
			gamut: OutputGamut::Bt2020,
			transfer: OutputTransfer::Srgb,
		},
	);
	for i in 0..3 {
		assert!(
			approx(samples[i], original[i], 1e-4),
			"round trip channel {i}: {} vs {}",
			samples[i],
			original[i]
		);
	}
}

#[test]
fn white_stays_white_through_acescg() {
	// Every primaries→AP1 matrix has rows summing to ~1, so the white
	// point maps to (1,1,1) in ACEScg.
	let mut samples = [1.0f32, 1.0, 1.0, 1.0];
	decode_to_acescg(&mut samples, SourcePrimaries::Bt709, SourceTransfer::Linear);
	for i in 0..3 {
		assert!(
			approx(samples[i], 1.0, 1e-3),
			"white channel {i} = {}",
			samples[i]
		);
	}
}

#[test]
fn srgb_transfer_anchors() {
	// The piecewise curve's join point and endpoints.
	assert!(approx(srgb_oetf(0.0), 0.0, 1e-6), "oetf(0)");
	assert!(approx(srgb_oetf(1.0), 1.0, 1e-6), "oetf(1)");
	assert!(approx(srgb_eotf(0.0), 0.0, 1e-6), "eotf(0)");
	assert!(approx(srgb_eotf(1.0), 1.0, 1e-6), "eotf(1)");
	// Mid-grey: linear 0.18 → ~0.461 sRGB code (the classic check).
	assert!(
		approx(srgb_oetf(0.18), 0.4613, 1e-3),
		"oetf(0.18) = {}",
		srgb_oetf(0.18)
	);
	// Round trip.
	for v in [0.0f32, 0.01, 0.18, 0.5, 0.99, 1.0] {
		assert!(approx(srgb_eotf(srgb_oetf(v)), v, 1e-4), "round trip {v}");
	}
}

#[test]
fn pq_transfer_anchors() {
	// PQ code 1.0 = 10 000 nits = normalized 1.0.
	assert!(
		approx(pq_eotf(1.0), 1.0, 1e-4),
		"pq eotf(1) = {}",
		pq_eotf(1.0)
	);
	// PQ code 0.5 ≈ 100 nits (normalized 0.01).
	assert!(
		approx(pq_eotf(0.5), 0.01008, 2e-3),
		"pq eotf(0.5) = {}",
		pq_eotf(0.5)
	);
	// Round trip.
	for v in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
		assert!(approx(pq_eotf(pq_oetf(v)), v, 1e-3), "pq round trip {v}");
	}
}

#[test]
fn hlg_transfer_anchors() {
	assert!(approx(hlg_eotf(0.0), 0.0, 1e-6), "hlg eotf(0)");
	assert!(
		approx(hlg_eotf(1.0), 1.0, 1e-3),
		"hlg eotf(1) = {}",
		hlg_eotf(1.0)
	);
	// The piecewise join: code 0.5 ↔ linear 1/12.
	assert!(
		approx(hlg_eotf(0.5), 1.0 / 12.0, 1e-3),
		"hlg eotf(0.5) = {}",
		hlg_eotf(0.5)
	);
	// Round trip.
	for v in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
		assert!(approx(hlg_eotf(hlg_oetf(v)), v, 1e-3), "hlg round trip {v}");
	}
}

#[test]
fn av_color_code_mapping() {
	assert_eq!(source_primaries_from_av(1), SourcePrimaries::Bt709);
	assert_eq!(source_primaries_from_av(9), SourcePrimaries::Bt2020);
	assert_eq!(source_primaries_from_av(12), SourcePrimaries::DisplayP3);
	assert_eq!(source_primaries_from_av(2), SourcePrimaries::Unknown);
	assert_eq!(source_transfer_from_av(1), SourceTransfer::SdrGamma);
	assert_eq!(source_transfer_from_av(4), SourceTransfer::Gamma22);
	assert_eq!(source_transfer_from_av(5), SourceTransfer::Gamma28);
	assert_eq!(source_transfer_from_av(6), SourceTransfer::SdrGamma);
	assert_eq!(source_transfer_from_av(13), SourceTransfer::SdrGamma);
	assert_eq!(source_transfer_from_av(14), SourceTransfer::SdrGamma);
	assert_eq!(source_transfer_from_av(15), SourceTransfer::SdrGamma);
	assert_eq!(source_transfer_from_av(16), SourceTransfer::Pq);
	assert_eq!(source_transfer_from_av(18), SourceTransfer::Hlg);
	assert_eq!(source_transfer_from_av(8), SourceTransfer::Linear);
	assert_eq!(source_transfer_from_av(3), SourceTransfer::Unknown);
	assert_eq!(source_transfer_from_av(7), SourceTransfer::Unknown);
}

#[test]
fn setting_round_trips() {
	assert_eq!(
		WorkingColorSpace::from_setting("acescg"),
		WorkingColorSpace::AcesCg
	);
	assert_eq!(
		WorkingColorSpace::from_setting("srgb_legacy"),
		WorkingColorSpace::SrgbLegacy
	);
	assert_eq!(
		WorkingColorSpace::from_setting("bogus"),
		WorkingColorSpace::AcesCg
	);
	assert_eq!(
		OutputGamut::from_setting("displayp3"),
		OutputGamut::DisplayP3
	);
	assert_eq!(OutputGamut::from_setting("bt2020"), OutputGamut::Bt2020);
	assert_eq!(OutputGamut::from_setting(""), OutputGamut::Srgb);
	assert_eq!(OutputTransfer::from_setting("pq"), OutputTransfer::Pq);
	assert_eq!(OutputTransfer::from_setting("hlg"), OutputTransfer::Hlg);
	assert_eq!(
		OutputTransfer::from_setting("gamma22"),
		OutputTransfer::Gamma22
	);
	assert_eq!(OutputTransfer::from_setting(""), OutputTransfer::Srgb);
	assert_eq!(
		OutputColorSpec::from_settings("displayp3", "pq"),
		OutputColorSpec {
			gamut: OutputGamut::DisplayP3,
			transfer: OutputTransfer::Pq
		}
	);
}

#[test]
fn legacy_working_space_is_pass_through() {
	let mut samples = [0.25f32, 0.5, 0.75, 1.0];
	let original = samples;
	working_to_display_target(
		&mut samples,
		WorkingColorSpace::SrgbLegacy,
		OutputColorSpec::default(),
	);
	assert_eq!(samples, original);
}

#[test]
fn gamma_transfer_anchors() {
	// Pure-power EOTFs at code 0.5. The 2.4 case is the BT.1886
	// mid-grey link: 0.5²·⁴ ≈ 0.189 (≈ the classic 18 % grey).
	assert!(
		approx(gamma_eotf(0.5, 2.4), 0.1894646, 1e-5),
		"eotf(0.5, 2.4) = {}",
		gamma_eotf(0.5, 2.4)
	);
	assert!(
		approx(gamma_eotf(0.5, 2.2), 0.2176376, 1e-5),
		"eotf(0.5, 2.2) = {}",
		gamma_eotf(0.5, 2.2)
	);
	assert!(
		approx(gamma_eotf(0.5, 2.8), 0.1435894, 1e-5),
		"eotf(0.5, 2.8) = {}",
		gamma_eotf(0.5, 2.8)
	);
	// Endpoints and round trips.
	for gamma in [2.2f32, 2.4, 2.8] {
		assert!(
			approx(gamma_eotf(0.0, gamma), 0.0, 1e-6),
			"eotf(0) g{gamma}"
		);
		assert!(
			approx(gamma_eotf(1.0, gamma), 1.0, 1e-6),
			"eotf(1) g{gamma}"
		);
		for v in [0.0f32, 0.18, 0.5, 0.9, 1.0] {
			assert!(
				approx(gamma_eotf(gamma_oetf(v, gamma), gamma), v, 1e-4),
				"gamma round trip {v} @ {gamma}"
			);
		}
	}
}

#[test]
fn lut_fast_transfer_matches_exact() {
	// The LUT'd fast paths track the exact functions over the whole
	// [0, 1] domain (and mirror negatives); far below 10-bit quanta.
	let cases: &[(&std::sync::LazyLock<[f32; LUT_N + 1]>, fn(f32) -> f32, &str)] = &[
		(&SRGB_EOTF_LUT, srgb_eotf, "srgb_eotf"),
		(&SRGB_OETF_LUT, srgb_oetf, "srgb_oetf"),
		(&GAMMA22_EOTF_LUT, |v| gamma_eotf(v, 2.2), "gamma22_eotf"),
		(&GAMMA22_OETF_LUT, |v| gamma_oetf(v, 2.2), "gamma22_oetf"),
		(&GAMMA28_EOTF_LUT, |v| gamma_eotf(v, 2.8), "gamma28_eotf"),
		(&PQ_EOTF_LUT, pq_eotf, "pq_eotf"),
		(&PQ_OETF_LUT, pq_oetf, "pq_oetf"),
		(&HLG_EOTF_LUT, hlg_eotf, "hlg_eotf"),
		(&HLG_OETF_LUT, hlg_oetf, "hlg_oetf"),
	];
	for &(lut, exact, name) in cases {
		for k in 0..=1000 {
			let v = k as f32 / 1000.0;
			for v in [v, -v] {
				let fast = lut_tf(lut, exact, v);
				let want = exact(v);
				assert!(
					(fast - want).abs() < 2e-4,
					"{name}({v}): fast {fast} vs exact {want}"
				);
			}
		}
	}
	// Above 1.0 the exact function is used.
	assert_eq!(lut_tf(&SRGB_OETF_LUT, srgb_oetf, 2.0), srgb_oetf(2.0));
	// The sRGB wrappers keep their linear toes exact.
	assert_eq!(srgb_eotf_fast(0.02), 0.02 / 12.92);
	assert_eq!(srgb_oetf_fast(0.001), 12.92 * 0.001);
}

#[test]
fn parallel_transform_matches_scalar_reference() {
	// A frame above the band threshold exercises the scoped-thread
	// path; it must agree with the scalar math pixel for pixel.
	let pixels = 1 << 20;
	let mut samples: Vec<f32> = (0..pixels * 4)
		.map(|i: usize| ((i.wrapping_mul(2654435761)) % 1000) as f32 / 999.0)
		.collect();
	let reference: Vec<f32> = samples
		.as_chunks::<4>()
		.0
		.iter()
		.flat_map(|px| {
			let mut v = [srgb_eotf(px[0]), srgb_eotf(px[1]), srgb_eotf(px[2]), px[3]];
			apply_mat(rgb_to_rgb_matrix(PRIMARIES_SRGB, PRIMARIES_AP1), &mut v);
			v
		})
		.collect();
	decode_to_acescg(
		&mut samples,
		SourcePrimaries::Bt709,
		SourceTransfer::SdrGamma,
	);
	for (i, (&got, &want)) in samples.iter().zip(reference.iter()).enumerate() {
		assert!(approx(got, want, 1e-4), "pixel {i}: {got} vs {want}");
	}
}

#[test]
fn bt709_decode_output_round_trip_is_identity() {
	// SDR video (BT.709 / BT.2020 NCL tags) decodes with the sRGB EOTF
	// (OETF inverse ≈ 2.2 — see the SourceTransfer docs), so an SDR
	// frame round-tripped through the ACEScg working space and back to
	// sRGB output must come back unchanged. Decoding with the BT.1886
	// display EOTF (2.4) instead made this round trip visibly dark
	// (the ~1.1 system gamma belongs at final display only).
	for code in [0.05f32, 0.18, 0.5, 0.75, 1.0] {
		let mut samples = [code, code, code, 1.0];
		decode_to_acescg(
			&mut samples,
			SourcePrimaries::Bt709,
			SourceTransfer::SdrGamma,
		);
		acescg_to_output(&mut samples, OutputColorSpec::default());
		for i in 0..3 {
			assert!(
				approx(samples[i], code, 1e-4),
				"BT.709 round trip code {code} channel {i}: {}",
				samples[i]
			);
		}
	}
	// ... and the bytes variant picks the same curve (sRGB piecewise,
	// not a pure power): code 0.5 → srgb_eotf(0.5) ≈ 0.2140.
	let mut bytes = [0u8; 16];
	bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes());
	bytes[4..8].copy_from_slice(&0.5f32.to_le_bytes());
	bytes[8..12].copy_from_slice(&0.5f32.to_le_bytes());
	decode_to_acescg_bytes(
		&mut bytes,
		1,
		SourcePrimaries::Bt709,
		SourceTransfer::SdrGamma,
	);
	let r = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
	let m = rgb_to_rgb_matrix(PRIMARIES_SRGB, PRIMARIES_AP1);
	let expected = mat_vec(m, [srgb_eotf(0.5); 3]);
	assert!(approx(r, expected[0], 1e-6), "bytes SdrGamma R: {r}");
}

#[test]
fn output_clamps_out_of_gamut_rgb() {
	// Pure AP1 red is far outside the sRGB gamut: after the AP1 → sRGB
	// matrix it has R > 1 and negative G/B. The P1 gamut-mapping contract
	// clips those channels to [0, 1] before the transfer encoding.
	let mut samples = [1.0f32, 0.0, 0.0, 0.5];
	acescg_to_output(
		&mut samples,
		OutputColorSpec {
			gamut: OutputGamut::Srgb,
			transfer: OutputTransfer::Srgb,
		},
	);
	let m = rgb_to_rgb_matrix(PRIMARIES_AP1, PRIMARIES_SRGB);
	let unclamped = mat_vec(m, [1.0, 0.0, 0.0]);
	assert!(
		unclamped[0] > 1.0 || unclamped[1] < 0.0 || unclamped[2] < 0.0,
		"AP1 red should leave the sRGB gamut, got {unclamped:?}"
	);
	let expected = [
		srgb_oetf(unclamped[0].clamp(0.0, 1.0)),
		srgb_oetf(unclamped[1].clamp(0.0, 1.0)),
		srgb_oetf(unclamped[2].clamp(0.0, 1.0)),
	];
	for c in 0..3 {
		assert!(
			approx(samples[c], expected[c], 1e-6),
			"clamped channel {c}: {} vs {}",
			samples[c],
			expected[c]
		);
		assert!(
			samples[c] >= 0.0 && samples[c] <= 1.0,
			"channel {c} escaped [0, 1]: {}",
			samples[c]
		);
	}
	assert_eq!(samples[3], 0.5, "alpha must be untouched");
}

#[test]
fn output_bytes_clamps_out_of_gamut_rgb() {
	let mut bytes = [0u8; 16];
	bytes[0..4].copy_from_slice(&1.0f32.to_le_bytes()); // R = AP1 red
	acescg_to_output_bytes(
		&mut bytes,
		1,
		OutputColorSpec {
			gamut: OutputGamut::Srgb,
			transfer: OutputTransfer::Gamma22,
		},
	);
	let m = rgb_to_rgb_matrix(PRIMARIES_AP1, PRIMARIES_SRGB);
	let unclamped = mat_vec(m, [1.0, 0.0, 0.0]);
	let expected = gamma_oetf(unclamped[0].clamp(0.0, 1.0), 2.2);
	let r = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
	assert!(approx(r, expected, 1e-6), "bytes R: {r} vs {expected}");
	// Alpha (still 0.0) passes through untouched.
	assert_eq!(f32::from_le_bytes(bytes[12..16].try_into().unwrap()), 0.0);
}

/// Build a multi-row 16-bit plane byte buffer from `u16` row samples with
/// the given row stride (padding is left as zero).
fn yuv_plane16(rows: &[&[u16]], width: usize, stride: usize) -> Vec<u8> {
	let mut plane = vec![0u8; rows.len().saturating_sub(1) * stride + width * 2];
	for (r, row) in rows.iter().enumerate() {
		for (x, s) in row.iter().take(width).enumerate() {
			let off = r * stride + x * 2;
			plane[off] = s.to_le_bytes()[0];
			plane[off + 1] = s.to_le_bytes()[1];
		}
	}
	plane
}

#[test]
fn yuv_bt709_limited_gray_anchors() {
	// White (Y = 235<<8) → RGB 1.0, black (Y = 16<<8) → 0.0, neutral
	// chroma 128<<8, BT.709 matrix, limited range. Stride is padded
	// (4 bytes vs 2 for width 1) to exercise stride handling.
	let width = 1;
	let height = 2;
	let stride = 4;
	let y = yuv_plane16(&[&[235 << 8], &[16 << 8]], width, stride);
	let u = yuv_plane16(&[&[128 << 8], &[128 << 8]], width, stride);
	let v = u.clone();
	let mut out = [0.0f32; 8];
	yuv444p16_to_rgb_f32(
		&y,
		stride,
		&u,
		stride,
		&v,
		stride,
		width,
		height,
		YuvMatrix::Bt709,
		false,
		&mut out,
	);
	for c in 0..3 {
		assert!(approx(out[c], 1.0, 1e-4), "white channel {c}: {}", out[c]);
		assert!(
			approx(out[4 + c], 0.0, 1e-4),
			"black channel {c}: {}",
			out[4 + c]
		);
	}
	assert_eq!(out[3], 1.0, "alpha");
	assert_eq!(out[7], 1.0, "alpha");
}

#[test]
fn yuv_bt601_full_range_green_anchor() {
	// Pure green in BT.601 full range (chroma span 65535, center 32768):
	// Y = 0.587·65535, Cb = 32768 + (−0.587/1.772)·65535,
	// Cr = 32768 + (−0.587/1.402)·65535, rounded to 16-bit codes.
	let width = 1;
	let height = 1;
	let stride = 2;
	let y = yuv_plane16(&[&[38469]], width, stride);
	let u = yuv_plane16(&[&[11059]], width, stride);
	let v = yuv_plane16(&[&[5329]], width, stride);
	let mut out = [0.0f32; 4];
	yuv444p16_to_rgb_f32(
		&y,
		stride,
		&u,
		stride,
		&v,
		stride,
		width,
		height,
		YuvMatrix::Bt601,
		true,
		&mut out,
	);
	assert!(approx(out[0], 0.0, 1e-3), "R = {}", out[0]);
	assert!(approx(out[1], 1.0, 1e-3), "G = {}", out[1]);
	assert!(approx(out[2], 0.0, 1e-3), "B = {}", out[2]);
}

#[test]
fn yuv_bt709_limited_green_anchor() {
	// Pure green in BT.709 limited range: Y = 16 + 0.7152·219 (×256),
	// Cb = 32768 + (−0.7152/1.8556)·57344,
	// Cr = 32768 + (−0.7152/1.5748)·57344.
	let width = 1;
	let height = 1;
	let stride = 2;
	let y = yuv_plane16(&[&[44193]], width, stride);
	let u = yuv_plane16(&[&[10666]], width, stride);
	let v = yuv_plane16(&[&[6725]], width, stride);
	let mut out = [0.0f32; 4];
	yuv444p16_to_rgb_f32(
		&y,
		stride,
		&u,
		stride,
		&v,
		stride,
		width,
		height,
		YuvMatrix::Bt709,
		false,
		&mut out,
	);
	assert!(approx(out[0], 0.0, 1e-3), "R = {}", out[0]);
	assert!(approx(out[1], 1.0, 1e-3), "G = {}", out[1]);
	assert!(approx(out[2], 0.0, 1e-3), "B = {}", out[2]);
}

#[test]
fn yuv_short_planes_return_without_panic() {
	// A 2×2 frame at stride 4 needs (2−1)·4 + 2·2 = 8 bytes per plane;
	// 4 bytes is too short → early return, `out` untouched.
	let mut out = [7.0f32; 4];
	yuv444p16_to_rgb_f32(
		&[0u8; 4],
		4,
		&[0u8; 4],
		4,
		&[0u8; 4],
		4,
		2,
		2,
		YuvMatrix::Bt709,
		false,
		&mut out,
	);
	assert_eq!(out, [7.0; 4]);
	// `out` too short for the frame → early return.
	let mut out2 = [7.0f32; 3];
	yuv444p16_to_rgb_f32(
		&[0u8; 8],
		4,
		&[0u8; 8],
		4,
		&[0u8; 8],
		4,
		2,
		2,
		YuvMatrix::Bt709,
		false,
		&mut out2,
	);
	assert_eq!(out2, [7.0; 3]);
}

#[test]
fn output_srgb_to_xyz_d65_white_and_red() {
	let spec = OutputColorSpec {
		gamut: OutputGamut::Srgb,
		transfer: OutputTransfer::Srgb,
	};
	// White (1,1,1) → the D65 white point at unit luminance.
	let mut white = [1.0f32, 1.0, 1.0, 1.0];
	output_spec_to_xyz_d65(&mut white, spec);
	let expected = WHITE_D65.xyz_unit_luminance();
	for i in 0..3 {
		assert!(
			approx(white[i], expected[i], 1e-4),
			"white → XYZ mismatch at {i}: {} vs {}",
			white[i],
			expected[i]
		);
	}
	// Red (1,0,0) → the published IEC 61966-2-1 sRGB→XYZ column.
	let mut red = [1.0f32, 0.0, 0.0, 1.0];
	output_spec_to_xyz_d65(&mut red, spec);
	let m = rgb_to_xyz_matrix(PRIMARIES_SRGB);
	for i in 0..3 {
		assert!(
			approx(red[i], m[i][0], 1e-6),
			"red → XYZ mismatch at {i}: {} vs {}",
			red[i],
			m[i][0]
		);
	}
	assert!(approx(red[0], 0.4124, 1e-3), "X = {}", red[0]);
	assert!(approx(red[1], 0.2126, 1e-3), "Y = {}", red[1]);
	assert!(approx(red[2], 0.0193, 1e-3), "Z = {}", red[2]);
}

#[test]
fn output_spec_to_xyz_d65_honors_transfer() {
	// Gamma-2.2 transfer: code 0.5 linearizes to 0.5^2.2 before the
	// gamut matrix.
	let spec = OutputColorSpec {
		gamut: OutputGamut::Srgb,
		transfer: OutputTransfer::Gamma22,
	};
	let mut s = [0.5f32, 0.5, 0.5, 1.0];
	output_spec_to_xyz_d65(&mut s, spec);
	let m = rgb_to_xyz_matrix(PRIMARIES_SRGB);
	let expected = mat_vec(m, [gamma_eotf(0.5, 2.2); 3]);
	for i in 0..3 {
		assert!(
			approx(s[i], expected[i], 1e-6),
			"Gamma22 → XYZ channel {i}: {} vs {}",
			s[i],
			expected[i]
		);
	}
	// PQ / HLG code 1.0 linearizes to 1.0 → the D65 white point for any
	// D65 gamut (also exercises the P3 matrix and the PQ/HLG branches).
	for transfer in [OutputTransfer::Pq, OutputTransfer::Hlg] {
		let spec = OutputColorSpec {
			gamut: OutputGamut::DisplayP3,
			transfer,
		};
		let mut w = [1.0f32, 1.0, 1.0, 1.0];
		output_spec_to_xyz_d65(&mut w, spec);
		let expected = WHITE_D65.xyz_unit_luminance();
		for i in 0..3 {
			assert!(
				approx(w[i], expected[i], 1e-4),
				"{transfer:?} white → XYZ mismatch at {i}: {} vs {}",
				w[i],
				expected[i]
			);
		}
	}
}
