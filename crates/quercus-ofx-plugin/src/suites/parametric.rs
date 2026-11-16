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

//! OfxParametricParameterSuite v1: parametric-curve parameters (curve/LUT
//! parameters of color-grading plugins).
//!
//! Semantics follow ofxParametricParam.h (the property table is also in HS
//! ofxhParam.cpp:285-330):
//! - curve = one [`crate::param_curve::Curve`] per dimension, stored in the
//!   parameter value [`crate::param::ParamValue::Parametric`] (def default /
//!   instance current value; `ParamInstance::from_def` copies the default);
//! - dimension and range come from the parameter properties
//!   [`crate::param::P_PARAMETRIC_DIMENSION`] (int, default 1) and
//!   [`crate::param::P_PARAMETRIC_RANGE`] (double×2, default (0,1)) —
//!   pre-set by paramDefine, changeable by the plugin during describe;
//! - evaluation = cubic Hermite (slope auto-differentiated); out-of-range
//!   parametricPosition values are clamped to the endpoint values, see
//!   [`crate::param_curve::Curve::evaluate`];
//! - Set/Add/Delete during describe (Def) change the **default value**
//!   (HS: "If a plugin wishes to set a different default value for a curve,
//!   it can use the suite to set key/value pairs on the descriptor. When a
//!   new instance is made, it will have these curve values as a default");
//! - phase 1 has no parameter animation (host
//!   `kOfxParamHostPropSupportsParametricAnimation = 0`): `time` is always
//!   ignored (curves are instance-level static), and `addAnimationKey` is
//!   ignored likewise;
//! - an unconfigured curve (inside the dimension but never written) evaluates
//!   and reads as the identity default — consistent with "the default default
//!   is an identity lookup"; every write path first fills in missing curves
//!   with the identity default before storing;
//! - error codes: bad handle → BadHandle, non-parametric parameter →
//!   BadHandle, curveIndex / nth out of range → BadIndex (the "Unknown"
//!   written in the header's GetNth/Set/Add docs is a generic term for
//!   unknown types; this implementation follows the task convention and
//!   normalizes to BadHandle/BadIndex).

use std::borrow::Cow;
use std::env;
use std::ffi::{c_double, c_int, c_void};
use std::panic;
use std::sync;

use crate::param::{ParamDef, ParamInstance, ParamValue};
use crate::param_curve::Curve;
use crate::property::{PropertySet, Value};
use crate::suites::{status, tag};

/// Function-table layout (matches the SDK
/// `OfxParametricParameterSuiteV1`; field order follows
/// ofxParametricParam.h).
#[repr(C)]
pub struct ParametricParameterSuiteV1 {
	/// parametricParamGetValue: evaluate the curve (out-of-range position
	/// clamped to the endpoint values).
	pub parametric_param_get_value:
		unsafe extern "C" fn(*mut c_void, c_int, c_double, c_double, *mut c_double) -> c_int,
	/// parametricParamGetNControlPoints: number of control points.
	pub parametric_param_get_n_control_points:
		unsafe extern "C" fn(*mut c_void, c_int, c_double, *mut c_int) -> c_int,
	/// parametricParamGetNthControlPoint: key/value of the nth control point.
	pub parametric_param_get_nth_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_int,
		*mut c_double,
		*mut c_double,
	) -> c_int,
	/// parametricParamSetNthControlPoint: replace the nth control point
	/// (re-sorted after a key change, slopes recomputed).
	pub parametric_param_set_nth_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_int,
		c_double,
		c_double,
		c_int,
	) -> c_int,
	/// parametricParamAddControlPoint: add/overwrite a control point (kept
	/// sorted).
	pub parametric_param_add_control_point:
		unsafe extern "C" fn(*mut c_void, c_int, c_double, c_double, c_double, c_int) -> c_int,
	/// parametricParamDeleteControlPoint: delete the nth control point.
	pub parametric_param_delete_control_point:
		unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
	/// parametricParamDeleteAllControlPoints: clear one dimension's curve.
	pub parametric_param_delete_all_control_points:
		unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

// ---- 句柄解析 -----------------------------------------------------------

/// param handle: a definition during describe (value = default), an instance
/// during instance phase. `Def` holds `&mut` (Set/Add/Delete edit the default
/// directly); `Instance` holds `&` (the current value is changed through the
/// internal Mutex of [`ParamInstance::set_ofx`]).
enum ParamRef<'a> {
	Def(&'a mut ParamDef),
	Instance(&'a ParamInstance),
}

/// Resolve a param handle passed in by the plugin into a usable parameter
/// reference.
///
/// The handle is the address of a tagged property set (see
/// [`crate::suites::tag`]): tag [`tag::PARAM_DEF`] resolves to a
/// describe-phase [`ParamDef`] (whose default can be edited directly), and
/// [`tag::PARAM_INSTANCE`] to an instance-phase [`ParamInstance`] (writes go
/// through its internal Mutex). This is the first step of every parametric
/// suite entry point.
///
/// Failure: when `handle` is a null pointer, or its tag is neither of the two
/// above (for example an effect / clip handle), returns
/// `kOfxStatErrBadHandle` and never yields a reference of the wrong type.
fn resolve_param(handle: *mut c_void) -> Result<ParamRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::PARAM_DEF => Ok(ParamRef::Def(&mut *(tag::strip(handle) as *mut ParamDef))),
			tag::PARAM_INSTANCE => Ok(ParamRef::Instance(
				&*(tag::strip(handle) as *const ParamInstance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// Common template for all suite entries: wrap the closure in
/// `catch_unwind` and map the result to an OFX status code.
///
/// `f` returning `Ok(())` → `kOfxStatOK`; returning `Err(code)` → that code
/// passes through unchanged; a closure panic → `kOfxStatFailed`, so unwinding
/// never crosses the C ABI. When the `QUERCUS_OFX_TRACE` environment variable
/// is set, non-OK results are printed to stderr together with the call site
/// recorded by `#[track_caller]`, to locate the failing suite entry.
///
/// Each suite function wraps only one `caught` layer; error codes propagate
/// upward through the inner `?` operators.
#[track_caller]
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = panic::Location::caller();
	let code = panic::catch_unwind(panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if code != status::OK && env::var_os("QUERCUS_OFX_TRACE").is_some() {
		eprintln!("[ofx] parametric suite error {code} at {caller}");
	}
	code
}

// ---- 模型存取 -----------------------------------------------------------

/// Fetch the parameter curves (Def = default value, Instance = current
/// value). A value that is not a parametric curve → BadHandle (the suite
/// semantics for a non-parametric parameter; checked before the index so a
/// non-parametric parameter always reports BadHandle).
fn curves_of(r: &ParamRef) -> Result<Vec<Curve>, c_int> {
	match r {
		ParamRef::Def(d) => match &d.default {
			ParamValue::Parametric(c) => Ok(c.clone()),
			_ => Err(status::ERR_BAD_HANDLE),
		},
		ParamRef::Instance(p) => match p.get() {
			ParamValue::Parametric(c) => Ok(c),
			_ => Err(status::ERR_BAD_HANDLE),
		},
	}
}

/// Write the whole curve set back to the parameter and let the host observe
/// the change.
///
/// `r` is the reference resolved by [`resolve_param`]; `curves` covers all
/// dimensions of the parameter (the caller first pads missing dimensions with
/// [`ensure_curve`]).
///
/// - Def (describe phase): replaces `ParamDef::default` directly, leaving
///   existing instances untouched;
/// - Instance (instance phase): writes the current value through
///   [`ParamInstance::set_ofx`] (undoable, the same path as scalar
///   parameters), then calls [`crate::suites::param::notify_changed`] to
///   notify the host `instanceChanged` — the curves are serialized to JSON
///   text by [`crate::param::notify_instance_changed`] and written back to the
///   node input.
fn write_curves(r: &mut ParamRef, curves: Vec<Curve>) {
	match r {
		ParamRef::Def(d) => d.default = ParamValue::Parametric(curves),
		ParamRef::Instance(p) => {
			let name = p.def.name.clone();
			let addr = &p.props as *const _ as usize;
			p.set_ofx(ParamValue::Parametric(curves));
			crate::suites::param::notify_changed(addr, &name);
		}
	}
}

/// Fetch the property set behind the handle.
///
/// By the handle convention the object stores `props: PropertySet` at offset
/// 0, and [`tag::strip`] removes the low 3 tag bits to yield the property-set
/// address, so no type lookup is needed. The returned reference is modeled
/// with a `&'static` lifetime. `handle` must be non-null and come from an
/// object tagged by this crate; this function cannot verify that, so the
/// caller must guarantee it.
fn props_of(handle: *mut c_void) -> &'static PropertySet {
	unsafe { &*tag::strip(handle) }
}

/// Dimension: `OfxParamPropParametricDimension` (int, default 1; a value ≤ 0
/// is defensively treated as 1 — the header requires "greater than 0").
fn dimension_of(handle: *mut c_void) -> usize {
	match props_of(handle).get(crate::param::P_PARAMETRIC_DIMENSION, 0) {
		Some(Value::Int(d)) => d.max(1) as usize,
		_ => 1,
	}
}

/// Range: `OfxParamPropParametricRange` (double×2, default (0,1)) — the
/// endpoints used when filling in the identity default for unconfigured
/// curves.
fn range_of(handle: *mut c_void) -> (f64, f64) {
	let props = props_of(handle);
	let lo = match props.get(crate::param::P_PARAMETRIC_RANGE, 0) {
		Some(Value::Double(d)) => d,
		_ => 0.0,
	};
	let hi = match props.get(crate::param::P_PARAMETRIC_RANGE, 1) {
		Some(Value::Double(d)) => d,
		_ => 1.0,
	};
	(lo, hi)
}

/// Validate and normalize the `curve_index` passed by the plugin.
///
/// The valid range is `0..dimension_of(handle)`, with the dimension taken
/// from the parameter property [`crate::param::P_PARAMETRIC_DIMENSION`]
/// (default 1). A negative value or one ≥ the dimension returns
/// `kOfxStatErrBadIndex`; on success it returns a usable `usize` index that
/// can directly index the curve set.
fn check_curve_index(handle: *mut c_void, curve_index: c_int) -> Result<usize, c_int> {
	let ci = usize::try_from(curve_index).map_err(|_| status::ERR_BAD_INDEX)?;
	if ci >= dimension_of(handle) {
		return Err(status::ERR_BAD_INDEX);
	}
	Ok(ci)
}

/// Read the `ci`-th curve.
///
/// `curves` is the parameter's current curve set and `range` its range (see
/// [`crate::param::P_PARAMETRIC_RANGE`], default (0, 1)). A curve inside the
/// dimension but never written is not stored, so an identity curve
/// ([`Curve::identity`]) is constructed on the fly to maintain
/// "unconfigured curve = identity lookup"; the returned [`Cow`] distinguishes
/// a borrow from a temporary and never modifies the parameter.
fn curve_at<'a>(curves: &'a [Curve], ci: usize, range: (f64, f64)) -> Cow<'a, Curve> {
	match curves.get(ci) {
		Some(c) => Cow::Borrowed(c),
		None => Cow::Owned(Curve::identity(range.0, range.1)),
	}
}

/// Ensure the `ci`-th curve exists and return a writable reference.
///
/// Write-path only: when `curves` is shorter than `ci`, pads with identity
/// defaults using `range` (the range) one by one until the index is
/// reachable; existing curves are preserved. Unlike the read-only
/// [`curve_at`], the padded result is stored together with this
/// [`write_curves`] call.
fn ensure_curve(curves: &mut Vec<Curve>, ci: usize, range: (f64, f64)) -> &mut Curve {
	while curves.len() <= ci {
		curves.push(Curve::identity(range.0, range.1));
	}
	&mut curves[ci]
}

// ---- suite 函数 ---------------------------------------------------------

/// `parametricParamGetValue`: evaluate the `curve_index`-th curve at
/// `parametric_position`.
///
/// `parametric_position` is the curve's independent variable, in the same
/// unit as the range [`crate::param::P_PARAMETRIC_RANGE`]; out-of-range
/// values are clamped to the endpoint values instead of reporting an error.
/// `time` is ignored (the host declares no parameter animation support, so
/// curves are instance-level static). On success the result is written to
/// `out`, in the same unit as the curve value.
///
/// Failure: null `out` → `kOfxStatErrValue`; bad handle / non-parametric
/// parameter → `kOfxStatErrBadHandle`; out-of-range `curve_index` →
/// `kOfxStatErrBadIndex`. Before any failure `out` is set to 0.0, so the
/// caller never reads a stale value.
///
/// # Safety
///
/// `out` must be a writable pointer to one `c_double`; `param` must be a
/// live param handle passed by the host to this suite.
unsafe extern "C" fn parametric_param_get_value(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	parametric_position: c_double,
	out: *mut c_double,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *out = 0.0 };
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let range = range_of(param);
		let v = curve_at(&curves, ci, range).evaluate(parametric_position);
		unsafe { *out = v };
		Ok(())
	})
}

/// `parametricParamGetNControlPoints`: read the control-point count of the
/// `curve_index`-th curve.
///
/// An unconfigured curve counts as the identity default, hence at least 2
/// points. `time` is ignored (no parameter animation). On success the count
/// is written to `out`.
///
/// Failure: null `out` → `kOfxStatErrValue`; bad handle / non-parametric
/// parameter → `kOfxStatErrBadHandle`; out-of-range `curve_index` →
/// `kOfxStatErrBadIndex`; `out` is set to 0 before failing.
///
/// # Safety
///
/// `out` must be a writable pointer to one `c_int`; `param` must be a live
/// param handle passed by the host.
unsafe extern "C" fn parametric_param_get_n_control_points(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	out: *mut c_int,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *out = 0 };
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let n = curve_at(&curves, ci, range_of(param)).len();
		unsafe { *out = n as c_int };
		Ok(())
	})
}

/// `parametricParamGetNthControlPoint`: read the (key, value) of the `nth`
/// control point on the `curve_index`-th curve.
///
/// Control points are stored in ascending key order; `nth` is their ordinal,
/// not the key. An unconfigured curve reads as the identity default. `time`
/// is ignored (no parameter animation). On success writes `key` and `value`
/// separately.
///
/// Failure: a null `key` or `value` → `kOfxStatErrValue`; bad handle /
/// non-parametric parameter → `kOfxStatErrBadHandle`; out-of-range
/// `curve_index` or `nth` → `kOfxStatErrBadIndex`; both outs are set to 0.0
/// before failing.
///
/// # Safety
///
/// `key` and `value` must each point to one `c_double`; `param` must be a
/// live param handle passed by the host.
unsafe extern "C" fn parametric_param_get_nth_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	nth: c_int,
	key: *mut c_double,
	value: *mut c_double,
) -> c_int {
	caught(|| {
		if key.is_null() || value.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe {
			*key = 0.0;
			*value = 0.0;
		};
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		let c = curve_at(&curves, ci, range_of(param));
		let p = c.nth(nth).ok_or(status::ERR_BAD_INDEX)?;
		unsafe {
			*key = p.key;
			*value = p.value;
		};
		Ok(())
	})
}

/// `parametricParamSetNthControlPoint`: replace the `nth` control point of
/// the `curve_index`-th curve with (key, value).
///
/// Changing the key may break ascending key order (the SDK header warns about
/// this explicitly): the model removes the point first, re-inserts it ordered
/// by the new key (a colliding key overwrites), and recomputes the adjacent
/// slopes ([`Curve::set_nth`]). Writes go through [`write_curves`], and on
/// success the host receives `instanceChanged`. Both `time` and
/// `addAnimationKey` are ignored (no parameter animation).
///
/// Failure: bad handle / non-parametric parameter → `kOfxStatErrBadHandle`;
/// out-of-range `curve_index` or `nth` → `kOfxStatErrBadIndex`; the parameter
/// is unchanged in that case.
///
/// # Safety
///
/// `param` must be a live param handle passed by the host; all other
/// parameters are passed by value with no additional pointer preconditions.
unsafe extern "C" fn parametric_param_set_nth_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	nth: c_int,
	key: c_double,
	value: c_double,
	_add_animation_key: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		ensure_curve(&mut curves, ci, range_of(param))
			.set_nth(nth, key, value)
			.map_err(|()| status::ERR_BAD_INDEX)?;
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// `parametricParamAddControlPoint`: add the control point (key, value) to
/// the `curve_index`-th curve.
///
/// An existing identical key is overwritten — the SDK header says "close
/// enough keys are overwritten", but this implementation only recognizes an
/// exact-equal key; otherwise the point is inserted in ascending key order.
/// Writes go through [`write_curves`], and on success the host receives
/// `instanceChanged`. Both `time` and `addAnimationKey` are ignored (no
/// parameter animation).
///
/// Failure: bad handle / non-parametric parameter → `kOfxStatErrBadHandle`;
/// out-of-range `curve_index` → `kOfxStatErrBadIndex`; the parameter is
/// unchanged in that case.
///
/// # Safety
///
/// `param` must be a live param handle passed by the host; the remaining
/// parameters are passed by value.
unsafe extern "C" fn parametric_param_add_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	key: c_double,
	value: c_double,
	_add_animation_key: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		ensure_curve(&mut curves, ci, range_of(param)).upsert(key, value);
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// `parametricParamDeleteControlPoint`: delete the `nth` control point of the
/// `curve_index`-th curve.
///
/// `nth` is the ordinal in ascending key order. After the deletion the
/// adjacent slopes are recomputed ([`Curve::delete_nth`]), writes go through
/// [`write_curves`], and on success the host receives `instanceChanged`. Both
/// `time` and `addAnimationKey` are ignored (no parameter animation).
///
/// Failure: bad handle / non-parametric parameter → `kOfxStatErrBadHandle`;
/// out-of-range `curve_index` or `nth` → `kOfxStatErrBadIndex`; the curve is
/// unchanged in that case.
///
/// # Safety
///
/// `param` must be a live param handle passed by the host; the remaining
/// parameters are passed by value.
unsafe extern "C" fn parametric_param_delete_control_point(
	param: *mut c_void,
	curve_index: c_int,
	nth: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		ensure_curve(&mut curves, ci, range_of(param))
			.delete_nth(nth)
			.map_err(|()| status::ERR_BAD_INDEX)?;
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// `parametricParamDeleteAllControlPoints`: clear all control points of the
/// `curve_index`-th curve.
///
/// After clearing, evaluation of that curve degrades to the identity (the
/// empty-curve branch of [`Curve::evaluate`]) instead of reporting an error;
/// written back, the state matches "never written". Writes go through
/// [`write_curves`], and on success the host receives `instanceChanged`. Both
/// `time` and `addAnimationKey` are ignored (no parameter animation).
///
/// Failure: bad handle / non-parametric parameter → `kOfxStatErrBadHandle`;
/// out-of-range `curve_index` → `kOfxStatErrBadIndex`; the curve is unchanged
/// in that case.
///
/// # Safety
///
/// `param` must be a live param handle passed by the host; the remaining
/// parameters are passed by value.
unsafe extern "C" fn parametric_param_delete_all_control_points(
	param: *mut c_void,
	curve_index: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		ensure_curve(&mut curves, ci, range_of(param)).clear();
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// Static suite instance.
pub fn suite_v1() -> &'static ParametricParameterSuiteV1 {
	static SUITE: sync::OnceLock<ParametricParameterSuiteV1> = sync::OnceLock::new();
	SUITE.get_or_init(|| ParametricParameterSuiteV1 {
		parametric_param_get_value,
		parametric_param_get_n_control_points,
		parametric_param_get_nth_control_point,
		parametric_param_set_nth_control_point,
		parametric_param_add_control_point,
		parametric_param_delete_control_point,
		parametric_param_delete_all_control_points,
	})
}

#[cfg(test)]
mod tests;
