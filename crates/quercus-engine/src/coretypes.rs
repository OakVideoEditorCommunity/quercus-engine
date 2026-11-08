//! C ABI for quercus-core's pure value types: `Rational`, `TimeRange`,
//! `PixelFormat` (prefix `quercus_core_`).
//!
//! All three cross the boundary by value in `repr(C)` mirrors. The C
//! header declares the identical layouts; the constructor/operator
//! functions exist so C consumers get quercus-core's exact semantics
//! (reduction, sentinel propagation, C++-compatible overflow behavior)
//! instead of reimplementing them.

use quercus_core::ocioutils::PixelFormat as VideoPixelFormat;

use crate::handle::NativeMirror;
use quercus_core::videoparams::{ColorRange, Interlacing, VideoParams, VideoType};
use quercus_core::{PixelFormat, Rational, TimeRange};

/// `repr(C)` mirror of quercus-core's `Rational`: always reduced with a
/// non-negative denominator; `0/0` is the null/invalid sentinel.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuercusRational {
	pub num: i64,
	pub den: i64,
}

/// Re-canonicalizes through `Rational::new`, so raw C input gets the
/// same reduction as any native value (idempotent for values that came
/// from [`NativeMirror::from_native`]).
impl crate::handle::NativeMirror for QuercusRational {
	type Native = Rational;

	fn from_native(r: &Rational) -> Self {
		Self {
			num: r.numerator(),
			den: r.denominator(),
		}
	}

	fn to_native(&self) -> Rational {
		Rational::new(self.num, self.den)
	}
}

/// `repr(C)` mirror of quercus-core's `TimeRange`: a half-open [in, out)
/// interval (normalized so `in <= out`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuercusTimeRange {
	/// Inclusive start.
	pub in_: QuercusRational,
	/// Exclusive end.
	pub out: QuercusRational,
}

impl crate::handle::NativeMirror for QuercusTimeRange {
	type Native = TimeRange;

	fn from_native(r: &TimeRange) -> Self {
		Self {
			in_: QuercusRational::from_native(&r.in_()),
			out: QuercusRational::from_native(&r.out()),
		}
	}

	fn to_native(&self) -> TimeRange {
		TimeRange::new(self.in_.to_native(), self.out.to_native())
	}
}

fn pixel_format_from_i32(v: i32) -> Option<PixelFormat> {
	match v {
		-1 => Some(PixelFormat::Invalid),
		0 => Some(PixelFormat::U8),
		1 => Some(PixelFormat::U10),
		2 => Some(PixelFormat::U16),
		3 => Some(PixelFormat::F16),
		4 => Some(PixelFormat::F32),
		_ => None,
	}
}

// ---------------------------------------------------------------------------
// Rational
// ---------------------------------------------------------------------------

/// Build a reduced rational from the raw components `num`/`den`.
///
/// Signs are normalized to a non-negative denominator and the fraction is
/// reduced against `i32::MAX`; `den == 0` (for any numerator) yields the null
/// sentinel `0/0`, while a zero numerator with a non-zero denominator yields
/// `0/1`. Use `quercus_core_rational_from_string` to parse text or
/// `quercus_core_rational_from_f64` for floats.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_new(num: i64, den: i64) -> QuercusRational {
	QuercusRational::from_native(&Rational::new(num, den))
}

/// The null/invalid sentinel (0/0).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_null() -> QuercusRational {
	QuercusRational::from_native(&Rational::NULL)
}

/// True when `r` is the null/invalid rational, i.e. its numerator is zero
/// (`0/0` after normalization).
///
/// This is the value produced by invalid parsing and by sentinel arithmetic;
/// the counterpart constructor is `quercus_core_rational_null`. Note that
/// `is_null` tests the numerator while `is_nan` tests the denominator, so a
/// hand-built struct with `num == 0` and `den != 0` is reported as null.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_is_null(r: QuercusRational) -> bool {
	r.to_native().is_null()
}

/// True for the NaN sentinel (produced by overflowed arithmetic).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_is_nan(r: QuercusRational) -> bool {
	r.to_native().is_nan()
}

/// Convert `r` to `f64` as `num / den`.
///
/// The null/NaN sentinel (`den == 0`) yields `f64::NAN`; large numerators can
/// lose precision in the division. Counterpart of the
/// `quercus_core_rational_from_f64` constructor.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_to_f64(r: QuercusRational) -> f64 {
	r.to_native().to_f64()
}

/// The rational closest to `value` within quercus-core's precision.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_from_f64(value: f64) -> QuercusRational {
	QuercusRational::from_native(&Rational::from_double(value))
}

/// Add two rationals: `a + b`, reduced against `i32::MAX`.
///
/// A `RATIONAL_MIN`/`RATIONAL_MAX` operand yields the null sentinel `0/0`; a
/// NaN operand propagates NaN. Mirrors the C++ `Rational::operator+`. Both
/// operands are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_add(a: QuercusRational, b: QuercusRational) -> QuercusRational {
	QuercusRational::from_native(&(a.to_native() + b.to_native()))
}

/// Subtract two rationals: `a - b`, reduced against `i32::MAX`.
///
/// A `RATIONAL_MIN`/`RATIONAL_MAX` operand yields the null sentinel `0/0`; a
/// NaN operand propagates NaN. Mirrors the C++ `Rational::operator-`. Both
/// operands are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_sub(a: QuercusRational, b: QuercusRational) -> QuercusRational {
	QuercusRational::from_native(&(a.to_native() - b.to_native()))
}

/// Multiply two rationals: `a * b`, reduced against `i32::MAX`.
///
/// A `RATIONAL_MIN`/`RATIONAL_MAX` operand yields the null sentinel `0/0`; a
/// NaN operand propagates NaN. Mirrors the C++ `Rational::operator*`. Both
/// operands are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_mul(a: QuercusRational, b: QuercusRational) -> QuercusRational {
	QuercusRational::from_native(&(a.to_native() * b.to_native()))
}

/// Divide `a` by `b`: `a / b`, reduced against `i32::MAX`.
///
/// A zero divisor, a `RATIONAL_MIN`/`RATIONAL_MAX` operand or a null result
/// yields the null sentinel `0/0`; a NaN operand propagates NaN. Mirrors the
/// C++ `Rational::operator/`. Both operands are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_div(a: QuercusRational, b: QuercusRational) -> QuercusRational {
	QuercusRational::from_native(&(a.to_native() / b.to_native()))
}

/// Three-way comparison of `a` and `b`: `-1`, `0` or `1` for
/// less/equal/greater.
///
/// The null/NaN rational (`0/0`) orders before every non-NaN value and equal
/// to itself, so sentinels compare deterministically (see `Ord for Rational`).
/// Both operands are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_cmp(a: QuercusRational, b: QuercusRational) -> i32 {
	match a.to_native().cmp(&b.to_native()) {
		std::cmp::Ordering::Less => -1,
		std::cmp::Ordering::Equal => 0,
		std::cmp::Ordering::Greater => 1,
	}
}

/// Write `r` in the project-XML text form (e.g. `"30000/1001"`) using
/// `snprintf` semantics.
///
/// At most `buf_len - 1` bytes are copied and a NUL terminator is added; the
/// return value is the **full** text length in bytes, so a value `>=
/// buf_len` means the output was truncated. Passing a null `buf` or
/// `buf_len == 0` writes nothing and just returns the required length (query
/// mode). See `codec::copy_str_out`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_to_string(r: QuercusRational, buf: *mut u8, buf_len: usize) -> i32 {
	crate::codec::copy_str_out(&r.to_native().to_display_string(), buf, buf_len)
}

/// Parse the project-XML rational text form, e.g. `"30000/1001"`.
///
/// Returns the null sentinel when `s` is null, not valid UTF-8, or not a
/// parseable rational — callers cannot distinguish those cases from an
/// explicitly null input. The text form matches
/// [`quercus_core_rational_to_string`], so parse/format round-trips.
///
/// # Safety
///
/// `s` must point to `len` readable bytes that stay valid for the call; a null
/// pointer is allowed and returns the null sentinel.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_core_rational_from_string(s: *const u8, len: usize) -> QuercusRational {
	let Some(s) = (unsafe { crate::codec::str_arg(s, len) }) else {
		return quercus_core_rational_null();
	};
	QuercusRational::from_native(&Rational::from_string(s))
}

/// `timebase.time_to_timestamp(time)`: rational time to an integer
/// timestamp in the given timebase.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_time_to_timestamp(
	timebase: QuercusRational,
	time: QuercusRational,
) -> i64 {
	timebase.to_native().time_to_timestamp(time.to_native())
}

/// `timebase.timestamp_to_time(ts)`: integer timestamp back to rational
/// time in the given timebase.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_rational_timestamp_to_time(
	timebase: QuercusRational,
	ts: i64,
) -> QuercusRational {
	QuercusRational::from_native(&timebase.to_native().timestamp_to_time(ts))
}

// ---------------------------------------------------------------------------
// TimeRange
// ---------------------------------------------------------------------------

/// A normalized half-open range (endpoints swapped when `out < in`).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_timerange_new(in_: QuercusRational, out: QuercusRational) -> QuercusTimeRange {
	QuercusTimeRange::from_native(&TimeRange::new(in_.to_native(), out.to_native()))
}

/// The length of `r`, computed as `out - in`.
///
/// Returns the null/NaN rational when either endpoint is a
/// `RATIONAL_MIN`/`RATIONAL_MAX` sentinel (the subtraction propagates NaN, C++
/// parity). The range constructors normalize their endpoints, so lengths built
/// through them are non-negative; a hand-built struct with `out < in` yields a
/// negative length because the fields are public.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_timerange_length(r: QuercusTimeRange) -> QuercusRational {
	QuercusRational::from_native(&r.to_native().length())
}

/// True when `t` lies in the half-open interval `[in, out)` of `r`.
///
/// The `out` endpoint is exclusive, so a zero-length range contains nothing.
/// Comparisons use the crate's total order (the null/NaN rational sorts before
/// every value) and never panic on sentinel values.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_timerange_contains(r: QuercusTimeRange, t: QuercusRational) -> bool {
	r.to_native().contains(t.to_native())
}

/// The overlap of two half-open ranges `a` and `b`.
///
/// Computed as `max(a.in, b.in) .. min(a.out, b.out)` and then normalized
/// like every [`TimeRange`], so for **disjoint** ranges the endpoints are
/// swapped and the result is the gap between them (C++ parity), not an empty
/// range; touching ranges yield a zero-length range instead.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_timerange_intersected(
	a: QuercusTimeRange,
	b: QuercusTimeRange,
) -> QuercusTimeRange {
	QuercusTimeRange::from_native(&a.to_native().intersected(&b.to_native()))
}

/// The smallest range covering both `a` and `b` (the union hull).
///
/// Runs from `min(a.in, b.in)` to `max(a.out, b.out)` and is normalized, so
/// two disjoint ranges still yield one range spanning the gap between them
/// (C++ `TimeRange::combined`); it never merges into a list of ranges. Both
/// inputs are taken by value.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_timerange_combined(a: QuercusTimeRange, b: QuercusTimeRange) -> QuercusTimeRange {
	QuercusTimeRange::from_native(&a.to_native().combined(&b.to_native()))
}

// ---------------------------------------------------------------------------
// PixelFormat
// ---------------------------------------------------------------------------

/// Whether `format` is a known [`PixelFormat`] discriminant.
///
/// Accepted codes: `-1` (`Invalid`), `0` (`U8`), `1` (`U10`), `2` (`U16`),
/// `3` (`F16`) and `4` (`F32`); every other `i32` returns `false`. Mirrors the
/// conversion used by [`quercus_core_pixelformat_bytes_per_channel`] and
/// [`quercus_core_pixelformat_bytes_per_pixel`], which return `-1` for the
/// same invalid inputs.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_pixelformat_is_valid(format: i32) -> bool {
	pixel_format_from_i32(format).is_some()
}

/// Bytes per channel (Invalid reads as 0); -1 when `format` is not a
/// PixelFormat discriminant at all.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_pixelformat_bytes_per_channel(format: i32) -> i32 {
	match pixel_format_from_i32(format) {
		Some(f) => f.bytes_per_channel() as i32,
		None => -1,
	}
}

/// Bytes per pixel for `channels` channels; -1 on invalid input.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_pixelformat_bytes_per_pixel(format: i32, channels: i32) -> i32 {
	if channels < 0 {
		return -1;
	}
	match pixel_format_from_i32(format) {
		Some(f) => f.bytes_per_pixel(channels as usize) as i32,
		None => -1,
	}
}

// ---------------------------------------------------------------------------
// VideoParams
// ---------------------------------------------------------------------------

/// `repr(C)` mirror of quercus-core's `VideoParams`. Enums cross as their
/// discriminant ints (`format` is a PixelFormat code, `interlacing` is
/// 0/1/2, `video_type` 0/1/2, `color_range` 0/1, bools as 0/1);
/// `colorspace` is a fixed NUL-terminated buffer (63 bytes max).
#[repr(C)]
#[derive(Clone, Debug)]
pub struct QuercusVideoParams {
	pub width: i32,
	pub height: i32,
	pub depth: i32,
	pub time_base_num: i32,
	pub time_base_den: i32,
	pub frame_rate_num: i32,
	pub frame_rate_den: i32,
	pub pixel_aspect_num: i32,
	pub pixel_aspect_den: i32,
	pub format: i32,
	pub channel_count: i32,
	pub interlacing: i32,
	pub divider: i32,
	pub enabled: i32,
	pub x: f32,
	pub y: f32,
	pub stream_index: i32,
	pub video_type: i32,
	pub start_time: i64,
	pub duration: i64,
	pub premultiplied_alpha: i32,
	pub color_range: i32,
	pub color_primaries: i32,
	pub color_transfer: i32,
	pub colorspace: [u8; 64],
}

impl crate::handle::NativeMirror for QuercusVideoParams {
	type Native = VideoParams;

	fn from_native(v: &VideoParams) -> Self {
		let (time_base_num, time_base_den) = v.time_base();
		let (frame_rate_num, frame_rate_den) = v.frame_rate();
		let (pixel_aspect_num, pixel_aspect_den) = v.pixel_aspect_ratio();
		let mut colorspace = [0u8; 64];
		let name = v.colorspace().as_bytes();
		let n = name.len().min(63);
		colorspace[..n].copy_from_slice(&name[..n]);
		Self {
			width: v.width(),
			height: v.height(),
			depth: v.depth(),
			time_base_num,
			time_base_den,
			frame_rate_num,
			frame_rate_den,
			pixel_aspect_num,
			pixel_aspect_den,
			format: v.format().code(),
			channel_count: v.channel_count(),
			interlacing: v.interlacing() as i32,
			divider: v.divider(),
			enabled: v.enabled() as i32,
			x: v.x(),
			y: v.y(),
			stream_index: v.stream_index(),
			video_type: v.video_type() as i32,
			start_time: v.start_time(),
			duration: v.duration(),
			premultiplied_alpha: v.premultiplied_alpha() as i32,
			color_range: v.color_range() as i32,
			color_primaries: v.color_primaries(),
			color_transfer: v.color_transfer(),
			colorspace,
		}
	}

	fn to_native(&self) -> VideoParams {
		let mut v = VideoParams::new();
		v.set_width(self.width);
		v.set_height(self.height);
		v.set_depth(self.depth);
		v.set_time_base(self.time_base_num, self.time_base_den);
		v.set_frame_rate(self.frame_rate_num, self.frame_rate_den);
		v.set_pixel_aspect_ratio(self.pixel_aspect_num, self.pixel_aspect_den);
		v.set_format(VideoPixelFormat::from_code(self.format));
		v.set_channel_count(self.channel_count);
		v.set_interlacing(match self.interlacing {
			1 => Interlacing::TopFirst,
			2 => Interlacing::BottomFirst,
			_ => Interlacing::None,
		});
		v.set_divider(self.divider);
		v.set_enabled(self.enabled != 0);
		v.set_x(self.x);
		v.set_y(self.y);
		v.set_stream_index(self.stream_index);
		v.set_video_type(match self.video_type {
			1 => VideoType::Still,
			2 => VideoType::ImageSequence,
			_ => VideoType::Video,
		});
		v.set_start_time(self.start_time);
		v.set_duration(self.duration);
		v.set_premultiplied_alpha(self.premultiplied_alpha != 0);
		v.set_color_range(match self.color_range {
			1 => ColorRange::Full,
			_ => ColorRange::Limited,
		});
		v.set_color_primaries(self.color_primaries);
		v.set_color_transfer(self.color_transfer);
		let end = self
			.colorspace
			.iter()
			.position(|&b| b == 0)
			.unwrap_or(self.colorspace.len());
		v.set_colorspace(std::str::from_utf8(&self.colorspace[..end]).unwrap_or(""));
		v
	}
}

/// The default (invalid) parameter set.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_default() -> QuercusVideoParams {
	QuercusVideoParams::from_native(&VideoParams::new())
}

/// Whether the parameter set describes a usable video stream.
///
/// True only when width and height are positive, the pixel aspect ratio is
/// not the null rational, the pixel format is a real format (not
/// `Invalid`/`Count`) and `channel_count > 0`. The resolution divider is not
/// part of the check. Returns `false` for a null `params`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_is_valid(params: *const QuercusVideoParams) -> bool {
	if params.is_null() {
		return false;
	}
	unsafe { &*params }.to_native().is_valid()
}

/// Frame width after applying the resolution divider, i.e. the width a
/// preview actually processes.
///
/// The divider is applied with truncating integer division, so odd sizes lose
/// up to `divider - 1` columns. Returns `0` when `params` is null.
///
/// # Panics
///
/// Panics (and therefore aborts at the C ABI boundary) when the stored
/// divider is `0`, because the underlying division is unchecked; validate the
/// parameter set before calling.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_effective_width(params: *const QuercusVideoParams) -> i32 {
	if params.is_null() {
		return 0;
	}
	unsafe { &*params }.to_native().effective_width()
}

/// Frame height after applying the resolution divider, i.e. the height a
/// preview actually processes.
///
/// The divider is applied with truncating integer division, so odd sizes lose
/// up to `divider - 1` rows. Returns `0` when `params` is null.
///
/// # Panics
///
/// Panics (and therefore aborts at the C ABI boundary) when the stored
/// divider is `0`, because the underlying division is unchecked; validate the
/// parameter set before calling.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_effective_height(params: *const QuercusVideoParams) -> i32 {
	if params.is_null() {
		return 0;
	}
	unsafe { &*params }.to_native().effective_height()
}

/// Bytes per pixel for this parameter set; -1 when params is null.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_bytes_per_pixel(params: *const QuercusVideoParams) -> i32 {
	if params.is_null() {
		return -1;
	}
	unsafe { &*params }.to_native().bytes_per_pixel()
}

/// Total bytes for one uncompressed frame of this parameter set.
///
/// Computed as `width * height * bytes_per_pixel` with wrapping `i32`
/// arithmetic (C++ parity), using the raw width/height — not the
/// divider-applied effective sizes — and adding no row alignment or padding;
/// an unknown pixel format contributes 0 bytes. Returns `-1` when `params` is
/// null; the pointer is read but not retained.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_core_videoparams_buffer_size(params: *const QuercusVideoParams) -> i32 {
	if params.is_null() {
		return -1;
	}
	unsafe { &*params }.to_native().buffer_size()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn r(num: i64, den: i64) -> QuercusRational {
		quercus_core_rational_new(num, den)
	}

	#[test]
	fn rational_new_reduces_and_normalizes_sign() {
		assert_eq!(r(2, 4), QuercusRational { num: 1, den: 2 });
		assert_eq!(r(1, -2), QuercusRational { num: -1, den: 2 });
		assert_eq!(r(0, 5), QuercusRational { num: 0, den: 1 });
		assert_eq!(r(3, 0), QuercusRational { num: 0, den: 0 }, "den 0 is the null sentinel");
	}

	#[test]
	fn rational_null_nan_and_cmp() {
		assert!(quercus_core_rational_is_null(quercus_core_rational_null()));
		assert!(!quercus_core_rational_is_null(r(1, 2)));
		assert!(!quercus_core_rational_is_nan(r(1, 2)));
		// The INT_MAX sentinel propagates NaN through arithmetic.
		let nan = quercus_core_rational_add(r(i32::MAX as i64, 1), r(i32::MAX as i64, 1));
		assert!(quercus_core_rational_is_nan(nan), "overflow sentinel: {nan:?}");

		assert_eq!(quercus_core_rational_cmp(r(1, 2), r(2, 3)), -1);
		assert_eq!(quercus_core_rational_cmp(r(1, 2), r(1, 2)), 0);
		assert_eq!(quercus_core_rational_cmp(r(3, 2), r(2, 3)), 1);
	}

	#[test]
	fn rational_arithmetic() {
		assert_eq!(quercus_core_rational_add(r(1, 2), r(1, 3)), r(5, 6));
		assert_eq!(quercus_core_rational_sub(r(1, 2), r(1, 3)), r(1, 6));
		assert_eq!(quercus_core_rational_mul(r(2, 3), r(3, 4)), r(1, 2));
		assert_eq!(quercus_core_rational_div(r(1, 2), r(1, 4)), r(2, 1));
	}

	#[test]
	fn rational_f64_and_string_roundtrips() {
		assert_eq!(quercus_core_rational_to_f64(r(1, 2)), 0.5);
		assert_eq!(quercus_core_rational_from_f64(0.5), r(1, 2));

		let rate = r(30000, 1001);
		let mut buf = [0u8; 32];
		let n = quercus_core_rational_to_string(rate, buf.as_mut_ptr(), buf.len());
		assert_eq!(n, "30000/1001".len() as i32);
		let parsed = unsafe { quercus_core_rational_from_string(buf.as_ptr(), n as usize) };
		assert_eq!(parsed, rate);

		let invalid = unsafe { quercus_core_rational_from_string("nope".as_ptr(), 4) };
		assert!(quercus_core_rational_is_null(invalid));
	}

	#[test]
	fn rational_timebase_conversions_roundtrip() {
		let timebase = r(1, 25);
		let ts = quercus_core_rational_time_to_timestamp(timebase, r(2, 1));
		assert_eq!(ts, 50);
		assert_eq!(quercus_core_rational_timestamp_to_time(timebase, ts), r(2, 1));
	}

	#[test]
	fn timerange_normalizes_and_measures() {
		// Endpoints are swapped when out < in.
		let range = quercus_core_timerange_new(r(10, 1), r(5, 1));
		assert_eq!(range.in_, r(5, 1));
		assert_eq!(range.out, r(10, 1));
		assert_eq!(quercus_core_timerange_length(range), r(5, 1));

		// Half-open: in is inclusive, out is exclusive.
		assert!(quercus_core_timerange_contains(range, r(5, 1)));
		assert!(!quercus_core_timerange_contains(range, r(10, 1)));
		assert!(!quercus_core_timerange_contains(range, r(4, 1)));
	}

	#[test]
	fn timerange_intersection_and_combination() {
		let a = quercus_core_timerange_new(r(0, 1), r(10, 1));
		let b = quercus_core_timerange_new(r(5, 1), r(15, 1));

		let inter = quercus_core_timerange_intersected(a, b);
		assert_eq!(inter.in_, r(5, 1));
		assert_eq!(inter.out, r(10, 1));

		let comb = quercus_core_timerange_combined(a, b);
		assert_eq!(comb.in_, r(0, 1));
		assert_eq!(comb.out, r(15, 1));
	}

	#[test]
	fn pixelformat_validity_and_sizes() {
		assert!(quercus_core_pixelformat_is_valid(-1));
		assert!(quercus_core_pixelformat_is_valid(4));
		assert!(!quercus_core_pixelformat_is_valid(5));

		assert_eq!(quercus_core_pixelformat_bytes_per_channel(4), 4); // F32
		assert_eq!(quercus_core_pixelformat_bytes_per_channel(0), 1); // U8
		assert_eq!(quercus_core_pixelformat_bytes_per_channel(-1), 0); // Invalid
		assert_eq!(quercus_core_pixelformat_bytes_per_channel(42), -1); // garbage

		assert_eq!(quercus_core_pixelformat_bytes_per_pixel(4, 4), 16);
		assert_eq!(quercus_core_pixelformat_bytes_per_pixel(4, -1), -1);
	}

	#[test]
	fn videoparams_default_and_roundtrip() {
		let mut p = quercus_core_videoparams_default();
		assert_eq!((p.width, p.height, p.divider), (0, 0, 1));
		assert_eq!(p.format, -1, "invalid pixel format by default");
		assert!(!quercus_core_videoparams_is_valid(&p));

		// Round-trip through native: every field survives.
		p.width = 1920;
		p.height = 1080;
		p.format = 4; // F32
		p.channel_count = 4;
		p.time_base_num = 1;
		p.time_base_den = 25;
		let name = b"acescg";
		p.colorspace[..name.len()].copy_from_slice(name);
		let native = p.to_native();
		let back = QuercusVideoParams::from_native(&native);
		assert_eq!((back.width, back.height, back.format, back.channel_count), (1920, 1080, 4, 4));
		assert_eq!((back.time_base_num, back.time_base_den), (1, 25));
		assert_eq!(&back.colorspace[..6], b"acescg");
		assert!(quercus_core_videoparams_is_valid(&back));

		// Computed helpers follow native semantics.
		p.divider = 2;
		assert_eq!(quercus_core_videoparams_effective_width(&p), 960);
		assert_eq!(quercus_core_videoparams_effective_height(&p), 540);
		assert!(quercus_core_videoparams_bytes_per_pixel(&p) > 0);
		assert!(quercus_core_videoparams_buffer_size(&p) > 0);

		// Null reads are safe.
		assert!(!quercus_core_videoparams_is_valid(std::ptr::null()));
		assert_eq!(quercus_core_videoparams_bytes_per_pixel(std::ptr::null()), -1);
	}
}
