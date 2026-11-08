//! Parametric curves (`quercus_ofx_curveset_*`): the curve model behind
//! `OfxParamTypeParametric` parameters (segment-wise cubic Hermite;
//! see `quercus_ofx_plugin::param_curve`), as a refcounted handle.
//!
//! JSON is the interchange format with node values and project files
//! (`curves_to_json`/`curves_from_json`); the handle API is for
//! building/inspecting curves without string munging.

use std::sync::Mutex;

use quercus_ofx_plugin::param_curve::{self, ControlPoint, Curve};

use crate::codec::{copy_str_out, str_arg};
use crate::handle::NativeMirror;

/// A curve control point (`repr(C)` mirror of
/// `quercus_ofx_plugin::param_curve::ControlPoint` — see
/// [`NativeMirror`](crate::handle::NativeMirror)): `key` is the
/// parametric position, `value` the result, `slope` the first
/// derivative (Hermite tangent; recomputed automatically by the
/// mutating calls).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusOfxCurvePoint {
	pub key: f64,
	pub value: f64,
	pub slope: f64,
}

impl crate::handle::NativeMirror for QuercusOfxCurvePoint {
	type Native = ControlPoint;

	fn from_native(p: &ControlPoint) -> Self {
		Self {
			key: p.key,
			value: p.value,
			slope: p.slope,
		}
	}

	fn to_native(&self) -> ControlPoint {
		ControlPoint {
			key: self.key,
			value: self.value,
			slope: self.slope,
		}
	}
}

/// A set of parametric curves (one per dimension of a parametric
/// parameter). Refcounted across the ABI
/// ([`quercus_ofx_curveset_add_ref`]/[`quercus_ofx_curveset_release`]).
pub struct OfxCurveSet {
	curves: Mutex<Vec<Curve>>,
}

crate::export_handle!(
	OfxCurveSet,
	quercus_ofx_curveset_add_ref,
	quercus_ofx_curveset_release
);

/// A new, empty curve set (ref count 1).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_curveset_new() -> *mut OfxCurveSet {
	crate::handle::into_ffi(OfxCurveSet {
		curves: Mutex::new(Vec::new()),
	}) as *mut OfxCurveSet
}

/// Parses curve-set JSON (the payload node values and project files carry).
///
/// `json`/`json_len` name a UTF-8 byte slice — not NUL-terminated — holding a
/// document produced by [`quercus_ofx_curveset_to_json`].
///
/// # Returns
///
/// A new handle with reference count 1 (balanced by one
/// `quercus_ofx_curveset_release`), or null on a null or invalid-UTF-8 input,
/// or on malformed JSON.
///
/// # Safety
///
/// `json` must be null or point to `json_len` readable bytes; the slice is
/// only borrowed for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_from_json(
	json: *const u8,
	json_len: usize,
) -> *mut OfxCurveSet {
	let Some(json) = (unsafe { str_arg(json, json_len) }) else {
		return std::ptr::null_mut();
	};
	let Some(curves) = param_curve::curves_from_json(json) else {
		return std::ptr::null_mut();
	};
	crate::handle::into_ffi(OfxCurveSet {
		curves: Mutex::new(curves),
	}) as *mut OfxCurveSet
}

/// Serializes the curve set to JSON, snprintf-style.
///
/// Writes a UTF-8, NUL-terminated document into `buf`, truncated to
/// `buf_len - 1` bytes when the buffer is too small; a null `buf` (or
/// `buf_len == 0`) queries the size without writing anything. This is the
/// payload node values and project files carry.
///
/// # Returns
///
/// The full JSON byte length excluding the NUL terminator, so a caller can
/// size its buffer first and detect truncation with `ret >= buf_len`; `-1` on
/// a null handle.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. `buf` must be null or valid for writing `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_to_json(
	set: *const OfxCurveSet,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if set.is_null() {
		return -1;
	}
	let curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let json = param_curve::curves_to_json(&curves);
	drop(curves);
	copy_str_out(&json, buf, buf_len)
}

/// Number of curves in the set, one per dimension of the parametric
/// parameter.
///
/// # Returns
///
/// The count, or `-1` on a null handle.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_curve_count(set: *const OfxCurveSet) -> i32 {
	if set.is_null() {
		return -1;
	}
	unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.len() as i32
}

/// Appends an empty curve to the set.
///
/// The new curve has no control points, so it evaluates as identity (see
/// [`quercus_ofx_curveset_evaluate`]) until
/// `quercus_ofx_curveset_upsert_point`/`_push_point` adds some.
///
/// # Returns
///
/// The index of the new curve, or `-1` on a null handle.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_add_curve(set: *const OfxCurveSet) -> i32 {
	if set.is_null() {
		return -1;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	curves.push(Curve::empty());
	(curves.len() - 1) as i32
}

/// Appends an identity curve over `[lo, hi]` — the parametric default.
///
/// `lo`/`hi` are the parameter's value range; the new curve gets the two
/// endpoint control points (`lo → lo`, `hi → hi`) and evaluates as `y = x`
/// between them. Used when a parametric parameter is created without a stored
/// curve.
///
/// # Returns
///
/// The index of the new curve, or `-1` on a null handle.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_add_identity_curve(
	set: *const OfxCurveSet,
	lo: f64,
	hi: f64,
) -> i32 {
	if set.is_null() {
		return -1;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	curves.push(Curve::identity(lo, hi));
	(curves.len() - 1) as i32
}

/// Number of control points in curve `curve`.
///
/// # Returns
///
/// `0` for an empty curve, or `-1` on a null handle or an out-of-range
/// `curve` index.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_point_count(set: *const OfxCurveSet, curve: i32) -> i32 {
	if set.is_null() || curve < 0 {
		return -1;
	}
	unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(curve as usize)
		.map(|c| c.len() as i32)
		.unwrap_or(-1)
}

/// Reads control point `index` of curve `curve` into `out`.
///
/// `index` is the position in the curve's key-sorted point list; on success
/// `*out` receives the point's key/value/slope as the `repr(C)` mirror of the
/// native `ControlPoint`. `out` is left untouched on failure.
///
/// # Returns
///
/// `true` when `*out` was written; `false` on a null handle or `out`, or when
/// `curve`/`index` is negative or out of range.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released, and `out` must be null or valid for writing one
/// `QuercusOfxCurvePoint`. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_point_at(
	set: *const OfxCurveSet,
	curve: i32,
	index: i32,
	out: *mut QuercusOfxCurvePoint,
) -> bool {
	if set.is_null() || out.is_null() || curve < 0 || index < 0 {
		return false;
	}
	let curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let Some(point) = curves
		.get(curve as usize)
		.and_then(|c| c.nth(index as usize))
	else {
		return false;
	};
	unsafe { *out = QuercusOfxCurvePoint::from_native(point) };
	true
}

/// Inserts or overwrites the control point at `key` in curve `curve`.
///
/// `key` is the parametric position and `value` the curve value there. Key
/// order is maintained and the neighbouring slopes are recomputed (unlike
/// [`quercus_ofx_curveset_push_point`], which keeps an explicit slope); this
/// is what the curve editor calls while dragging a point.
///
/// # Returns
///
/// `true` when the point was stored; `false` on a null handle or an
/// out-of-range `curve` index.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_upsert_point(
	set: *const OfxCurveSet,
	curve: i32,
	key: f64,
	value: f64,
) -> bool {
	if set.is_null() || curve < 0 {
		return false;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let Some(c) = curves.get_mut(curve as usize) else {
		return false;
	};
	c.upsert(key, value);
	true
}

/// Inserts a control point with its explicit Hermite slope preserved.
///
/// `point.key` keeps the curve's points sorted and `point.slope` is stored
/// as-is (unlike [`quercus_ofx_curveset_upsert_point`], which recomputes
/// slopes); a point with the same key is overwritten. `curve` is the curve
/// index.
///
/// # Returns
///
/// `true` when the point was inserted or replaced; `false` on a null handle or
/// an out-of-range `curve` index.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_push_point(
	set: *const OfxCurveSet,
	curve: i32,
	point: QuercusOfxCurvePoint,
) -> bool {
	if set.is_null() || curve < 0 {
		return false;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let Some(curve) = curves.get_mut(curve as usize) else {
		return false;
	};
	let point = point.to_native();
	let index = curve.points.partition_point(|existing| existing.key < point.key);
	if curve.points.get(index).is_some_and(|existing| existing.key == point.key) {
		curve.points[index] = point;
	} else {
		curve.points.insert(index, point);
	}
	true
}

/// Deletes control point `index` of curve `curve`.
///
/// `index` is the position in the curve's key-sorted point list; the
/// neighbouring slopes are recomputed afterwards.
///
/// # Returns
///
/// `true` when the point was deleted; `false` on a null handle, a negative
/// `curve`/`index`, or when either is out of range.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_delete_point(
	set: *const OfxCurveSet,
	curve: i32,
	index: i32,
) -> bool {
	if set.is_null() || curve < 0 || index < 0 {
		return false;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let Some(c) = curves.get_mut(curve as usize) else {
		return false;
	};
	c.delete_nth(index as usize).is_ok()
}

/// Removes all control points of curve `curve`.
///
/// The curve then evaluates as identity (`y = x`); this is how a curve is
/// reset to the parametric default without removing the curve itself.
///
/// # Returns
///
/// `true` when the curve was cleared; `false` on a null handle or an
/// out-of-range `curve` index.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_clear_curve(set: *const OfxCurveSet, curve: i32) -> bool {
	if set.is_null() || curve < 0 {
		return false;
	}
	let mut curves = unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let Some(c) = curves.get_mut(curve as usize) else {
		return false;
	};
	c.clear();
	true
}

/// Evaluates curve `curve` of the set at `x` (cubic Hermite).
///
/// Keys outside the curve's key range clamp to the endpoint values and an
/// empty curve evaluates as identity (`x`), matching
/// `quercus_ofx_plugin::param_curve`. The parametric-parameter widget uses
/// this to sample the curve it draws.
///
/// # Returns
///
/// The curve's value at `x`, or `NaN` on a null handle or an out-of-range
/// `curve` index.
///
/// # Safety
///
/// `set` must be null or a live curve-set handle obtained from
/// `quercus_ofx_curveset_new`/`_from_json` whose last reference has not been
/// released. The internal mutex serializes concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_curveset_evaluate(
	set: *const OfxCurveSet,
	curve: i32,
	x: f64,
) -> f64 {
	if set.is_null() || curve < 0 {
		return f64::NAN;
	}
	unsafe { &*set }
		.curves
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(curve as usize)
		.map(|c| c.evaluate(x))
		.unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn build_edit_evaluate_roundtrip() {
		unsafe {
			let set = quercus_ofx_curveset_new();
			assert!(!set.is_null());
			assert_eq!(quercus_ofx_curveset_curve_count(set), 0);

			assert_eq!(quercus_ofx_curveset_add_identity_curve(set, 0.0, 1.0), 0);
			assert_eq!(quercus_ofx_curveset_point_count(set, 0), 2);
			assert_eq!(quercus_ofx_curveset_evaluate(set, 0, 0.5), 0.5);

			// Upsert a midpoint and read it back (sorted by key).
			assert!(quercus_ofx_curveset_upsert_point(set, 0, 0.5, 0.25));
			assert_eq!(quercus_ofx_curveset_point_count(set, 0), 3);
			let mut p = QuercusOfxCurvePoint::default();
			assert!(quercus_ofx_curveset_point_at(set, 0, 1, &mut p));
			assert_eq!((p.key, p.value), (0.5, 0.25));
			assert!(quercus_ofx_curveset_evaluate(set, 0, 0.5).is_finite());

			// JSON roundtrip preserves the model.
			let n = quercus_ofx_curveset_to_json(set, std::ptr::null_mut(), 0);
			assert!(n > 0);
			let mut buf = vec![0u8; n as usize + 1];
			assert_eq!(
				quercus_ofx_curveset_to_json(set, buf.as_mut_ptr(), buf.len()),
				n
			);
			buf.truncate(n as usize);
			let set2 = quercus_ofx_curveset_from_json(buf.as_ptr(), buf.len());
			assert!(!set2.is_null());
			assert_eq!(quercus_ofx_curveset_curve_count(set2), 1);
			assert_eq!(quercus_ofx_curveset_point_count(set2, 0), 3);
			assert_eq!(
				quercus_ofx_curveset_evaluate(set, 0, 0.5),
				quercus_ofx_curveset_evaluate(set2, 0, 0.5)
			);

			// Edit: delete, clear, out-of-range failures.
			assert!(quercus_ofx_curveset_delete_point(set2, 0, 1));
			assert_eq!(quercus_ofx_curveset_point_count(set2, 0), 2);
			assert!(quercus_ofx_curveset_clear_curve(set2, 0));
			assert_eq!(quercus_ofx_curveset_point_count(set2, 0), 0);
			assert_eq!(
				quercus_ofx_curveset_evaluate(set2, 0, 0.7),
				0.7,
				"empty is identity"
			);
			assert!(quercus_ofx_curveset_point_at(set2, 0, 0, &mut p) == false);
			assert!(!quercus_ofx_curveset_upsert_point(set2, 9, 0.0, 0.0));
			assert!(quercus_ofx_curveset_evaluate(set2, 9, 0.0).is_nan());

			quercus_ofx_curveset_release(set);
			quercus_ofx_curveset_release(set2);
			quercus_ofx_curveset_release(std::ptr::null());
		}
	}

	#[test]
	fn null_and_malformed_input_fail_closed() {
		unsafe {
			assert!(quercus_ofx_curveset_from_json(std::ptr::null(), 0).is_null());
			assert!(quercus_ofx_curveset_from_json("{".as_ptr(), 1).is_null());
			assert_eq!(quercus_ofx_curveset_curve_count(std::ptr::null()), -1);
			assert_eq!(
				quercus_ofx_curveset_to_json(std::ptr::null(), std::ptr::null_mut(), 0),
				-1
			);
		}
	}
}
