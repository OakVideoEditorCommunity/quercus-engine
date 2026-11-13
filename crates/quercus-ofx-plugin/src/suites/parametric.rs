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
use std::ffi::{c_double, c_int, c_void};

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
	pub parametric_param_get_value: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_double,
		*mut c_double,
	) -> c_int,
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
	pub parametric_param_add_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_double,
		c_double,
		c_int,
	) -> c_int,
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
			tag::PARAM_INSTANCE => Ok(ParamRef::Instance(&*(
				tag::strip(handle) as *const ParamInstance
			))),
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
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if code != status::OK && std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
	static SUITE: std::sync::OnceLock<ParametricParameterSuiteV1> = std::sync::OnceLock::new();
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
mod tests {
	use super::*;
	use std::ffi::{c_char, CString};
	use std::sync::Arc;

	use crate::descriptor::EffectDescriptor;
	use crate::host::Plugin;
	use crate::instance::Instance;
	use crate::param::{ParamInstance, ParamSetInstance};
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(&d.props as *const PropertySet, tag::DESCRIPTOR)
	}

	/// Fake plugin (a host::Plugin is constructed only to feed an Arc to
	/// Instance; fields other than describe are not touched by this test).
	fn dummy_plugin(descriptor: EffectDescriptor) -> Arc<Plugin> {
		unsafe extern "C" fn dummy_entry(
			_: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> c_int {
			status::OK
		}
		Arc::new(Plugin {
			identifier: "test.plugin".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor,
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		})
	}

	fn instance_handle(i: &Instance) -> *mut c_void {
		tag::make(&i.props as *const PropertySet, tag::INSTANCE)
	}

	/// Define a parametric parameter "curve" during describe (default
	/// dimension 1); returns the descriptor and its param handle.
	fn define_parametric() -> (EffectDescriptor, *mut c_void) {
		let desc = EffectDescriptor::new();
		let s = crate::suites::param::suite_v1();
		let dhandle = descriptor_handle(&desc);
		let t = cs(crate::param::TYPE_PARAMETRIC);
		let n = cs("curve");
		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph), 0);
		}
		assert_eq!(tag::kind(ph), tag::PARAM_DEF);
		(desc, ph)
	}

	/// Instantiate the describe product (copy default → instance value);
	/// returns (instance Arc, instance handle, instance param handle).
	fn instantiate(desc: EffectDescriptor) -> (Arc<Instance>, *mut c_void, *mut c_void) {
		let params = ParamSetInstance {
			params: desc
				.params
				.iter()
				.map(|d| Box::new(ParamInstance::from_def((**d).clone())))
				.collect(),
		};
		let plugin = dummy_plugin(desc);
		let inst = Arc::new(Instance {
			props: PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: vec![],
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		});
		let ih = instance_handle(&inst);
		let s = crate::suites::param::suite_v1();
		let n = cs("curve");
		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_get_handle)(ih, n.as_ptr(), &mut ph, std::ptr::null_mut()), 0);
		}
		assert_eq!(tag::kind(ph), tag::PARAM_INSTANCE);
		(inst, ih, ph)
	}

	/// Define during describe and instantiate in one step (default dimension
	/// 1).
	fn make_instance() -> (Arc<Instance>, *mut c_void, *mut c_void) {
		let (desc, _dph) = define_parametric();
		instantiate(desc)
	}

	fn ps() -> &'static ParametricParameterSuiteV1 {
		suite_v1()
	}

	/// Describe-phase defaults: 2 identity control points (0,0)/(1,1);
	/// evaluation = identity.
	#[test]
	fn describe_defaults_are_identity() {
		let (_desc, ph) = define_parametric();
		let s = ps();
		let mut n = -1;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 0, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.0, 0.0));
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (1.0, 1.0));
		// 恒等求值（越界钳制端点值）。
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
		}
		assert_eq!(out, 0.5);
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, -3.0, &mut out), 0);
			assert_eq!(out, 0.0);
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 7.0, &mut out), 0);
			assert_eq!(out, 1.0);
		}
	}

	/// Set/Add/Delete during describe change the **default value**, copied to
	/// the instance at instantiation.
	#[test]
	fn descriptor_edits_become_instance_defaults() {
		let (desc, ph) = define_parametric();
		let s = ps();
		// describe 期改默认：加两个点，改第 1 个点的值。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.25, 0.5, 0), 0);
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.75, 0.9, 0), 0);
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.25, 0.6, 0), 0);
		}
		// 默认值本身生效。
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.6);
		// 实例化：默认曲线随 from_def 复制。
		let (_inst, _ih, iph) = instantiate(desc);
		unsafe {
			assert_eq!((s.parametric_param_get_value)(iph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.6);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(iph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 4); // (0,0) (0.25,0.6) (0.75,0.9) (1,1)
	}

	/// Full instance-phase chain: Add → Set → Delete → DeleteAll, with
	/// evaluation following the model.
	#[test]
	fn instance_add_set_delete_eval_chain() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();

		// Add 中点 (0.3, 0.7)：插值性质 + 有序。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.3, 0.7, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.3, &mut out), 0);
		}
		assert_eq!(out, 0.7);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3);

		// SetNth 改 (0.3,0.7) → (0.4,0.6)。
		unsafe {
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.4, 0.6, 0), 0);
		}
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.4, 0.6));
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.4, &mut out), 0);
		}
		assert_eq!(out, 0.6);

		// Delete 第 1 点 → 回到双点。
		unsafe {
			assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
		}
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);

		// DeleteAll → 0 点，求值退化为恒等。
		unsafe {
			assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
		}
		assert_eq!(n, 0);
		assert_eq!(out, 0.5);
	}

	/// SetNth with a changed key breaks ordering → automatic re-sort (the
	/// semantics the header warns about).
	#[test]
	fn set_nth_reorders_on_key_change() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.5, 0), 0);
			// 把第 0 点 (0,0) 挪到 0.75 → 序列变 (0.5) (0.75) (1)。
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 0, 0.75, 0.75, 0), 0);
		}
		let (mut k, mut v) = (-1.0, -1.0);
		for (i, (ek, ev)) in [(0.5, 0.5), (0.75, 0.75), (1.0, 1.0)].iter().enumerate() {
			unsafe {
				assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, i as c_int, &mut k, &mut v), 0);
			}
			assert_eq!((k, v), (*ek, *ev), "nth {i}");
		}
	}

	/// Add with the same key → overwrite (no new point).
	#[test]
	fn add_overwrites_same_key() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.4, 0), 0);
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.9, 0), 0);
		}
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3); // (0,0) (0.5,0.9) (1,1)
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.5, 0.9));
	}

	/// Error codes: bad handle / non-parametric / out-of-range curveIndex /
	/// out-of-range nth / null out.
	#[test]
	fn error_codes() {
		let s = ps();
		let mut out = 0.0;
		// 空句柄 → BadHandle。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(std::ptr::null_mut(), 0, 0.0, 0.5, &mut out),
				status::ERR_BAD_HANDLE
			);
		}
		// 空 out → ErrValue。
		let (_inst, _ih, ph) = make_instance();
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 0, 0.0, 0.5, std::ptr::null_mut()),
				status::ERR_VALUE
			);
		}
		// curveIndex 越界（维度 1）→ BadIndex。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 1, 0.0, 0.5, &mut out),
				status::ERR_BAD_INDEX
			);
		}
		// nth 越界（2 点曲线）→ BadIndex。
		let mut k = 0.0;
		unsafe {
			assert_eq!(
				(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 5, &mut k, &mut out),
				status::ERR_BAD_INDEX
			);
		}
		// 非 parametric 参数 → BadHandle（Integer 参数的句柄）。
		let desc = EffectDescriptor::new();
		let psuite = crate::suites::param::suite_v1();
		let dhandle = descriptor_handle(&desc);
		let t = cs("OfxParamTypeInteger");
		let n = cs("gain");
		let mut gph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut gph), 0);
			// parametric 也定义一个，便于实例化后拿两个 handle。
			let pt = cs(crate::param::TYPE_PARAMETRIC);
			let pn = cs("curve");
			let mut pph: *mut c_void = std::ptr::null_mut();
			assert_eq!((psuite.param_define)(dhandle, pt.as_ptr(), pn.as_ptr(), &mut pph), 0);
			let params = ParamSetInstance {
				params: desc
					.params
					.iter()
					.map(|d| Box::new(ParamInstance::from_def((**d).clone())))
					.collect(),
			};
			let plugin = dummy_plugin(desc);
			let inst = Arc::new(Instance {
				props: PropertySet::new(),
				plugin,
				context: "OfxImageEffectContextFilter".into(),
				params,
				clips: vec![],
				node_identity: std::sync::atomic::AtomicUsize::new(0),
				destroyed: std::sync::atomic::AtomicBool::new(false),
				sequence_range: std::sync::Mutex::new(None),
				progress_cb: std::sync::Mutex::new(None),
				cancel: std::sync::atomic::AtomicBool::new(false),
				edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
				render_lock: std::sync::Mutex::new(()),
				interact: std::sync::Mutex::new(None),
			});
			let ih = instance_handle(&inst);
			let mut g: *mut c_void = std::ptr::null_mut();
			let gn = cs("gain");
			assert_eq!((psuite.param_get_handle)(ih, gn.as_ptr(), &mut g, std::ptr::null_mut()), 0);
			assert_eq!(
				(s.parametric_param_get_value)(g, 0, 0.0, 0.5, &mut out),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.parametric_param_add_control_point)(g, 0, 0.0, 0.5, 0.5, 0),
				status::ERR_BAD_HANDLE
			);
		}
	}

	/// Driven by the dimension property: with dimension=2 the second
	/// dimension works and the third is BadIndex; unconfigured curves read as
	/// the identity default.
	#[test]
	fn dimension_prop_drives_curves() {
		let (desc, ph) = define_parametric();
		// 插件经属性 suite 把维度改成 2。
		let psuite = crate::suites::property::suite_v1();
		let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
		unsafe {
			assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
		}
		let s = ps();
		// 第二维：Add 后求值可用。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 1, 0.0, 0.25, 0.5, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 1, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.5);
		// 第一维未配置 → 恒等默认。
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.25);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);
		// 第三维 → BadIndex。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 2, 0.0, 0.5, &mut out),
				status::ERR_BAD_INDEX
			);
			assert_eq!(
				(s.parametric_param_add_control_point)(ph, 2, 0.0, 0.5, 0.5, 0),
				status::ERR_BAD_INDEX
			);
		}
		// 维度属性随 from_def 复制到实例（实例句柄上同样生效）。
		let (_inst, _ih, iph) = instantiate(desc);
		let mut n = -1;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(iph, 1, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3); // (0,0) (0.25,0.5) (1,1)
	}

	/// The range property feeds the identity default of unconfigured curves
	/// (endpoints = range values).
	#[test]
	fn range_prop_shapes_implicit_identity() {
		let (_desc, ph) = define_parametric();
		let psuite = crate::suites::property::suite_v1();
		let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
		let range = cs(crate::param::P_PARAMETRIC_RANGE);
		unsafe {
			assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
			// range = (0, 255)。
			assert_eq!((psuite.set_double)(ph, range.as_ptr(), 0, 0.0), 0);
			assert_eq!((psuite.set_double)(ph, range.as_ptr(), 1, 255.0), 0);
		}
		let s = ps();
		// 未配置的第二维：恒等默认端点 = range。
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 1, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (255.0, 255.0));
	}

	/// UI properties (UIColour double×3N / InteractBackground pointer) go
	/// through the generic property suite set/get and are copied to the
	/// instance by from_def.
	#[test]
	fn ui_props_set_get_and_copy() {
		let (desc, ph) = define_parametric();
		let psuite = crate::suites::property::suite_v1();
		// UIColour：3 个 double（维度 1 → 1 个 RGB 三元组）。
		let colour = cs(crate::param::P_PARAMETRIC_UI_COLOUR);
		let rgb: [f64; 3] = [1.0, 0.0, 0.5];
		unsafe {
			assert_eq!((psuite.set_double_n)(ph, colour.as_ptr(), 3, rgb.as_ptr()), 0);
		}
		// InteractBackground：任意指针。
		let bg = cs(crate::param::P_PARAMETRIC_INTERACT_BG);
		let sentinel = 0x1234usize as *mut c_void;
		unsafe {
			assert_eq!((psuite.set_pointer)(ph, bg.as_ptr(), 0, sentinel), 0);
		}
		// 读回。
		let mut got = [0.0; 3];
		unsafe {
			assert_eq!((psuite.get_double_n)(ph, colour.as_ptr(), 3, got.as_mut_ptr()), 0);
		}
		assert_eq!(got, rgb);
		let mut gotp: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.get_pointer)(ph, bg.as_ptr(), 0, &mut gotp), 0);
		}
		assert_eq!(gotp, sentinel);
		// 实例 props 深拷贝。
		let (_inst, _ih, iph) = instantiate(desc);
		let mut got2 = [0.0; 3];
		unsafe {
			assert_eq!((psuite.get_double_n)(iph, colour.as_ptr(), 3, got2.as_mut_ptr()), 0);
		}
		assert_eq!(got2, rgb);
		let mut gotp2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.get_pointer)(iph, bg.as_ptr(), 0, &mut gotp2), 0);
		}
		assert_eq!(gotp2, sentinel);
	}

	/// Instance edits go through the instance-changed notify path (with no
	/// node bound it is a no-op; passing means it does not panic; the
	/// throttling semantics of the registration path are covered by the param
	/// suite tests).
	#[test]
	fn instance_edits_go_through_notify_path() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.2, 0.8, 0), 0);
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.2, 0.9, 0), 0);
			assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
			assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.2, &mut out), 0);
		}
		assert_eq!(out, 0.2);
	}
}
